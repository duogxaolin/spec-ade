//! ACP routes — spawn agent processes and relay their event streams.
//!
//! Spec: `docs/specs/SPEC-003-acp-orchestration.md` §3.1–§3.2.
//!
//! ```text
//! GET    /api/acp/agents          configured agent catalogue → 200 [AgentEntry]
//! POST   /api/acp/spawn           {agentId, projectId}       → 201 {id, agentCapabilities, …}
//! GET    /api/acp                 live connections           → 200 [ConnectionSummary]
//! GET    /api/acp/{id}/stderr     captured stderr            → 200 {stderr}
//! DELETE /api/acp/{id}            kill the process group     → 204
//! WS     /api/acp/{id}/ws         ?sessionId=<id>&after_seq=N
//! ```
//!
//! Unlike SPEC-001's terminal socket, **both directions are JSON text**: every
//! payload here is structured, so there is no raw-bytes path to preserve.
//!
//! SECURITY: mounted inside the authenticated sub-router, so the token and Origin
//! gates run before any agent process is spawned — the same spawn-after-auth
//! ordering the PTY routes rely on.

use axum::{
    Json, Router,
    extract::{
        Path, Query, State, WebSocketUpgrade,
        ws::{CloseFrame, Message, WebSocket, close_code},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{any, delete, get, post},
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;

use crate::AppState;
use crate::acp::agent::AcpAgentEntry;
use crate::acp::connection::{AcpConnection, AcpError, WatcherGuard};
use crate::acp::event::AcpEvent;
use crate::acp::log::Replay;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/acp/agents", get(list_agents))
        .route("/acp/spawn", post(spawn))
        .route("/acp", get(list_connections))
        .route("/acp/{id}/stderr", get(get_stderr))
        .route("/acp/{id}", delete(kill))
        .route("/acp/{id}/ws", any(acp_ws))
}

/// Map an ACP-layer error onto a status code plus JSON body.
///
/// `Spawn` is **502**, not 500 (§3.1): the failure happened inside an external
/// process the user configured, so reporting it as a server bug would send them
/// looking in the wrong place. The gathered stderr rides along in `detail`.
impl IntoResponse for AcpError {
    fn into_response(self) -> Response {
        let (status, group) = match &self {
            AcpError::Spawn(_) | AcpError::Agent(_) => (StatusCode::BAD_GATEWAY, "agent"),
            AcpError::Closed => (StatusCode::GONE, "connection"),
            AcpError::NoSession(_) => (StatusCode::NOT_FOUND, "session"),
            AcpError::Busy => (StatusCode::CONFLICT, "session"),
            AcpError::Permission(_) => (StatusCode::BAD_REQUEST, "permission"),
        };
        (
            status,
            Json(json!({ "error": group, "detail": self.to_string() })),
        )
            .into_response()
    }
}

fn error_response(status: StatusCode, group: &'static str, detail: impl Into<String>) -> Response {
    (
        status,
        Json(json!({ "error": group, "detail": detail.into() })),
    )
        .into_response()
}

/// `GET /api/acp/agents` — the catalogue from `settings.json` (§3.4, read-only).
async fn list_agents(State(state): State<AppState>) -> Json<Vec<AcpAgentEntry>> {
    Json(state.settings.snapshot().acp_agents)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SpawnBody {
    agent_id: String,
    project_id: String,
}

/// `POST /api/acp/spawn` — start an agent for a project.
///
/// Returns only after `initialize` answered, so a 201 means the agent is usable.
async fn spawn(State(state): State<AppState>, Json(body): Json<SpawnBody>) -> Response {
    let settings = state.settings.snapshot();

    let Some(entry) = settings.acp_agents.iter().find(|a| a.id == body.agent_id) else {
        return error_response(
            StatusCode::NOT_FOUND,
            "agent",
            format!("no agent {}", body.agent_id),
        );
    };
    // The project must exist before spawning: its path is the session `cwd` and
    // the `fs/*` sandbox root, so an unknown project has no safe root to use.
    if !settings.projects.iter().any(|p| p.id == body.project_id) {
        return error_response(
            StatusCode::NOT_FOUND,
            "project",
            format!("no project {}", body.project_id),
        );
    }

    match state.acp.spawn(entry, &body.project_id).await {
        Ok(conn) => (
            StatusCode::CREATED,
            Json(json!({
                "id": conn.id,
                "agentId": conn.agent_id,
                "projectId": conn.project_id,
                "agentInfo": conn.agent_info,
                "agentCapabilities": conn.agent_capabilities,
            })),
        )
            .into_response(),
        Err(e) => e.into_response(),
    }
}

/// `GET /api/acp` — live connections. Dead ones are dropped, never listed (A19).
async fn list_connections(
    State(state): State<AppState>,
) -> Json<Vec<crate::acp::ConnectionSummary>> {
    Json(state.acp.list())
}

/// `GET /api/acp/{id}/stderr` — the agent's captured stderr ([INVENTED-11]).
///
/// When an agent misbehaves this is frequently the only explanation available.
async fn get_stderr(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.acp.get(&id) {
        Some(conn) => Json(json!({ "stderr": conn.stderr() })).into_response(),
        None => error_response(
            StatusCode::NOT_FOUND,
            "connection",
            format!("no connection {id}"),
        ),
    }
}

/// `DELETE /api/acp/{id}` — kill the agent and its whole process group (A20).
async fn kill(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    if state.acp.kill(&id).await {
        StatusCode::NO_CONTENT.into_response()
    } else {
        error_response(
            StatusCode::NOT_FOUND,
            "connection",
            format!("no connection {id}"),
        )
    }
}

// ---- WebSocket -------------------------------------------------------------

/// Query string of the WS route.
///
/// Names are given explicitly rather than via `rename_all`: §3.2 pins
/// `?sessionId=…&after_seq=N`, mixing the two conventions (`after_seq` matches
/// SPEC-001's terminal socket, which clients already speak). A blanket
/// `rename_all` would quietly turn the second into `afterSeq` and every replay
/// request would silently resend the whole log.
#[derive(Debug, Deserialize)]
struct WsQuery {
    /// Which session to attach to — a connection can hold several (§3.2).
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    /// Highest `seq` the client already has; omitted replays from the start.
    after_seq: Option<u64>,
    /// Consumed by the auth middleware. Declared so `Query` does not reject it.
    #[allow(dead_code)]
    token: Option<String>,
}

/// `GET /api/acp/{id}/ws` — attach to one session's event stream.
///
/// `sessionId` is Spec ADE's session id (what `POST /api/projects/{id}/sessions`
/// returned), not the agent's — the client never has to know the agent's id. It is
/// resolved here to the agent session the event log is actually keyed by.
///
/// A missing or foreign `sessionId` closes with **1008** per §3.2. That needs the
/// socket upgraded first: a pre-upgrade HTTP error reaches a browser as a failed
/// handshake with no code, and the spec pins the code. An unknown *connection* is
/// different — there is nothing to attach to at all, so that stays a plain 404.
async fn acp_ws(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(conn) = state.acp.get(&id) else {
        return error_response(
            StatusCode::NOT_FOUND,
            "connection",
            format!("no connection {id}"),
        );
    };

    let after_seq = query.after_seq.unwrap_or(0);
    let session = query
        .session_id
        .as_deref()
        .and_then(|sid| state.acp.get_session(sid));

    ws.on_upgrade(move |socket| async move {
        let Some(session_id) = query.session_id else {
            close_with_policy(socket, "sessionId is required").await;
            return;
        };
        let Some(session) = session else {
            close_with_policy(socket, &format!("no session {session_id}")).await;
            return;
        };
        // A session belonging to some *other* connection must not attach here: it
        // would open a socket on which events could never arrive.
        if session.connection_id != id {
            close_with_policy(
                socket,
                &format!("session {session_id} is not on connection {id}"),
            )
            .await;
            return;
        }
        bridge(socket, conn, session, after_seq).await;
    })
}

/// Close with 1008 Policy Violation plus a reason the client can log (§3.2).
async fn close_with_policy(mut socket: WebSocket, reason: &str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: close_code::POLICY,
            reason: reason.to_string().into(),
        })))
        .await;
}

