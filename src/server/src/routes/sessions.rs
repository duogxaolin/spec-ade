//! Session routes — bind a project to a session on a live ACP connection.
//!
//! Spec: `docs/specs/SPEC-003-acp-orchestration.md` §3.1.
//!
//! ```text
//! POST   /api/projects/{id}/sessions   {connectionId} → 201 Session
//! GET    /api/projects/{id}/sessions   → 200 [Session]
//! DELETE /api/sessions/{id}            → 204
//! ```
//!
//! Sessions live in RAM only this phase ([INVENTED-9]): persisting to
//! `acp-history/{session_id}.json` waits on the block format SPEC-004's chat UI
//! settles, and writing a format now would only have to be migrated.
//!
//! The session `cwd` is the project's canonical path, which is also the `fs/*`
//! sandbox root — one project, one root, no way for the two to disagree.

use std::path::PathBuf;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get},
};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::acp::SessionInfo;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{id}/sessions",
            get(list_sessions).post(create_session),
        )
        .route("/sessions/{id}", delete(delete_session))
}

fn error_response(status: StatusCode, group: &'static str, detail: impl Into<String>) -> Response {
    (
        status,
        Json(json!({ "error": group, "detail": detail.into() })),
    )
        .into_response()
}

/// Look up a project's canonical root.
///
/// Returns `Option` rather than `Result<_, Response>` so the error path stays a
/// cheap `None` — an `axum::Response` in an `Err` variant is 128 bytes carried
/// through every success path too.
fn project_root(state: &AppState, id: &str) -> Option<PathBuf> {
    state
        .settings
        .snapshot()
        .projects
        .iter()
        .find(|p| p.id == id)
        .map(|p| PathBuf::from(&p.path))
}

fn no_such_project(id: &str) -> Response {
    error_response(StatusCode::NOT_FOUND, "project", format!("no project {id}"))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateSession {
    connection_id: String,
}

/// `POST /api/projects/{id}/sessions` — open an ACP session on a connection.
///
/// The 201 carries `agentSessionId` (A3): the id the *agent* assigned. It is
/// reported rather than hidden because it is what appears in agent-side logs, so
/// having it makes a cross-system trace possible.
async fn create_session(
    State(state): State<AppState>,
    Path(project_id): Path<String>,
    Json(body): Json<CreateSession>,
) -> Response {
    let Some(root) = project_root(&state, &project_id) else {
        return no_such_project(&project_id);
    };

    let Some(conn) = state.acp.get(&body.connection_id) else {
        return error_response(
            StatusCode::NOT_FOUND,
            "connection",
            format!("no connection {}", body.connection_id),
        );
    };
    // A connection spawned for another project would run the session against the
    // wrong `cwd` and, worse, sandbox `fs/*` to the wrong root.
    if conn.project_id != project_id {
        return error_response(
            StatusCode::CONFLICT,
            "connection",
            format!(
                "connection {} belongs to project {}, not {project_id}",
                conn.id, conn.project_id
            ),
        );
    }

    match state.acp.create_session(&conn, &project_id, root).await {
        Ok(info) => (StatusCode::CREATED, Json(info)).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `GET /api/projects/{id}/sessions` — sessions of one project, oldest first.
async fn list_sessions(State(state): State<AppState>, Path(project_id): Path<String>) -> Response {
    // Validate the project so an unknown id is a 404 rather than an empty list
    // that reads as "this project simply has no sessions".
    if project_root(&state, &project_id).is_none() {
        return no_such_project(&project_id);
    }
    let mut sessions: Vec<SessionInfo> = state.acp.list_sessions(&project_id);
    // Stable order for the UI. `createdAt` is fixed-width RFC 3339 UTC, so
    // lexicographic order is chronological; `id` breaks ties inside one millisecond.
    sessions.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    Json(sessions).into_response()
}

/// `DELETE /api/sessions/{id}` — forget a session.
///
/// The agent process stays up: one connection serves many sessions ([INVENTED-1]),
/// so killing it here would take down the sessions still in use. Removing the last
/// session leaves the connection idle, and the idle reaper ([INVENTED-10]) collects
/// it if nobody comes back.
async fn delete_session(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.acp.remove_session(&id) {
        Some(_) => StatusCode::NO_CONTENT.into_response(),
        None => error_response(StatusCode::NOT_FOUND, "session", format!("no session {id}")),
    }
}
