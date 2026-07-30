//! Phase 1 integration tests — SPEC-001 (terminal over WebSocket).
//!
//! These drive a real server on an ephemeral port and a real `tokio-tungstenite`
//! client, because the things most likely to break are exactly the things an
//! in-process router test can't see: the PTY threads, the broadcast/scrollback
//! handoff, and the auth/origin gates running during the WS upgrade.
//!
//! Every test spawns `/bin/sh` explicitly rather than the user's `$SHELL` — a
//! developer's zsh with a heavyweight prompt would make output assertions
//! flaky, and CI may not have the same shell at all (SPEC-001 §8).

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use spec_ade_server::{AppState, auth, build_router};
use tokio_tungstenite::tungstenite::{
    Message as TMessage,
    client::IntoClientRequest,
    http::{HeaderValue, StatusCode as TStatus},
};

/// Generous enough for a shell to start and echo on a loaded machine, short
/// enough that a hang fails the suite instead of stalling it.
const TIMEOUT: Duration = Duration::from_secs(15);

struct TestServer {
    addr: std::net::SocketAddr,
    token: String,
    client: reqwest_lite::Client,
}

impl TestServer {
    /// Bind the app on `127.0.0.1:0`, isolating its data dir to a temp path so
    /// generated shell-integration files never touch the developer's config.
    async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    /// As [`TestServer::start`], letting a test adjust the state first — used to
    /// shrink the scrollback so the prune/replay-gap path is reachable.
    async fn start_with(configure: impl FnOnce(&mut AppState)) -> Self {
        let token = format!("tok-{}", uuid::Uuid::new_v4());
        let data_dir = std::env::temp_dir().join(format!("spec-ade-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).unwrap();

        let mut state = AppState::new(token.clone());
        state.data_dir = data_dir;
        configure(&mut state);

        let app = build_router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        Self {
            addr,
            token,
            client: reqwest_lite::Client::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// `POST /api/terminals` with an explicit `/bin/sh`, asserting success.
    ///
    /// Returns the terminal id. Every test that isn't *about* spawn failures goes
    /// through here, so a spawn that unexpectedly fails names its own status and
    /// body instead of surfacing as a `None` unwrap several lines later.
    async fn spawn_id(&self, extra: Value) -> String {
        let (status, body) = self.spawn_sh(extra).await;
        assert_eq!(status, TStatus::CREATED, "spawn failed; body: {body}");
        body["id"]
            .as_str()
            .unwrap_or_else(|| panic!("spawn returned no id; body: {body}"))
            .to_string()
    }

    /// The output cursor once the shell has gone quiet.
    ///
    /// A shell keeps writing after its last visible output (the next prompt), so
    /// reading `seq` once races that write: attach at a cursor taken too early
    /// and the server correctly replays bytes the test didn't expect. Polling
    /// until the value stops moving removes the race without a blind sleep.
    async fn settled_seq(&self, id: &str) -> u64 {
        let read = async || {
            let (_, list) = self
                .client
                .request("GET", &self.url("/api/terminals"), &self.token, None)
                .await;
            list.as_array()
                .and_then(|ts| ts.iter().find(|t| t["id"] == id))
                .and_then(|t| t["seq"].as_u64())
                .unwrap_or_else(|| panic!("terminal {id} not in list: {list}"))
        };

        let deadline = tokio::time::Instant::now() + TIMEOUT;
        let mut last = read().await;
        loop {
            tokio::time::sleep(Duration::from_millis(120)).await;
            let now = read().await;
            if now == last {
                return now;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "terminal {id} never stopped producing output"
            );
            last = now;
        }
    }

    /// `POST /api/terminals` with an explicit `/bin/sh`.
    async fn spawn_sh(&self, extra: Value) -> (TStatus, Value) {
        let mut body = json!({ "shell": "/bin/sh", "rows": 24, "cols": 80 });
        if let (Some(base), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
            for (k, v) in extra {
                base.insert(k.clone(), v.clone());
            }
        }
        self.client
            .request("POST", &self.url("/api/terminals"), &self.token, Some(body))
            .await
    }

    /// Open the terminal WS, optionally resuming from a byte offset.
    async fn connect_ws(
        &self,
        id: &str,
        after_seq: Option<u64>,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let mut url = format!(
            "ws://{}/api/terminals/{id}/ws?token={}",
            self.addr, self.token
        );
        if let Some(seq) = after_seq {
            url.push_str(&format!("&after_seq={seq}"));
        }
        let (socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        socket
    }
}

/// A WS client that accumulates output bytes and control messages.
struct Client {
    socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    output: Vec<u8>,
    /// Every control message received, so a test can assert about a message that
    /// should *not* have arrived.
    json_seen: Vec<Value>,
}

impl Client {
    fn new(
        socket: tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> Self {
        Self {
            socket,
            output: Vec::new(),
            json_seen: Vec::new(),
        }
    }

    async fn send_json(&mut self, value: Value) {
        self.socket
            .send(TMessage::Text(value.to_string().into()))
            .await
            .unwrap();
    }

    /// Pump frames until `predicate` accepts a JSON control message.
    ///
    /// Binary frames are appended to `output` along the way, so a caller can
    /// wait for `ready` and still keep the bytes that arrived first.
    async fn wait_for_json<F>(&mut self, mut predicate: F) -> Value
    where
        F: FnMut(&Value) -> bool,
    {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            let frame = tokio::time::timeout_at(deadline, self.socket.next())
                .await
                .expect("timed out waiting for a control message")
                .expect("socket closed while waiting")
                .expect("websocket error");
            match frame {
                TMessage::Binary(bytes) => self.output.extend_from_slice(&bytes),
                TMessage::Text(text) => {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    self.json_seen.push(value.clone());
                    if predicate(&value) {
                        return value;
                    }
                }
                TMessage::Close(_) => panic!("socket closed before the expected message"),
                _ => {}
            }
        }
    }

    /// Pump frames until the accumulated output contains `needle`.
    async fn wait_for_output(&mut self, needle: &str) -> String {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            if let Some(text) = self.output_text().find(needle).map(|_| self.output_text()) {
                return text;
            }
            let frame = tokio::time::timeout_at(deadline, self.socket.next())
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "timed out waiting for {needle:?}; output so far: {:?}",
                        self.output_text()
                    )
                })
                .expect("socket closed while waiting")
                .expect("websocket error");
            match frame {
                TMessage::Binary(bytes) => self.output.extend_from_slice(&bytes),
                TMessage::Close(_) => panic!(
                    "socket closed before {needle:?}; output: {:?}",
                    self.output_text()
                ),
                _ => {}
            }
        }
    }

    fn output_text(&self) -> String {
        String::from_utf8_lossy(&self.output).into_owned()
    }

    async fn wait_for_ready(&mut self) -> Value {
        self.wait_for_json(|v| v["type"] == "ready").await
    }
}

// ---- REST surface ----------------------------------------------------------

#[tokio::test]
async fn spawn_returns_id_and_live_pid() {
    let server = TestServer::start().await;
    let (status, body) = server.spawn_sh(json!({})).await;

    assert_eq!(status, TStatus::CREATED);
    assert!(body["id"].is_string(), "body: {body}");
    let pid = body["pid"].as_u64().expect("pid must be reported");
    assert_eq!(body["rows"], 24);
    assert_eq!(body["cols"], 80);

    // The process must actually exist — `kill -0` probes without signalling.
    let alive = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .unwrap()
        .success();
    assert!(alive, "spawned pid {pid} is not running");
}

#[tokio::test]
async fn spawn_honours_cwd_and_rejects_a_bad_one() {
    let server = TestServer::start().await;

    let (status, body) = server.spawn_sh(json!({ "cwd": "/tmp" })).await;
    assert_eq!(status, TStatus::CREATED);
    // macOS resolves /tmp to /private/tmp; canonicalization is intentional so
    // the reported cwd matches what OSC 7 will later report.
    let cwd = body["cwd"].as_str().unwrap();
    assert!(cwd.ends_with("/tmp"), "cwd was {cwd}");

    let (status, body) = server
        .spawn_sh(json!({ "cwd": "/definitely/not/a/real/path" }))
        .await;
    assert_eq!(status, TStatus::BAD_REQUEST, "body: {body}");

    // A file is not a directory.
    let (status, _) = server.spawn_sh(json!({ "cwd": "/etc/hosts" })).await;
    assert_eq!(status, TStatus::BAD_REQUEST);
}

#[tokio::test]
async fn list_then_delete_kills_the_process() {
    let server = TestServer::start().await;
    let (status, body) = server.spawn_sh(json!({})).await;
    assert_eq!(status, TStatus::CREATED, "spawn failed; body: {body}");
    let id = body["id"].as_str().unwrap().to_string();
    let pid = body["pid"].as_u64().unwrap();

    let (status, list) = server
        .client
        .request("GET", &server.url("/api/terminals"), &server.token, None)
        .await;
    assert_eq!(status, TStatus::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], id.as_str());
    assert_eq!(list[0]["alive"], true);

