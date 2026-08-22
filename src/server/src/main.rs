//! Spec ADE backend — single Axum binary entry point.
//!
//! Architecture: docs/analysis/02-architecture.md — one origin serving the SPA
//! (embedded via rust-embed), REST for CRUD, WebSocket for terminal + chat
//! streaming, SSE for git watch. Roadmap: docs/analysis/07-build-roadmap.md —
//! this is Pha 0 (skeleton): bind host/port, mount the router, serve the SPA,
//! expose `/api/health` + an authenticated WS echo.
//!
//! SECURITY (06-api-contract.md §warning): PTY/ACP-over-WS without auth = RCE by
//! design. Binding loopback is NOT enough (DNS-rebinding / CSRF-on-WS). The token
//! gate in `spec_ade_server::auth` runs on every `/api/*` route except health,
//! from Phase 0 on — before any PTY/ACP handler ships. See `auth.rs`.

use std::net::SocketAddr;

use clap::Parser;
use spec_ade_server::{AppState, build_router, settings::Settings};

/// Default bind host — overridable via `SPEC_ADE_HOST` or `-H/--host`.
const DEFAULT_HOST: &str = "0.0.0.0";
/// Default bind port — overridable via `SPEC_ADE_PORT` or `-p/--port`.
const DEFAULT_PORT: u16 = 4123;

/// CLI arguments (07 Pha 0). Env vars provide defaults so the same knobs work
/// via `SPEC_ADE_HOST`/`SPEC_ADE_PORT`; an explicit flag overrides the env.
#[derive(Debug, Parser)]
#[command(name = "spec-ade-server", version, about = "Spec ADE backend server")]
struct Cli {
    /// Host/interface to bind (env: SPEC_ADE_HOST).
    #[arg(short = 'H', long, env = "SPEC_ADE_HOST", default_value = DEFAULT_HOST)]
    host: String,

    /// Port to bind (env: SPEC_ADE_PORT).
    #[arg(short = 'p', long, env = "SPEC_ADE_PORT", default_value_t = DEFAULT_PORT)]
    port: u16,

    /// Do not open the app in a browser on startup.
    #[arg(long)]
    no_open: bool,
}

#[tokio::main]
async fn main() {
    // Tracing from RUST_LOG (defaults to info for our crate) — deep-dive/02 log.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "spec_ade_server=info,tower_http=info".into()),
        )
        .init();

    let cli = Cli::parse();

    // Load (or first-run generate) the session token from the data dir.
    let settings = Settings::load_or_init().expect("failed to load settings / auth token");
    let state = AppState::new(settings.auth_token);

    let app = build_router(state.clone());

    // autoStart ([INVENTED-5], SPEC-007 §5.7): enabled claws come up at boot, so
    // a schedule keeps firing with no UI open. Best-effort per claw.
    {
        let loaded = state.settings.snapshot();
        state.claws.autostart(&loaded, &state.acp).await;
    }

    let addr: SocketAddr = format!("{}:{}", cli.host, cli.port)
        .parse()
        .expect("invalid host/port");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind listener");
    let local_addr = listener.local_addr().expect("listener has no local addr");

    // The URL a browser can actually reach: 0.0.0.0 isn't connectable, so show
    // loopback. Include the token so the SPA can authenticate on first load.
    let display_host = if cli.host == "0.0.0.0" || cli.host == "::" {
        "127.0.0.1".to_string()
    } else {
        cli.host.clone()
    };
    let url = format!(
        "http://{display_host}:{}/?token={}",
        local_addr.port(),
        state.auth_token
    );

    tracing::info!("spec-ade-server listening on http://{local_addr}");
    tracing::info!("open: {url}");

    if !cli.no_open {
        open_browser(&url);
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("server error");

    // Graceful shutdown (SPEC-007 §5.7): the serve future resolved, meaning the
    // listener is closed — no request can start a new claw now. Abort every loop
    // and kill its connection before the process exits.
    state.claws.stop_all(&state.acp).await;
}

/// Best-effort browser open. Non-fatal: a failure just logs a warning so the
/// server still runs headless (CI, remote, `--no-open`).
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let cmd = ("open", vec![url]);
    #[cfg(target_os = "linux")]
    let cmd = ("xdg-open", vec![url]);
    #[cfg(target_os = "windows")]
    let cmd = ("cmd", vec!["/C", "start", "", url]);

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    {
        let (bin, args) = cmd;
        if let Err(e) = std::process::Command::new(bin).args(args).spawn() {
            tracing::warn!("could not open browser ({bin}): {e}");
        }
    }
}

/// Resolve on Ctrl-C for a clean shutdown (deep-dive 02 §3.2 graceful shutdown).
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received");
}
