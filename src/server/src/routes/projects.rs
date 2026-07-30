//! Project routes — CRUD, file tree, file read/write, entry mutations.
//!
//! Spec: `docs/specs/SPEC-002-file-tree-editor.md` §3.2–§3.6.
//! Contract source: 06-api-contract.md §Projects/§Files [PROPOSED].
//!
//! ```text
//! GET    /api/projects              list (sorted by sortOrder, then name)
//! POST   /api/projects              {path, name?, icon?}       → 201 Project
//! PUT    /api/projects/{id}         {name??, icon??, sortOrder??}
//! DELETE /api/projects/{id}         → 204
//! GET    /api/projects/{id}/tree    ?path=<rel>                → DirListing
//! GET    /api/projects/{id}/file    ?path=<rel>                → ReadResult
//! PUT    /api/projects/{id}/file    ?path=<rel> {content,rev?} → WriteResult
//! POST   /api/projects/{id}/entries {path, kind}               → 201
//! PATCH  /api/projects/{id}/entries ?path=<rel> {newPath}
//! DELETE /api/projects/{id}/entries ?path=<rel>&recursive=
//! ```
//!
//! All filesystem work runs in `spawn_blocking` (02:49-55). Mounted inside the
//! authed router so token + Origin gates run before any of this.

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
use crate::files::{self, CreateKind, FileError, PathError};
use crate::settings::{ProjectEntry, SettingsError};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects", get(list_projects).post(create_project))
        .route(
            "/projects/{id}",
            axum::routing::put(update_project).delete(delete_project),
        )
        .route("/projects/{id}/tree", get(get_tree))
        .route("/projects/{id}/file", get(read_file).put(write_file))
        .route(
            "/projects/{id}/entries",
            post(create_entry).patch(rename_entry).delete(delete_entry),
        )
}

// ---- error mapping ---------------------------------------------------------

/// Handler-level error carrying the HTTP mapping (SPEC-002 §3.6).
struct ApiError {
    status: StatusCode,
    group: &'static str,
    detail: String,
    extra: Option<serde_json::Value>,
}

impl ApiError {
    fn new(status: StatusCode, group: &'static str, detail: impl Into<String>) -> Self {
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

impl From<FileError> for ApiError {
    fn from(e: FileError) -> Self {
        match &e {
            FileError::Path(PathError::Escapes) => {
                ApiError::new(StatusCode::FORBIDDEN, "path", e.to_string())
            }
            FileError::Path(_) | FileError::NotADirectory(_) => {
                ApiError::new(StatusCode::BAD_REQUEST, "path", e.to_string())
            }
            FileError::NotFound(_) => ApiError::new(StatusCode::NOT_FOUND, "file", e.to_string()),
            FileError::Conflict(_) => {
                ApiError::new(StatusCode::CONFLICT, "conflict", e.to_string())
            }
            FileError::Io(_) => {
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "io", e.to_string())
            }
        }
    }
}

impl From<SettingsError> for ApiError {
    fn from(e: SettingsError) -> Self {
        let status = match &e {
            SettingsError::Invalid(_) => StatusCode::BAD_REQUEST,
            SettingsError::Forbidden(_) => StatusCode::FORBIDDEN,
            SettingsError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        ApiError::new(status, "settings", e.to_string())
    }
}

/// spawn_blocking join failure — a bug, not a client error.
fn task_failed(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "io",
        format!("task failed: {e}"),
    )
}

/// Look up a project and return its canonical root.
fn project_root(state: &AppState, id: &str) -> Result<PathBuf, ApiError> {
    state
        .settings
        .snapshot()
        .projects
        .iter()
        .find(|p| p.id == id)
        .map(|p| PathBuf::from(&p.path))
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "project", format!("no project {id}")))
}

// ---- project CRUD ----------------------------------------------------------

async fn list_projects(State(state): State<AppState>) -> Json<Vec<ProjectEntry>> {
    let mut projects = state.settings.snapshot().projects;
    projects.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then_with(|| a.name.cmp(&b.name))
    });
    Json(projects)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateProject {
    path: String,
    name: Option<String>,
    icon: Option<String>,
}

/// Icon constraint ([INVENTED-5]): short string the server never interprets.
fn validate_icon(icon: &str) -> Result<(), ApiError> {
    if icon.chars().count() > 8 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "project",
            "icon must be at most 8 characters",
        ));
    }
    Ok(())
}

