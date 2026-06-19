//! The search→replace engine. Scans bytes for known dummies and swaps them for
//! real secrets. **No header-format logic, no auto-injection** — only exact
//! dummy-byte matches are replaced; everything else passes through verbatim.

use crate::snapshot::SecretSnapshot;

/// Result of rewriting a byte buffer.
#[derive(Debug, Clone)]
pub struct RewriteResult {
    pub bytes: Vec<u8>,
    /// Which dummies (by index into the table) were matched.
    pub matched: Vec<usize>,
}

/// The pure rewriter over a snapshot.
pub struct Rewriter<'a> {
    snap: &'a SecretSnapshot,
}

impl<'a> Rewriter<'a> {
    pub fn new(snap: &'a SecretSnapshot) -> Self {
        Self { snap }
    }

    /// Scan `input` for any known dummy and replace each with the real secret.
    /// Returns the rewritten bytes + which table entries matched.
    pub fn rewrite(&self, input: &[u8]) -> RewriteResult {
        let mut out: Vec<u8> = Vec::with_capacity(input.len());
        let mut matched = Vec::new();
        let mut i = 0;
        while i < input.len() {
            let mut found = None;
            for (idx, e) in self.snap.rewrite.iter().enumerate() {
                let needle = e.search.as_bytes();
                if input[i..].starts_with(needle) {
                    found = Some((idx, needle.len()));
                    break;
                }
            }
            if let Some((idx, nlen)) = found {
                let real = self.snap.real.get(&self.snap.rewrite[idx].replace);
                if let Some(r) = real {
                    out.extend_from_slice(r.as_bytes());
                }
                matched.push(idx);
                i += nlen;
            } else {
                out.push(input[i]);
                i += 1;
            }
        }
        RewriteResult { bytes: out, matched }
    }

    /// Convenience: did any known dummy appear in `input`?
    pub fn contains_any_dummy(&self, input: &[u8]) -> bool {
        let mut i = 0;
        while i < input.len() {
            let mut hit = false;
            for e in &self.snap.rewrite {
                if input[i..].starts_with(e.search.as_bytes()) {
                    hit = true;
                    break;
                }
            }
            if hit {
                return true;
            }
            i += 1;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::RewriteEntry;

    fn snap() -> SecretSnapshot {
        let mut real = std::collections::BTreeMap::new();
        real.insert("gh".into(), "ghp_REAL".into());
        real.insert("oi".into(), "sk-REAL".into());
        let rewrite = vec![
            RewriteEntry { search: "ghp_qwanDUMMY01".into(), replace: "gh".into() },
            RewriteEntry { search: "sk-qwanDUMMY7c".into(), replace: "oi".into() },
        ];
        SecretSnapshot { real, rewrite }
    }

    #[test]
    fn swaps_dummy_in_bearer_header() {
        let s = snap();
        let r = Rewriter::new(&s);
        let input = b"Authorization: Bearer ghp_qwanDUMMY01";
        let res = r.rewrite(input);
        assert_eq!(res.bytes, b"Authorization: Bearer ghp_REAL".to_vec());
        assert_eq!(res.matched, vec![0]);
    }

    #[test]
    fn swaps_dummy_in_basic_auth() {
        let s = snap();
        let r = Rewriter::new(&s);
        let input = b"Basic base64(x-access-token:ghp_qwanDUMMY01)";
        let res = r.rewrite(input);
        assert!(res.bytes.windows(8).any(|w| w == b"ghp_REAL"));
    }

    #[test]
    fn swaps_two_distinct_dummies_in_one_request() {
        let s = snap();
        let r = Rewriter::new(&s);
        let input = b"token1=ghp_qwanDUMMY01&token2=sk-qwanDUMMY7c";
        let res = r.rewrite(input);
        assert_eq!(res.matched, vec![0, 1]);
        assert!(res.bytes.windows(8).any(|w| w == b"ghp_REAL"));
        assert!(res.bytes.windows(7).any(|w| w == b"sk-REAL"));
    }

    #[test]
    fn no_dummy_passes_through_verbatim() {
        let s = snap();
        let r = Rewriter::new(&s);
        let input = b"GET / HTTP/1.1\r\nHost: api.github.com\r\n\r\n";
        let res = r.rewrite(input);
        assert_eq!(res.bytes, input);
        assert!(res.matched.is_empty());
    }

    #[test]
    fn partial_dummy_not_matched() {
        let s = snap();
        let r = Rewriter::new(&s);
        let input = b"Bearer ghp_qwanDUMMY0";
        let res = r.rewrite(input);
        assert!(res.matched.is_empty());
        assert_eq!(res.bytes, input);
    }
}
