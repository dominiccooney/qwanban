//! The stub `serve()` loop (stub-loader). Drives the bootstrap protocol over a
//! single bidirectional byte stream (TCP in prod, tokio duplex in tests):
//! HELLO -> AUTH -> command loop (PUSH_AGENT/WriteFile/LAUNCH) -> relay stdout/stderr/exit.

use crate::protocol::*;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use tokio::io::AsyncReadExt;

#[derive(Debug, Clone)]
pub struct ServeConfig {
    pub work_dir: PathBuf,
    pub case_bootstrap_secret: String,
    pub expected_stub_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServeOutcome {
    ChildExited(i32),
    HostClosed,
    AuthRejected,
    VersionMismatch { got: u32, want: u32 },
}

/// Serve one bootstrap session over a single bidirectional stream.
pub async fn serve<S>(mut stream: S, cfg: &ServeConfig) -> std::io::Result<ServeOutcome>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    // 1. HELLO
    let hello = match read_frame(&mut stream).await {
        Ok(Frame::Hello(h)) => h,
        Ok(_) => {
            let _ = write_frame(&mut stream, &Frame::Error { message: "expected HELLO".into() }).await;
            return Ok(ServeOutcome::HostClosed);
        }
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(ServeOutcome::HostClosed),
        Err(e) => return Err(e),
    };
    if hello.stub_version != cfg.expected_stub_version {
        let _ = write_frame(
            &mut stream,
            &Frame::Error {
                message: format!("stub_version mismatch: got {} want {}", hello.stub_version, cfg.expected_stub_version),
            },
        )
        .await;
        return Ok(ServeOutcome::VersionMismatch { got: hello.stub_version, want: cfg.expected_stub_version });
    }

    // 2. AUTH
    match read_frame(&mut stream).await? {
        Frame::Auth { case_bootstrap_secret } if case_bootstrap_secret == cfg.case_bootstrap_secret => {
            write_frame(&mut stream, &Frame::Ack { ok: true, detail: "authed".into() }).await?;
        }
        _ => {
            write_frame(&mut stream, &Frame::Ack { ok: false, detail: "bad secret".into() }).await?;
            return Ok(ServeOutcome::AuthRejected);
        }
    }

    // 3. Command loop
    loop {
        let frame = match read_frame(&mut stream).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(ServeOutcome::HostClosed),
            Err(e) => return Err(e),
        };
        match frame {
            Frame::PushAgent(p) => {
                let payload = read_payload(&mut stream, p.len).await?;
                let hash = hex_sha256(&payload);
                if hash != p.sha256 {
                    write_frame(&mut stream, &Frame::Ack { ok: false, detail: "hash_mismatch".into() }).await?;
                    continue;
                }
                let dest = cfg.work_dir.join("qwan-guest");
                tokio::fs::write(&dest, &payload).await?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755));
                }
                write_frame(&mut stream, &Frame::Ack { ok: true, detail: hash }).await?;
            }
            Frame::WriteFile(w) => {
                let payload = read_payload(&mut stream, w.len).await?;
                let dest = if std::path::Path::new(&w.path).is_absolute() {
                    PathBuf::from(&w.path)
                } else {
                    cfg.work_dir.join(&w.path)
                };
                if let Some(parent) = dest.parent() {
                    let _ = tokio::fs::create_dir_all(parent).await;
                }
                tokio::fs::write(&dest, &payload).await?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(mode) = u32::from_str_radix(w.mode.trim_start_matches("0o"), 8) {
                        let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(mode));
                    }
                }
                write_frame(&mut stream, &Frame::Ack { ok: true, detail: w.path }).await?;
            }
            Frame::Launch(l) => {
                write_frame(&mut stream, &Frame::Ack { ok: true, detail: "launched".into() }).await?;
                let code = spawn_and_relay(&l, &cfg.work_dir, &mut stream).await?;
                write_frame(&mut stream, &Frame::Exit { code }).await?;
                return Ok(ServeOutcome::ChildExited(code));
            }
            Frame::Error { message } => {
                tracing::warn!("host sent error: {message}");
                return Ok(ServeOutcome::HostClosed);
            }
            other => {
                let _ = write_frame(&mut stream, &Frame::Error { message: format!("unexpected frame: {other:?}") }).await;
            }
        }
    }
}

