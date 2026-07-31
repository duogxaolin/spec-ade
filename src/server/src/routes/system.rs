//! System routes — host metrics + process control.
//!
//! Spec: `docs/specs/SPEC-006-search-monitor.md` §3.2–§3.4.
//!
//! ```text
//! GET  /api/system/metrics    ?topN=&sort=cpu|memory
//! SSE  /api/system/watch
//! POST /api/system/kill/{pid}   {"signal": "term"}
//! ```
//!
//! These are host-wide, not per-project, and they sit behind the same token gate
//! as everything else in `/api/*`. That is not a new privilege: a token holder
//! already has a shell through the terminal API, so `POST kill` grants nothing
//! the caller could not do with three keystrokes ([SPEC-006 INVENTED-11]).
//!
//! Both read endpoints answer from the shared sampler rather than building their
//! own `System`. Self-sampling per request would report `cpu.usage = 0.0` every
//! time — CPU usage is a delta, and a fresh instance has nothing to subtract from
//! (§5.5).

use std::convert::Infallible;
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
use crate::monitor::{self, DEFAULT_TOP_N, KillError, KillSignal, Metrics, SortBy};
use crate::routes::error::{ApiError, task_failed};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/system/metrics", get(metrics))
        .route("/system/watch", get(watch))
        .route("/system/kill/{pid}", post(kill))
}

// ---- error mapping ---------------------------------------------------------

impl From<KillError> for ApiError {
    fn from(e: KillError) -> Self {
        let status = match &e {
            KillError::BadSignal(_) => StatusCode::BAD_REQUEST,
            KillError::NotFound(_) => StatusCode::NOT_FOUND,
            // 400: the pid is real but this API will not touch it, and the client
            // fixes that by choosing a different one.
            KillError::Refused(_) => StatusCode::BAD_REQUEST,
            // 501: the signal does not exist on this platform (`kill_with` →
            // `None`). Distinct from 403 below, which means it exists and the OS
            // said no (§3.4).
            KillError::Unsupported(_) => StatusCode::NOT_IMPLEMENTED,
            KillError::Failed(_) => StatusCode::FORBIDDEN,
        };
        let group = match &e {
            KillError::BadSignal(_) => "signal",
            _ => "process",
        };
        ApiError::new(status, group, e.to_string())
    }
}

// ---- metrics ---------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MetricsParams {
    #[serde(default)]
    top_n: Option<usize>,
    #[serde(default)]
    sort: Option<String>,
}

/// `GET /api/system/metrics` (D22–D26).
async fn metrics(
    State(state): State<AppState>,
    Query(params): Query<MetricsParams>,
) -> Json<Metrics> {
    let snapshot = state.monitor.latest().await;
    Json(monitor::narrow(
        &snapshot,
        SortBy::parse(params.sort.as_deref()),
        params.top_n.unwrap_or(DEFAULT_TOP_N),
    ))
}

/// `SSE /api/system/watch` (D27–D30).
///
/// Exists so the sparklines get an even cadence ([SPEC-006 INVENTED-8]): a client
/// polling on a timer drifts whenever the tab is throttled, and N open tabs would
/// mean N process-table enumerations per interval. `GET /metrics` stays as the
/// documented fallback.
///
/// `Lagged` is skipped rather than reported: the payload is a full state snapshot,
/// so a subscriber that fell behind wants the newest one and nothing else — the
/// same reasoning as the git watch stream (SPEC-005 §5.7).
async fn watch(
    State(state): State<AppState>,
    Query(params): Query<MetricsParams>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = state.monitor.subscribe().await;
    let sort = SortBy::parse(params.sort.as_deref());
    let top_n = params.top_n.unwrap_or(DEFAULT_TOP_N);

    let stream = BroadcastStream::new(rx).filter_map(move |item| match item {
        Ok(snapshot) => Some(Ok(Event::default()
            .event("metrics")
            .json_data(monitor::narrow(&snapshot, sort, top_n))
            .unwrap_or_else(|e| {
                Event::default()
                    .event("error")
                    .data(format!("serialize failed: {e}"))
            }))),
        Err(_) => None,
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

// ---- kill ------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct KillBody {
    /// `term` (default) | `kill` | `int` | `hup`.
    #[serde(default)]
    signal: Option<String>,
}

/// `POST /api/system/kill/{pid}` (D31–D35).
async fn kill(
    State(_state): State<AppState>,
    Path(pid): Path<u32>,
    body: Option<Json<KillBody>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // The body is optional so `POST …/kill/123` with no payload works and means
    // the default signal — which is the request the UI's confirm dialog sends.
    let raw = body.and_then(|Json(b)| b.signal).unwrap_or_default();
    let signal = KillSignal::parse(&raw).ok_or(KillError::BadSignal(raw))?;

    // `refresh_processes` walks the process table; on a busy machine that is
    // milliseconds of blocking work, which belongs off the runtime.
    tokio::task::spawn_blocking(move || monitor::kill(pid, signal))
        .await
        .map_err(task_failed)??;

    Ok(Json(
        json!({ "ok": true, "pid": pid, "signal": signal.name() }),
    ))
}
