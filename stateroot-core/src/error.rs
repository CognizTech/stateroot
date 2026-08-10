//! Shared error types for service clients.
//!
//! Cloud backends (StateSmith, Auth, FileSystem) respond with the commons
//! JSON envelope: `{success, data, error: {code, message, details}, meta}`.
//! [`ApiError`] captures transport failures, envelope-level server errors and
//! unexpected response shapes in one place so the individual clients can map
//! them into their own typed errors.

use thiserror::Error;

/// Error returned by the HTTP layer and the envelope unwrap.
#[derive(Debug, Error)]
pub enum ApiError {
    /// Transport-level failure (connect, TLS, timeout, body decode).
    #[error("http transport error: {0}")]
    Transport(String),

    /// The server answered with an error envelope (or a non-2xx status).
    #[error("server error {code}: {message}")]
    Server {
        /// HTTP status when one was available.
        status: Option<u16>,
        /// Envelope error code, e.g. `VALIDATION_ERROR`, `NOT_FOUND`.
        code: String,
        /// Human readable server message.
        message: String,
    },

    /// The body did not match the expected contract.
    #[error("unexpected response shape: {0}")]
    UnexpectedResponse(String),

    /// Every retry attempt failed.
    #[error("request failed after {0} attempts")]
    RetriesExhausted(usize),
}

impl ApiError {
    /// Build a [`ApiError::Server`] from parts.
    pub fn server(
        status: Option<u16>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        ApiError::Server {
            status,
            code: code.into(),
            message: message.into(),
        }
    }

    /// True when the envelope code reports a missing resource.
    pub fn is_not_found(&self) -> bool {
        match self {
            ApiError::Server { code, status, .. } => {
                matches!(
                    code.as_str(),
                    "NOT_FOUND" | "RESOURCE_NOT_FOUND" | "not_found"
                ) || *status == Some(404)
            }
            _ => false,
        }
    }

    /// True when the envelope code reports an optimistic-concurrency conflict.
    pub fn is_conflict(&self) -> bool {
        match self {
            ApiError::Server { code, status, .. } => code == "CONFLICT" || *status == Some(409),
            _ => false,
        }
    }

    /// True when the server reports synthesis as unavailable (503 with
    /// error_code `SYNTHESIS_UNAVAILABLE`) — the caller's fallback signal.
    pub fn is_synthesis_unavailable(&self) -> bool {
        match self {
            ApiError::Server { code, status, .. } => {
                code == "SYNTHESIS_UNAVAILABLE" || *status == Some(503)
            }
            _ => false,
        }
    }

    /// True when the server reports goal drafting as unavailable (503 with
    /// error_code `GOAL_DRAFT_UNAVAILABLE`) — the local-template fallback
    /// signal for `stateroot goal draft`.
    pub fn is_goal_draft_unavailable(&self) -> bool {
        match self {
            ApiError::Server { code, status, .. } => {
                code == "GOAL_DRAFT_UNAVAILABLE" || *status == Some(503)
            }
            _ => false,
        }
    }

    /// True when the envelope code reports missing or invalid authentication.
    pub fn is_unauthorized(&self) -> bool {
        matches!(
            self,
            ApiError::Server { code, .. }
                if matches!(code.as_str(), "UNAUTHORIZED" | "unauthorized" | "invalid_api_key")
        )
    }

    /// True when the envelope code reports a disabled/unavailable service.
    pub fn is_unavailable(&self) -> bool {
        matches!(
            self,
            ApiError::Server { code, .. }
                if matches!(code.as_str(), "SERVICE_UNAVAILABLE" | "service_unavailable")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_classifiers() {
        let nf = ApiError::server(Some(200), "RESOURCE_NOT_FOUND", "nope");
        assert!(nf.is_not_found());
        assert!(!nf.is_conflict());
        let c = ApiError::server(None, "CONFLICT", "rev mismatch");
        assert!(c.is_conflict());
        let u = ApiError::server(Some(401), "UNAUTHORIZED", "bad token");
        assert!(u.is_unauthorized());
    }
}
