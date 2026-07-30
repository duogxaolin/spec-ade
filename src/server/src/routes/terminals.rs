//! Terminal routes — PTY spawning and bidirectional I/O over WebSocket.
//!
//! Spec: `docs/specs/SPEC-001-terminal.md` (contract: 06 §Terminal).
//!
//! ```text
//! POST   /api/terminals             spawn a PTY            → 201 {id, pid, rows, cols, cwd}
//! GET    /api/terminals             list                   → 200 [TerminalInfo]
//! DELETE /api/terminals/{id}        kill + forget          → 204
//! WS     /api/terminals/{id}/ws     I/O (?after_seq=N)
//! ```
//!
//! Protocol (SPEC-001 §3): control is JSON in both directions; **output is a raw
//! binary frame** so xterm.js can `write()` the bytes with no re-encoding, and
//! escape sequences (bracketed paste, OSC) survive untouched.
//!
//! SECURITY: this is the RCE-prone endpoint the whole auth design exists for. It
//! is mounted inside the sub-router carrying both `require_auth` and
//! `require_origin`, so a rejected upgrade never reaches the PTY layer.

use axum::{
    Json, Router,
    extract::{
        Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{any, delete, post},
};
use base64::Engine as _;
use bytes::Bytes;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;

use crate::AppState;
use crate::pty::{PtyError, SpawnOptions, Terminal, TerminalEvent};

/// Mount the terminal surface. Merged into the authenticated `/api` router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/terminals", post(spawn).get(list))
        .route("/terminals/{id}", delete(kill))
        .route("/terminals/{id}/ws", any(terminal_ws))
}

/// Map a PTY-layer error onto a status code plus JSON body.
impl IntoResponse for PtyError {
    fn into_response(self) -> Response {
        let (status, detail) = match &self {
            PtyError::NotFound => (StatusCode::NOT_FOUND, self.to_string()),
            PtyError::BadCwd(_) => (StatusCode::BAD_REQUEST, self.to_string()),
            PtyError::Pty(_) | PtyError::Io(_) => {
                (StatusCode::INTERNAL_SERVER_ERROR, self.to_string())
            }
        };
        (
            status,
            Json(json!({ "error": "terminal", "detail": detail })),
        )
            .into_response()
    }
}

/// `POST /api/terminals` — spawn a shell.
///
/// The body is optional so `POST` with no payload yields a login shell in `$HOME`.
async fn spawn(
    State(state): State<AppState>,
    body: Option<Json<SpawnOptions>>,
) -> Result<Response, PtyError> {
    let opts = body.map(|Json(o)| o).unwrap_or_default();
    let manager = state.pty.clone();
    let data_dir = state.data_dir.clone();

    // fork/exec blocks; keep it off the async worker threads. The closure still
    // runs inside the runtime context, which `PtyManager::spawn` needs for its
    // `tokio::spawn`ed pump.
    let info = tokio::task::spawn_blocking(move || manager.spawn(opts, data_dir))
        .await
        .map_err(|e| PtyError::Pty(format!("spawn task failed: {e}")))??;

    Ok((StatusCode::CREATED, Json(info)).into_response())
}

/// `GET /api/terminals` — every terminal the server knows about.
async fn list(State(state): State<AppState>) -> Json<Vec<crate::pty::TerminalInfo>> {
    Json(state.pty.list())
}