/// Client → server frames (§3.2).
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    /// Open a turn.
    Prompt {
        text: String,
    },
    /// Ask the agent to stop the running turn.
    Cancel,
    /// Answer a parked `permission_request`.
    #[serde(rename_all = "camelCase")]
    PermissionResponse {
        request_id: String,
        /// The option the user picked. Absent — or `cancelled: true` — is a
        /// dismissal, which still has to reach the agent as an outcome.
        option_id: Option<String>,
        #[serde(default)]
        cancelled: bool,
    },
    Ping {
        ts: Option<i64>,
    },
}

/// Bridge one WebSocket to one session until either side goes away.
///
/// Closing the socket does **not** kill the agent (A21): the connection outlives
/// any single browser tab, the same rule as SPEC-001's terminals.
///
/// Two ids are in play. `session.agent_session_id` addresses the ACP layer (the
/// event log is keyed by it, because that is what arrives on every
/// `session/update`); `session.id` is what goes out on the wire, so the client
/// only ever sees the id the REST API gave it.
async fn bridge(
    mut socket: WebSocket,
    conn: AcpConnection,
    session: crate::acp::SessionInfo,
    after_seq: u64,
) {
    let agent_session = session.agent_session_id.as_str();
    let session_id = session.id.as_str();

    // `attach` subscribes before replaying, so an event emitted between the two
    // steps lands in the broadcast instead of being missed by both (§5.7).
    let (replay, mut events, state, watcher) = match conn.attach(agent_session, after_seq) {
        Ok(attached) => attached,
        Err(e) => {
            let _ = send_error(&mut socket, session_id, &e.to_string()).await;
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };
    // Held for the socket's lifetime: while it lives the connection is not idle
    // ([INVENTED-10]). Dropping it on any return path is the whole point of RAII
    // here — an early `return` must not leave the connection looking watched.
    let _watcher: WatcherGuard = watcher;

    let mut cursor = after_seq;
    if !send_replay(&mut socket, session_id, replay, &mut cursor).await {
        return;
    }

    // Lifecycle state, so a client attaching mid-turn knows a turn is running
    // without inferring it from the replayed events.
    if send_json(
        &mut socket,
        &json!({
            "type": "ready",
            "sessionId": session_id,
            "connectionId": conn.id,
            "seq": cursor,
            "state": state,
        }),
    )
    .await
    .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(logged) => {
                    // Replay and broadcast overlap by design; `seq` is the filter.
                    if logged.seq <= cursor {
                        continue;
                    }
                    cursor = logged.seq;
                    let closed = matches!(logged.event, AcpEvent::ConnectionClosed { .. });
                    if send_event(&mut socket, logged.seq, session_id, &logged.event)
                        .await
                        .is_err()
                    {
                        return;
                    }
                    // Nothing further will ever arrive for this session.
                    if closed {
                        let _ = socket.send(Message::Close(None)).await;
                        return;
                    }
                }
                // Fell more than the broadcast capacity behind. The log is still
                // authoritative, so re-read from the cursor rather than dropping
                // events (§5.7).
                Err(RecvError::Lagged(skipped)) => {
                    tracing::debug!(
                        "acp: session {session_id} client lagged {skipped} events; replaying"
                    );
                    match conn.replay(agent_session, cursor) {
                        Ok(catchup) => {
                            if !send_replay(&mut socket, session_id, catchup, &mut cursor).await {
                                return;
                            }
                        }
                        // The session vanished while catching up.
                        Err(_) => {
                            let _ = socket.send(Message::Close(None)).await;
                            return;
                        }
                    }
                }
                // Sender dropped: the session slot is gone.
                Err(RecvError::Closed) => {
                    let _ = socket.send(Message::Close(None)).await;
                    return;
                }
            },

            incoming = socket.recv() => match incoming {
                Some(Ok(msg)) => {
                    if !handle_client_message(msg, &conn, &session, &mut socket).await {
                        return;
                    }
                }
                // Socket closed or errored — leave the agent running (A21).
                Some(Err(_)) | None => return,
            },
        }
    }
}