    let (status, _) = server
        .client
        .request(
            "DELETE",
            &server.url(&format!("/api/terminals/{id}")),
            &server.token,
            None,
        )
        .await;
    assert_eq!(status, TStatus::NO_CONTENT);

    // Gone from the registry.
    let (_, list) = server
        .client
        .request("GET", &server.url("/api/terminals"), &server.token, None)
        .await;
    assert!(list.as_array().unwrap().is_empty());

    // And the process is really dead (SIGKILL is asynchronous, so poll).
    let dead = wait_until(Duration::from_secs(5), || {
        !std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    })
    .await;
    assert!(dead, "pid {pid} survived DELETE");

    // Deleting twice is a 404, not a silent success.
    let (status, _) = server
        .client
        .request(
            "DELETE",
            &server.url(&format!("/api/terminals/{id}")),
            &server.token,
            None,
        )
        .await;
    assert_eq!(status, TStatus::NOT_FOUND);
}

// ---- Security gates --------------------------------------------------------

#[tokio::test]
async fn rest_and_ws_require_a_token() {
    let server = TestServer::start().await;
    let id = server.spawn_id(json!({})).await;

    // REST without a token.
    let (status, _) = server
        .client
        .request_raw("GET", &server.url("/api/terminals"), None, None)
        .await;
    assert_eq!(status, TStatus::UNAUTHORIZED);

    // WS upgrade without a token must fail the handshake — this is the RCE
    // guardrail the whole design exists for (deep-dive 02 §4).
    let url = format!("ws://{}/api/terminals/{id}/ws", server.addr);
    assert!(
        tokio_tungstenite::connect_async(url).await.is_err(),
        "WS upgrade without a token must be rejected"
    );
}