async fn create_project(
    State(state): State<AppState>,
    Json(body): Json<CreateProject>,
) -> Result<Response, ApiError> {
    if let Some(icon) = &body.icon {
        validate_icon(icon)?;
    }

    // Canonicalize first: uniqueness is on the canonical path ([INVENTED-4]),
    // so `/tmp/x` and `/private/tmp/x` are the same project on macOS.
    let requested = PathBuf::from(&body.path);
    let canonical = tokio::task::spawn_blocking(move || {
        let meta = std::fs::metadata(&requested).ok()?;
        if !meta.is_dir() {
            return None;
        }
        requested.canonicalize().ok()
    })
    .await
    .map_err(task_failed)?
    .ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "project",
            format!("{} is not an existing directory", body.path),
        )
    })?;

    let canonical_str = canonical.display().to_string();
    let name = body.name.clone().unwrap_or_else(|| {
        canonical
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| canonical_str.clone())
    });

    let store = state.settings.clone();
    let icon = body.icon.clone();
    let created = tokio::task::spawn_blocking(move || {
        store.update(move |settings| {
            if let Some(existing) = settings.projects.iter().find(|p| p.path == canonical_str) {
                // Signalled out-of-band so the handler can shape the 409 with
                // the existing id (SPEC-002 §3.2).
                return Err(SettingsError::Invalid(format!(
                    "__duplicate__:{}",
                    existing.id
                )));
            }
            let next_order = settings
                .projects
                .iter()
                .map(|p| p.sort_order)
                .max()
                .map_or(0, |m| m + 1);
            let entry = ProjectEntry {
                id: uuid::Uuid::new_v4().to_string(),
                path: canonical_str.clone(),
                name,
                icon,
                sort_order: next_order,
            };
            settings.projects.push(entry.clone());
            Ok(entry)
        })
    })
    .await
    .map_err(task_failed)?;

    match created {
        Ok(entry) => Ok((StatusCode::CREATED, Json(entry)).into_response()),
        Err(SettingsError::Invalid(msg)) if msg.starts_with("__duplicate__:") => {
            let existing_id = msg.trim_start_matches("__duplicate__:").to_string();
            Err(ApiError {
                status: StatusCode::CONFLICT,
                group: "project",
                detail: "path is already registered".into(),
                extra: Some(json!({ "existingId": existing_id })),
            })
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateProject {
    #[serde(default, deserialize_with = "crate::settings::double_option")]
    name: Option<Option<String>>,
    #[serde(default, deserialize_with = "crate::settings::double_option")]
    icon: Option<Option<String>>,
    #[serde(default, deserialize_with = "crate::settings::double_option")]
    sort_order: Option<Option<i64>>,
}

async fn update_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateProject>,
) -> Result<Json<ProjectEntry>, ApiError> {
    if let Some(Some(icon)) = &body.icon {
        validate_icon(icon)?;
    }
    if let Some(Some(name)) = &body.name
        && name.trim().is_empty()
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "project",
            "name cannot be empty",
        ));
    }

    let store = state.settings.clone();
    let updated = tokio::task::spawn_blocking(move || {
        store.update(move |settings| {
            let entry = settings
                .projects
                .iter_mut()
                .find(|p| p.id == id)
                .ok_or_else(|| SettingsError::Invalid(format!("__missing__:{id}")))?;

            // Option<Option<T>> per 06 §Settings: absent=keep, null=default, value=set.
            if let Some(name) = &body.name {
                match name {
                    Some(v) => entry.name = v.clone(),
                    // null name → back to the directory-name default.
                    None => {
                        entry.name = std::path::Path::new(&entry.path)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| entry.path.clone());
                    }
                }
            }
            if let Some(icon) = &body.icon {
                entry.icon = icon.clone();
            }
            if let Some(sort_order) = &body.sort_order {
                entry.sort_order = sort_order.unwrap_or(0);
            }
            Ok(entry.clone())
        })
    })
    .await
    .map_err(task_failed)?;

    match updated {
        Ok(entry) => Ok(Json(entry)),
        Err(SettingsError::Invalid(msg)) if msg.starts_with("__missing__:") => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "project",
            "no such project",
        )),
        Err(e) => Err(e.into()),
    }
}

