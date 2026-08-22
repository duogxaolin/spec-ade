//! Bootstrap integration tests (07-build-roadmap.md Pha 0 deliverable #7).
//!
//! Two styles:
//! - HTTP assertions drive the router in-process via `tower::ServiceExt::oneshot`
//!   (no socket needed) — health JSON, and the auth gate (401 without token,
//!   200 with a valid token).
//! - WS assertions run the app on an ephemeral listener and open a real
//!   WebSocket with `tokio-tungstenite` to prove the echo round-trips and that
//!   the token gate applies to the upgrade too.
//!
//! SECURITY coverage: the auth tests are the guardrail for the whole design
//! (PTY/ACP-over-WS = RCE without a token — deep-dive 02 §4). A wrong token, a
//! missing token, and a token of the wrong length must all be rejected.

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use spec_ade_server::{AppState, VERSION, auth, build_router};
use tower::ServiceExt; // for `oneshot`

/// A router with a known token, for in-process HTTP assertions.
fn test_app() -> (axum::Router, String) {
    let token = "test-token-abc123".to_string();
    let app = build_router(AppState::new(token.clone()));
    (app, token)
}

#[tokio::test]
async fn health_returns_ok_json_without_token() {
    let (app, _token) = test_app();

    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);

    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["version"], VERSION);
}

#[tokio::test]
async fn api_without_token_is_unauthorized() {
    let (app, _token) = test_app();

    // The echo WS route lives under the auth layer; a plain GET (no upgrade,
    // no token) must be rejected by the middleware before any handler runs.
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/ws/echo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn api_with_valid_token_passes_auth() {
    let (app, token) = test_app();

    // With a valid token the request clears the auth layer. It's a non-upgrade
    // GET to a WS route, so it won't be 200, but it must NOT be 401 — proving
    // the token was accepted. (axum returns 400/426 for a bad WS upgrade.)
    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/ws/echo")
                .header(auth::TOKEN_HEADER, &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_ne!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn api_with_wrong_token_is_unauthorized() {
    let (app, _token) = test_app();

    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/ws/echo")
                .header(auth::TOKEN_HEADER, "totally-wrong-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn token_accepted_via_cookie() {
    let (app, token) = test_app();

    let res = app
        .oneshot(
            Request::builder()
                .uri("/api/ws/echo")
                .header(header::COOKIE, format!("{}={}", auth::TOKEN_COOKIE, token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_ne!(res.status(), StatusCode::UNAUTHORIZED);
}

#[test]
fn constant_time_compare_rejects_wrong_and_mismatched_length() {
    // Correct token accepted.
    assert!(auth::token_matches("secret-token", "secret-token"));
    // Same length, different content — rejected.
    assert!(!auth::token_matches("secret-token", "secret-tokeX"));
    // Different length — rejected (ct_eq returns 0 on length mismatch).
    assert!(!auth::token_matches("secret-token", "secret"));
    assert!(!auth::token_matches(
        "secret-token",
        "secret-token-plus-extra"
    ));
    // Empty provided token — rejected.
    assert!(!auth::token_matches("secret-token", ""));
}

#[tokio::test]
async fn spa_fallback_serves_index_for_unknown_route() {
    let (app, _token) = test_app();

    // A non-/api client-side route must return the SPA index (200), not 404,
    // so Vue Router history mode works (06 §SPA fallback).
    let res = app
        .oneshot(
            Request::builder()
                .uri("/some/client/side/route")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let ctype = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ctype.starts_with("text/html"), "content-type was {ctype}");
}

// ---- WebSocket round-trip over a real listener -----------------------------

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message as TMessage;

/// Bind the app on an ephemeral port and return the bound address.
async fn spawn_server(token: &str) -> std::net::SocketAddr {
    let app = build_router(AppState::new(token.to_string()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

#[tokio::test]
async fn ws_echo_round_trips_with_valid_token() {
    let token = "ws-token-xyz";
    let addr = spawn_server(token).await;

    // Token passed via query param — the only channel a browser WebSocket can
    // use during the upgrade (deep-dive 02 §4.4 #2).
    let url = format!("ws://{addr}/api/ws/echo?token={token}");
    let (mut socket, _resp) = tokio_tungstenite::connect_async(url).await.unwrap();

    socket.send(TMessage::Text("hello".into())).await.unwrap();

    let reply = socket.next().await.unwrap().unwrap();
    assert_eq!(reply, TMessage::Text("hello".into()));

    socket.close(None).await.unwrap();
}

#[tokio::test]
async fn ws_echo_rejected_without_token() {
    let token = "ws-token-xyz";
    let addr = spawn_server(token).await;

    // No token on the upgrade → the auth middleware returns 401 before upgrade,
    // so the handshake fails.
    let url = format!("ws://{addr}/api/ws/echo");
    let result = tokio_tungstenite::connect_async(url).await;
    assert!(
        result.is_err(),
        "WS upgrade without a token must be rejected"
    );
}