#[tokio::test]
async fn ws_rejects_a_foreign_origin_even_with_a_valid_token() {
    let server = TestServer::start().await;
    let id = server.spawn_id(json!({})).await;

    let url = format!(
        "ws://{}/api/terminals/{id}/ws?token={}",
        server.addr, server.token
    );
    let mut request = url.into_client_request().unwrap();
    request.headers_mut().insert(
        "origin",
        HeaderValue::from_static("http://evil.example.com"),
    );

    assert!(
        tokio_tungstenite::connect_async(request).await.is_err(),
        "a leaked token must not be usable from a hostile page (CSRF-on-WS)"
    );

    // The allowlisted origin still works, so the check isn't just blanket-denying.
    let mut request = format!(
        "ws://{}/api/terminals/{id}/ws?token={}",
        server.addr, server.token
    )
    .into_client_request()
    .unwrap();
    request.headers_mut().insert(
        "origin",
        HeaderValue::from_str(&format!("http://localhost:{}", server.addr.port())).unwrap(),
    );
    assert!(tokio_tungstenite::connect_async(request).await.is_ok());
}

#[tokio::test]
async fn ws_for_an_unknown_terminal_is_rejected() {
    let server = TestServer::start().await;
    let url = format!(
        "ws://{}/api/terminals/does-not-exist/ws?token={}",
        server.addr, server.token
    );
    assert!(tokio_tungstenite::connect_async(url).await.is_err());
}

