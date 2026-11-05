//! One error type for the whole HTTP surface.
//!
//! Library crates return their own `thiserror` types; the server converts them
//! into `AppError`, which is the only thing that reaches the wire. The response
//! body is `{"error": <code>, "message"?: <text>}` — the shape `web/src/lib/api.js`
//! parses, keying its sign-in gate on the 401 code `unauthenticated`. 500s carry
//! no message (internals are logged, never leaked).

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// Convenience alias for handler results.
pub type AppResult<T> = Result<T, AppError>;

/// Every failure the HTTP layer can produce, preserving the Go server's status
/// codes.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// 401 — no valid session.
    #[error("unauthenticated")]
    Unauthorized,
    /// 403 — authenticated but not permitted.
    #[error("forbidden")]
    Forbidden,
    /// 404 — resource does not exist.
    #[error("not found")]
    NotFound,
    /// 409 — optimistic-concurrency conflict (stale playback version).
    #[error("conflict")]
    Conflict,
    /// 428 — retained for wire compatibility; unused now Navidrome linking is gone.
    #[error("precondition required")]
    PreconditionRequired,
    /// 400 — client sent something invalid; the message is safe to return.
    #[error("{0}")]
    BadRequest(String),
    /// 500 — the message is logged, never returned.
    #[error("{0}")]
    Internal(String),
    /// 502 — an upstream service failed, and the distinction matters to the
    /// user: "ListenBrainz is down" and "your token is wrong" call for
    /// completely different actions, and the PWA already branches on 502 to say
    /// so. Returning 500 for both made that branch unreachable.
    #[error("{0}")]
    BadGateway(String),
}

impl AppError {
    #[must_use]
    pub const fn status(&self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict => StatusCode::CONFLICT,
            Self::PreconditionRequired => StatusCode::PRECONDITION_REQUIRED,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::BadGateway(_) => StatusCode::BAD_GATEWAY,
        }
    }

    /// The machine-readable `error` code the PWA branches on.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthenticated",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::PreconditionRequired => "precondition_required",
            Self::BadRequest(_) => "bad_request",
            Self::Internal(_) => "internal",
            Self::BadGateway(_) => "bad_gateway",
        }
    }

    #[must_use]
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }

    #[must_use]
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self::BadRequest(msg.into())
    }

    #[must_use]
    pub fn bad_gateway(msg: impl Into<String>) -> Self {
        Self::BadGateway(msg.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let code = self.code();
        let message = match &self {
            Self::BadRequest(m) => m.clone(),
            Self::Internal(m) => {
                tracing::error!(error = %m, "internal error");
                String::new()
            }
            _ => String::new(),
        };
        let mut obj = serde_json::Map::new();
        obj.insert("error".into(), json!(code));
        if !message.is_empty() {
            obj.insert("message".into(), json!(message));
        }
        (status, Json(serde_json::Value::Object(obj))).into_response()
    }
}

impl From<mh_db::DbError> for AppError {
    fn from(e: mh_db::DbError) -> Self {
        match e {
            mh_db::DbError::Sqlx(sqlx::Error::RowNotFound) => Self::NotFound,
            other => Self::Internal(other.to_string()),
        }
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => Self::NotFound,
            other => Self::Internal(other.to_string()),
        }
    }
}

impl From<mh_media::MediaError> for AppError {
    fn from(e: mh_media::MediaError) -> Self {
        Self::Internal(e.to_string())
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        Self::Internal(e.to_string())
    }
}

impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        Self::Internal(e.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        Self::Internal(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn unauthorized_maps_to_401_with_unauthenticated_code() {
        assert_eq!(AppError::Unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(AppError::Unauthorized.code(), "unauthenticated");
    }

    #[test]
    fn conflict_maps_to_409() {
        assert_eq!(AppError::Conflict.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn row_not_found_becomes_404() {
        let err: AppError = sqlx::Error::RowNotFound.into();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
    }
}
