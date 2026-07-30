//! Settings routes — `GET/PUT /api/settings` (06-api-contract.md §Settings [DOCS]).
//!
//! Spec: `docs/specs/SPEC-002-file-tree-editor.md` §3.1.
//!
//! PUT is a partial update with `Option<Option<T>>` semantics: absent = keep,
//! null = back to default, value = set. Only the `editor` branch is exposed
//! ([INVENTED-1]): `authToken` is never readable nor writable (an authed client
//! that could rotate the token turns a frontend bug into a lockout), and
//! `projects` has its own API — two write paths would mean two sources of truth.

use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;
use crate::settings::{EditorPatch, EditorSettings, SettingsError};

pub fn router() -> Router<AppState> {
    Router::new().route("/settings", get(get_settings).put(put_settings))
}

/// The externally visible settings document — deliberately *not* the on-disk
/// `Settings` struct, so adding an internal field can never leak it by accident.
#[derive(Debug, Serialize)]
struct SettingsView {
    editor: EditorSettings,
}

/// Body of `PUT /api/settings`. `deny_unknown_fields` turns a typo'd or
/// forbidden top-level key (`authToken`, `projects`, …) into a 400/403 instead
/// of a silent no-op.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SettingsPatch {
    #[serde(default)]
    editor: Option<EditorPatch>,
}

impl IntoResponse for SettingsError {
    fn into_response(self) -> Response {
        let status = match &self {
            SettingsError::Invalid(_) => StatusCode::BAD_REQUEST,
            SettingsError::Forbidden(_) => StatusCode::FORBIDDEN,
            SettingsError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (
            status,
            Json(json!({ "error": "settings", "detail": self.to_string() })),
        )
            .into_response()
    }
}

async fn get_settings(State(state): State<AppState>) -> Json<serde_json::Value> {
    let snapshot = state.settings.snapshot();
    Json(json!(SettingsView {
        editor: snapshot.editor
    }))
}

/// Apply a partial update. The raw body is inspected first so a forbidden key
/// gets a 403 with a pointed message, distinct from the generic 400 a typo gets.
async fn put_settings(
    State(state): State<AppState>,
    Json(raw): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, SettingsError> {
    if let Some(obj) = raw.as_object() {
        for forbidden in ["authToken", "auth_token", "projects"] {
            if obj.contains_key(forbidden) {
                return Err(SettingsError::Forbidden(format!(
                    "{forbidden} cannot be modified via PUT /api/settings"
                )));
            }
        }
    }

    let patch: SettingsPatch = serde_json::from_value(raw)
        .map_err(|e| SettingsError::Invalid(format!("bad settings patch: {e}")))?;

    let store = state.settings.clone();
    let updated = tokio::task::spawn_blocking(move || {
        store.update(|settings| {
            if let Some(editor_patch) = &patch.editor {
                editor_patch.apply(&mut settings.editor)?;
            }
            Ok(settings.editor.clone())
        })
    })
    .await
    .map_err(|e| SettingsError::Io(format!("settings task failed: {e}")))??;

    Ok(Json(json!(SettingsView { editor: updated })))
}
