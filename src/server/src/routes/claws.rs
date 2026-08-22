//! Claws routes — autonomous agents that run skills on a cron schedule
//! (SPEC-007 §3.2).
//!
//! ```text
//! GET    /api/claws                  ?projectId=<id>  → 200 [{…definition, status}]
//! POST   /api/claws                  ClawInput        → 201 {definition, status}
//! PUT    /api/claws/{id}             ClawInput        → 200 {definition, status}
//! DELETE /api/claws/{id}                              → 204
//! POST   /api/claws/{id}/start                        → 200 {status}
//! POST   /api/claws/{id}/stop                         → 200 {status}
//! GET    /api/projects/{id}/skills                    → 200 [Skill]   ([INVENTED-3])
//! ```
//!
//! Every mutation persists through `SettingsStore::update` (SPEC-007 §5.5) so a
//! failed disk write never leaves RAM and `settings.json` disagreeing. A running
//! Claw is stopped before its definition changes or disappears — the runtime
//! task owns a connection to a project directory, and letting it outlive that
//! directory is exactly the orphaned-agent bug §5.6 exists to prevent.

use std::path::PathBuf;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;

use crate::AppState;
use crate::claws::{ClawDefinition, ClawError, ClawInput, ClawStatus};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/claws", get(list_claws).post(create_claw))
        .route(
            "/claws/{id}",
            get(get_claw).put(update_claw).delete(delete_claw),
        )
        .route("/claws/{id}/start", post(start_claw))
        .route("/claws/{id}/stop", post(stop_claw))
        .route("/projects/{id}/skills", get(list_skills))
}

/// Map the pure layer's errors onto the SPEC-007 §3.2 table.
///
/// Kept as one function (not an `IntoResponse` impl) so every route funnels
/// through it and no variant can grow a second, diverging mapping.
fn claw_error(e: ClawError) -> ApiClawError {
    let status = match &e {
        ClawError::Invalid(_) => StatusCode::BAD_REQUEST,
        ClawError::Cron { .. } => StatusCode::BAD_REQUEST,
        ClawError::UnknownAgent(_) | ClawError::UnknownProject(_) | ClawError::NotFound(_) => {
            StatusCode::NOT_FOUND
        }
        ClawError::Conflict(_) => StatusCode::CONFLICT,
        ClawError::Spawn(_) => StatusCode::BAD_GATEWAY,
    };
    let group = match &e {
        // The cron group rides with the offending schedule index so the UI can
        // mark exactly the right row red (E3).
        ClawError::Cron { index, detail } => {
            return ApiClawError {
                status,
                body: json!({
                    "error": "cron",
                    "detail": format!("schedule {index}: {detail}"),
                    "schedule": index,
                }),
            };
        }
        // The §3.2 table names the *resource* group: an unknown agent is group
        // `agent`, an unknown project group `project` — only definition-local
        // problems are group `claw`.
        ClawError::Invalid(_) | ClawError::NotFound(_) | ClawError::Conflict(_) => "claw",
        ClawError::UnknownAgent(_) => "agent",
        ClawError::UnknownProject(_) => "project",
        // Same reasoning as SPEC-003's spawn mapping: the failure is in the
        // user-configured agent process, not this server.
        ClawError::Spawn(_) => "agent",
    };
    ApiClawError {
        status,
        body: json!({ "error": group, "detail": e.to_string() }),
    }
}

/// [`claw_error`]'s payload — a pre-built JSON body plus status.
struct ApiClawError {
    status: StatusCode,
    body: serde_json::Value,
}

impl IntoResponse for ApiClawError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

/// Query of `GET /api/claws`.
///
/// `rename_all` is load-bearing: the documented wire form is `?projectId=…`,
/// matching every other camelCase identifier this API speaks — without it serde
/// would silently accept only `?project_id=` and the filter would no-op.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListQuery {
    /// Only Claws of this project.
    project_id: Option<String>,
    /// Consumed by the auth middleware; declared so `Query` does not reject it.
    #[allow(dead_code)]
    token: Option<String>,
}

/// One `GET` row: the definition flattened with its read-only runtime view
/// ("list all claw definitions **with** runtime status", `claws.mdx:76`).
fn row(def: &ClawDefinition, state: &AppState) -> serde_json::Value {
    let mut value = serde_json::to_value(def).expect("ClawDefinition is serializable");
    let status: ClawStatus = state.claws.status(def);
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "status".into(),
            serde_json::to_value(status).expect("ClawStatus is serializable"),
        );
    }
    value
}

