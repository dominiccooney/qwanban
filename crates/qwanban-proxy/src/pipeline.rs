//! The request-handling pipeline (§mitm-proxy): allowlist → method policy →
//! search→replace rewrite → forward → audit. Pure decision logic + a `Transport`
//! trait so it's testable in the dev VM with a `MockTransport` (no real TLS).

use crate::allowlist::Allowlist;
use crate::audit::{AuditRecord, AuditSink};
use async_trait::async_trait;
use qwanban_proto::id::CaseId;
use qwanban_proto::QwanResult;
use qwanban_vault::{Rewriter, Vault};
use std::sync::Arc;

/// An incoming proxied request, decomposed into the parts the rewriter scans.
#[derive(Debug, Clone)]
pub struct ProxyRequest {
    pub case_id: CaseId,
    pub host: String,
    pub method: String,
    pub path: String,
    pub headers: Vec<u8>,
    pub url: Vec<u8>,
    pub body: Vec<u8>,
}

/// The pipeline's decision for a request.
#[derive(Debug, Clone)]
pub enum ProxyDecision {
    /// Allowed + (possibly) rewritten. Forward these bytes. `matched` is empty
    /// for a passthrough (no dummy found).
    Allow {
        headers: Vec<u8>,
        url: Vec<u8>,
        body: Vec<u8>,
        matched: Vec<usize>,
    },
    /// Host not allowlisted, or method not allowed.
    Block { status: u16, reason: String },
}

/// Run the pure pipeline (no I/O). This is the unit-testable core.
pub fn run_pipeline(allowlist: &Allowlist, rewriter: &Rewriter<'_>, req: &ProxyRequest) -> ProxyDecision {
    // 1+2. allowlist + method policy
    if !allowlist.is_allowed(&req.host, &req.method) {
        return ProxyDecision::Block {
            status: 403,
            reason: format!("blocked host/method: {} {}", req.method, req.host),
        };
    }
    // 3. search→replace (no-injection: if no dummy, returns the input unchanged)
    let res = rewriter.rewrite_request(&req.headers, &req.url, &req.body);
    ProxyDecision::Allow {
        headers: res.headers,
        url: res.url,
        body: res.body,
        matched: res.matched,
    }
}

/// The upstream transport. Real impl: validated TLS to the real host. Tests: mock.
#[async_trait]
pub trait Transport: Send + Sync {
    async fn forward(&self, host: &str, headers: &[u8], url: &[u8], body: &[u8]) -> QwanResult<Vec<u8>>;
}

/// A mock transport that records what it received + returns a canned response.
pub struct MockTransport {
    pub last_host: parking_lot::Mutex<Option<String>>,
    pub last_received: parking_lot::Mutex<Vec<u8>>,
    pub response: Vec<u8>,
}

impl MockTransport {
    pub fn new(response: Vec<u8>) -> Self {
        Self {
            last_host: parking_lot::Mutex::new(None),
            last_received: parking_lot::Mutex::new(Vec::new()),
            response,
        }
    }
    /// What bytes did the upstream actually receive? (assert the real secret is here)
    pub fn received(&self) -> Vec<u8> {
        self.last_received.lock().clone()
    }
}

#[async_trait]
impl Transport for MockTransport {
    async fn forward(&self, host: &str, headers: &[u8], url: &[u8], body: &[u8]) -> QwanResult<Vec<u8>> {
        *self.last_host.lock() = Some(host.to_string());
        let mut all = Vec::new();
        all.extend_from_slice(headers);
        all.extend_from_slice(url);
        all.extend_from_slice(body);
        *self.last_received.lock() = all;
        Ok(self.response.clone())
    }
}

/// The proxy server: holds an allowlist + vault + audit sink + transport.
pub struct ProxyServer {
    pub allowlist: Allowlist,
    pub vault: Arc<dyn Vault>,
    pub audit: Arc<dyn AuditSink>,
    pub transport: Arc<dyn Transport>,
}

