//! Turning errors into HTTP responses.
//!
//! Upstream answers every failure with `{"error": "..."}` and the matching status, so clients
//! that already parse Midgard errors keep working.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use midgard_core::Error;
use serde_json::json;

/// Wrapper so `midgard_core::Error` can be returned straight out of a handler.
pub struct ApiError(pub Error);

impl From<Error> for ApiError {
    fn from(e: Error) -> ApiError {
        ApiError(e)
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> ApiError {
        ApiError(Error::Internal(e.to_string()))
    }
}

impl From<midgard_db::DbError> for ApiError {
    fn from(e: midgard_db::DbError) -> ApiError {
        ApiError(e.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.0.status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

        // Internal errors carry query text and connection details, so the full message goes to
        // the log and only the sanitised one goes over the wire.
        if status.is_server_error() {
            tracing::error!(error = %self.0, "request failed");
        }

        (status, Json(json!({ "error": self.0.public_message() }))).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn body_of(r: Response) -> serde_json::Value {
        let bytes = to_bytes(r.into_body(), 64 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn bad_requests_keep_their_message() {
        let r = ApiError(Error::bad_request("interval must be one of ...")).into_response();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_of(r).await["error"], "interval must be one of ...");
    }

    #[tokio::test]
    async fn not_found_keeps_its_message() {
        let r = ApiError(Error::not_found("pool BTC.BTC not found")).into_response();
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_of(r).await["error"], "pool BTC.BTC not found");
    }

    #[tokio::test]
    async fn internal_errors_do_not_leak_the_query() {
        let r =
            ApiError(Error::internal("SELECT secret FROM x: connection refused")).into_response();
        assert_eq!(r.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body_of(r).await["error"], "Internal Server Error");
    }

    #[tokio::test]
    async fn sqlx_errors_are_internal() {
        let e: ApiError = sqlx::Error::RowNotFound.into();
        assert_eq!(
            e.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