/// Send a replay batch, prefixed by `truncated` when history had a gap.
///
/// Returns `false` if the socket died mid-send.
async fn send_replay(
    socket: &mut WebSocket,
    session_id: &str,
    replay: Replay,
    cursor: &mut u64,
) -> bool {
    // Tell the client about the hole (A13) rather than splicing unrelated events
    // together and letting the UI render a conversation that never happened.
    if let Some(from_seq) = replay.truncated_from
        && send_event(
            socket,
            from_seq,
            session_id,
            &AcpEvent::Truncated { from_seq },
        )
        .await
        .is_err()
    {
        return false;
    }
    for logged in replay.events {
        if logged.seq <= *cursor {
            continue;
        }
        if send_event(socket, logged.seq, session_id, &logged.event)
            .await
            .is_err()
        {
            return false;
        }
        *cursor = logged.seq;
    }
    true
}

/// Apply one client frame. Returns `false` when the socket should be closed.
async fn handle_client_message(
    msg: Message,
    conn: &AcpConnection,
    session: &crate::acp::SessionInfo,
    socket: &mut WebSocket,
) -> bool {
    let agent_session = session.agent_session_id.as_str();
    let session_id = session.id.as_str();
    match msg {
        Message::Text(text) => match serde_json::from_str::<ClientMessage>(&text) {
            Ok(ClientMessage::Prompt { text }) => match conn.prompt(agent_session, text).await {
                Ok(()) => true,
                // `Busy` is the [INVENTED-4] path (A15): report it and leave the
                // running turn untouched rather than queueing behind it.
                Err(e) => send_error(socket, session_id, &e.to_string()).await,
            },
            Ok(ClientMessage::Cancel) => match conn.cancel(agent_session).await {
                Ok(()) => true,
                Err(e) => send_error(socket, session_id, &e.to_string()).await,
            },
            Ok(ClientMessage::PermissionResponse {
                request_id,
                option_id,
                cancelled,
            }) => {
                let choice = if cancelled {
                    None
                } else {
                    option_id.as_deref()
                };
                match conn.respond_permission(agent_session, &request_id, choice) {
                    Ok(()) => true,
                    // An option the agent never offered lands here. The request
                    // stays parked so the user can answer again (A10) — answering
                    // the agent with a guess would be worse than making them retry.
                    Err(e) => send_error(socket, session_id, &e.to_string()).await,
                }
            }
            Ok(ClientMessage::Ping { ts }) => {
                send_json(socket, &json!({ "type": "pong", "ts": ts }))
                    .await
                    .is_ok()
            }
            // Report rather than ignore: a frontend sending the wrong shape should
            // find out immediately instead of debugging silence.
            Err(e) => send_error(socket, session_id, &format!("bad message: {e}")).await,
        },
        // Everything here is JSON text; a binary frame is a client bug.
        Message::Binary(_) => {
            send_error(socket, session_id, "binary frames are not accepted").await
        }
        // axum answers protocol-level Ping itself; the app-level one is JSON.
        Message::Ping(_) | Message::Pong(_) => true,
        Message::Close(_) => false,
    }
}