impl ProxyServer {
    /// Handle one request end-to-end: pipeline → forward → audit. Returns the
    /// upstream response (or an error for blocks).
    pub async fn handle(&self, req: &ProxyRequest) -> QwanResult<Vec<u8>> {
        let snap = self.vault.snapshot();
        let rewriter = Rewriter::new(&snap);
        let decision = run_pipeline(&self.allowlist, &rewriter, req);
        match decision {
            ProxyDecision::Block { status, reason } => {
                self.audit
                    .record(AuditRecord {
                        case_id: req.case_id.clone(),
                        host: req.host.clone(),
                        method: req.method.clone(),
                        path: req.path.clone(),
                        status,
                        bytes_up: 0,
                        bytes_down: 0,
                        matched_dummies: vec![],
                    })
                    .await;
                Err(qwanban_proto::QwanError::new(
                    qwanban_proto::QwanCode::PermissionDenied,
                    reason,
                ))
            }
            ProxyDecision::Allow { headers, url, body, matched } => {
                let resp = self.transport.forward(&req.host, &headers, &url, &body).await?;
                self.audit
                    .record(AuditRecord {
                        case_id: req.case_id.clone(),
                        host: req.host.clone(),
                        method: req.method.clone(),
                        path: req.path.clone(),
                        status: 200,
                        bytes_up: (headers.len() + url.len() + body.len()) as u64,
                        bytes_down: resp.len() as u64,
                        matched_dummies: matched,
                    })
                    .await;
                Ok(resp)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allowlist::{HostMatch, HostRule};
    use qwanban_vault::{RewriteEntry, SecretSnapshot};

    fn snap() -> SecretSnapshot {
        let mut real = std::collections::BTreeMap::new();
        real.insert("gh".into(), "ghp_REAL".into());
        real.insert("oi".into(), "sk-REAL".into());
        let rewrite = vec![
            RewriteEntry { search: "ghp_DUMMY".into(), replace: "gh".into() },
            RewriteEntry { search: "sk_DUMMY".into(), replace: "oi".into() },
        ];
        SecretSnapshot { real, rewrite }
    }

    fn al() -> Allowlist {
        Allowlist {
            rules: vec![HostRule {
                match_: HostMatch::Exact("api.github.com".into()),
                allow_methods: vec!["GET".into(), "POST".into()],
            }],
        }
    }

    fn req(host: &str, method: &str, headers: &[u8], url: &[u8], body: &[u8]) -> ProxyRequest {
        ProxyRequest {
            case_id: CaseId::from_str_inner("c1"),
            host: host.into(),
            method: method.into(),
            path: "/".into(),
            headers: headers.to_vec(),
            url: url.to_vec(),
            body: body.to_vec(),
        }
    }

    #[test]
    fn block_unknown_host() {
        let s = snap();
        let r = Rewriter::new(&s);
        let d = run_pipeline(&al(), &r, &req("evil.com", "GET", b"", b"", b""));
        assert!(matches!(d, ProxyDecision::Block { status: 403, .. }));
    }

    #[test]
    fn block_method_not_allowed() {
        let s = snap();
        let r = Rewriter::new(&s);
        let d = run_pipeline(&al(), &r, &req("api.github.com", "DELETE", b"", b"", b""));
        assert!(matches!(d, ProxyDecision::Block { status: 403, .. }));
    }

    #[test]
    fn allow_with_dummy_swapped_in_bearer_header() {
        let s = snap();
        let r = Rewriter::new(&s);
        let d = run_pipeline(
            &al(),
            &r,
            &req("api.github.com", "GET", b"Authorization: Bearer ghp_DUMMY", b"", b""),
        );
        match d {
            ProxyDecision::Allow { headers, matched, .. } => {
                assert!(headers.windows(8).any(|w| w == b"ghp_REAL"));
                assert_eq!(matched, vec![0]);
            }
            _ => panic!("expected Allow"),
        }
    }

    #[test]
    fn allow_passthrough_when_no_dummy() {
        let s = snap();
        let r = Rewriter::new(&s);
        let d = run_pipeline(
            &al(),
            &r,
            &req("api.github.com", "GET", b"Accept: application/json", b"/repos", b""),
        );
        match d {
            ProxyDecision::Allow { headers, url, matched, .. } => {
                assert_eq!(headers, b"Accept: application/json");
                assert_eq!(url, b"/repos");
                assert!(matched.is_empty());
            }
            _ => panic!("expected Allow"),
        }
    }

    #[test]
    fn allow_juggles_two_distinct_dummies() {
        let s = snap();
        let r = Rewriter::new(&s);
        let d = run_pipeline(
            &al(),
            &r,
            &req(
                "api.github.com",
                "POST",
                b"Authorization: Bearer ghp_DUMMY",
                b"",
                b"{\"key\":\"sk_DUMMY\"}",
            ),
        );
        match d {
            ProxyDecision::Allow { headers, body, matched, .. } => {
                assert!(headers.windows(8).any(|w| w == b"ghp_REAL"));
                assert!(body.windows(7).any(|w| w == b"sk-REAL"));
                assert_eq!(matched, vec![0, 1]);
            }
            _ => panic!("expected Allow"),
        }
    }
}