/// Resolve `(agent, project_root)` for a definition, mirroring
/// `routes/acp.rs::spawn`'s validation order: agent first, then project.
fn resolve_targets(
    state: &AppState,
    def: &ClawDefinition,
) -> Result<(crate::acp::agent::AcpAgentEntry, PathBuf), ClawError> {
    let settings = state.settings.snapshot();
    let agent = settings
        .acp_agents
        .iter()
        .find(|a| a.id == def.agent_id)
        .cloned()
        .ok_or_else(|| ClawError::UnknownAgent(def.agent_id.clone()))?;
    let project = settings
        .projects
        .iter()
        .find(|p| p.id == def.project_id)
        .ok_or_else(|| ClawError::UnknownProject(def.project_id.clone()))?;
    Ok((agent, PathBuf::from(&project.path)))
}

/// `GET /api/claws?projectId=…` — every definition with live status merged in.
async fn list_claws(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
) -> Json<serde_json::Value> {
    let settings = state.settings.snapshot();
    let rows: Vec<serde_json::Value> = settings
        .claws
        .iter()
        .filter(|c| {
            query
                .project_id
                .as_deref()
                .is_none_or(|id| c.project_id == id)
        })
        .map(|def| row(def, &state))
        .collect();
    Json(json!(rows))
}

/// `GET /api/claws/{id}` — one row, or 404 group `claw`.
async fn get_claw(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let settings = state.settings.snapshot();
    match settings.claws.iter().find(|c| c.id == id) {
        Some(def) => Json(row(def, &state)).into_response(),
        None => claw_error(ClawError::NotFound(id)).into_response(),
    }
}

/// Validate a `POST`/`PUT` body against the current catalogue, returning the
/// finished definition. Order matters for the error table (§3.2): agent →
/// project → everything `into_definition` checks.
async fn validate_input(
    state: &AppState,
    id: String,
    Json(body): Json<ClawInput>,
) -> Result<ClawDefinition, ApiClawError> {
    let settings = state.settings.snapshot();
    if !settings.acp_agents.iter().any(|a| a.id == body.agent_id) {
        return Err(claw_error(ClawError::UnknownAgent(body.agent_id.clone())));
    }
    if !settings.projects.iter().any(|p| p.id == body.project_id) {
        return Err(claw_error(ClawError::UnknownProject(
            body.project_id.clone(),
        )));
    }
    body.into_definition(id).map_err(claw_error)
}

/// `POST /api/claws` — create (never start). The id is server-generated and
/// clients are told not to send one by ignoring any they do.
async fn create_claw(
    State(state): State<AppState>,
    body: Result<Json<ClawInput>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => {
            return claw_error(ClawError::Invalid(format!("invalid body: {e}"))).into_response();
        }
    };
    let def = match validate_input(&state, uuid::Uuid::new_v4().to_string(), Json(body)).await {
        Ok(d) => d,
        Err(e) => return e.into_response(),
    };

    let stored = def.clone();
    match state.settings.update(|s| {
        s.claws.push(stored);
        Ok(())
    }) {
        Ok(()) => (StatusCode::CREATED, Json(row(&def, &state))).into_response(),
        Err(e) => io_error(e).into_response(),
    }
}

