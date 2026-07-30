//! Auth — token gate for every `/api/*` route except `/api/health`.
//!
//! SECURITY (docs/analysis/06-api-contract.md §warning, deep-dive 02 §4):
//! PTY/ACP-over-WebSocket without auth is remote code execution *by design*.
//! Binding loopback is NOT enough — any web page in the user's browser can open
//! `new WebSocket("ws://localhost:<port>/...")` (CSRF-on-WS, not blocked by the
//! Same-Origin Policy) or use DNS-rebinding to reach us. So a random session
//! token gates every `/api/*` route from Phase 0 on, *before* any terminal/ACP
//! handler ever ships. This is the single highest risk in the whole design.
//!
//! Pattern borrowed from sshx (`sshx-server/src/web/socket.rs:99-128`): the
//! token is compared with `subtle::ConstantTimeEq` so a wrong token can't be
//! recovered via a timing side-channel. A plain `==` on `&str`/`&[u8]` short-
//! circuits on the first differing byte and leaks the shared prefix length.

use axum::{
    Json,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;
use subtle::ConstantTimeEq;

use crate::AppState;

/// Header carrying the session token (primary channel used by the SPA/CLI).
pub const TOKEN_HEADER: &str = "x-spec-ade-token";
/// Cookie name carrying the session token (browser fallback / Tauri WebView).
pub const TOKEN_COOKIE: &str = "spec_ade_token";
/// Query-param carrying the token — needed for WebSocket upgrades, where custom
/// headers can't be set from browser `WebSocket` clients (deep-dive 02 §4.4 #2).
pub const TOKEN_QUERY: &str = "token";

/// Constant-time equality on the raw token bytes.
///
/// `subtle`'s `[u8]: ConstantTimeEq` returns `0` immediately when the lengths
/// differ, then compares every remaining byte without early exit — so neither
/// the length beyond the shared prefix nor the first mismatch position leaks.
pub fn token_matches(expected: &str, provided: &str) -> bool {
    expected.as_bytes().ct_eq(provided.as_bytes()).into()
}

/// Extract the presented token from (in order) the token header, an
/// `Authorization: Bearer <token>` header, the token cookie, or a `?token=`
/// query param. Returns `None` if none is present.
fn extract_token(req: &Request) -> Option<String> {
    let headers = req.headers();

    if let Some(v) = headers.get(TOKEN_HEADER).and_then(|v| v.to_str().ok()) {
        return Some(v.to_string());
    }

    if let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        && let Some(bearer) = auth.strip_prefix("Bearer ")
    {
        return Some(bearer.trim().to_string());
    }

    if let Some(cookie_header) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) {
        for pair in cookie_header.split(';') {
            let pair = pair.trim();
            if let Some(val) = pair.strip_prefix(&format!("{TOKEN_COOKIE}=")) {
                return Some(val.to_string());
            }
        }
    }

    if let Some(query) = req.uri().query() {
        for pair in query.split('&') {
            if let Some(val) = pair.strip_prefix(&format!("{TOKEN_QUERY}=")) {
                // Query values may be percent-encoded; tokens are hex so this is
                // usually a no-op, but decode defensively.
                let decoded = percent_decode(val);
                return Some(decoded);
            }
        }
    }

    None
}

/// Minimal percent-decoder for query tokens (avoids pulling in a URL crate for
/// Phase 0). Handles `%XX` escapes and `+` → space; leaves other bytes as-is.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Axum middleware gating a route group behind the session token.
///
/// Mounted via `from_fn_with_state` on the authenticated `/api/*` sub-router.
/// `/api/health` is intentionally mounted OUTSIDE this layer so liveness checks
/// need no token.
pub async fn require_auth(State(state): State<AppState>, req: Request, next: Next) -> Response {
    match extract_token(&req) {
        Some(token) if token_matches(&state.auth_token, &token) => next.run(req).await,
        _ => unauthorized(),
    }
}

/// 401 with a small JSON body and a `WWW-Authenticate` hint.
fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Bearer")],
        Json(json!({ "error": "unauthorized", "detail": "missing or invalid session token" })),
    )
        .into_response()
}

// ---- Origin allowlist (CSRF-on-WebSocket defence) --------------------------

