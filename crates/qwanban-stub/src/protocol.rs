//! The bootstrap protocol frames + codec (§stub-loader). Length-prefixed:
//! `[4-byte big-endian len][JSON body]`. Binary payloads (PUSH_AGENT/WriteFile
//! file bytes) are sent as a separate length-prefixed blob after the control
//! frame, read by `read_payload`. Pure codec, unit-testable over a duplex stream.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    Hello,
    Auth,
    PushAgent,
    WriteFile,
    Launch,
    Ack,
    Stream,
    Exit,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub stub_version: u32,
    pub os: qwanban_proto::broker::GuestOs,
    pub arch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushAgent {
    pub sha256: String,
    pub len: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteFile {
    pub path: String,
    pub mode: String,
    pub len: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Launch {
    pub command: String,
    pub shell: String,
    pub cwd: String,
    pub env: std::collections::BTreeMap<String, String>,
}

/// A decoded protocol frame. Serializes as a tagged JSON object so the codec is
/// a single length-prefixed JSON line per control frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Frame {
    Hello(Hello),
    Auth { case_bootstrap_secret: String },
    PushAgent(PushAgent),
    WriteFile(WriteFile),
    Launch(Launch),
    Ack { ok: bool, detail: String },
    Stream { fd: u8, bytes: Vec<u8> },
    Exit { code: i32 },
    Error { message: String },
}

impl Frame {
    /// Does this frame carry a following binary payload of `len` bytes?
    pub fn payload_len(&self) -> Option<u64> {
        match self {
            Frame::PushAgent(p) => Some(p.len),
            Frame::WriteFile(w) => Some(w.len),
            _ => None,
        }
    }
}

pub fn is_ok(ack: &Frame) -> bool {
    matches!(ack, Frame::Ack { ok: true, .. })
}

/// Write a control frame as `[4-byte BE len][JSON]`.
pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, frame: &Frame) -> std::io::Result<()> {
    let json = serde_json::to_vec(frame).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    let len = json.len() as u32;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(&json).await?;
    w.flush().await
}

/// Read a control frame.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Frame> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    serde_json::from_slice(&buf).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Write a binary payload blob: `[4-byte BE len][bytes]`.
pub async fn write_payload<W: AsyncWrite + Unpin>(w: &mut W, bytes: &[u8]) -> std::io::Result<()> {
    let len = bytes.len() as u32;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(bytes).await?;
    w.flush().await
}

/// Read a binary payload blob; the len prefix must match `expected`.
pub async fn read_payload<R: AsyncRead + Unpin>(r: &mut R, expected: u64) -> std::io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as u64;
    if len != expected {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("payload len mismatch: header said {expected}, blob said {len}"),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_roundtrips_json() {
        let h = Hello { stub_version: 1, os: qwanban_proto::broker::GuestOs::Linux, arch: "x86_64".into() };
        let s = serde_json::to_string(&Frame::Hello(h.clone())).unwrap();
        let f: Frame = serde_json::from_str(&s).unwrap();
        match f {
            Frame::Hello(h2) => assert_eq!(h2.stub_version, 1),
            _ => panic!("wrong variant"),
        }
    }

    #[tokio::test]
    async fn frame_roundtrips_over_duplex() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        let f = Frame::Ack { ok: true, detail: "hi".into() };
        write_frame(&mut client, &f).await.unwrap();
        let f2 = read_frame(&mut server).await.unwrap();
        assert!(is_ok(&f2));
    }

    #[tokio::test]
    async fn payload_roundtrips() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        let data = b"binary agent bytes here";
        write_payload(&mut client, data).await.unwrap();
        let got = read_payload(&mut server, data.len() as u64).await.unwrap();
        assert_eq!(got, data);
    }

    #[tokio::test]
    async fn payload_len_mismatch_detected() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        write_payload(&mut client, b"short").await.unwrap();
        let err = read_payload(&mut server, 999).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }
}