// ---- WebSocket I/O ---------------------------------------------------------

#[tokio::test]
async fn ready_then_input_produces_output() {
    let server = TestServer::start().await;
    let id = server.spawn_id(json!({})).await;

    let mut client = Client::new(server.connect_ws(&id, None).await);
    let ready = client.wait_for_ready().await;
    assert_eq!(ready["id"], id.as_str());
    assert!(ready["pid"].is_u64());
    assert_eq!(ready["rows"], 24);

    // The quote trick makes the *typed* line and the *printed* result differ, so
    // matching the result can't be satisfied by the PTY's echo of the input.
    client
        .send_json(json!({ "type": "input", "data": "echo spec-ade''-marker\n" }))
        .await;
    let text = client.wait_for_output("spec-ade-marker").await;
    assert!(
        text.contains("spec-ade''-marker"),
        "the typed line should be echoed too: {text:?}"
    );
}

#[tokio::test]
async fn submit_appends_a_carriage_return() {
    let server = TestServer::start().await;
    let id = server.spawn_id(json!({})).await;

    let mut client = Client::new(server.connect_ws(&id, None).await);
    client.wait_for_ready().await;

    // No newline in the payload: only the server-side CR can make sh run it. The
    // quote trick means seeing `submitted-ok` proves the command *ran*, rather
    // than just matching the echoed input.
    client
        .send_json(json!({ "type": "submit", "data": "echo submitted''-ok" }))
        .await;
    client.wait_for_output("submitted-ok").await;
}

#[tokio::test]
async fn base64_input_delivers_raw_bytes() {
    let server = TestServer::start().await;
    let id = server.spawn_id(json!({})).await;

    let mut client = Client::new(server.connect_ws(&id, None).await);
    client.wait_for_ready().await;

    let encoded = base64_encode(b"echo b64-path\n");
    client
        .send_json(json!({ "type": "input_b64", "data": encoded }))
        .await;
    client.wait_for_output("b64-path").await;

    // Malformed base64 is reported, not silently dropped.
    client
        .send_json(json!({ "type": "input_b64", "data": "!!!not base64!!!" }))
        .await;
    let err = client.wait_for_json(|v| v["type"] == "error").await;
    assert!(
        err["message"].as_str().unwrap().contains("base64"),
        "got {err}"
    );
}

#[tokio::test]
async fn binary_frame_is_accepted_as_input() {
    let server = TestServer::start().await;
    let id = server.spawn_id(json!({})).await;

    let mut client = Client::new(server.connect_ws(&id, None).await);
    client.wait_for_ready().await;

    client
        .socket
        .send(TMessage::Binary(b"echo binary-frame\n".to_vec().into()))
        .await
        .unwrap();
    client.wait_for_output("binary-frame").await;
}

#[tokio::test]
async fn resize_reaches_the_shell() {
    let server = TestServer::start().await;
    let id = server.spawn_id(json!({})).await;

    let mut client = Client::new(server.connect_ws(&id, None).await);
    client.wait_for_ready().await;

    client
        .send_json(json!({ "type": "resize", "rows": 40, "cols": 120 }))
        .await;
    // `stty size` reads the kernel winsize, so this proves the ioctl landed and
    // the PTY is the child's controlling terminal (unix.rs:257-274).
    client
        .send_json(json!({ "type": "input", "data": "stty size\n" }))
        .await;
    let text = client.wait_for_output("40 120").await;
    assert!(text.contains("40 120"), "got {text:?}");

    // The REST view reflects it too.
    let (_, list) = server
        .client
        .request("GET", &server.url("/api/terminals"), &server.token, None)
        .await;
    assert_eq!(list[0]["rows"], 40);
    assert_eq!(list[0]["cols"], 120);
}