/// Non-fatal error: reported to the client, session keeps running.
///
/// No `seq`: this is not a logged event but a direct answer to the frame that
/// caused it. Giving it one would corrupt the client's replay cursor.
async fn send_error(socket: &mut WebSocket, session_id: &str, message: &str) -> bool {
    send_json(
        socket,
        &json!({ "type": "error", "sessionId": session_id, "message": message }),
    )
    .await
    .is_ok()
}

/// Serialize one logged event with its `seq` and `sessionId` (§3.2).
async fn send_event(
    socket: &mut WebSocket,
    seq: u64,
    session_id: &str,
    event: &AcpEvent,
) -> Result<(), axum::Error> {
    let mut value = match serde_json::to_value(event) {
        Ok(v) => v,
        // Unreachable for the current `AcpEvent` shapes, but dropping the frame
        // silently would leave a gap in the client's `seq` run with no explanation.
        Err(e) => json!({ "type": "error", "message": format!("unserializable event: {e}") }),
    };
    if let Some(obj) = value.as_object_mut() {
        obj.insert("seq".into(), json!(seq));
        obj.insert("sessionId".into(), json!(session_id));
    }
    send_json(socket, &value).await
}

async fn send_json(socket: &mut WebSocket, value: &serde_json::Value) -> Result<(), axum::Error> {
    socket.send(Message::Text(value.to_string().into())).await
}
