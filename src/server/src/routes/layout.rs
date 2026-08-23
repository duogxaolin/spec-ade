//! Layout routes — `GET/PUT /api/layout` (SPEC-008 §3.3).
//!
//! The server persists pane trees as OPAQUE JSON: it stores and returns them
//! verbatim and never parses the pane grammar. That grammar (leaf/split,
//! ratios, tab kinds) lives entirely in the frontend, so the layout schema can
//! evolve without a server release ([INVENTED-8-server]).
//!
//! PUT is a top-level field merge: each of `projectLayouts`, `lastLayout`,
//! `layoutPresets` is replaced when present in the body and kept when absent —
//! so a client persisting only `lastLayout` cannot wipe its saved trees.
//!
//! Two guards keep an opaque store from becoming an abuse vector: a 256 KiB body
//! cap (a pane tree is small; anything larger is a bug or a data-store attempt),
//! and a registered-project-key check on `projectLayouts` (a stale client must
//! not resurrect layouts for deleted projects, nor grow settings.json unbounded).

use std::collections::{HashMap, HashSet};

use axum::{Json, Router, body::Bytes, extract::State, http::StatusCode, routing::get};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::AppState;
use crate::routes::error::{ApiError, task_failed};
use crate::settings::{SettingsError, double_option};

/// Hard cap on a PUT body (SPEC-008 §3.3). A pane tree is a handful of nested
/// objects; past a quarter-megabyte it is a client bug or an attempt to use
/// settings.json as a data store, so we reject rather than persist it.
const MAX_LAYOUT_BYTES: usize = 256 * 1024;

pub fn router() -> Router<AppState> {
    Router::new().route("/layout", get(get_layout).put(put_layout))
}

/// The externally visible layout document (camelCase), assembled from the three
/// opaque settings fields. Deliberately not the on-disk `Settings` struct.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LayoutView {
    project_layouts: HashMap<String, Value>,
    last_layout: Option<Value>,
    layout_presets: Vec<Value>,
}

/// Body of `PUT /api/layout`. `deny_unknown_fields` turns a typo'd top-level key
/// into a 400 instead of a silent no-op; nested tree values stay opaque `Value`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LayoutPatch {
    /// Present replaces the whole map; absent keeps it.
    #[serde(default)]
    project_layouts: Option<HashMap<String, Value>>,
    /// Double-option: absent = keep, null = clear, value = set.
    #[serde(default, deserialize_with = "double_option")]
    last_layout: Option<Option<Value>>,
    /// Present replaces the whole list; absent keeps it.
    #[serde(default)]
    layout_presets: Option<Vec<Value>>,
}

async fn get_layout(State(state): State<AppState>) -> Json<LayoutView> {
    let s = state.settings.snapshot();
    Json(LayoutView {
        project_layouts: s.project_layouts,
        last_layout: s.last_layout,
        layout_presets: s.layout_presets,
    })
}

async fn put_layout(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Json<LayoutView>, ApiError> {
    if body.len() > MAX_LAYOUT_BYTES {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "layout",
            format!(
                "layout body {} bytes exceeds cap of {MAX_LAYOUT_BYTES}",
                body.len()
            ),
        ));
    }

    let patch: LayoutPatch = serde_json::from_slice(&body).map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "layout",
            format!("bad layout body: {e}"),
        )
    })?;

    // Validate project keys before the write: every key in `projectLayouts` must
    // name a registered project.
    if let Some(map) = &patch.project_layouts {
        let known: HashSet<String> = state
            .settings
            .snapshot()
            .projects
            .into_iter()
            .map(|p| p.id)
            .collect();
        for key in map.keys() {
            if !known.contains(key) {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "layout",
                    format!("unknown project id in projectLayouts: {key}"),
                ));
            }
        }
    }

    let store = state.settings.clone();
    let result = tokio::task::spawn_blocking(move || {
        store.update(move |settings| {
            if let Some(map) = patch.project_layouts {
                settings.project_layouts = map;
            }
            if let Some(last) = patch.last_layout {
                // Some(None) = clear, Some(Some(v)) = set.
                settings.last_layout = last;
            }
            if let Some(presets) = patch.layout_presets {
                settings.layout_presets = presets;
            }
            Ok(LayoutView {
                project_layouts: settings.project_layouts.clone(),
                last_layout: settings.last_layout.clone(),
                layout_presets: settings.layout_presets.clone(),
            })
        })
    })
    .await
    .map_err(task_failed)?;

    let updated = result.map_err(settings_err)?;
    Ok(Json(updated))
}

/// Map a settings-store failure onto the shared error shape. Our mutation never
/// returns `Invalid`/`Forbidden`, so in practice this is the `Io` (save failed)
/// path — but the match stays exhaustive so a future validation can't slip out
/// untranslated.
fn settings_err(e: SettingsError) -> ApiError {
    match e {
        SettingsError::Io(m) => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "io", m),
        SettingsError::Invalid(m) => ApiError::new(StatusCode::BAD_REQUEST, "layout", m),
        SettingsError::Forbidden(m) => ApiError::new(StatusCode::FORBIDDEN, "layout", m),
    }
}