#[tokio::test]
async fn ping_is_answered_with_pong() {
    let server = TestServer::start().await;
    let id = server.spawn_id(json!({})).await;

    let mut client = Client::new(server.connect_ws(&id, None).await);
    client.wait_for_ready().await;

    client.send_json(json!({ "type": "ping", "ts": 42 })).await;
    let pong = client.wait_for_json(|v| v["type"] == "pong").await;
    assert_eq!(pong["ts"], 42);
}

#[tokio::test]
async fn malformed_message_is_reported_not_ignored() {
    let server = TestServer::start().await;
    let id = server.spawn_id(json!({})).await;

    let mut client = Client::new(server.connect_ws(&id, None).await);
    client.wait_for_ready().await;

    client.send_json(json!({ "type": "nonsense" })).await;
    let err = client.wait_for_json(|v| v["type"] == "error").await;
    assert!(err["message"].is_string());

    // The socket survives a bad frame — one typo must not kill the session.
    client
        .send_json(json!({ "type": "input", "data": "echo still-alive\n" }))
        .await;
    client.wait_for_output("still-alive").await;
}

#[tokio::test]
async fn shell_exit_emits_exit_event_with_code() {
    let server = TestServer::start().await;
    let id = server.spawn_id(json!({})).await;

    let mut client = Client::new(server.connect_ws(&id, None).await);
    client.wait_for_ready().await;

    client
        .send_json(json!({ "type": "input", "data": "exit 3\n" }))
        .await;
    let exit = client.wait_for_json(|v| v["type"] == "exit").await;
    assert_eq!(exit["code"], 3, "exit event: {exit}");
    assert!(exit["signal"].is_null());
}

#[tokio::test]
async fn a_client_attaching_after_exit_still_learns_it_died() {
    let server = TestServer::start().await;
    let id = server.spawn_id(json!({})).await;

    let mut first = Client::new(server.connect_ws(&id, None).await);
    first.wait_for_ready().await;
    first
        .send_json(json!({ "type": "input", "data": "exit 7\n" }))
        .await;
    first.wait_for_json(|v| v["type"] == "exit").await;

    // The broadcast that carried the exit is long gone; a fresh client must
    // still be told, or its UI would show a live terminal forever.
    let mut second = Client::new(server.connect_ws(&id, None).await);
    let exit = second.wait_for_json(|v| v["type"] == "exit").await;
    assert_eq!(exit["code"], 7);
}

// ---- Reconnect / replay ----------------------------------------------------

#[tokio::test]
async fn reconnect_replays_history_without_loss_or_duplication() {
    let server = TestServer::start().await;
    let id = server.spawn_id(json!({})).await;

    let mut first = Client::new(server.connect_ws(&id, None).await);
    first.wait_for_ready().await;
    first
        // Quote trick (as elsewhere): `before-reload` appears only in the
        // command's *result*, never in the PTY's echo of the typed line. Without
        // it, waiting for the needle can return on the echo alone, leaving the
        // cursor mid-command and making step (2) fail intermittently.
        .send_json(json!({ "type": "input", "data": "echo before''-reload\n" }))
        .await;
    first.wait_for_output("before-reload").await;
    drop(first); // simulate closing the tab

    // The authoritative cursor, taken once the shell has stopped writing — the
    // client's own byte count would race the prompt that follows the output.
    let cursor = server.settled_seq(&id).await;

    // (1) A fresh attach with no cursor replays everything from the start.
    let mut full = Client::new(server.connect_ws(&id, None).await);
    let ready = full.wait_for_ready().await;
    let replayed_all = full.output.clone();
    assert!(
        full.output_text().contains("before-reload"),
        "replay must include pre-disconnect output: {:?}",
        full.output_text()
    );
    assert_eq!(
        ready["seq"].as_u64().unwrap(),
        full.output.len() as u64,
        "ready.seq must equal the bytes actually replayed"
    );
    assert_eq!(
        ready["seq"].as_u64().unwrap(),
        cursor,
        "a full replay must land on the same cursor REST reports"
    );
    drop(full);

    // (2) Resuming from that cursor sends nothing already seen, and the stream
    // continues byte-for-byte — the same stream, not a re-render.
    let mut resumed = Client::new(server.connect_ws(&id, Some(cursor)).await);
    let ready = resumed.wait_for_ready().await;
    assert_eq!(
        ready["seq"].as_u64().unwrap(),
        cursor + resumed.output.len() as u64
    );
    let replayed = resumed.output_text();
    assert!(
        !replayed.contains("before-reload"),
        "output before the cursor must not be resent: {replayed:?}"
    );

    // (3) The resumed socket is fully functional, and its output continues the
    // stream the full replay showed rather than starting over.
    resumed
        .send_json(json!({ "type": "input", "data": "echo after''-reload\n" }))
        .await;
    resumed.wait_for_output("after-reload").await;
    let mut whole = replayed_all;
    whole.extend_from_slice(&resumed.output);
    let text = String::from_utf8_lossy(&whole);
    assert_eq!(
        text.matches("before-reload").count(),
        1,
        "history plus resumed stream must contain the pre-reload output exactly once: {text:?}"
    );
}

