//! Git routes — status/diff/log/blame and mutations, plus an SSE watch stream.
//!
//! Spec: `docs/specs/SPEC-005-git-integration.md` §3.
//! Contract source: `docs/analysis/06-api-contract.md` §Git.
//!
//! ```text
//! GET  /api/projects/{id}/git/status
//! GET  /api/projects/{id}/git/diff        ?path=&staged=
//! GET  /api/projects/{id}/git/log         ?limit=&before=&path=
//! GET  /api/projects/{id}/git/commit/{oid}
//! GET  /api/projects/{id}/git/branches
//! GET  /api/projects/{id}/git/blame       ?path=
//! GET  /api/projects/{id}/git/blob        ?path=&rev=
//! GET  /api/projects/{id}/git/conflict    ?path=
//! SSE  /api/projects/{id}/git/watch
//! POST /api/projects/{id}/git/{stage,stage-content,unstage-content,discard,discard-content,
//!                              commit,branch,checkout,merge,resolve}
//! ```
//!
//! The read/write split is structural (deep-dive 03 §1, TL;DR #1): reads go through
//! `git::repo` (git2, inside `spawn_blocking`), mutations through `git::mutate`
//! (the `git` CLI, async-native). This module only translates HTTP.
//!
//! Every mutation responds with the fresh `GitStatus` ([SPEC-005 INVENTED-6]) so
//! the client never has to follow a write with a read — which matters when agents
//! are changing files concurrently.

use std::convert::Infallible;
use std::path::PathBuf;
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use tokio_stream::{Stream, StreamExt, wrappers::BroadcastStream};

use crate::AppState;
use crate::git::{
    GitCli, GitError, mutate,
    repo::{self, BlobRev, GitStatus},
    watch::WatchEvent,
};
use crate::routes::error::{ApiError, task_failed};
use crate::routes::projects::project_root;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/git/status", get(status))
        .route("/projects/{id}/git/diff", get(diff))
        .route("/projects/{id}/git/log", get(log))
        .route("/projects/{id}/git/commit/{oid}", get(commit_detail))
        .route("/projects/{id}/git/branches", get(branches))
        .route("/projects/{id}/git/blame", get(blame))
        .route("/projects/{id}/git/blob", get(blob))
        .route("/projects/{id}/git/conflict", get(conflict))
        .route("/projects/{id}/git/watch", get(watch))
        .route("/projects/{id}/git/stage", post(stage))
        .route("/projects/{id}/git/stage-content", post(stage_content))
        .route("/projects/{id}/git/unstage-content", post(unstage_content))
        .route("/projects/{id}/git/commit", post(commit))
        .route("/projects/{id}/git/discard", post(discard))
        .route("/projects/{id}/git/discard-content", post(discard_content))
        .route("/projects/{id}/git/branch", post(create_branch))
        .route("/projects/{id}/git/checkout", post(checkout))
        .route("/projects/{id}/git/merge", post(merge))
        .route("/projects/{id}/git/resolve", post(resolve))
}

// ---- error mapping ---------------------------------------------------------

impl From<GitError> for ApiError {
    fn from(e: GitError) -> Self {
        let status = match &e {
            // Not a repository is a normal state for `GET status` (handled before
            // this conversion) but a real 400 for anything else: there is nothing
            // to diff or commit.
            GitError::NotARepo => StatusCode::BAD_REQUEST,
            GitError::Path(_) => StatusCode::BAD_REQUEST,
            // 403: understood and refused, on the same line the file API draws for
            // `PathError::Escapes` (C17).
            GitError::Forbidden(_) => StatusCode::FORBIDDEN,
            GitError::NotFound(_) => StatusCode::NOT_FOUND,
            // 409, not 400: the request was well-formed, the repository state
            // refuses it. The client offers a different action rather than fixing
            // its own payload.
            GitError::NothingToCommit
            | GitError::Conflict { .. }
            | GitError::UpToDate
            | GitError::Blocked(_) => StatusCode::CONFLICT,
            // 503: `git` is absent, so mutations are unavailable while reads keep
            // working ([SPEC-005 INVENTED-13]).
            GitError::GitMissing => StatusCode::SERVICE_UNAVAILABLE,
            GitError::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
            GitError::CommandFailed { .. } | GitError::Libgit2(_) | GitError::Io(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };

        // Conflicted paths ride along as a field so the client can open the merge
        // editor on them without re-reading the status.
        let extra = match &e {
            GitError::Conflict { paths, .. } if !paths.is_empty() => {
                Some(json!({ "paths": paths }))
            }
            _ => None,
        };

        ApiError {
            status,
            group: e.group(),
            detail: e.to_string(),
            extra,
        }
    }
}

/// Run a blocking `git2` read on the pool.
///
/// The closure owns the root because a `git2::Repository` is `!Send` and must be
/// opened and dropped inside it (deep-dive 03 §4 #1).
async fn blocking<T, F>(root: PathBuf, f: F) -> Result<T, ApiError>
where
    F: FnOnce(&std::path::Path) -> Result<T, GitError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || f(&root))
        .await
        .map_err(task_failed)?
        .map_err(ApiError::from)
}