/// `PUT /api/claws/{id}` — full replace (§3.2): no patch semantics, the whole
/// definition is rewritten from the body. A running Claw is restarted on the new
/// config — there is no "saved but still running the old config" state.
async fn update_claw(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<ClawInput>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(b) => b,
        Err(e) => {
            return claw_error(ClawError::Invalid(format!("invalid body: {e}"))).into_response();
        }
    };
    let new_def = match validate_input(&state, id.clone(), Json(body)).await {
        Ok(d) => d,
        Err(e) => return e.into_response(),
    };

    // A running Claw must not survive onto the new config: stop it before the
    // write so no trigger can fire against a half-replaced definition. The
    // restart happens after the save succeeds. Only a *live* state counts as
    // running — an `error` slot is the E21 placeholder, which the next `start`
    // replaces; treating it as running would stop nothing and then the restart's
    // own start below would collide with it (409).
    let was_running = matches!(
        state.claws.status(&new_def).state,
        crate::claws::ClawState::Starting
            | crate::claws::ClawState::Running
            | crate::claws::ClawState::Idle
    );
    if was_running {
        state.claws.stop(&id, &state.acp).await;
    }

    let stored = new_def.clone();
    // E16: a bad schedule must leave the old definition untouched. Validation
    // already ran above, so reaching the store means the body is good — but the
    // update closure re-checks existence under the same lock the write uses,
    // closing the race where a concurrent DELETE removes the Claw between the
    // snapshot above and this write.
    match state.settings.update(move |s| {
        let slot = s.claws.iter_mut().find(|c| c.id == id).ok_or(
            crate::settings::SettingsError::Invalid(format!("no claw {id}")),
        )?;
        *slot = stored;
        Ok(())
    }) {
        Ok(()) => {}
        Err(crate::settings::SettingsError::Invalid(m)) if m.starts_with("no claw ") => {
            return claw_error(ClawError::NotFound(new_def.id.clone())).into_response();
        }
        Err(e) => return io_error(e).into_response(),
    }

    // Restart only after the new definition is durably saved: a crash between
    // stop and start then leaves a stopped-but-saved Claw, not a running ghost.
    if was_running {
        match resolve_targets(&state, &new_def) {
            Ok((agent, root)) => {
                if let Err(e) = state.claws.start(&new_def, agent, root, &state.acp).await {
                    // E21-shaped: report the spawn failure but keep the saved
                    // definition — the config itself was valid.
                    return claw_error(e).into_response();
                }
            }
            Err(e) => return claw_error(e).into_response(),
        }
    }
    Json(row(&new_def, &state)).into_response()
}

/// `DELETE /api/claws/{id}` — stop any running instance, drop the definition.
/// Second delete finds nothing and returns 404 (E13).
async fn delete_claw(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let exists = {
        let settings = state.settings.snapshot();
        settings.claws.iter().any(|c| c.id == id)
    };
    if !exists {
        return claw_error(ClawError::NotFound(id)).into_response();
    }

    // Stop first: aborting the loop before removing the definition guarantees no
    // window where a trigger fires against a deleted Claw.
    state.claws.stop(&id, &state.acp).await;

    match state.settings.update(move |s| {
        s.claws.retain(|c| c.id != id);
        Ok(())
    }) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => io_error(e).into_response(),
    }
}

/// `POST /api/claws/{id}/start` — bring up the connection now (200 `{status}`).
///
/// Already running is a 409 (E18); a spawn failure is a 502 carrying stderr and
/// leaves `state = error` visible in `GET` (E21).
async fn start_claw(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(def) = current_definition(&state, &id) else {
        return claw_error(ClawError::NotFound(id)).into_response();
    };
    let (agent, root) = match resolve_targets(&state, &def) {
        Ok(t) => t,
        Err(e) => return claw_error(e).into_response(),
    };
    match state.claws.start(&def, agent, root, &state.acp).await {
        Ok(()) => Json(json!({ "status": state.claws.status(&def) })).into_response(),
        Err(e) => claw_error(e).into_response(),
    }
}

/// `POST /api/claws/{id}/stop` — kill the loop + connection. Idempotent (E20):
/// stopping an already-stopped Claw succeeds.
async fn stop_claw(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(def) = current_definition(&state, &id) else {
        return claw_error(ClawError::NotFound(id)).into_response();
    };
    state.claws.stop(&id, &state.acp).await;
    Json(json!({ "status": state.claws.status(&def) })).into_response()
}

/// Snapshot of one definition by id, or `None` when unknown.
fn current_definition(state: &AppState, id: &str) -> Option<ClawDefinition> {
    state
        .settings
        .snapshot()
        .claws
        .into_iter()
        .find(|c| c.id == id)
}

/// `GET /api/projects/{id}/skills` ([INVENTED-3], §3.4).
///
/// Re-scanned per call — "appears automatically … no restart needed"
/// (`skills.mdx:69`) — and off-thread like every filesystem walk since SPEC-002.
async fn list_skills(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let settings = state.settings.snapshot();
    let Some(project) = settings.projects.iter().find(|p| p.id == id) else {
        return claw_error(ClawError::UnknownProject(id)).into_response();
    };
    let root = PathBuf::from(&project.path);

    let skills = tokio::task::spawn_blocking(move || {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        crate::claws::skill::discover(&root, home.as_deref())
    })
    .await;

    match skills {
        Ok(skills) => Json(json!(skills)).into_response(),
        Err(e) => crate::routes::error::task_failed(e).into_response(),
    }
}

/// Persist failure — 500 group `io`, matching the other settings-mutating routes.
fn io_error(e: crate::settings::SettingsError) -> ApiClawError {
    ApiClawError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        body: json!({ "error": "io", "detail": e.to_string() }),
    }
}