/// `DELETE /api/terminals/{id}` — kill the shell and drop the registry entry.
async fn kill(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, PtyError> {
    state.pty.remove(&id)?;
    Ok(StatusCode::NO_CONTENT)
}

/// Query string of the WS route.
#[derive(Debug, Deserialize)]
struct WsQuery {
    /// Bytes of output the client already has. Omitted = send all history.
    after_seq: Option<u64>,
    /// Consumed by the auth middleware; declared so `deny_unknown_fields`-free
    /// deserialization doesn't surprise us later.
    #[allow(dead_code)]
    token: Option<String>,
}

/// `GET /api/terminals/{id}/ws` — upgrade and bridge the PTY.
///
/// The terminal is looked up *before* the upgrade so an unknown id is a clean
/// `404` instead of a socket that opens and immediately closes.
async fn terminal_ws(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, PtyError> {
    let terminal = state.pty.get(&id).ok_or(PtyError::NotFound)?;
    Ok(ws.on_upgrade(move |socket| bridge(socket, terminal, query.after_seq)))
}

/// Client → server frames (SPEC-001 §3.1).
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    /// Raw keystrokes / paste, already UTF-8.
    Input {
        data: String,
    },
    /// Bytes that don't survive a JSON string.
    InputB64 {
        data: String,
    },
    /// A whole line: `data` + CR (contract [DOCS]; SPEC-001 §4 [INVENTED-2]).
    Submit {
        data: String,
    },
    Resize {
        rows: u16,
        cols: u16,
    },
    Ping {
        ts: Option<i64>,
    },
}

/// Bridge one WebSocket to one terminal until either side goes away.
///
/// Order of business: replay history, announce `ready`, then relay both
/// directions until the shell exits or the socket closes. Closing the socket does
/// **not** kill the shell (SPEC-001 §4 [INVENTED-4]).
async fn bridge(mut socket: WebSocket, terminal: std::sync::Arc<Terminal>, after_seq: Option<u64>) {
    let requested = after_seq.unwrap_or(0);
    let attachment = terminal.attach(after_seq);
    let mut events = attachment.events;
    // How many output bytes this client has been sent — its replay cursor.
    let mut cursor = attachment.seq;

    // Tell the client if history it asked for had already been pruned, so it can
    // mark the gap rather than silently splice unrelated output together.
    if attachment.from_seq > requested
        && send_json(
            &mut socket,
            &json!({ "type": "truncated", "fromSeq": attachment.from_seq }),
        )
        .await
        .is_err()
    {
        return;
    }

    for chunk in attachment.replay {
        if socket.send(Message::Binary(chunk)).await.is_err() {
            return;
        }
    }

    let info = terminal.info();
    if send_json(
        &mut socket,
        &json!({
            "type": "ready",
            "id": info.id,
            "pid": info.pid,
            "rows": info.rows,
            "cols": info.cols,
            "cwd": info.cwd,
            "seq": cursor,
        }),
    )
    .await
    .is_err()
    {
        return;
    }

    // A client attaching to an already-dead shell needs the exit event too — the
    // broadcast that carried it is long gone.
    if let Some(exit) = attachment.exit {
        let _ = send_json(&mut socket, &exit_json(&exit)).await;
        let _ = socket.send(Message::Close(None)).await;
        return;
    }

    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(TerminalEvent::Output { data, seq_end }) => {
                    cursor = seq_end;
                    if socket.send(Message::Binary(data)).await.is_err() {
                        return;
                    }
                }
                Ok(TerminalEvent::Cwd(path)) => {
                    if send_json(&mut socket, &json!({ "type": "cwd", "path": path })).await.is_err() {
                        return;
                    }
                }
                Ok(TerminalEvent::Exit(exit)) => {
                    let _ = send_json(&mut socket, &exit_json(&exit)).await;
                    let _ = socket.send(Message::Close(None)).await;
                    return;
                }
                // This client fell more than BROADCAST_CAPACITY chunks behind.
                // The scrollback still has those bytes, so re-attach from the
                // cursor instead of dropping output on the floor.
                Err(RecvError::Lagged(skipped)) => {
                    tracing::debug!("terminal {} client lagged {skipped} chunks; replaying", terminal.id);
                    let catchup = terminal.attach(Some(cursor));
                    events = catchup.events;
                    if catchup.from_seq > cursor
                        && send_json(&mut socket, &json!({ "type": "truncated", "fromSeq": catchup.from_seq }))
                            .await
                            .is_err()
                    {
                        return;
                    }
                    for chunk in catchup.replay {
                        if socket.send(Message::Binary(chunk)).await.is_err() {
                            return;
                        }
                    }
                    cursor = catchup.seq;
                }
                // Sender dropped: the terminal was removed from the registry.
                Err(RecvError::Closed) => {
                    let _ = socket.send(Message::Close(None)).await;
                    return;
                }
            },

            incoming = socket.recv() => match incoming {
                Some(Ok(msg)) => {
                    if !handle_client_message(msg, &terminal, &mut socket).await {
                        return;
                    }
                }
                // Socket closed or errored — leave the shell running.
                Some(Err(_)) | None => return,
            },
        }
    }
}

/// Apply one client frame. Returns `false` when the socket should be closed.
async fn handle_client_message(msg: Message, terminal: &Terminal, socket: &mut WebSocket) -> bool {
    match msg {
        Message::Text(text) => match serde_json::from_str::<ClientMessage>(&text) {
            Ok(ClientMessage::Input { data }) => terminal
                .write_input(Bytes::from(data.into_bytes()))
                .await
                .is_ok(),
            Ok(ClientMessage::Submit { data }) => {
                // CR, not LF: a PTY in canonical mode expects carriage return as
                // the line terminator, which is what a real Enter key sends.
                let mut bytes = data.into_bytes();
                bytes.push(b'\r');
                terminal.write_input(Bytes::from(bytes)).await.is_ok()
            }
            Ok(ClientMessage::InputB64 { data }) => {
                match base64::engine::general_purpose::STANDARD.decode(data.as_bytes()) {
                    Ok(bytes) => terminal.write_input(Bytes::from(bytes)).await.is_ok(),
                    Err(e) => send_json(
                        socket,
                        &json!({ "type": "error", "message": format!("invalid base64: {e}") }),
                    )
                    .await
                    .is_ok(),
                }
            }
            Ok(ClientMessage::Resize { rows, cols }) => match terminal.resize(rows, cols) {
                Ok(()) => true,
                Err(e) => send_json(
                    socket,
                    &json!({ "type": "error", "message": format!("resize failed: {e}") }),
                )
                .await
                .is_ok(),
            },
            Ok(ClientMessage::Ping { ts }) => {
                send_json(socket, &json!({ "type": "pong", "ts": ts }))
                    .await
                    .is_ok()
            }
            // Report rather than ignore: a frontend sending the wrong shape
            // should find out immediately, not debug silence.
            Err(e) => send_json(
                socket,
                &json!({ "type": "error", "message": format!("bad message: {e}") }),
            )
            .await
            .is_ok(),
        },
        // Binary C→S is the fast path for raw input (SPEC-001 §3.1).
        Message::Binary(bytes) => terminal.write_input(bytes).await.is_ok(),
        // axum answers Ping automatically; app-level ping is the JSON one.
        Message::Ping(_) | Message::Pong(_) => true,
        Message::Close(_) => false,
    }
}

/// Serialize the exit event. `code` is null when a signal killed the shell.
fn exit_json(exit: &crate::pty::ExitInfo) -> serde_json::Value {
    json!({ "type": "exit", "code": exit.code, "signal": exit.signal })
}

/// Send a JSON control frame as WS text.
async fn send_json(socket: &mut WebSocket, value: &serde_json::Value) -> Result<(), axum::Error> {
    socket.send(Message::Text(value.to_string().into())).await
}