/// The `GitCli` for a project — mutations only.
fn cli_for(state: &AppState, id: &str) -> Result<GitCli, ApiError> {
    Ok(GitCli::new(project_root(state, id)?))
}

// ---- reads -----------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PathQuery {
    #[serde(default)]
    path: String,
}

/// `GET …/git/status` (C1–C7).
///
/// A project that is not a git repository answers `200 {isRepo: false}`, not an
/// error: the panel shows "not a git repository" as information, and a red error
/// banner for the normal case of a plain directory would be wrong (C5, §1).
async fn status(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<GitStatus>, ApiError> {
    let root = project_root(&state, &id)?;
    let status = blocking(root, |root| {
        if repo::is_repo(root) {
            repo::status(root)
        } else {
            Ok(GitStatus::not_a_repo())
        }
    })
    .await?;
    Ok(Json(status))
}

#[derive(Debug, Deserialize)]
struct DiffQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    staged: bool,
}

/// `GET …/git/diff` (C8–C10).
async fn diff(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<DiffQuery>,
) -> Result<Json<repo::GitDiff>, ApiError> {
    let root = project_root(&state, &id)?;
    let rel = crate::git::relative_path(&query.path)?;
    let staged = query.staged;
    let diff = blocking(root, move |root| repo::diff(root, &rel, staged)).await?;
    Ok(Json(diff))
}

#[derive(Debug, Deserialize)]
struct LogQuery {
    limit: Option<usize>,
    /// Cursor from a previous page's `nextBefore` ([SPEC-005 INVENTED-7]).
    before: Option<String>,
    /// Restrict the history to commits touching this path.
    path: Option<String>,
}

/// `GET …/git/log` (C11).
async fn log(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<LogQuery>,
) -> Result<Json<repo::GitLog>, ApiError> {
    let root = project_root(&state, &id)?;
    let limit = query.limit.unwrap_or(repo::DEFAULT_LOG_LIMIT);
    let before = query.before;
    // A path filter goes through the same guard as everything else, so `../` in a
    // query string cannot widen the walk beyond the project.
    let path = query
        .path
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .map(crate::git::relative_path)
        .transpose()?;

    let log = blocking(root, move |root| {
        repo::log(root, limit, before.as_deref(), path.as_deref())
    })
    .await?;
    Ok(Json(log))
}

/// `GET …/git/commit/{oid}` (C12).
async fn commit_detail(
    State(state): State<AppState>,
    Path((id, oid)): Path<(String, String)>,
) -> Result<Json<repo::CommitDetail>, ApiError> {
    let root = project_root(&state, &id)?;
    let detail = blocking(root, move |root| repo::commit_detail(root, &oid)).await?;
    Ok(Json(detail))
}

/// `GET …/git/branches` (C13).
async fn branches(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<repo::GitBranches>, ApiError> {
    let root = project_root(&state, &id)?;
    let branches = blocking(root, repo::branches).await?;
    Ok(Json(branches))
}

/// `GET …/git/blame` (C14).
async fn blame(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<PathQuery>,
) -> Result<Json<repo::GitBlame>, ApiError> {
    let root = project_root(&state, &id)?;
    let rel = crate::git::relative_path(&query.path)?;
    let blame = blocking(root, move |root| repo::blame(root, &rel)).await?;
    Ok(Json(blame))
}

#[derive(Debug, Deserialize)]
struct BlobQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    rev: String,
}

/// `GET …/git/blob` (C15).
async fn blob(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<BlobQuery>,
) -> Result<Json<repo::GitBlob>, ApiError> {
    let root = project_root(&state, &id)?;
    let rel = crate::git::relative_path(&query.path)?;
    let rev = BlobRev::parse(&query.rev)?;
    let blob = blocking(root, move |root| repo::blob(root, &rel, rev)).await?;
    Ok(Json(blob))
}

/// `GET …/git/conflict` (C30).
async fn conflict(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<PathQuery>,
) -> Result<Json<repo::GitConflict>, ApiError> {
    let root = project_root(&state, &id)?;
    let rel = crate::git::relative_path(&query.path)?;
    let conflict = blocking(root, move |root| repo::conflict(root, &rel)).await?;
    Ok(Json(conflict))
}

// ---- watch (SSE) -----------------------------------------------------------

