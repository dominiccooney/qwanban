//! Route resolution + model allowlist enforcement (§inference-router).

use qwanban_proto::config::{InferenceConfig, RouteTarget};
use qwanban_proto::QwanCode;

#[derive(Debug, Clone)]
pub struct ResolvedRoute {
    pub model: String,
    pub target: RouteTarget,
    pub base_url: Option<String>,
    pub secret_name: Option<String>,
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
    pub fn resolve(&self, model: &str, case_allowed: &[String]) -> qwanban_proto::QwanResult<ResolvedRoute> {
        if !case_allowed.iter().any(|m| m == model) {
            return Err(qwanban_proto::QwanError::new(
                QwanCode::PermissionDenied,
                format!("model {model} not allowed for this case"),
            ));
        }
        let route = self.cfg.routes.iter().find(|r| r.model == model).ok_or_else(|| {
            qwanban_proto::QwanError::new(
                QwanCode::NotFound,
                format!("no route configured for {model}"),
            )
        })?;
        Ok(ResolvedRoute {
            model: route.model.clone(),
            target: route.target,
            base_url: route.base_url.clone(),
            secret_name: route.secret.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qwanban_proto::config::InferenceRoute;

    fn cfg() -> InferenceConfig {
        InferenceConfig {
            lmstudio_url: "http://127.0.0.1:1234/v1".into(),
            routes: vec![
                InferenceRoute {
                    model: "qwen2.5-coder-32b".into(),
                    target: RouteTarget::Lmstudio,
                    base_url: None,
                    secret: None,
                },
                InferenceRoute {
                    model: "gpt-4o".into(),
                    target: RouteTarget::Cloud,
                    base_url: Some("https://api.openai.com/v1".into()),
                    secret: Some("openai_key".into()),
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
    fn resolve_returns_cloud_secret() {
        let r = RouteResolver::new(cfg());
        let route = r.resolve("gpt-4o", &["gpt-4o".into()]).unwrap();
        assert_eq!(route.target, RouteTarget::Cloud);
        assert_eq!(route.secret_name.as_deref(), Some("openai_key"));
    }
}
