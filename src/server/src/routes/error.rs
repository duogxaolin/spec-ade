//! Shared handler error type (SPEC-002 §3.6).
//!
//! One JSON error shape across every route: `{error: <group>, detail: <text>}`,
//! plus optional extra fields a specific failure needs (`existingId`,
//! `currentRev`, conflicted `paths`). The client switches on `error`, so the group
//! names are part of the contract and must stay stable.
//!
//! Lives here rather than in one route module because SPEC-005 needed the same
//! shape and copying it would have let the two drift.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;

/// Handler-level error carrying its HTTP mapping.
pub struct ApiError {
    pub status: StatusCode,
    pub group: &'static str,
    pub detail: String,
    pub extra: Option<serde_json::Value>,
}

impl ApiError {
    pub fn new(status: StatusCode, group: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            group,
            detail: detail.into(),
            extra: None,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({ "error": self.group, "detail": self.detail });
        if let (Some(obj), Some(serde_json::Value::Object(extra))) =
            (body.as_object_mut(), self.extra)
        {
            for (k, v) in extra {
                obj.insert(k, v);
            }
        }
        (self.status, Json(body)).into_response()
    }
}

/// `spawn_blocking` join failure — a bug on our side, not a client error.
pub fn task_failed(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "io",
        format!("task failed: {e}"),
    )
}
