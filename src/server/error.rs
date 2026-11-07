use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug)]
pub enum AppError {
    NotFound(String),
    BadRequest(String),
    Conflict(String),
    Unauthorized(String),
    Forbidden(String),
    Internal(String),
    ServiceUnavailable(String),
    RequestTimeout(String),
    TooManyRequests(String),
    /// HTTP 422 (Typesense: a request it understood but will not run, e.g. per_page above 250).
    Unprocessable(String),
    /// HTTP 507 Insufficient Storage — returned by admission control
    /// when accepting the request would exhaust available memory.
    InsufficientStorage(String),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(msg) => write!(f, "Not Found: {msg}"),
            Self::BadRequest(msg) => write!(f, "Bad Request: {msg}"),
            Self::Conflict(msg) => write!(f, "Conflict: {msg}"),
            Self::Unauthorized(msg) => write!(f, "Unauthorized: {msg}"),
            Self::Forbidden(msg) => write!(f, "Forbidden: {msg}"),
            Self::Internal(msg) => write!(f, "Internal Error: {msg}"),
            Self::ServiceUnavailable(msg) => write!(f, "Service Unavailable: {msg}"),
            Self::RequestTimeout(msg) => write!(f, "Request Timeout: {msg}"),
            Self::TooManyRequests(msg) => write!(f, "Too Many Requests: {msg}"),
            Self::Unprocessable(msg) => write!(f, "Unprocessable Entity: {msg}"),
            Self::InsufficientStorage(msg) => write!(f, "Insufficient Storage: {msg}"),
        }
    }
}

impl std::error::Error for AppError {}

impl AppError {
    /// The HTTP status and the message.
    pub fn status(&self) -> (StatusCode, &str) {
        let (status, _, message) = self.parts();
        (status, message)
    }

    fn parts(&self) -> (StatusCode, &'static str, &str) {
        match self {
            Self::NotFound(msg) => (StatusCode::NOT_FOUND, "not_found", msg.as_str()),
            Self::BadRequest(msg) => (StatusCode::BAD_REQUEST, "bad_request", msg.as_str()),
            Self::Conflict(msg) => (StatusCode::CONFLICT, "conflict", msg.as_str()),
            Self::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, "unauthorized", msg.as_str()),
            Self::Forbidden(msg) => (StatusCode::FORBIDDEN, "forbidden", msg.as_str()),
            Self::Internal(msg) => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal", msg.as_str())
            }
            Self::ServiceUnavailable(msg) => {
                (StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", msg.as_str())
            }
            Self::RequestTimeout(msg) => {
                (StatusCode::REQUEST_TIMEOUT, "request_timeout", msg.as_str())
            }
            Self::TooManyRequests(msg) => {
                (StatusCode::TOO_MANY_REQUESTS, "too_many_requests", msg.as_str())
            }
            Self::Unprocessable(msg) => {
                (StatusCode::UNPROCESSABLE_ENTITY, "unprocessable", msg.as_str())
            }
            Self::InsufficientStorage(msg) => {
                (StatusCode::INSUFFICIENT_STORAGE, "insufficient_storage", msg.as_str())
            }
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, error_type, message) = self.parts();

        if status.is_server_error() {
            tracing::error!(status = status.as_u16(), error_type, message, "HTTP error");
        } else {
            tracing::debug!(status = status.as_u16(), error_type, message, "HTTP error");
        }

        let body = json!({ "message": message });
        (status, axum::response::Json(body)).into_response()
    }
}
