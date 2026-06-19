//! `qwanban-inference` — OpenAI-compatible router (§inference-router). Serves
//! `/v1/models` (intersection of routes + case's allowed_models) and forwards
//! chat requests; for cloud routes swaps the dummy bytes for the real secret via
//! the vault's search→replace (same model as the proxy, Q6). SSE streaming
//! passes through unbuffered.

pub mod routing;

pub use routing::{RouteResolver, ResolvedRoute};