async fn spawn_and_relay<S: tokio::io::AsyncWrite + Unpin>(
    l: &Launch,
    work_dir: &std::path::Path,
    write: &mut S,
) -> std::io::Result<i32> {
    let mut cmd = if l.shell == "cmd" || l.shell == "powershell" {
        let mut c = tokio::process::Command::new(&l.shell);
        c.arg("/C").arg(&l.command);
        c
    } else {
        let mut c = tokio::process::Command::new(&l.shell);
        c.arg("-c").arg(&l.command);
        c
    };
    cmd.current_dir(if l.cwd.is_empty() {
        work_dir
    } else {
        let p = std::path::Path::new(&l.cwd);
        if p.exists() { p } else { work_dir }
    });
    for (k, v) in &l.env {
        cmd.env(k, v);
    }
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null());
    let mut child = cmd.spawn()?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let mut buf = [0u8; 4096];
    loop {
        match stdout.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => write_frame(write, &Frame::Stream { fd: 1, bytes: buf[..n].to_vec() }).await?,
            Err(_) => break,
        }
    }
    loop {
        match stderr.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => write_frame(write, &Frame::Stream { fd: 2, bytes: buf[..n].to_vec() }).await?,
            Err(_) => break,
        }
    }
    let status = child.wait().await?;
    Ok(status.code().unwrap_or(-1))
}