#[tokio::test]
async fn attaching_at_the_current_cursor_replays_nothing() {
    let server = TestServer::start().await;
    let id = server.spawn_id(json!({})).await;

    let mut first = Client::new(server.connect_ws(&id, None).await);
    first.wait_for_ready().await;
    first
        .send_json(json!({ "type": "input", "data": "echo set''tle\n" }))
        .await;
    first.wait_for_output("settle").await;
    drop(first);

    // The authoritative cursor, read once the shell stopped writing: taken while
    // the prompt is still in flight, "replays nothing" would be a lie.
    let seq = server.settled_seq(&id).await;

    let mut client = Client::new(server.connect_ws(&id, Some(seq)).await);
    let ready = client.wait_for_ready().await;
    assert_eq!(ready["seq"].as_u64().unwrap(), seq);
    assert!(
        client.output.is_empty(),
        "nothing should be replayed, got {:?}",
        client.output_text()
    );
}

#[tokio::test]
async fn two_clients_both_see_live_output() {
    let server = TestServer::start().await;
    let id = server.spawn_id(json!({})).await;

    let mut a = Client::new(server.connect_ws(&id, None).await);
    a.wait_for_ready().await;
    let mut b = Client::new(server.connect_ws(&id, None).await);
    b.wait_for_ready().await;

    a.send_json(json!({ "type": "input", "data": "echo shared-view\n" }))
        .await;

    // Both attached sockets are fed by the same broadcast.
    a.wait_for_output("shared-view").await;
    b.wait_for_output("shared-view").await;
}

#[tokio::test]
async fn a_cursor_older_than_the_history_reports_a_gap() {
    // SPEC-001 A8. The scrollback is shrunk to a few hundred bytes so ordinary
    // shell output overruns it; at the production 12 MiB threshold this path
    // would need megabytes of output to reach.
    let server = TestServer::start_with(|state| {
        state.pty = spec_ade_server::pty::PtyManager::with_scrollback_limits(256, 384);
    })
    .await;
    let id = server.spawn_id(json!({})).await;

    let mut first = Client::new(server.connect_ws(&id, None).await);
    first.wait_for_ready().await;

    // Push well past the prune threshold, then confirm from REST that the
    // oldest bytes really are gone rather than assuming it.
    for i in 0..12 {
        first
            .send_json(json!({ "type": "input", "data": format!("echo filler-{i}-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n") }))
            .await;
    }
    first.wait_for_output("filler-11-").await;
    drop(first);

    let end = server.settled_seq(&id).await;
    assert!(end > 384, "not enough output to force a prune: {end}");

    // Ask for byte 0, which has certainly been pruned.
    let mut stale = Client::new(server.connect_ws(&id, Some(0)).await);
    let truncated = stale.wait_for_json(|v| v["type"] == "truncated").await;
    let from = truncated["fromSeq"]
        .as_u64()
        .expect("fromSeq must be a number");
    assert!(
        from > 0,
        "a gap notice must say where data resumes, got {truncated}"
    );

    // The notice precedes the replay, and `ready.seq` still lands on the true
    // end of the stream so the client's cursor is correct despite the hole.
    let ready = stale.wait_for_ready().await;
    assert_eq!(ready["seq"].as_u64().unwrap(), end);
    assert_eq!(
        from + stale.output.len() as u64,
        end,
        "replayed bytes must fill exactly from the reported gap to the cursor"
    );

    // A current cursor is not a gap — the notice is specific to lost history.
    let mut fresh = Client::new(server.connect_ws(&id, Some(end)).await);
    fresh.wait_for_ready().await;
    assert!(
        !fresh.json_seen.iter().any(|v| v["type"] == "truncated"),
        "an up-to-date client must not be told about a gap"
    );
}

