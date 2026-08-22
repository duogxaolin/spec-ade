//! Spec ADE backend library — the Axum app assembly shared by the binary
//! (`main.rs`) and the integration tests (`tests/`).
//!
//! Architecture: docs/analysis/02-architecture.md — one origin serving the SPA
//! (embedded via rust-embed), REST for CRUD, WebSocket for terminal + chat
//! streaming, SSE for git watch. Roadmap: docs/analysis/07-build-roadmap.md,
//! Pha 0 (skeleton).
//!
//! SECURITY (06-api-contract.md §warning, deep-dive 02 §4): PTY/ACP-over-WS
//! without auth is RCE by design. Binding loopback is NOT enough. Every `/api/*`
//! route except `/api/health` sits behind the token gate in `auth`.

pub mod auth;
pub mod files;
pub mod settings;
pub mod spa;
pub mod storage;
pub mod ws;

// PTY layer (SPEC-001) + the route modules. Later-phase route submodules are
// still stubs, kept compilable so `cargo build` stays green.
pub mod pty;
pub mod routes;

// Later-phase modules (docs/analysis/07-build-roadmap.md).
pub mod acp;
pub mod claws;
pub mod git;
pub mod monitor;
pub mod search;

use axum::{
    Json, Router, middleware,
    routing::{any, get},
};
use serde_json::json;

/// Crate version, surfaced by `/api/health`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Shared application state.
///
/// `Clone` because Axum clones state into each request extension. Everything here
/// is either a short `String` or an `Arc` handle, so cloning stays cheap.
#[derive(Clone)]
pub struct AppState {
    /// Random session token gating every `/api/*` route except health.
    pub auth_token: String,
    /// Live PTY registry (SPEC-001). Terminals outlive the sockets watching them.
    pub pty: pty::PtyManager,
    /// Data directory — where generated shell-integration files are written.
    pub data_dir: std::path::PathBuf,
    /// Shared settings document (SPEC-002): editor prefs + project registry.
    pub settings: settings::SettingsStore,
    /// Live ACP agent connections (SPEC-003). Like terminals, an agent outlives
    /// the sockets watching it — closing a tab must not kill a running turn.
    pub acp: acp::AcpManager,
    /// Git status watchers, one poll task per watched project (SPEC-005). Shared
    /// so ten tabs on one project cost one `git status` per interval, not ten.
    pub git: git::GitWatchers,
    /// Host metrics sampler (SPEC-006). One `sysinfo::System` for the whole
    /// server: CPU usage is a delta between refreshes, so a per-request instance
    /// would report 0% forever.
    pub monitor: monitor::MetricsSampler,
    /// Running Claw registry (SPEC-007). Like terminals and agents, a Claw
    /// outlives the sockets watching it — the loop task owns the connection.
    pub claws: claws::ClawRuntime,
}

impl AppState {
    /// State with a given token, a fresh PTY registry, and the resolved data dir.
    ///
    /// Falls back to the system temp dir if the data dir can't be resolved: the
    /// only thing it holds at this point is regenerable scratch files, so a
    /// missing `HOME` should degrade rather than refuse to start.
    pub fn new(auth_token: String) -> Self {
        let data_dir = storage::config_dir().unwrap_or_else(|_| std::env::temp_dir());
        Self::with_data_dir(auth_token, data_dir)
    }

    /// State rooted at an explicit data dir — the seam tests use so settings
    /// and shell-integration files stay inside a temp dir.
    pub fn with_data_dir(auth_token: String, data_dir: std::path::PathBuf) -> Self {
        let settings_path = data_dir.join("settings.json");
        let loaded = settings::Settings::load_or_init_at(&settings_path).unwrap_or_else(|e| {
            tracing::warn!("settings load failed ({e}); starting with defaults");
            settings::Settings::default()
        });
        Self {
            auth_token,
            pty: pty::PtyManager::new(),
            settings: settings::SettingsStore::new(loaded, settings_path),
            acp: acp::AcpManager::new(),
            git: git::GitWatchers::new(),
            monitor: monitor::MetricsSampler::new(),
            claws: claws::ClawRuntime::new(),
            data_dir,
        }
    }
}

/// Assemble the top-level Axum router.
///
/// Layout:
/// - `/api/health` — liveness, mounted OUTSIDE auth so probes need no token.
/// - `/api/*`      — everything else, behind `auth::require_auth`.
/// - fallback      — the embedded SPA (`spa::serve`), with `index.html` fallback
///   for unmatched non-`/api` routes (SPA history mode, 06 §SPA fallback).
pub fn build_router(state: AppState) -> Router {
    // Authenticated API surface. New `/api/*` routes (acp, git, …) get merged
    // into `authed` in later phases so they inherit both gates below.
    //
    // Layer order: `require_origin` is applied last, so it runs *first* on the
    // way in (tower layers wrap outward-in). A hostile origin is rejected before
    // we even look at the token — and long before any PTY is spawned
    // (deep-dive 02 §4.4 #5, ttyd's spawn-after-auth ordering).
    let authed = Router::new()
        .route("/ws/echo", any(ws::echo_ws))
        .merge(routes::terminals::router())
        .merge(routes::settings::router())
        .merge(routes::projects::router())
        .merge(routes::acp::router())
        .merge(routes::sessions::router())
        .merge(routes::git::router())
        .merge(routes::search::router())
        .merge(routes::system::router())
        // Claws (SPEC-007): CRUD + start/stop + the skills catalogue route.
        .merge(routes::claws::router())
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ))
        .layer(middleware::from_fn(auth::require_origin));

    // `/api/health` is merged alongside `authed` (not layered) so it stays
    // token-free, while everything in `authed` is gated. Nesting the combined
    // router once under `/api` avoids the route-overlap panic that a top-level
    // `/api/health` route plus `nest("/api", …)` would trigger.
    let api = Router::new().route("/health", get(health)).merge(authed);

    Router::new()
        .nest("/api", api)
        .fallback(spa::serve)
        .with_state(state)
}

/// Liveness probe. Returns `200` with `{ "status": "ok", "version": "<crate>" }`.
///
/// Mounted outside the auth layer — an unauthenticated caller can check
/// liveness but learns nothing sensitive.
async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "version": VERSION }))
}
