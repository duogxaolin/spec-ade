//! Phase 3 integration tests — SPEC-003 (ACP orchestration).
//!
//! Same shape as phase1/phase2: a real server on an ephemeral port, driving a
//! **real ACP agent subprocess** — the `mock_acp_agent` dev binary, which speaks
//! the protocol using the same crate the server does. Tests never touch `claude`
//! or `codex`: those need network and credentials and are non-deterministic, so
//! nothing about them could be asserted binarily (§7). Real agents are for §8.
//!
//! `MOCK_ACP_SCRIPT` selects the mock's behaviour; each test registers an agent
//! entry carrying that env var, so one fixture binary covers every scenario.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use spec_ade_server::acp::AcpManager;
use spec_ade_server::acp::connection::{ACP_IDLE_TIMEOUT, AcpLimits};
use spec_ade_server::acp::permission::ACP_PERMISSION_TIMEOUT;
use spec_ade_server::{AppState, build_router};
use tokio_tungstenite::tungstenite::Message as TMessage;
use tokio_tungstenite::tungstenite::http::StatusCode as TStatus;

/// Generous enough for a subprocess spawn on a loaded CI box, short enough that a
/// hang fails the test instead of stalling the suite.
const TIMEOUT: Duration = Duration::from_secs(20);

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct TestServer {
    addr: std::net::SocketAddr,
    token: String,
    client: reqwest_lite::Client,
    data_dir: std::path::PathBuf,
    cleanup: Vec<std::path::PathBuf>,
}

impl TestServer {
    /// Boot a server with the shipped timeouts.
    ///
    /// Spelled out rather than `AcpLimits::default()` so the suite is hermetic: the
    /// defaults honour env overrides, and a developer with
    /// `SPEC_ADE_ACP_PERMISSION_SECS` exported would otherwise see unrelated tests
    /// start cancelling their permission requests.
    async fn start() -> Self {
        Self::start_with_limits(AcpLimits {
            permission_timeout: ACP_PERMISSION_TIMEOUT,
            idle_timeout: ACP_IDLE_TIMEOUT,
        })
        .await
    }