/// `SSE …/git/watch` (C32–C35).
///
/// Two event types: `status` carries a `GitStatus`, `stopped` says the watcher gave
/// up and the client should poll instead (C34).
///
/// `BroadcastStream` yields `Err(Lagged)` when a subscriber falls behind. Here that
/// is **skipped, not an error**: the payload is state, so a lagging client wants
/// the newest value and nothing else. This is the opposite of the PTY stream in
/// SPEC-001, where dropping bytes would corrupt the terminal (§5.6).
async fn watch(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let root = project_root(&state, &id)?;
    let rx = state.git.subscribe(&root).await;

    let stream = BroadcastStream::new(rx).filter_map(|item| match item {
        Ok(WatchEvent::Status(status)) => Some(Ok(Event::default()
            .event("status")
            .json_data(&*status)
            .unwrap_or_else(|e| {
                // Serializing our own DTO cannot fail in practice; if it somehow
                // does, say so on the stream rather than killing it silently.
                Event::default()
                    .event("error")
                    .data(format!("serialize failed: {e}"))
            }))),
        Ok(WatchEvent::Stopped { reason }) => {
            Some(Ok(Event::default().event("stopped").data(reason)))
        }
        // Lagged: the next value is already on its way, so drop this notice.
        Err(_) => None,
    });

    Ok(Sse::new(stream).keep_alive(
        // Comment frames keep a proxy from closing an idle stream. 15s is well
        // inside the usual 30–60s idle timeout.
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

// ---- mutations -------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StageBody {
    paths: Vec<String>,
    /// `true` unstages instead of staging (C17).
    #[serde(default)]
    unstage: bool,
}

/// `POST …/git/stage` (C16–C17).
async fn stage(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<StageBody>,
) -> Result<Json<GitStatus>, ApiError> {
    let cli = cli_for(&state, &id)?;
    Ok(Json(mutate::stage(&cli, &body.paths, body.unstage).await?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ContentBody {
    path: String,
    /// The complete document the target side should contain after one hunk action.
    content: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DiscardContentBody {
    path: String,
    content: String,
    /// Blob id of the worktree document whose hunk controls were rendered.
    expected_oid: String,
}

/// `POST …/git/stage-content` — replace one index entry, not the worktree.
async fn stage_content(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ContentBody>,
) -> Result<Json<GitStatus>, ApiError> {
    let cli = cli_for(&state, &id)?;
    Ok(Json(
        mutate::stage_content(&cli, &body.path, &body.content).await?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UnstageContentBody {
    path: String,
    content: String,
    /// Whether the selected result has a path at HEAD. False removes a newly-added
    /// path from the index while preserving its worktree file.
    exists: bool,
}

/// `POST …/git/unstage-content` — replace or remove one index entry.
async fn unstage_content(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UnstageContentBody>,
) -> Result<Json<GitStatus>, ApiError> {
    let cli = cli_for(&state, &id)?;
    Ok(Json(
        mutate::unstage_content(&cli, &body.path, &body.content, body.exists).await?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CommitBody {
    message: String,
    #[serde(default)]
    amend: bool,
}

/// `POST …/git/commit` (C18–C21).
async fn commit(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<CommitBody>,
) -> Result<Json<GitStatus>, ApiError> {
    let cli = cli_for(&state, &id)?;
    Ok(Json(mutate::commit(&cli, &body.message, body.amend).await?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PathsBody {
    paths: Vec<String>,
}

/// `POST …/git/discard` (C22–C23).
async fn discard(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<PathsBody>,
) -> Result<Json<GitStatus>, ApiError> {
    let cli = cli_for(&state, &id)?;
    Ok(Json(mutate::discard(&cli, &body.paths).await?))
}

/// `POST …/git/discard-content` — atomically replace only the worktree document.
async fn discard_content(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<DiscardContentBody>,
) -> Result<Json<GitStatus>, ApiError> {
    let cli = cli_for(&state, &id)?;
    Ok(Json(
        mutate::discard_content(&cli, &body.path, &body.content, &body.expected_oid).await?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BranchBody {
    name: String,
    start_point: Option<String>,
    /// Switch to the new branch after creating it.
    #[serde(default)]
    checkout: bool,
}

/// `POST …/git/branch` (C24).
async fn create_branch(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<BranchBody>,
) -> Result<Json<GitStatus>, ApiError> {
    let cli = cli_for(&state, &id)?;
    Ok(Json(
        mutate::branch(&cli, &body.name, body.start_point.as_deref(), body.checkout).await?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CheckoutBody {
    target: String,
    /// Discard local changes to switch anyway ([SPEC-005 INVENTED-11]).
    #[serde(default)]
    force: bool,
}

/// `POST …/git/checkout` (C25–C26).
async fn checkout(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<CheckoutBody>,
) -> Result<Json<GitStatus>, ApiError> {
    let cli = cli_for(&state, &id)?;
    Ok(Json(
        mutate::checkout(&cli, &body.target, body.force).await?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MergeBody {
    /// Ref to merge in, or `"--abort"`-style action via `abort`.
    #[serde(default)]
    from: String,
    #[serde(default)]
    no_ff: bool,
    /// Abort a merge in progress instead of starting one (C29).
    #[serde(default)]
    abort: bool,
}

/// `POST …/git/merge` (C27–C29).
async fn merge(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<MergeBody>,
) -> Result<Json<GitStatus>, ApiError> {
    let cli = cli_for(&state, &id)?;
    if body.abort {
        return Ok(Json(mutate::merge_abort(&cli).await?));
    }
    Ok(Json(mutate::merge(&cli, &body.from, body.no_ff).await?))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResolveBody {
    path: String,
    /// The resolved file content, markers already removed by the client.
    content: String,
}

/// `POST …/git/resolve` (C31).
async fn resolve(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ResolveBody>,
) -> Result<Json<GitStatus>, ApiError> {
    let cli = cli_for(&state, &id)?;
    Ok(Json(
        mutate::resolve(&cli, &body.path, &body.content).await?,
    ))
}
