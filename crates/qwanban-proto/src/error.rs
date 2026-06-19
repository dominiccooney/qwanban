//! Canonical error model (components README §S5). Every component maps its
//! failures onto `QwanError` with a stable `QwanCode`, so the broker can return
//! consistent gRPC/HTTP error semantics and the agent gets actionable signals.

use thiserror::Error;

/// Canonical error codes. Stable across the wire; never reorder (serialize as
/// the variant name string).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QwanCode {
    /// Caller supplied an invalid argument (bad id, out-of-bounds coords, …).
    InvalidArg,
    /// A referenced resource was not found.
    NotFound,
    /// Authentication failed (bad case token).
    Unauthenticated,
    /// Authenticated but not permitted (model not allowed for this case, …).
    PermissionDenied,
    /// The resource already exists / case already in a terminal state.
    AlreadyExists,
    /// A precondition for the op isn't met (e.g. `finish` after finish).
    FailedPrecondition,
    /// Operation couldn't be completed right now (capture not ready, …).
    Unavailable,
    /// Host resource caps exhausted (no free `max_concurrent_cases` slot — §5.8).
    ResourceExhausted,
    /// Internal/unexpected failure (OS call failed, encoder crash, …).
    Internal,
    /// Deadline exceeded (max_runtime, push timeout, …).
    DeadlineExceeded,
    /// Upstream returned an error (inference provider, GitHub, …).
    Upstream,
}

impl QwanCode {
    /// Suggested HTTP status for a code (used by HTTP-facing surfaces).
    pub fn http_status(self) -> u16 {
        match self {
            QwanCode::InvalidArg | QwanCode::FailedPrecondition => 400,
            QwanCode::Unauthenticated => 401,
            QwanCode::PermissionDenied => 403,
            QwanCode::NotFound => 404,
            QwanCode::AlreadyExists => 409,
            QwanCode::ResourceExhausted => 429,
            QwanCode::Unavailable => 503,
            QwanCode::DeadlineExceeded => 504,
            QwanCode::Internal | QwanCode::Upstream => 502,
        }
    }
}

impl std::fmt::Display for QwanCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

/// The canonical qwanban error.
#[derive(Debug, Error)]
pub enum QwanError {
    #[error("{code}: {message}")]
    Structured {
        code: QwanCode,
        message: String,
        // Optional: which case this error pertains to.
        case_id: Option<crate::CaseId>,
    },
    /// A wrapped boxed std error (kept simple to avoid an anyhow dep in proto).
    #[error("{0}")]
    Other(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),
}

impl QwanError {
    pub fn new(code: QwanCode, message: impl Into<String>) -> Self {
        QwanError::Structured {
            code,
            message: message.into(),
            case_id: None,
        }
    }
    pub fn with_case(mut self, case_id: crate::CaseId) -> Self {
        if let QwanError::Structured { case_id: c, .. } = &mut self {
            *c = Some(case_id);
        }
        self
    }
    pub fn code(&self) -> QwanCode {
        match self {
            QwanError::Structured { code, .. } => *code,
            QwanError::Other(_) => QwanCode::Internal,
        }
    }
    pub fn message(&self) -> String {
        match self {
            QwanError::Structured { message, .. } => message.clone(),
            QwanError::Other(e) => format!("{e}"),
        }
    }
}

/// Convenience: an invalid-arg error.
pub fn invalid_arg(msg: impl Into<String>) -> QwanError {
    QwanError::new(QwanCode::InvalidArg, msg)
}

/// Convenience: a not-found error.
pub fn not_found(msg: impl Into<String>) -> QwanError {
    QwanError::new(QwanCode::NotFound, msg)
}

/// Convenience: an internal error.
pub fn internal(msg: impl Into<String>) -> QwanError {
    QwanError::new(QwanCode::Internal, msg)
}

pub type QwanResult<T> = Result<T, QwanError>;

impl From<std::io::Error> for QwanError {
    fn from(e: std::io::Error) -> Self {
        QwanError::new(QwanCode::Internal, format!("io: {e}"))
    }
}

impl From<std::num::ParseIntError> for QwanError {
    fn from(e: std::num::ParseIntError) -> Self {
        QwanError::new(QwanCode::InvalidArg, format!("parse int: {e}"))
    }
}

impl From<serde_json::Error> for QwanError {
    fn from(e: serde_json::Error) -> Self {
        QwanError::new(QwanCode::InvalidArg, format!("json: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_status_mapping_is_sensible() {
        assert_eq!(QwanCode::Unauthenticated.http_status(), 401);
        assert_eq!(QwanCode::PermissionDenied.http_status(), 403);
        assert_eq!(QwanCode::ResourceExhausted.http_status(), 429);
        assert_eq!(QwanCode::InvalidArg.http_status(), 400);
    }

    #[test]
    fn with_case_attaches_case_id() {
        let e = QwanError::new(QwanCode::NotFound, "nope")
            .with_case(crate::CaseId::from_str_inner("case_x"));
        match e {
            QwanError::Structured { case_id, .. } => {
                assert_eq!(case_id.unwrap().as_str(), "case_x");
            }
            _ => panic!(),
        }
    }
}
