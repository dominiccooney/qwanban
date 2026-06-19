//! Route resolution + model allowlist enforcement (§inference-router).
//!
//! NOTE: there is no inference *server*. This is pure routing logic the manifest
//! builder + orchestrator call. LM Studio is reached directly over the vSwitch
//! (no key); cloud routes go through the MITM proxy (which swaps the secret).
//! This crate never touches secrets.

use qwanban_proto::config::{InferenceConfig, RouteTarget};
use qwanban_proto::QwanCode;

#[derive(Debug, Clone)]
pub struct ResolvedRoute {
    pub model: String,
    pub target: RouteTarget,
    /// The base URL the guest's OpenAI client should use: the host LM Studio URL
    /// for `Lmstudio` routes (direct, no key), or the cloud provider URL for
    /// `Cloud` routes (reached via the proxy, which swaps the secret).
    pub base_url: String,
}

pub struct RouteResolver {
    cfg: InferenceConfig,
}

impl RouteResolver {
    pub fn new(cfg: InferenceConfig) -> Self {
        Self { cfg }
    }

    /// `/v1/models` = intersection of configured routes and the case's allowed_models.
    pub fn allowed_models(&self, case_allowed: &[String]) -> Vec<String> {
        let configured: std::collections::HashSet<&str> =
            self.cfg.routes.iter().map(|r| r.model.as_str()).collect();
        case_allowed
            .iter()
            .filter(|m| configured.contains(m.as_str()))
            .cloned()
            .collect()
    }

    /// Resolve a model requested by the guest against the case's allowlist.
    /// Returns the base URL the guest should point its OpenAI client at.
    pub fn resolve(&self, model: &str, case_allowed: &[String]) -> qwanban_proto::QwanResult<ResolvedRoute> {
        if !case_allowed.iter().any(|m| m == model) {
            return Err(qwanban_proto::QwanError::new(
                QwanCode::PermissionDenied,
                format!("model {model} not allowed for this case"),
            ));
        }
        let route = self.cfg.routes.iter().find(|r| r.model == model).ok_or_else(|| {
            qwanban_proto::QwanError::new(QwanCode::NotFound, format!("no route configured for {model}"))
        })?;
        // LM Studio: the guest calls the host LM Studio directly (no key, no proxy).
        // Cloud: the guest calls the cloud URL, routed through the proxy which swaps the secret.
        let base_url = match route.target {
            RouteTarget::Lmstudio => self.cfg.lmstudio_url.clone(),
            RouteTarget::Cloud => route.base_url.clone().unwrap_or_default(),
        };
        Ok(ResolvedRoute {
            model: route.model.clone(),
            target: route.target,
            base_url,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qwanban_proto::config::InferenceRoute;

    fn cfg() -> InferenceConfig {
        InferenceConfig {
            lmstudio_url: "http://10.0.75.1:1234/v1".into(),
            routes: vec![
                InferenceRoute {
                    model: "qwen2.5-coder-32b".into(),
                    target: RouteTarget::Lmstudio,
                    base_url: None,
                },
                InferenceRoute {
                    model: "gpt-4o".into(),
                    target: RouteTarget::Cloud,
                    base_url: Some("https://api.openai.com/v1".into()),
                },
            ],
        }
    }

    #[test]
    fn allowed_models_intersects() {
        let r = RouteResolver::new(cfg());
        let allowed = r.allowed_models(&["qwen2.5-coder-32b".into(), "claude".into()]);
        assert_eq!(allowed, vec!["qwen2.5-coder-32b"]);
    }

    #[test]
    fn resolve_rejects_disallowed_model() {
        let r = RouteResolver::new(cfg());
        let err = r.resolve("gpt-4o", &["qwen2.5-coder-32b".into()]).unwrap_err();
        assert_eq!(err.code(), QwanCode::PermissionDenied);
    }

    #[test]
    fn resolve_lmstudio_returns_direct_url() {
        let r = RouteResolver::new(cfg());
        let route = r.resolve("qwen2.5-coder-32b", &["qwen2.5-coder-32b".into()]).unwrap();
        assert_eq!(route.target, RouteTarget::Lmstudio);
        // points at the host LM Studio, no proxy needed
        assert_eq!(route.base_url, "http://10.0.75.1:1234/v1");
    }

    #[test]
    fn resolve_cloud_returns_cloud_url() {
        let r = RouteResolver::new(cfg());
        let route = r.resolve("gpt-4o", &["gpt-4o".into()]).unwrap();
        assert_eq!(route.target, RouteTarget::Cloud);
        // points at the real cloud URL (the guest reaches it via the proxy)
        assert_eq!(route.base_url, "https://api.openai.com/v1");
    }
}