async fn delete_project(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let store = state.settings.clone();
    let removed = tokio::task::spawn_blocking({
        let id = id.clone();
        move || {
            store.update(move |settings| {
                let before = settings.projects.len();
                settings.projects.retain(|p| p.id != id);
                Ok(settings.projects.len() < before)
            })
        }
    })
    .await
    .map_err(task_failed)??;

    if removed {
        // Cascade ACP (SPEC-003): the project's agents hold its path as their
        // session `cwd` and `fs/*` sandbox root. Left running they would keep
        // working against a directory the user just deregistered.
        // TODO(spec-008): cascade layout (06:23 also requires it; no layout yet).
        let killed = state.acp.kill_project(&id).await;
        if killed > 0 {
            tracing::info!("project {id} deleted: killed {killed} ACP connection(s)");
        }
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "project",
            "no such project",
        ))
    }
}

// ---- tree ------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PathQuery {
    #[serde(default)]
    path: String,
}

async fn get_tree(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<PathQuery>,
) -> Result<Json<files::DirListing>, ApiError> {
    let root = project_root(&state, &id)?;
    let rel = query.path;

    let listing = tokio::task::spawn_blocking(move || -> Result<files::DirListing, FileError> {
        let abs = files::resolve(&root, &rel)?;
        let meta = std::fs::metadata(&abs).map_err(|_| FileError::NotFound(rel.clone()))?;
        if !meta.is_dir() {
            return Err(FileError::NotADirectory(format!(
                "{rel} is not a directory"
            )));
        }
        Ok(files::list_dir(&abs, &rel)?)
    })
    .await
    .map_err(task_failed)??;

    Ok(Json(listing))
}

// ---- file read/write -------------------------------------------------------

async fn read_file(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<PathQuery>,
) -> Result<Json<files::ReadResult>, ApiError> {
    let root = project_root(&state, &id)?;
    let rel = query.path;
    let result = tokio::task::spawn_blocking(move || files::read(&root, &rel))
        .await
        .map_err(task_failed)??;
    Ok(Json(result))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WriteBody {
    content: String,
    /// Optimistic-concurrency tag; absent = force overwrite (SPEC-002 §3.4).
    rev: Option<String>,
}

async fn write_file(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<PathQuery>,
    Json(body): Json<WriteBody>,
) -> Result<Response, ApiError> {
    let root = project_root(&state, &id)?;
    let rel = query.path;

    let result = tokio::task::spawn_blocking(move || {
        files::write(&root, &rel, &body.content, body.rev.as_deref())
    })
    .await
    .map_err(task_failed)?;

    match result {
        Ok(write_result) => Ok(Json(write_result).into_response()),
        Err(FileError::Conflict(detail)) => {
            // The 409 carries the current rev so the client can offer "Ghi đè"
            // (force overwrite) with an informed target.
            let current = detail
                .split("rev ")
                .nth(1)
                .and_then(|s| s.split(',').next())
                .unwrap_or("")
                .to_string();
            Err(ApiError {
                status: StatusCode::CONFLICT,
                group: "conflict",
                detail,
                extra: Some(json!({ "currentRev": current })),
            })
        }
        Err(e) => Err(e.into()),
    }
}

// ---- entries (create / rename / delete) ------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateEntryBody {
    path: String,
    kind: String,
}

async fn create_entry(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<CreateEntryBody>,
) -> Result<Response, ApiError> {
    let root = project_root(&state, &id)?;
    let kind = match body.kind.as_str() {
        "file" => CreateKind::File,
        "dir" => CreateKind::Dir,
        other => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "path",
                format!("kind must be \"file\" or \"dir\", got {other:?}"),
            ));
        }
    };

    let rel = body.path.clone();
    tokio::task::spawn_blocking(move || files::create(&root, &rel, kind))
        .await
        .map_err(task_failed)??;

    Ok((
        StatusCode::CREATED,
        Json(json!({ "path": body.path, "kind": body.kind })),
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RenameBody {
    new_path: String,
}

async fn rename_entry(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<PathQuery>,
    Json(body): Json<RenameBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let root = project_root(&state, &id)?;
    let from = query.path;
    let to = body.new_path.clone();
    tokio::task::spawn_blocking(move || files::rename(&root, &from, &to))
        .await
        .map_err(task_failed)??;
    Ok(Json(json!({ "path": body.new_path })))
}

#[derive(Debug, Deserialize)]
struct DeleteQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    recursive: bool,
}

async fn delete_entry(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<DeleteQuery>,
) -> Result<StatusCode, ApiError> {
    let root = project_root(&state, &id)?;
    let rel = query.path;
    let recursive = query.recursive;
    tokio::task::spawn_blocking(move || files::delete(&root, &rel, recursive))
        .await
        .map_err(task_failed)??;
    Ok(StatusCode::NO_CONTENT)
}
