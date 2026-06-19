//! The parsed `secrets.toml` snapshot + types.

use serde::{Deserialize, Serialize};

/// A real secret value (zeroizing on drop is out of scope for v1 plain-text).
#[derive(Debug, Clone)]
pub struct RealSecret(pub String);

impl RealSecret {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One row of the search→replace table: a unique dummy string → a real secret name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewriteEntry {
    /// The unique, real-looking dummy string the guest carries.
    pub search: String,
    /// Name of a `[real]` entry to substitute.
    pub replace: String,
}

/// The parsed `secrets.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecretsFile {
    /// `name = "value"` real secrets.
    #[serde(default)]
    pub real: std::collections::BTreeMap<String, String>,
    /// The dummy→secret search→replace table.
    #[serde(default)]
    pub rewrite: Vec<RewriteEntry>,
}

/// An immutable snapshot held behind an `ArcSwap` for hot-reload.
#[derive(Debug, Clone)]
pub struct SecretSnapshot {
    pub real: std::collections::BTreeMap<String, String>,
    pub rewrite: Vec<RewriteEntry>,
}

impl SecretSnapshot {
    pub fn from_file(file: SecretsFile) -> qwanban_proto::QwanResult<Self> {
        // Validate: every replace resolves; every search is unique.
        let mut seen = std::collections::HashSet::new();
        for e in &file.rewrite {
            if !file.real.contains_key(&e.replace) {
                return Err(qwanban_proto::invalid_arg(format!(
                    "rewrite '{}' -> unknown real secret '{}'",
                    e.search, e.replace
                )));
            }
            if !seen.insert(e.search.as_str()) {
                return Err(qwanban_proto::invalid_arg(format!(
                    "duplicate dummy search string: '{}'",
                    e.search
                )));
            }
        }
        Ok(Self {
            real: file.real,
            rewrite: file.rewrite,
        })
    }

    pub fn secret(&self, name: &str) -> Option<RealSecret> {
        self.real.get(name).map(|v| RealSecret(v.clone()))
    }
}

impl SecretsFile {
    pub fn parse(toml_str: &str) -> qwanban_proto::QwanResult<Self> {
        toml::from_str(toml_str).map_err(|e| {
            qwanban_proto::QwanError::new(
                qwanban_proto::QwanCode::InvalidArg,
                format!("secrets.toml parse: {e}"),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_validate_ok() {
        let s = r#"
[real]
github_token = "ghp_REAL"
openai_key = "sk-REAL"

[[rewrite]]
search = "ghp_qwanDUMMY01"
replace = "github_token"

[[rewrite]]
search = "sk-qwanDUMMY7c"
replace = "openai_key"
"#;
        let f = SecretsFile::parse(s).unwrap();
        let snap = SecretSnapshot::from_file(f).unwrap();
        assert_eq!(snap.secret("github_token").unwrap().as_str(), "ghp_REAL");
        assert_eq!(snap.rewrite.len(), 2);
    }

    #[test]
    fn rejects_unresolved_replace() {
        let f = SecretsFile {
            real: Default::default(),
            rewrite: vec![RewriteEntry {
                search: "x".into(),
                replace: "missing".into(),
            }],
        };
        assert!(SecretSnapshot::from_file(f).is_err());
    }

    #[test]
    fn rejects_duplicate_search() {
        let mut real = std::collections::BTreeMap::new();
        real.insert("k".into(), "v".into());
        let f = SecretsFile {
            real,
            rewrite: vec![
                RewriteEntry { search: "dup".into(), replace: "k".into() },
                RewriteEntry { search: "dup".into(), replace: "k".into() },
            ],
        };
        assert!(SecretSnapshot::from_file(f).is_err());
    }
}