// ---- OSC 7 cwd tracking ----------------------------------------------------

#[tokio::test]
async fn osc7_from_the_shell_becomes_a_cwd_event() {
    let server = TestServer::start().await;
    let id = server.spawn_id(json!({})).await;

    let mut client = Client::new(server.connect_ws(&id, None).await);
    client.wait_for_ready().await;

    // Emit OSC 7 straight from the shell to exercise the *server's* scanner in
    // isolation: a synthetic path nothing else could produce.
    //
    // Note this asserts only the event, not the REST view: the shell's own hook
    // emits the real `$PWD` at the next prompt, which legitimately overwrites
    // this synthetic value. The REST side is covered by the `cd` test below,
    // where the hook and the assertion agree on one directory.
    client
        .send_json(json!({
            "type": "input",
            "data": "printf '\\033]7;file://localhost/spec-ade-synthetic\\a'\n"
        }))
        .await;

    let cwd = client
        .wait_for_json(|v| v["type"] == "cwd" && v["path"] == "/spec-ade-synthetic")
        .await;
    assert_eq!(cwd["path"], "/spec-ade-synthetic");
}

#[tokio::test]
async fn injected_hook_reports_cwd_after_a_real_cd() {
    // End-to-end proof that shell integration works: no synthetic escape, just
    // `cd`. Without the injected `PROMPT_COMMAND` a stock bash emits no OSC 7 at
    // all (deep-dive 02 §5.3), so a passing assertion here means the injection
    // landed and the scanner picked it up.
    if !std::path::Path::new("/bin/bash").exists() {
        eprintln!("skipping: /bin/bash not present");
        return;
    }

    let server = TestServer::start().await;
    // Canonicalize the target: on macOS `/tmp` is a symlink to `/private/tmp`,
    // and the shell reports the path it actually resolved to.
    let target = std::fs::canonicalize(std::env::temp_dir()).unwrap();
    let target = target.join(format!("spec-ade-cd-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&target).unwrap();
    let target = target.display().to_string();

    let (status, body) = server.spawn_sh(json!({ "shell": "/bin/bash" })).await;
    assert_eq!(status, TStatus::CREATED);
    let id = body["id"].as_str().unwrap().to_string();

    let mut client = Client::new(server.connect_ws(&id, None).await);
    client.wait_for_ready().await;

    client
        .send_json(json!({ "type": "input", "data": format!("cd '{target}'\n") }))
        .await;

    let cwd = client
        .wait_for_json(|v| v["type"] == "cwd" && v["path"] == target.as_str())
        .await;
    assert_eq!(cwd["path"], target.as_str());

    // The REST view now reflects it — stable, because every later prompt emits
    // the same directory.
    let (_, list) = server
        .client
        .request("GET", &server.url("/api/terminals"), &server.token, None)
        .await;
    assert_eq!(list[0]["cwd"], target.as_str());

    let _ = std::fs::remove_dir_all(&target);
}

#[tokio::test]
async fn escape_sequences_are_passed_through_untouched() {
    let server = TestServer::start().await;
    let id = server.spawn_id(json!({})).await;

    let mut client = Client::new(server.connect_ws(&id, None).await);
    client.wait_for_ready().await;

    // Bracketed-paste markers must survive verbatim — stripping them would break
    // safe paste in xterm.js (deep-dive 02 §5.2).
    client
        .send_json(json!({
            "type": "input",
            "data": "printf 'A\\033[200~B\\033[201~C\\n'\n"
        }))
        .await;
    client.wait_for_output("A\x1b[200~B\x1b[201~C").await;
}

#[tokio::test]
async fn multibyte_utf8_survives_pty_chunking() {
    let server = TestServer::start().await;
    let id = server.spawn_id(json!({})).await;

    let mut client = Client::new(server.connect_ws(&id, None).await);
    client.wait_for_ready().await;

    // A long run of 3-byte characters is very likely to straddle an 8 KiB read
    // boundary, which is exactly what the hold-back logic exists for.
    client
        .send_json(json!({
            "type": "input",
            "data": "for i in 1 2 3 4 5 6 7 8 9 0; do printf 'áéíóú日本語ñ%.0s' $(seq 1 200); done; echo DONE-UTF8\n"
        }))
        .await;
    let text = client.wait_for_output("DONE-UTF8").await;

    // No replacement characters: nothing was decoded lossily, and no frame cut a
    // character in half.
    assert!(
        !text.contains('\u{FFFD}'),
        "output contains U+FFFD — a character was split or lossily decoded"
    );
    assert!(text.contains("日本語"));
}

// ---- helpers ---------------------------------------------------------------

/// Poll `check` until it returns true or the deadline passes.
async fn wait_until<F: FnMut() -> bool>(limit: Duration, mut check: F) -> bool {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        if check() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Minimal HTTP client over the app's own router-facing socket.
///
/// Deliberately hand-rolled instead of pulling `reqwest` into dev-dependencies:
/// the tests need four verbs against loopback with one optional header, and a
/// full HTTP stack would be a large dependency for that.
mod reqwest_lite {
    use serde_json::Value;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_tungstenite::tungstenite::http::StatusCode;

    pub struct Client;

    impl Client {
        pub fn new() -> Self {
            Self
        }

        /// Request with the session token attached.
        pub async fn request(
            &self,
            method: &str,
            url: &str,
            token: &str,
            body: Option<Value>,
        ) -> (StatusCode, Value) {
            self.request_raw(method, url, Some(token), body).await
        }

        /// Request with the token optional, for auth-rejection tests.
        pub async fn request_raw(
            &self,
            method: &str,
            url: &str,
            token: Option<&str>,
            body: Option<Value>,
        ) -> (StatusCode, Value) {
            let rest = url.strip_prefix("http://").expect("http:// url");
            let (authority, path) = match rest.find('/') {
                Some(i) => (&rest[..i], &rest[i..]),
                None => (rest, "/"),
            };

            let mut stream = tokio::net::TcpStream::connect(authority).await.unwrap();

            let payload = body.map(|b| b.to_string()).unwrap_or_default();
            let mut request =
                format!("{method} {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n");
            if let Some(token) = token {
                request.push_str(&format!("{}: {token}\r\n", super::auth::TOKEN_HEADER));
            }
            if !payload.is_empty() {
                request.push_str("Content-Type: application/json\r\n");
                request.push_str(&format!("Content-Length: {}\r\n", payload.len()));
            }
            request.push_str("\r\n");
            request.push_str(&payload);

            stream.write_all(request.as_bytes()).await.unwrap();
            let mut raw = Vec::new();
            stream.read_to_end(&mut raw).await.unwrap();

            let text = String::from_utf8_lossy(&raw).into_owned();
            let (head, body) = text.split_once("\r\n\r\n").expect("malformed response");
            let code: u16 = head
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|c| c.parse().ok())
                .expect("no status code");

            let json = match body.trim() {
                "" => Value::Null,
                payload => serde_json::from_str(payload).unwrap_or_else(|e| {
                    panic!("response body was not JSON ({e}); raw response: {text:?}")
                }),
            };
            (StatusCode::from_u16(code).unwrap(), json)
        }
    }
}
