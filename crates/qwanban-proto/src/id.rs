//! Typed identifiers (components README §S3). Newtypes wrap strings to prevent
//! mixing a `JobId` with a `CaseId` at the type level. All serialize as plain
//! strings.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Macro to declare a string-backed id newtype with serde + display + from_str.
macro_rules! id_type {
    ($name:ident, $prefix:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// Generate a fresh id with the type's prefix.
            pub fn new() -> Self {
                Self(format!("{}_{}", $prefix, short_random_suffix()))
            }
            pub fn from_str_inner(s: &str) -> Self {
                Self(s.to_string())
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
        impl FromStr for $name {
            type Err = std::convert::Infallible;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok(Self(s.to_string()))
            }
        }
        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }
        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }
    };
}

id_type!(JobId, "job");
id_type!(CaseId, "case");
id_type!(BreadcrumbId, "bc");
id_type!(ClipId, "clip");
id_type!(VideoSegmentId, "seg");
id_type!(InputEventId, "evt");
id_type!(CheckpointId, "ckpt");

/// A short, lowercase-hex random suffix for generated ids. Uses a simple
/// thread-local counter + timestamp + thread id to avoid pulling in uuid here
/// (ids only need to be unique-enough for a case, not globally unique).
fn short_random_suffix() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    format!("{t:012x}{n:04x}")
}

/// A VM identifier as Hyper-V knows it (the GUID).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VmId(pub String);

impl VmId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for VmId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
