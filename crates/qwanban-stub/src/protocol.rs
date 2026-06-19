//! The bootstrap protocol frames (§stub-loader). Length-prefixed; pure codec,
//! unit-testable over an in-memory duplex stream in the dev VM.

use serde::{Deserialize, Serialize};

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

/// A decoded protocol frame.
#[derive(Debug, Clone)]
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

pub fn is_ok(ack: &Frame) -> bool {
    matches!(ack, Frame::Ack { ok: true, .. })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_roundtrips_json() {
        let h = Hello {
            stub_version: 1,
            os: qwanban_proto::broker::GuestOs::Linux,
            arch: "x86_64".into(),
        };
        let s = serde_json::to_string(&h).unwrap();
        let h2: Hello = serde_json::from_str(&s).unwrap();
        assert_eq!(h2.stub_version, 1);
    }
}