fn hex_sha256(b: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(b);
    let out = h.finalize();
    let mut s = String::with_capacity(64);
    for byte in out {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    fn cfg(dir: &std::path::Path) -> ServeConfig {
        ServeConfig { work_dir: dir.to_path_buf(), case_bootstrap_secret: "bootstrap-secret".into(), expected_stub_version: 1 }
    }
    fn hello() -> Frame {
        Frame::Hello(Hello { stub_version: 1, os: qwanban_proto::broker::GuestOs::Linux, arch: "x86_64".into() })
    }

    #[tokio::test]
    async fn auth_rejects_bad_secret() {
        let dir = tempfile::tempdir().unwrap();
        let (mut client, server) = duplex(4096);
        let cfg = cfg(dir.path());
        let t = tokio::spawn(async move { serve(server, &cfg).await });
        write_frame(&mut client, &hello()).await.unwrap();
        write_frame(&mut client, &Frame::Auth { case_bootstrap_secret: "WRONG".into() }).await.unwrap();
        let ack = read_frame(&mut client).await.unwrap();
        assert!(!is_ok(&ack));
        assert_eq!(t.await.unwrap().unwrap(), ServeOutcome::AuthRejected);
    }

    #[tokio::test]
    async fn version_mismatch_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let (mut client, server) = duplex(4096);
        let cfg = cfg(dir.path());
        let t = tokio::spawn(async move { serve(server, &cfg).await });
        write_frame(&mut client, &Frame::Hello(Hello { stub_version: 99, os: qwanban_proto::broker::GuestOs::Linux, arch: "x86_64".into() })).await.unwrap();
        assert_eq!(t.await.unwrap().unwrap(), ServeOutcome::VersionMismatch { got: 99, want: 1 });
    }

    #[tokio::test]
    async fn push_agent_correct_hash_acks() {
        let dir = tempfile::tempdir().unwrap();
        let (mut client, server) = duplex(8192);
        let cfg = cfg(dir.path());
        let t = tokio::spawn(async move { serve(server, &cfg).await });
        write_frame(&mut client, &hello()).await.unwrap();
        write_frame(&mut client, &Frame::Auth { case_bootstrap_secret: "bootstrap-secret".into() }).await.unwrap();
        assert!(is_ok(&read_frame(&mut client).await.unwrap()));
        let agent_bytes = b"#!/bin/sh\necho hi\n";
        let hash = hex_sha256(agent_bytes);
        write_frame(&mut client, &Frame::PushAgent(PushAgent { sha256: hash, len: agent_bytes.len() as u64 })).await.unwrap();
        write_payload(&mut client, agent_bytes).await.unwrap();
        assert!(is_ok(&read_frame(&mut client).await.unwrap()));
        assert_eq!(std::fs::read(dir.path().join("qwan-guest")).unwrap(), agent_bytes);
        drop(client);
        let _ = t.await;
    }

    #[tokio::test]
    async fn push_agent_wrong_hash_acks_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let (mut client, server) = duplex(8192);
        let cfg = cfg(dir.path());
        let t = tokio::spawn(async move { serve(server, &cfg).await });
        write_frame(&mut client, &hello()).await.unwrap();
        write_frame(&mut client, &Frame::Auth { case_bootstrap_secret: "bootstrap-secret".into() }).await.unwrap();
        let _ = read_frame(&mut client).await.unwrap();
        let agent_bytes = b"agent bytes";
        write_frame(&mut client, &Frame::PushAgent(PushAgent { sha256: "deadbeef".into(), len: agent_bytes.len() as u64 })).await.unwrap();
        write_payload(&mut client, agent_bytes).await.unwrap();
        assert!(!is_ok(&read_frame(&mut client).await.unwrap()));
        assert!(!dir.path().join("qwan-guest").exists());
        drop(client);
        let _ = t.await;
    }

    #[tokio::test]
    async fn write_file_writes_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let (mut client, server) = duplex(8192);
        let cfg = cfg(dir.path());
        let t = tokio::spawn(async move { serve(server, &cfg).await });
        write_frame(&mut client, &hello()).await.unwrap();
        write_frame(&mut client, &Frame::Auth { case_bootstrap_secret: "bootstrap-secret".into() }).await.unwrap();
        let _ = read_frame(&mut client).await.unwrap();
        let content = b"{\"manifest\":\"v1\"}";
        write_frame(&mut client, &Frame::WriteFile(WriteFile { path: "manifest.json".into(), mode: "0644".into(), len: content.len() as u64 })).await.unwrap();
        write_payload(&mut client, content).await.unwrap();
        assert!(is_ok(&read_frame(&mut client).await.unwrap()));
        assert_eq!(std::fs::read(dir.path().join("manifest.json")).unwrap(), content);
        drop(client);
        let _ = t.await;
    }

    #[tokio::test]
    async fn launch_relays_stdout_and_exit() {
        let dir = tempfile::tempdir().unwrap();
        let (mut client, server) = duplex(8192);
        let cfg = cfg(dir.path());
        let t = tokio::spawn(async move { serve(server, &cfg).await });
        write_frame(&mut client, &hello()).await.unwrap();
        write_frame(&mut client, &Frame::Auth { case_bootstrap_secret: "bootstrap-secret".into() }).await.unwrap();
        let _ = read_frame(&mut client).await.unwrap();
        // OS-appropriate echo
        #[cfg(windows)]
        let (shell, command) = ("cmd", "echo hello");
        #[cfg(not(windows))]
        let (shell, command) = ("sh", "echo hello");
        write_frame(&mut client, &Frame::Launch(Launch { command: command.into(), shell: shell.into(), cwd: "".into(), env: Default::default() })).await.unwrap();
        let _ = read_frame(&mut client).await.unwrap(); // launch ack
        let mut got_stdout = String::new();
        let mut exit_code: Option<i32> = None;
        for _ in 0..10 {
            match read_frame(&mut client).await {
                Ok(Frame::Stream { fd: 1, bytes }) => got_stdout.push_str(&String::from_utf8_lossy(&bytes)),
                Ok(Frame::Exit { code }) => { exit_code = Some(code); break; }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        assert!(got_stdout.contains("hello"), "stdout was: {got_stdout:?}");
        assert_eq!(exit_code, Some(0));
        assert_eq!(t.await.unwrap().unwrap(), ServeOutcome::ChildExited(0));
    }
}
