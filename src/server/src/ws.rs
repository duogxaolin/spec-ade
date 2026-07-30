//! WebSocket echo endpoint — the ping-pong foundation for terminal + ACP.
//!
//! `GET /api/ws/echo` upgrades to a WebSocket and echoes every text/binary
//! message straight back. It is the minimal, testable proof that the single-
//! origin WS transport works end to end (deep-dive 02 §3: WS and SPA share one
//! Router → one origin, no CORS). Later phases replace this shape with the real
//! PTY relay (`/api/terminals/{id}/ws`) and ACP relay (`/api/acp/{id}/ws`).
//!
//! SECURITY: this route is mounted INSIDE the authenticated `/api` sub-router,
//! so the token gate (`crate::auth::require_auth`) runs during the HTTP upgrade
//! request before the socket is established — a browser page cannot open it
//! cross-origin without the token (deep-dive 02 §4).

use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::Response,
};

/// Handle the `/api/ws/echo` upgrade. Auth already ran on the upgrade request.
pub async fn echo_ws(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(handle_echo)
}

/// Echo loop: reflect text and binary frames; answer pings; exit on close.
async fn handle_echo(mut socket: WebSocket) {
    while let Some(Ok(msg)) = socket.recv().await {
        match msg {
            Message::Text(text) => {
                if socket.send(Message::Text(text)).await.is_err() {
                    break;
                }
            }
            Message::Binary(bytes) => {
                if socket.send(Message::Binary(bytes)).await.is_err() {
                    break;
                }
            }
            // axum answers Ping frames with Pong automatically; nothing to echo.
            Message::Ping(_) | Message::Pong(_) => {}
            Message::Close(_) => break,
        }
    }
}