    /// Boot a server whose `settings.json` already lists the mock agents.
    ///
    /// The catalogue is written to disk *before* `AppState` loads it: §3.4 makes
    /// the agent list read-only this phase, so there is no endpoint to add one,
    /// and seeding the file is exactly how a user would do it.
    async fn start_with_limits(limits: AcpLimits) -> Self {
        let token = format!("tok-{}", uuid::Uuid::new_v4());
        let data_dir = std::env::temp_dir().join(format!("spec-ade-p3-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).unwrap();

        let mock = env!("CARGO_BIN_EXE_mock_acp_agent");
        let mut agents: Vec<Value> = SCRIPTS
            .iter()
            .map(|script| {
                json!({
                    "id": format!("mock-{script}"),
                    "name": format!("Mock ({script})"),
                    "command": mock,
                    "args": [],
                    "env": { "MOCK_ACP_SCRIPT": script },
                })
            })
            .collect();
        // A command that does not exist, for the 502 path (A2).
        agents.push(json!({
            "id": "missing",
            "name": "Missing binary",
            "command": "/nonexistent/spec-ade-not-a-real-agent",
            "args": [],
            "env": {},
        }));
        // Top-level `Settings` keys are snake_case — only its nested types carry
        // `rename_all = "camelCase"`. Writing `acpAgents` here would be silently
        // ignored and the seeded claude/codex catalogue would load instead.
        std::fs::write(
            data_dir.join("settings.json"),
            json!({ "auth_token": token, "acp_agents": agents }).to_string(),
        )
        .unwrap();

        let mut state = AppState::with_data_dir(token.clone(), data_dir.clone());
        state.acp = AcpManager::with_limits(limits);
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
            data_dir: data_dir.clone(),
            cleanup: vec![data_dir],
        }
    }

    /// Boot a second server over an existing data dir.
    ///
    /// §3.4 makes the agent catalogue read-only at runtime, so a test that needs a
    /// new agent entry edits `settings.json` and reboots — the same thing a user
    /// would do. Reusing the dir keeps the registered projects too.
    async fn from_data_dir(data_dir: std::path::PathBuf, token: String) -> Self {
        let mut state = AppState::with_data_dir(token.clone(), data_dir.clone());
        state.acp = AcpManager::with_limits(AcpLimits {
            permission_timeout: ACP_PERMISSION_TIMEOUT,
            idle_timeout: ACP_IDLE_TIMEOUT,
        });
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
            data_dir,
            // The first server owns the dir's cleanup; removing it twice would
            // delete the fixture out from under the still-running original.
            cleanup: Vec::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    async fn req(&self, method: &str, path: &str, body: Option<Value>) -> (TStatus, Value) {
        self.client
            .request(method, &self.url(path), &self.token, body)
            .await
    }

    /// Register a project fixture dir containing `files`.
    async fn fixture_project(&mut self, files: &[(&str, &str)]) -> (String, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("spec-ade-p3fix-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        for (rel, content) in files {
            let p = root.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&p, content).unwrap();
        }
        let (status, body) = self
            .req(
                "POST",
                "/api/projects",
                Some(json!({ "path": root.display().to_string() })),
            )
            .await;
        assert_eq!(status, TStatus::CREATED, "fixture project failed: {body}");
        self.cleanup.push(root.clone());
        let canonical = root.canonicalize().unwrap();
        (body["id"].as_str().unwrap().to_string(), canonical)
    }

    /// Spawn a mock agent running `script`, and assert it handshook.
    async fn spawn_agent(&self, script: &str, project_id: &str) -> Value {
        let (status, body) = self
            .req(
                "POST",
                "/api/acp/spawn",
                Some(json!({ "agentId": format!("mock-{script}"), "projectId": project_id })),
            )
            .await;
        assert_eq!(
            status,
            TStatus::CREATED,
            "spawn of script {script} failed: {body}"
        );
        body
    }

    /// Open a session on a connection, returning the session row.
    async fn open_session(&self, project_id: &str, connection_id: &str) -> Value {
        let (status, body) = self
            .req(
                "POST",
                &format!("/api/projects/{project_id}/sessions"),
                Some(json!({ "connectionId": connection_id })),
            )
            .await;
        assert_eq!(status, TStatus::CREATED, "session create failed: {body}");
        body
    }

    /// The common setup: project → agent → session → attached socket.
    async fn attach(&mut self, script: &str) -> Attached {
        let (project_id, root) = self.fixture_project(&[("mock.txt", MOCK_FILE)]).await;
        let conn = self.spawn_agent(script, &project_id).await;
        let connection_id = conn["id"].as_str().unwrap().to_string();
        let session = self.open_session(&project_id, &connection_id).await;
        let session_id = session["id"].as_str().unwrap().to_string();
        let client = self.connect_ws(&connection_id, &session_id, None).await;
        Attached {
            project_id,
            root,
            connection_id,
            session_id,
            client,
        }
    }

    async fn connect_ws(&self, conn_id: &str, session_id: &str, after_seq: Option<u64>) -> Client {
        let mut url = format!(
            "ws://{}/api/acp/{conn_id}/ws?sessionId={session_id}&token={}",
            self.addr, self.token
        );
        if let Some(seq) = after_seq {
            url.push_str(&format!("&after_seq={seq}"));
        }
        let (socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let mut client = Client::new(socket);
        client.wait_for(|v| v["type"] == "ready").await;
        client
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        for dir in &self.cleanup {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Everything one attached session test needs.
struct Attached {
    #[allow(dead_code)]
    project_id: String,
    root: std::path::PathBuf,
    connection_id: String,
    session_id: String,
    client: Client,
}

/// Mock scripts that get a registered agent entry.
const SCRIPTS: &[&str] = &[
    "chunks",
    "thought",
    "tool_call",
    "permission",
    "refusal",
    "max_tokens",
    "plan",
    "unknown_variant",
    "fs_read",
    "fs_write",
    "slow",
];

/// Fixture file the `fs_read` script reads. Line numbers matter for A16.
const MOCK_FILE: &str = "line one\nline two\nline three\nline four\n";

/// A WS client that records every frame it has seen.
struct Client {
    socket: Socket,
    /// Every JSON frame received, in order — so a test can assert about a frame
    /// that should *not* have arrived, not just one that did.
    seen: Vec<Value>,
}

impl Client {
    fn new(socket: Socket) -> Self {
        Self {
            socket,
            seen: Vec::new(),
        }
    }

    async fn send(&mut self, value: Value) {
        self.socket
            .send(TMessage::Text(value.to_string().into()))
            .await
            .unwrap();
    }

    /// Pump frames until `predicate` accepts one, recording everything seen.
    async fn wait_for<F>(&mut self, mut predicate: F) -> Value
    where
        F: FnMut(&Value) -> bool,
    {
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        loop {
            // Check what already arrived before blocking again — a predicate can
            // match a frame pulled in while waiting for an earlier one.
            if let Some(hit) = self.seen.iter().rev().find(|v| predicate(v)) {
                return hit.clone();
            }
            let frame = tokio::time::timeout_at(deadline, self.socket.next())
                .await
                .unwrap_or_else(|_| panic!("timed out; frames so far: {:?}", self.seen))
                .expect("socket closed while waiting")
                .expect("websocket error");
            match frame {
                TMessage::Text(text) => {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    let matched = predicate(&value);
                    self.seen.push(value.clone());
                    if matched {
                        return value;
                    }
                }
                TMessage::Close(_) => {
                    panic!(
                        "socket closed while waiting; frames so far: {:?}",
                        self.seen
                    )
                }
                _ => {}
            }
        }
    }

    /// Drain whatever is already buffered, without waiting for anything new.
    ///
    /// Used to prove a frame did *not* arrive: the assertion needs the stream to
    /// go quiet, and a fixed sleep is the only way to observe absence.
    async fn drain(&mut self) {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(400);
        while let Ok(Some(Ok(frame))) = tokio::time::timeout_at(deadline, self.socket.next()).await
        {
            if let TMessage::Text(text) = frame {
                self.seen.push(serde_json::from_str(&text).unwrap());
            }
        }
    }

    fn types(&self) -> Vec<&str> {
        self.seen
            .iter()
            .filter_map(|v| v["type"].as_str())
            .collect()
    }

    fn all(&self, ty: &str) -> Vec<&Value> {
        self.seen.iter().filter(|v| v["type"] == ty).collect()
    }

    /// Concatenated `message_chunk` text, in `seq` order.
    fn message_text(&self) -> String {
        self.all("message_chunk")
            .iter()
            .filter_map(|v| v["text"].as_str())
            .collect()
    }

    async fn close(mut self) {
        let _ = self.socket.close(None).await;
    }
}

/// `(seq, type)` for the logged events a client received, in arrival order.
///
/// `ready` and `pong` are excluded: they are socket-protocol frames, not log
/// entries, and `ready` carries the replay *cursor* in `seq` rather than an id of
/// its own — including it would make an exact replay comparison compare apples to
/// oranges (it lands in a different position on a reattach).
fn event_log(client: &Client) -> Vec<(u64, String)> {
    client
        .seen
        .iter()
        .filter(|v| v["type"] != "ready" && v["type"] != "pong")
        .filter_map(|v| Some((v["seq"].as_u64()?, v["type"].as_str()?.to_string())))
        .collect()
}

// ---- spawn / lifecycle -----------------------------------------------------

#[tokio::test]
async fn spawn_handshake_returns_capabilities() {
    // A1 + A22.
    let mut server = TestServer::start().await;
    let (project_id, _) = server.fixture_project(&[]).await;

    let body = server.spawn_agent("chunks", &project_id).await;
    let conn_id = body["id"].as_str().unwrap();
    assert!(!conn_id.is_empty(), "spawn must return a connection id");
    assert_eq!(body["projectId"], project_id);
    assert!(
        body["agentCapabilities"].is_object(),
        "agentCapabilities must be reported so the UI can gate features: {body}"
    );
    // A22: the mock echoes back what the client advertised, which is the only way
    // to prove the *client* side of the handshake was honest.
    assert_eq!(
        body["agentInfo"]["name"], "mock-acp-agent",
        "agentInfo must identify the real agent: {body}"
    );

    let (status, list) = server.req("GET", "/api/acp", None).await;
    assert_eq!(status, TStatus::OK);
    let rows = list.as_array().unwrap();
    assert_eq!(rows.len(), 1, "the live connection must be listed: {list}");
    assert_eq!(rows[0]["id"], conn_id);
    assert_eq!(rows[0]["sessionCount"], 0);
}

#[tokio::test]
async fn client_capabilities_advertise_fs_but_not_terminal() {
    // A22 from the agent's point of view: the mock reports back the
    // `ClientCapabilities` it received, so an over-claim would be visible here.
    let mut server = TestServer::start().await;
    let (project_id, _) = server.fixture_project(&[]).await;
    let conn = server.spawn_agent("chunks", &project_id).await;

    let (status, body) = server
        .req(
            "GET",
            &format!("/api/acp/{}/stderr", conn["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(status, TStatus::OK);
    let stderr = body["stderr"].as_str().unwrap();
    assert!(
        stderr.contains("clientCapabilities"),
        "the mock logs what it was told; got: {stderr}"
    );
    assert!(
        stderr.contains("readTextFile=true") && stderr.contains("writeTextFile=true"),
        "fs.* must be advertised true ([INVENTED-8]): {stderr}"
    );
    assert!(
        stderr.contains("terminal=false"),
        "terminal must be advertised false — claiming it would make the agent \
         issue terminal/create calls that can only fail: {stderr}"
    );
}

#[tokio::test]
async fn spawn_bad_command_returns_502_with_stderr() {
    // A2 + [INVENTED-11]. 502 not 500: the failure is in an external process.
    let mut server = TestServer::start().await;
    let (project_id, _) = server.fixture_project(&[]).await;

    let (status, body) = server
        .req(
            "POST",
            "/api/acp/spawn",
            Some(json!({ "agentId": "missing", "projectId": project_id })),
        )
        .await;
    assert_eq!(status, TStatus::BAD_GATEWAY, "got {status}: {body}");
    assert_eq!(body["error"], "agent");
    assert!(
        !body["detail"].as_str().unwrap_or_default().is_empty(),
        "detail must carry why it failed, or debugging is guesswork: {body}"
    );

    // A2's second half: no phantom row for a connection that never handshook.
    let (_, list) = server.req("GET", "/api/acp", None).await;
    assert_eq!(
        list.as_array().unwrap().len(),
        0,
        "a failed spawn must not register: {list}"
    );
}

#[tokio::test]
async fn spawn_unknown_agent_or_project_is_404() {
    let mut server = TestServer::start().await;
    let (project_id, _) = server.fixture_project(&[]).await;

    let (status, body) = server
        .req(
            "POST",
            "/api/acp/spawn",
            Some(json!({ "agentId": "nope", "projectId": project_id })),
        )
        .await;
    assert_eq!(status, TStatus::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "agent");

    let (status, body) = server
        .req(
            "POST",
            "/api/acp/spawn",
            Some(json!({ "agentId": "mock-chunks", "projectId": "no-such-project" })),
        )
        .await;
    assert_eq!(status, TStatus::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "project");
}

#[tokio::test]
async fn agents_endpoint_lists_the_catalogue() {
    let server = TestServer::start().await;
    let (status, body) = server.req("GET", "/api/acp/agents", None).await;
    assert_eq!(status, TStatus::OK);
    let ids: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["id"].as_str())
        .collect();
    assert!(ids.contains(&"mock-chunks"), "got {ids:?}");
}

#[tokio::test]
async fn session_new_returns_agent_session_id() {
    // A3.
    let mut server = TestServer::start().await;
    let (project_id, root) = server.fixture_project(&[]).await;
    let conn = server.spawn_agent("chunks", &project_id).await;
    let conn_id = conn["id"].as_str().unwrap();

    let session = server.open_session(&project_id, conn_id).await;
    assert!(
        session["agentSessionId"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "the agent's own session id must be reported: {session}"
    );
    assert_ne!(
        session["id"], session["agentSessionId"],
        "Spec ADE's id and the agent's id are deliberately distinct"
    );
    assert_eq!(
        session["cwd"].as_str().unwrap(),
        root.to_str().unwrap(),
        "cwd must be the project root — it is also the fs/* sandbox root"
    );

    let (status, list) = server
        .req("GET", &format!("/api/projects/{project_id}/sessions"), None)
        .await;
    assert_eq!(status, TStatus::OK);
    assert_eq!(list.as_array().unwrap().len(), 1, "{list}");

    // The connection now reports the session.
    let (_, conns) = server.req("GET", "/api/acp", None).await;
    assert_eq!(conns[0]["sessionCount"], 1, "{conns}");
}

#[tokio::test]
async fn session_on_a_foreign_connection_is_refused() {
    // A connection spawned for another project would sandbox `fs/*` to the wrong
    // root, so binding a session across projects has to fail loudly.
    let mut server = TestServer::start().await;
    let (project_a, _) = server.fixture_project(&[]).await;
    let (project_b, _) = server.fixture_project(&[]).await;
    let conn = server.spawn_agent("chunks", &project_a).await;

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{project_b}/sessions"),
            Some(json!({ "connectionId": conn["id"] })),
        )
        .await;
    assert_eq!(status, TStatus::CONFLICT, "{body}");
    assert_eq!(body["error"], "connection");
}

// ---- streaming a turn ------------------------------------------------------

#[tokio::test]
async fn prompt_streams_chunks_then_turn_complete() {
    // A4.
    let mut server = TestServer::start().await;
    let mut a = server.attach("chunks").await;

    a.client
        .send(json!({ "type": "prompt", "text": "hi" }))
        .await;
    let done = a.client.wait_for(|v| v["type"] == "turn_complete").await;

    assert_eq!(done["stopReason"], "end_turn", "{done}");
    let chunks = a.client.all("message_chunk");
    assert!(
        !chunks.is_empty(),
        "expected at least one chunk, saw {:?}",
        a.client.types()
    );
    assert_eq!(
        a.client.all("turn_complete").len(),
        1,
        "exactly one turn_complete per turn: {:?}",
        a.client.types()
    );
    assert!(
        !a.client.message_text().is_empty(),
        "chunk text must survive the translation"
    );

    // Every event carries `seq` and `sessionId` (§3.2), and event `seq`s ascend —
    // a client relies on that to detect loss. `ready` is excluded: its `seq` is the
    // replay *cursor* (the last event already sent), not an event of its own, so it
    // legitimately repeats the preceding event's number.
    let seqs: Vec<u64> = a
        .client
        .seen
        .iter()
        .filter(|v| v["type"] != "ready")
        .filter_map(|v| v["seq"].as_u64())
        .collect();
    assert!(
        seqs.windows(2).all(|w| w[1] > w[0]),
        "event seq must ascend: {seqs:?}"
    );
    for event in &a.client.seen {
        if event["type"] == "ready" || event["type"] == "pong" {
            continue;
        }
        assert_eq!(
            event["sessionId"].as_str(),
            Some(a.session_id.as_str()),
            "events must carry Spec ADE's session id, not the agent's: {event}"
        );
    }
}

#[tokio::test]
async fn thought_chunks_are_reported_separately() {
    // §3.2: reasoning is rendered apart from the answer, so it must not be folded
    // into `message_chunk`.
    let mut server = TestServer::start().await;
    let mut a = server.attach("thought").await;

    a.client
        .send(json!({ "type": "prompt", "text": "hi" }))
        .await;
    a.client.wait_for(|v| v["type"] == "turn_complete").await;

    assert!(
        !a.client.all("thought_chunk").is_empty(),
        "expected a thought_chunk: {:?}",
        a.client.types()
    );
    assert!(
        !a.client.all("message_chunk").is_empty(),
        "the answer must still arrive: {:?}",
        a.client.types()
    );
}

#[tokio::test]
async fn refusal_and_max_tokens_pass_through_as_stop_reasons() {
    // A5. All five stop reasons are normal ends of a turn; `refusal` in particular
    // must not be reported as an error.
    for (script, expected) in [("refusal", "refusal"), ("max_tokens", "max_tokens")] {
        let mut server = TestServer::start().await;
        let mut a = server.attach(script).await;

        a.client
            .send(json!({ "type": "prompt", "text": "hi" }))
            .await;
        let done = a.client.wait_for(|v| v["type"] == "turn_complete").await;

        assert_eq!(done["stopReason"], expected, "script {script}: {done}");
        assert!(
            a.client.all("error").is_empty(),
            "a {expected} stop is not an error: {:?}",
            a.client.seen
        );
    }
}

#[tokio::test]
async fn tool_call_then_update_keeps_the_patch_sparse() {
    // A6. A merged patch carrying invented defaults would silently overwrite the
    // title/kind the agent actually set.
    let mut server = TestServer::start().await;
    let mut a = server.attach("tool_call").await;

    a.client
        .send(json!({ "type": "prompt", "text": "hi" }))
        .await;
    a.client.wait_for(|v| v["type"] == "turn_complete").await;

    let calls = a.client.all("tool_call");
    assert_eq!(calls.len(), 1, "{:?}", a.client.types());
    assert!(
        calls[0]["toolCall"]["title"].is_string(),
        "the initial call carries its title: {}",
        calls[0]
    );

    let updates = a.client.all("tool_call_update");
    assert!(!updates.is_empty(), "{:?}", a.client.types());
    let last = updates.last().unwrap();
    let patch = last["toolCall"].as_object().unwrap();
    assert_eq!(patch["status"], "completed", "{last}");
    assert!(
        !patch.contains_key("title"),
        "an absent field must stay absent, not be filled with a default: {patch:?}"
    );
    assert!(
        !patch.contains_key("kind"),
        "an absent field must stay absent: {patch:?}"
    );
}

#[tokio::test]
async fn plan_replaces_rather_than_accumulates() {
    // A7. The mock sends 2 entries then 1; a server that appended would show 3.
    let mut server = TestServer::start().await;
    let mut a = server.attach("plan").await;

    a.client
        .send(json!({ "type": "prompt", "text": "hi" }))
        .await;
    a.client.wait_for(|v| v["type"] == "turn_complete").await;

    let plans = a.client.all("plan");
    assert_eq!(
        plans.len(),
        2,
        "expected two snapshots: {:?}",
        a.client.types()
    );
    let first = plans[0]["plan"]["entries"].as_array().unwrap();
    let second = plans[1]["plan"]["entries"].as_array().unwrap();
    assert_eq!(first.len(), 2, "{:?}", plans[0]);
    assert_eq!(
        second.len(),
        1,
        "the second plan is a full snapshot, so it must be smaller, not cumulative: {:?}",
        plans[1]
    );
}

#[tokio::test]
async fn unknown_session_update_variant_is_ignored() {
    // A8. The schema already carries variants this phase does not model, and more
    // will be added; one unknown notification must not take down a live agent.
    let mut server = TestServer::start().await;
    let mut a = server.attach("unknown_variant").await;

    a.client
        .send(json!({ "type": "prompt", "text": "hi" }))
        .await;
    a.client.wait_for(|v| v["type"] == "turn_complete").await;

    // The mock sends a known-good chunk *after* the unknown one; receiving it is
    // what proves the connection survived rather than merely not crashing.
    assert!(
        a.client.message_text().contains("still alive"),
        "the connection must survive an unknown variant: {:?}",
        a.client.seen
    );
    assert!(
        a.client.all("error").is_empty(),
        "an unmodelled variant is skipped, not surfaced as an error: {:?}",
        a.client.seen
    );
    // And nothing junk leaked through under a made-up type.
    let known = [
        "ready",
        "message_chunk",
        "thought_chunk",
        "tool_call",
        "tool_call_update",
        "plan",
        "usage",
        "mode",
        "session_state",
        "turn_complete",
    ];
    for ty in a.client.types() {
        assert!(known.contains(&ty), "unexpected event type {ty} leaked");
    }
}

#[tokio::test]
async fn second_prompt_while_prompting_errors_without_disturbing_the_turn() {
    // A15 + [INVENTED-4]: no silent queue — a queued prompt reads to the user as
    // a lost message.
    let mut server = TestServer::start().await;
    let mut a = server.attach("slow").await;

    a.client
        .send(json!({ "type": "prompt", "text": "first" }))
        .await;
    a.client
        .wait_for(|v| v["type"] == "session_state" && v["state"] == "prompting")
        .await;

    a.client
        .send(json!({ "type": "prompt", "text": "second" }))
        .await;
    let err = a.client.wait_for(|v| v["type"] == "error").await;
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("already in progress"),
        "{err}"
    );

    // The first turn is untouched: cancel it and it still completes normally.
    a.client.send(json!({ "type": "cancel" })).await;
    let done = a.client.wait_for(|v| v["type"] == "turn_complete").await;
    assert_eq!(done["stopReason"], "cancelled", "{done}");
}

#[tokio::test]
async fn cancel_mid_turn_yields_cancelled() {
    // A14. Proves the command loop can still process a command while a turn runs —
    // awaiting the prompt inline would deadlock exactly here.
    let mut server = TestServer::start().await;
    let mut a = server.attach("slow").await;

    a.client
        .send(json!({ "type": "prompt", "text": "long" }))
        .await;
    a.client
        .wait_for(|v| v["type"] == "session_state" && v["state"] == "prompting")
        .await;
    a.client.send(json!({ "type": "cancel" })).await;

    let done = a.client.wait_for(|v| v["type"] == "turn_complete").await;
    assert_eq!(done["stopReason"], "cancelled", "{done}");

    // Back to idle, so the user can prompt again rather than being stuck.
    a.client
        .wait_for(|v| v["type"] == "session_state" && v["state"] == "idle")
        .await;
}

#[tokio::test]
async fn ping_is_answered_and_bad_frames_are_reported() {
    let mut server = TestServer::start().await;
    let mut a = server.attach("chunks").await;

    a.client.send(json!({ "type": "ping", "ts": 42 })).await;
    let pong = a.client.wait_for(|v| v["type"] == "pong").await;
    assert_eq!(pong["ts"], 42);

    // A frontend sending the wrong shape should find out, not debug silence.
    a.client.send(json!({ "type": "not_a_real_frame" })).await;
    let err = a.client.wait_for(|v| v["type"] == "error").await;
    assert!(
        err["message"].as_str().unwrap().contains("bad message"),
        "{err}"
    );
}

// ---- permission (two-phase) ------------------------------------------------

#[tokio::test]
async fn permission_round_trip_unblocks_agent() {
    // A9. The ACP request is held open across a user round-trip; answering it must
    // let the agent proceed.
    let mut server = TestServer::start().await;
    let mut a = server.attach("permission").await;

    a.client
        .send(json!({ "type": "prompt", "text": "write" }))
        .await;
    let req = a
        .client
        .wait_for(|v| v["type"] == "permission_request")
        .await;

    let options = req["options"].as_array().unwrap();
    assert!(!options.is_empty(), "options must reach the user: {req}");
    assert!(
        req["requestId"].as_str().is_some_and(|s| !s.is_empty()),
        "{req}"
    );
    assert!(
        req["toolCall"].is_object(),
        "the user must see what they are approving: {req}"
    );
    let allow = options
        .iter()
        .find(|o| o["kind"] == "allow_once")
        .unwrap_or(&options[0]);

    a.client
        .send(json!({
            "type": "permission_response",
            "requestId": req["requestId"],
            "optionId": allow["optionId"],
        }))
        .await;

    let resolved = a
        .client
        .wait_for(|v| v["type"] == "permission_resolved")
        .await;
    assert_eq!(resolved["outcome"], "selected", "{resolved}");

    let done = a.client.wait_for(|v| v["type"] == "turn_complete").await;
    assert_eq!(
        done["stopReason"], "end_turn",
        "the agent must run to completion after approval: {done}"
    );
    // The mock echoes the outcome it actually received, so this is end-to-end:
    // the option id the user picked reached the agent, not just *an* answer.
    let expected = format!("selected:{}", allow["optionId"].as_str().unwrap());
    assert!(
        a.client.message_text().contains(&expected),
        "the agent must observe the approval as {expected:?}: {:?}",
        a.client.seen
    );
}

#[tokio::test]
async fn permission_bad_option_id_is_rejected_and_request_stays_open() {
    // A10. Forwarding an option the agent never offered would answer a question
    // with a guess; the request must stay answerable instead.
    let mut server = TestServer::start().await;
    let mut a = server.attach("permission").await;

    a.client
        .send(json!({ "type": "prompt", "text": "write" }))
        .await;
    let req = a
        .client
        .wait_for(|v| v["type"] == "permission_request")
        .await;

    a.client
        .send(json!({
            "type": "permission_response",
            "requestId": req["requestId"],
            "optionId": "definitely-not-offered",
        }))
        .await;
    let err = a.client.wait_for(|v| v["type"] == "error").await;
    assert!(
        err["message"].as_str().unwrap().contains("not offered"),
        "{err}"
    );

    // Still parked: the correct answer now works.
    a.client.drain().await;
    assert!(
        a.client.all("permission_resolved").is_empty(),
        "a rejected option must not resolve the request: {:?}",
        a.client.seen
    );
    assert!(
        a.client.all("turn_complete").is_empty(),
        "the agent must still be waiting: {:?}",
        a.client.seen
    );

    let allow = req["options"].as_array().unwrap()[0].clone();
    a.client
        .send(json!({
            "type": "permission_response",
            "requestId": req["requestId"],
            "optionId": allow["optionId"],
        }))
        .await;
    a.client.wait_for(|v| v["type"] == "turn_complete").await;
}

#[tokio::test]
async fn permission_cancel_answers_the_agent() {
    // Every escape route has to produce an outcome, or the agent hangs forever.
    let mut server = TestServer::start().await;
    let mut a = server.attach("permission").await;

    a.client
        .send(json!({ "type": "prompt", "text": "write" }))
        .await;
    let req = a
        .client
        .wait_for(|v| v["type"] == "permission_request")
        .await;

    a.client
        .send(json!({
            "type": "permission_response",
            "requestId": req["requestId"],
            "cancelled": true,
        }))
        .await;

    let resolved = a
        .client
        .wait_for(|v| v["type"] == "permission_resolved")
        .await;
    assert_eq!(resolved["outcome"], "cancelled", "{resolved}");
    a.client.wait_for(|v| v["type"] == "turn_complete").await;
}

#[tokio::test]
async fn cancelling_a_turn_releases_a_parked_permission() {
    // A parked permission blocks the very turn being cancelled, so `cancel` has to
    // release it too — otherwise cancel appears to do nothing.
    let mut server = TestServer::start().await;
    let mut a = server.attach("permission").await;

    a.client
        .send(json!({ "type": "prompt", "text": "write" }))
        .await;
    a.client
        .wait_for(|v| v["type"] == "permission_request")
        .await;
    a.client.send(json!({ "type": "cancel" })).await;

    let resolved = a
        .client
        .wait_for(|v| v["type"] == "permission_resolved")
        .await;
    assert_eq!(resolved["outcome"], "cancelled", "{resolved}");
    a.client.wait_for(|v| v["type"] == "turn_complete").await;
}

#[tokio::test]
async fn permission_timeout_answers_cancelled() {
    // A11 ([INVENTED-6]). Nobody answers, so the sweep has to. The alternative is an
    // agent blocked on `session/request_permission` for the life of the process.
    //
    // §7 planned this as a unit test with an injected duration. It cannot be one:
    // the crate exposes no public `Responder` constructor, so a parked request
    // cannot exist without a live connection. Hence a real agent with the timeout
    // turned down via `AcpLimits`.
    let mut server = TestServer::start_with_limits(AcpLimits {
        permission_timeout: Duration::from_secs(1),
        idle_timeout: ACP_IDLE_TIMEOUT,
    })
    .await;
    let mut a = server.attach("permission").await;

    a.client
        .send(json!({ "type": "prompt", "text": "write" }))
        .await;
    let req = a
        .client
        .wait_for(|v| v["type"] == "permission_request")
        .await;

    // No answer is ever sent.
    let resolved = a
        .client
        .wait_for(|v| v["type"] == "permission_resolved")
        .await;
    assert_eq!(resolved["outcome"], "cancelled", "{resolved}");
    assert_eq!(
        resolved["requestId"], req["requestId"],
        "the resolution must name the request that expired: {resolved}"
    );

    // The agent has to actually receive it, not just have the request dropped: the
    // mock echoes its outcome, so this proves `Cancelled` crossed the wire.
    let done = a.client.wait_for(|v| v["type"] == "turn_complete").await;
    assert_eq!(done["stopReason"], "end_turn", "{done}");
    assert!(
        a.client.message_text().contains("cancelled"),
        "the agent must observe the timeout as Cancelled: {:?}",
        a.client.seen
    );

    // And the request is gone, not merely answered — a late answer must not be
    // forwarded to an agent that has already moved on.
    a.client
        .send(json!({
            "type": "permission_response",
            "requestId": req["requestId"],
            "optionId": req["options"][0]["optionId"],
        }))
        .await;
    let err = a.client.wait_for(|v| v["type"] == "error").await;
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("no pending permission"),
        "{err}"
    );
}

// ---- idle reaper -----------------------------------------------------------

#[tokio::test]
async fn idle_connection_is_reaped_but_a_watched_one_survives() {
    // [INVENTED-10] / §8 step 9. An agent process costs hundreds of MB, so one
    // nobody is using is a real leak — but reaping is only safe if "in use" is
    // judged correctly, so both halves are asserted in one test against the same
    // clock: the abandoned connection goes, the watched one stays.
    let mut server = TestServer::start_with_limits(AcpLimits {
        permission_timeout: ACP_PERMISSION_TIMEOUT,
        idle_timeout: Duration::from_secs(2),
    })
    .await;

    // Watched: a session and an attached socket, i.e. an open chat tab.
    let watched = server.attach("chunks").await;

    // Abandoned: spawned, never given a session, never attached to.
    let (project_id, _) = server.fixture_project(&[]).await;
    let idle = server.spawn_agent("chunks", &project_id).await;
    let idle_id = idle["id"].as_str().unwrap().to_string();

    // The reaper runs on the sweep tick, so allow the timeout plus one interval.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let (_, list) = server.req("GET", "/api/acp", None).await;
        let ids: Vec<&str> = list
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["id"].as_str())
            .collect();
        if !ids.contains(&idle_id.as_str()) {
            assert!(
                ids.contains(&watched.connection_id.as_str()),
                "a connection with a session and a live socket must not be reaped: {list}"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the idle connection was never reaped: {list}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Reaped means gone, not merely hidden: the id no longer resolves.
    let (status, _) = server
        .req("GET", &format!("/api/acp/{idle_id}/stderr"), None)
        .await;
    assert_eq!(status, TStatus::NOT_FOUND, "a reaped id must not resolve");

    // The survivor is still usable, which is the whole point of the exception.
    let mut watched = watched;
    watched
        .client
        .send(json!({ "type": "prompt", "text": "hi" }))
        .await;
    watched
        .client
        .wait_for(|v| v["type"] == "turn_complete")
        .await;
}

// ---- fs/* reverse calls ----------------------------------------------------

#[tokio::test]
async fn fs_read_reaches_the_agent_with_1_based_slicing() {
    // A16. The mock reads the fixture with line=2, limit=2 and echoes what it got,
    // so an off-by-one in the slice is visible.
    let mut server = TestServer::start().await;
    let mut a = server.attach("fs_read").await;

    a.client
        .send(json!({ "type": "prompt", "text": "read" }))
        .await;
    a.client.wait_for(|v| v["type"] == "turn_complete").await;

    let text = a.client.message_text();
    assert!(
        text.contains("line two") && text.contains("line three"),
        "line=2 limit=2 must yield lines 2-3 (1-based): {text:?}"
    );
    assert!(
        !text.contains("line one"),
        "1-based line=2 must not include line 1: {text:?}"
    );
    assert!(
        !text.contains("line four"),
        "limit=2 must stop after two lines: {text:?}"
    );
}

#[tokio::test]
async fn fs_read_outside_root_is_refused_without_leaking_content() {
    // A17 + [INVENTED-7]. The agent sends absolute paths; trusting them is how an
    // agent ends up reading ~/.ssh.
    let mut server = TestServer::start().await;
    let mut a = server.attach("fs_read").await;

    // Second prompt: the mock's fs_read script tries an outside-root path when the
    // prompt text asks it to.
    a.client
        .send(json!({ "type": "prompt", "text": "escape" }))
        .await;
    a.client.wait_for(|v| v["type"] == "turn_complete").await;

    let text = a.client.message_text();
    assert!(
        text.contains("refused"),
        "the agent must be told no: {text:?}"
    );
    assert!(
        !text.contains("root:"),
        "no /etc/passwd content may reach the client: {text:?}"
    );
}

#[tokio::test]
async fn fs_write_lands_on_disk() {
    // A18. The write goes through SPEC-002's atomic path, with no `rev` check —
    // the agent has no rev, and its write is the user's explicit intent.
    let mut server = TestServer::start().await;
    let mut a = server.attach("fs_write").await;

    a.client
        .send(json!({ "type": "prompt", "text": "write" }))
        .await;
    a.client.wait_for(|v| v["type"] == "turn_complete").await;

    let written = a.root.join("agent-wrote.txt");
    let content = std::fs::read_to_string(&written)
        .unwrap_or_else(|e| panic!("{} should exist: {e}", written.display()));
    assert!(content.contains("written by the agent"), "{content:?}");
}

// ---- replay ---------------------------------------------------------------

#[tokio::test]
async fn ws_close_keeps_agent_alive_and_replay_has_no_gap_or_dup() {
    // A12 + A21. Reattaching from a cursor is the reload path: the whole point is
    // no event lost and none delivered twice.
    let mut server = TestServer::start().await;
    let mut a = server.attach("chunks").await;

    a.client
        .send(json!({ "type": "prompt", "text": "hi" }))
        .await;
    // Wait for the post-turn `idle` too, not just `turn_complete`: the session
    // returns to idle immediately after, and closing the socket in between would
    // leave the first pass one event short of what the log holds.
    a.client
        .wait_for(|v| {
            v["type"] == "session_state" && v["state"] == "idle" && v["seq"].as_u64() > Some(1)
        })
        .await;

    let first_pass = event_log(&a.client);
    assert!(!first_pass.is_empty());
    a.client.close().await;

    // The agent survived the socket closing (A21).
    let (_, list) = server.req("GET", "/api/acp", None).await;
    assert_eq!(
        list.as_array().unwrap().len(),
        1,
        "closing a socket must not kill the agent: {list}"
    );

    // Reattach from scratch: the whole history replays.
    let mut full = server
        .connect_ws(&a.connection_id, &a.session_id, Some(0))
        .await;
    full.drain().await;
    let replayed = event_log(&full);
    assert_eq!(
        replayed, first_pass,
        "replay from 0 must reproduce the stream exactly"
    );
    assert!(
        full.all("truncated").is_empty(),
        "nothing was pruned, so there is no gap to report"
    );

    // Reattach from a mid-stream cursor: only the tail, exactly once.
    let cut = first_pass[first_pass.len() / 2].0;
    let mut tail = server
        .connect_ws(&a.connection_id, &a.session_id, Some(cut))
        .await;
    tail.drain().await;
    let tail_seqs: Vec<u64> = event_log(&tail).into_iter().map(|(seq, _)| seq).collect();
    let expected: Vec<u64> = first_pass
        .iter()
        .map(|(s, _)| *s)
        .filter(|s| *s > cut)
        .collect();
    assert_eq!(
        tail_seqs, expected,
        "after_seq={cut} must yield exactly the events above it"
    );
}

#[tokio::test]
async fn ready_frame_reports_current_state() {
    // A client attaching mid-turn must learn a turn is running without inferring
    // it from the replayed events.
    let mut server = TestServer::start().await;
    let mut a = server.attach("slow").await;

    a.client
        .send(json!({ "type": "prompt", "text": "long" }))
        .await;
    a.client
        .wait_for(|v| v["type"] == "session_state" && v["state"] == "prompting")
        .await;

    let mut second = server
        .connect_ws(&a.connection_id, &a.session_id, None)
        .await;
    let ready = second.seen.iter().find(|v| v["type"] == "ready").unwrap();
    assert_eq!(
        ready["state"], "prompting",
        "a mid-turn attach must see the live state: {ready}"
    );

    a.client.send(json!({ "type": "cancel" })).await;
    a.client.wait_for(|v| v["type"] == "turn_complete").await;
    // Both sockets see the same turn end — the broadcast fans out, it does not
    // hand the turn to whoever attached last.
    second.wait_for(|v| v["type"] == "turn_complete").await;
}

// ---- WS error handling ----------------------------------------------------

#[tokio::test]
async fn ws_requires_a_session_belonging_to_the_connection() {
    // §3.2: a missing or foreign `sessionId` closes with 1008 rather than opening a
    // socket on which events could never arrive.
    let mut server = TestServer::start().await;
    let (project_id, _) = server.fixture_project(&[]).await;
    let conn_a = server.spawn_agent("chunks", &project_id).await;
    let conn_b = server.spawn_agent("chunks", &project_id).await;
    let session_on_a = server
        .open_session(&project_id, conn_a["id"].as_str().unwrap())
        .await;

    // Missing sessionId.
    let url = format!(
        "ws://{}/api/acp/{}/ws?token={}",
        server.addr,
        conn_a["id"].as_str().unwrap(),
        server.token
    );
    assert_close_code(&url, 1008, "missing sessionId").await;

    // A session that exists, but on another connection.
    let url = format!(
        "ws://{}/api/acp/{}/ws?sessionId={}&token={}",
        server.addr,
        conn_b["id"].as_str().unwrap(),
        session_on_a["id"].as_str().unwrap(),
        server.token
    );
    assert_close_code(&url, 1008, "foreign sessionId").await;
}

#[tokio::test]
async fn ws_on_unknown_connection_is_404() {
    // An unknown *connection* has nothing to attach to at all, so it never becomes
    // a socket — the handshake itself fails.
    let server = TestServer::start().await;
    let url = format!(
        "ws://{}/api/acp/no-such-connection/ws?sessionId=x&token={}",
        server.addr, server.token
    );
    assert!(
        tokio_tungstenite::connect_async(url).await.is_err(),
        "unknown connection must not upgrade"
    );

    let (status, body) = server
        .req("GET", "/api/acp/no-such-connection/stderr", None)
        .await;
    assert_eq!(status, TStatus::NOT_FOUND, "{body}");
}

/// Assert a WS upgrade succeeds but is immediately closed with `code`.
async fn assert_close_code(url: &str, code: u16, what: &str) {
    let (mut socket, _) = tokio_tungstenite::connect_async(url)
        .await
        .unwrap_or_else(|e| {
            panic!(
                "{what}: upgrade should succeed so the close code is \
                                   observable, got {e}"
            )
        });
    let frame = tokio::time::timeout(TIMEOUT, socket.next())
        .await
        .unwrap_or_else(|_| panic!("{what}: timed out waiting for the close"))
        .unwrap_or_else(|| panic!("{what}: stream ended with no close frame"))
        .unwrap_or_else(|e| panic!("{what}: {e}"));
    match frame {
        TMessage::Close(Some(close)) => assert_eq!(
            u16::from(close.code),
            code,
            "{what}: wrong close code (reason: {})",
            close.reason
        ),
        other => panic!("{what}: expected a close frame, got {other:?}"),
    }
}

// ---- teardown -------------------------------------------------------------

#[tokio::test]
async fn delete_connection_closes_sockets_and_kills_the_process() {
    // A20. `AcpAgent` puts the child in its own process group and SIGKILLs the
    // group on drop, so an agent behind `npx` cannot be orphaned.
    let mut server = TestServer::start().await;
    let mut a = server.attach("chunks").await;

    let (status, _) = server
        .req("DELETE", &format!("/api/acp/{}", a.connection_id), None)
        .await;
    assert_eq!(status, TStatus::NO_CONTENT);

    // The socket is told why before it closes.
    let closed = a
        .client
        .wait_for(|v| v["type"] == "connection_closed")
        .await;
    assert!(
        closed["reason"].as_str().is_some(),
        "the client must learn why: {closed}"
    );

    let (_, list) = server.req("GET", "/api/acp", None).await;
    assert_eq!(
        list.as_array().unwrap().len(),
        0,
        "a killed connection must leave the list: {list}"
    );

    let (status, _) = server
        .req("DELETE", &format!("/api/acp/{}", a.connection_id), None)
        .await;
    assert_eq!(
        status,
        TStatus::NOT_FOUND,
        "a second delete has nothing to delete"
    );
}

#[tokio::test]
async fn external_kill_emits_connection_closed() {
    // A19. The mock's `die_on_start` script exits right after the handshake, which
    // is the same transport EOF an externally-killed agent produces.
    let mut server = TestServer::start().await;
    let (project_id, _) = server.fixture_project(&[]).await;

    // `die_on_start` is deliberately not in SCRIPTS: it dies before `initialize`
    // answers, which is the 502 path, not this one. Register it inline.
    let mock = env!("CARGO_BIN_EXE_mock_acp_agent");
    let settings_path = server.data_dir.join("settings.json");
    let mut settings: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    settings["acp_agents"].as_array_mut().unwrap().push(json!({
        "id": "dies",
        "name": "Dies after handshake",
        "command": mock,
        "args": [],
        "env": { "MOCK_ACP_SCRIPT": "die_after_handshake" },
    }));
    std::fs::write(&settings_path, settings.to_string()).unwrap();

    // Reboot so the new catalogue is loaded (§3.4 is read-only at runtime).
    let mut server2 =
        TestServer::from_data_dir(server.data_dir.clone(), server.token.clone()).await;
    let (project_id2, _) = server2.fixture_project(&[]).await;
    let _ = project_id;

    let (status, conn) = server2
        .req(
            "POST",
            "/api/acp/spawn",
            Some(json!({ "agentId": "dies", "projectId": project_id2 })),
        )
        .await;
    assert_eq!(
        status,
        TStatus::CREATED,
        "handshake must succeed first: {conn}"
    );
    let conn_id = conn["id"].as_str().unwrap().to_string();
    let session = server2.open_session(&project_id2, &conn_id).await;
    let mut client = server2
        .connect_ws(&conn_id, session["id"].as_str().unwrap(), None)
        .await;

    // The script exits once prompted.
    client
        .send(json!({ "type": "prompt", "text": "die" }))
        .await;
    let closed = client.wait_for(|v| v["type"] == "connection_closed").await;
    assert!(closed["reason"].as_str().is_some(), "{closed}");
    client
        .wait_for(|v| v["type"] == "session_state" && v["state"] == "closed")
        .await;

    let (_, list) = server2.req("GET", "/api/acp", None).await;
    assert_eq!(
        list.as_array().unwrap().len(),
        0,
        "a dead connection must leave the list: {list}"
    );
}

#[tokio::test]
async fn deleting_a_project_kills_its_agents() {
    // The project's path is the session `cwd` and the `fs/*` sandbox root; an
    // agent left running would keep working against a deregistered directory.
    let mut server = TestServer::start().await;
    let mut a = server.attach("chunks").await;

    let (status, _) = server
        .req("DELETE", &format!("/api/projects/{}", a.project_id), None)
        .await;
    assert_eq!(status, TStatus::NO_CONTENT);

    a.client
        .wait_for(|v| v["type"] == "connection_closed")
        .await;
    let (_, list) = server.req("GET", "/api/acp", None).await;
    assert_eq!(list.as_array().unwrap().len(), 0, "{list}");

    // Sessions of that project are gone too.
    let (status, _) = server
        .req(
            "GET",
            &format!("/api/projects/{}/sessions", a.project_id),
            None,
        )
        .await;
    assert_eq!(status, TStatus::NOT_FOUND);
}

#[tokio::test]
async fn deleting_a_session_leaves_the_agent_running() {
    // One connection serves many sessions ([INVENTED-1]), so dropping one session
    // must not take down the others.
    let mut server = TestServer::start().await;
    let (project_id, _) = server.fixture_project(&[]).await;
    let conn = server.spawn_agent("chunks", &project_id).await;
    let conn_id = conn["id"].as_str().unwrap();
    let s1 = server.open_session(&project_id, conn_id).await;
    let s2 = server.open_session(&project_id, conn_id).await;

    let (status, _) = server
        .req(
            "DELETE",
            &format!("/api/sessions/{}", s1["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(status, TStatus::NO_CONTENT);

    let (_, list) = server.req("GET", "/api/acp", None).await;
    assert_eq!(
        list.as_array().unwrap().len(),
        1,
        "the agent must survive: {list}"
    );

    // The surviving session still works.
    let mut client = server
        .connect_ws(conn_id, s2["id"].as_str().unwrap(), None)
        .await;
    client.send(json!({ "type": "prompt", "text": "hi" })).await;
    client.wait_for(|v| v["type"] == "turn_complete").await;

    let (status, _) = server
        .req(
            "DELETE",
            &format!("/api/sessions/{}", s1["id"].as_str().unwrap()),
            None,
        )
        .await;
    assert_eq!(status, TStatus::NOT_FOUND, "a second delete finds nothing");
}

#[tokio::test]
async fn acp_routes_require_auth() {
    // The whole auth design exists for this surface: an agent process is RCE.
    let server = TestServer::start().await;
    for (method, path) in [
        ("GET", "/api/acp"),
        ("GET", "/api/acp/agents"),
        ("POST", "/api/acp/spawn"),
    ] {
        let (status, _) = server
            .client
            .request_raw(method, &server.url(path), None, Some(json!({})))
            .await;
        assert_eq!(
            status,
            TStatus::UNAUTHORIZED,
            "{method} {path} must be gated"
        );
    }
}

// ---- minimal HTTP client (same rationale as phase1/phase2) -----------------
//
// Copied rather than shared: an integration test binary can't import from a
// sibling test file, and a `tests/common/` module would be compiled into every
// test target whether it needs it or not.

mod reqwest_lite {
    use serde_json::Value;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_tungstenite::tungstenite::http::StatusCode;

    const TOKEN_HEADER: &str = "x-spec-ade-token";

    pub struct Client;

    impl Client {
        pub fn new() -> Self {
            Self
        }

        pub async fn request(
            &self,
            method: &str,
            url: &str,
            token: &str,
            body: Option<Value>,
        ) -> (StatusCode, Value) {
            self.send(method, url, Some(token), None, body).await
        }

        pub async fn request_raw(
            &self,
            method: &str,
            url: &str,
            token: Option<&str>,
            body: Option<Value>,
        ) -> (StatusCode, Value) {
            self.send(method, url, token, None, body).await
        }

        async fn send(
            &self,
            method: &str,
            url: &str,
            token: Option<&str>,
            origin: Option<&str>,
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
                request.push_str(&format!("{TOKEN_HEADER}: {token}\r\n"));
            }
            if let Some(origin) = origin {
                request.push_str(&format!("Origin: {origin}\r\n"));
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