/// Hosts allowed to originate requests, port-insensitive.
///
/// A WebSocket handshake is not subject to the Same-Origin Policy and has no
/// CORS preflight, so any page the user visits could open a socket to our port.
/// The token blocks that on its own — but a token can leak (a screenshot, a
/// shared log line, `ps` output), and this is the second lock on the same door
/// (deep-dive 02 §4.2 `check_origin`, §4.4 #3).
const ALLOWED_ORIGIN_HOSTS: &[&str] = &["localhost", "127.0.0.1", "[::1]", "::1"];

/// Origin schemes accepted alongside the host allowlist. `tauri://localhost` is
/// what the Tauri v2 WebView sends (SPEC-009 ships that shell).
const ALLOWED_ORIGIN_SCHEMES: &[&str] = &["http", "https", "tauri"];

/// Decide whether an `Origin` header value may talk to us.
///
/// `None` (header absent) is allowed: browsers always send `Origin` on a
/// WebSocket handshake and on cross-origin fetches, so an absent header means a
/// non-browser client (curl, the CLI, integration tests, Tauri's native side) —
/// which the token already gates. Rejecting it would break those clients while
/// blocking nothing a browser can do.
pub fn origin_allowed(origin: Option<&str>) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    // `null` is what a sandboxed iframe or a `file://` page sends. Treat it as
    // hostile: no legitimate Spec ADE client reports it.
    if origin == "null" || origin.is_empty() {
        return false;
    }

    let Some((scheme, rest)) = origin.split_once("://") else {
        return false;
    };
    if !ALLOWED_ORIGIN_SCHEMES.contains(&scheme) {
        return false;
    }

    // Strip the port. IPv6 literals are bracketed (`[::1]:4123`), so only split
    // on a colon that follows the closing bracket.
    let host = match rest.rfind(']') {
        Some(close) => &rest[..=close],
        None => rest.split(':').next().unwrap_or(rest),
    };

    ALLOWED_ORIGIN_HOSTS
        .iter()
        .any(|allowed| host.eq_ignore_ascii_case(allowed))
}

/// Axum middleware rejecting requests from a non-allowlisted `Origin`.
///
/// Mounted on the same authenticated sub-router as `require_auth`, so it runs
/// during the WS upgrade — before any PTY is spawned (ttyd's ordering,
/// deep-dive 02 §4.4 #5).
pub async fn require_origin(req: Request, next: Next) -> Response {
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok());

    if origin_allowed(origin) {
        next.run(req).await
    } else {
        tracing::warn!(
            "rejected request from disallowed origin: {}",
            origin.unwrap_or("<invalid>")
        );
        (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "forbidden", "detail": "origin not allowed" })),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::origin_allowed;

    #[test]
    fn allows_loopback_on_any_port() {
        for origin in [
            "http://localhost:4123",
            "http://127.0.0.1:4123",
            "http://localhost:5173", // Vite dev server
            "https://localhost:4123",
            "http://[::1]:4123",
            "http://localhost", // no port
            "http://LocalHost:4123",
        ] {
            assert!(origin_allowed(Some(origin)), "should allow {origin}");
        }
    }

    #[test]
    fn allows_tauri_webview() {
        assert!(origin_allowed(Some("tauri://localhost")));
    }

    #[test]
    fn allows_absent_origin() {
        // Non-browser clients (CLI, tests, Tauri native) send no Origin; the
        // token still gates them.
        assert!(origin_allowed(None));
    }

    #[test]
    fn rejects_foreign_and_spoofed_origins() {
        for origin in [
            "http://evil.com",
            "https://evil.com:4123",
            // Prefix/suffix tricks against a naive `contains`/`starts_with`.
            "http://localhost.evil.com",
            "http://evil.com/localhost",
            "http://notlocalhost",
            "http://127.0.0.1.evil.com",
            // Non-http schemes we don't ship.
            "ws://localhost:4123",
            "file://localhost",
            // Sandboxed iframe / file:// page.
            "null",
            "",
            // Malformed.
            "localhost:4123",
            "http:/localhost",
        ] {
            assert!(!origin_allowed(Some(origin)), "should reject {origin:?}");
        }
    }
}
