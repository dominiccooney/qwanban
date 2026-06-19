//! Host allowlist (§mitm-proxy). Independent of the rewrite table. Exact match
//! beats suffix; unknown host ⇒ 403 Blocked.

#[derive(Debug, Clone)]
pub struct HostRule {
    pub match_: HostMatch,
    pub allow_methods: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum HostMatch {
    Exact(String),
    Suffix(String),
}

#[derive(Debug, Clone, Default)]
pub struct Allowlist {
    pub rules: Vec<HostRule>,
}

impl Allowlist {
    /// Find the matching rule for `host`; exact beats suffix.
    pub fn match_rule(&self, host: &str) -> Option<&HostRule> {
        // exact first
        if let Some(r) = self
            .rules
            .iter()
            .find(|r| matches!(&r.match_, HostMatch::Exact(h) if h == host))
        {
            return Some(r);
        }
        // then suffix (longest suffix wins for determinism)
        let mut best: Option<(&HostRule, usize)> = None;
        for r in &self.rules {
            if let HostMatch::Suffix(s) = &r.match_ {
                if host.ends_with(s) {
                    let len = s.len();
                    if best.map(|(_, l)| len > l).unwrap_or(true) {
                        best = Some((r, len));
                    }
                }
            }
        }
        best.map(|(r, _)| r)
    }

    pub fn is_allowed(&self, host: &str, method: &str) -> bool {
        match self.match_rule(host) {
            None => false,
            Some(r) => {
                r.allow_methods.is_empty()
                    || r
                        .allow_methods
                        .iter()
                        .any(|m| m.eq_ignore_ascii_case(method))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn al() -> Allowlist {
        Allowlist {
            rules: vec![
                HostRule {
                    match_: HostMatch::Exact("api.github.com".into()),
                    allow_methods: vec!["GET".into(), "POST".into()],
                },
                HostRule {
                    match_: HostMatch::Suffix("blob.core.windows.net".into()),
                    allow_methods: vec!["GET".into()],
                },
            ],
        }
    }

    #[test]
    fn exact_beats_suffix() {
        let a = al();
        assert!(a.is_allowed("api.github.com", "GET"));
        assert!(!a.is_allowed("api.github.com", "DELETE"));
    }

    #[test]
    fn suffix_matches_subdomains() {
        let a = al();
        assert!(a.is_allowed("foo.blob.core.windows.net", "GET"));
        assert!(!a.is_allowed("foo.blob.core.windows.net", "POST"));
    }

    #[test]
    fn unknown_host_denied() {
        let a = al();
        assert!(!a.is_allowed("evil.example.com", "GET"));
    }
}
