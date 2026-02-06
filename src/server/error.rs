use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};

use super::types::ErrorResponse;

#[derive(Debug)]
pub(crate) enum ApiError {
    BadRequest(String),
    NotFound(String),
    Conflict(String),
    Unprocessable(String),
    Internal(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::BadRequest(msg) => write!(f, "Bad Request: {msg}"),
            ApiError::NotFound(msg) => write!(f, "Not Found: {msg}"),
            ApiError::Conflict(msg) => write!(f, "Conflict: {msg}"),
            ApiError::Unprocessable(msg) => write!(f, "Unprocessable: {msg}"),
            ApiError::Internal(msg) => write!(f, "Internal Error: {msg}"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            ApiError::Conflict(msg) => (StatusCode::CONFLICT, msg),
            ApiError::Unprocessable(msg) => (StatusCode::UNPROCESSABLE_ENTITY, msg),
            ApiError::Internal(msg) => {
                tracing::error!("Internal server error: {msg}");
                (StatusCode::INTERNAL_SERVER_ERROR, msg)
            }
        };

        (status, Json(ErrorResponse { error: message })).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(err: anyhow::Error) -> Self {
        ApiError::Internal(err.to_string())
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(err: rusqlite::Error) -> Self {
        ApiError::Internal(format!("Database error: {err}"))
    }
}
