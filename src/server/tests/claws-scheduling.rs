//! SPEC-007 claws integration tests — scheduling, permissions policy, keep-alive,
//! skills discovery (E9–E42).
//!
//! Same strategy as the other suites: a real server on an ephemeral port driving a
//! **real ACP agent subprocess** — the `mock_acp_agent` dev binary, selected per
//! agent entry via `MOCK_ACP_SCRIPT`, so one fixture binary covers every scenario.
//! Schedules fire for real (a `* * * * * *` cron), so timing assertions are
//! generous lower bounds, never tight upper bounds.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use spec_ade_server::acp::AcpManager;
use spec_ade_server::acp::connection::{ACP_IDLE_TIMEOUT, AcpLimits};
use spec_ade_server::acp::permission::ACP_PERMISSION_TIMEOUT;
use spec_ade_server::{AppState, build_router};
use tokio_tungstenite::tungstenite::Message as TMessage;
use tokio_tungstenite::tungstenite::http::StatusCode as TStatus;

/// Generous enough for a subprocess spawn on a loaded box, short enough that a
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

    async fn start_with_limits(limits: AcpLimits) -> Self {
        let token = format!("tok-{}", uuid::Uuid::new_v4());
        let data_dir =
            std::env::temp_dir().join(format!("spec-ade-claws-{}", uuid::Uuid::new_v4()));
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
        // A command that does not exist anywhere — the 502 spawn-failure path
        // needs an agent whose process can never start.
        agents.push(json!({
            "id": "missing",
            "name": "Missing binary",
            "command": "/nonexistent/spec-ade-not-a-real-agent",
            "args": [],
            "env": {},
        }));
        // Top-level `Settings` keys are snake_case — only its nested types carry
        // `rename_all = "camelCase"`.
        std::fs::write(
            data_dir.join("settings.json"),
            json!({ "auth_token": token, "acp_agents": agents }).to_string(),
        )
        .unwrap();

        let mut state = AppState::with_data_dir(token.clone(), data_dir.clone());
        state.acp = AcpManager::with_limits(limits);
        Self::boot(state, token, data_dir).await
    }

    /// Boot a second server over an existing data dir — the restart path. Reusing
    /// the dir keeps registered projects, agents and claw definitions.
    async fn from_data_dir(data_dir: std::path::PathBuf, token: String) -> Self {
        let mut state = AppState::with_data_dir(token.clone(), data_dir.clone());
        state.acp = AcpManager::with_limits(AcpLimits {
            permission_timeout: ACP_PERMISSION_TIMEOUT,
            idle_timeout: ACP_IDLE_TIMEOUT,
        });
        // The first server owns the dir's cleanup; the second must not delete
        // the fixture out from under the still-running original, so it starts
        // with an empty cleanup list.
        let mut server = Self::boot(state, token, data_dir).await;
        server.cleanup = Vec::new();
        server
    }

    async fn boot(state: AppState, token: String, data_dir: std::path::PathBuf) -> Self {
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
            cleanup: vec![data_dir.clone()],
            data_dir,
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
        let root = std::env::temp_dir().join(format!("spec-ade-clawsfix-{}", uuid::Uuid::new_v4()));
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

    /// Same as [`Self::fixture_project`] but named for intent when the files are
    /// `SKILL.md` layouts — the route reads them through skill discovery.
    async fn fixture_project_with_skills(
        &mut self,
        skills: &[(&str, &str)],
    ) -> (String, std::path::PathBuf) {
        self.fixture_project(skills).await
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

/// Mock scripts that get a registered agent entry. `missing` (a command that does
/// not exist, for the 502 path) is appended inline in `start`.
const SCRIPTS: &[&str] = &["chunks", "permission", "slow", "die_after_handshake"];

// ---- claws helpers ---------------------------------------------------------

/// Minimal `ClawInput`: everything else takes its serde default.
fn claw_input(name: &str, agent_id: &str, project_id: &str) -> Value {
    json!({
        "name": name,
        "agentId": agent_id,
        "projectId": project_id,
        "schedules": [],
    })
}

/// A schedule that fires every second — fast enough to observe, slow enough that
/// assertions are orderings, not races.
fn every_second(prompts: Value) -> Value {
    json!({ "cron": "* * * * * *", "prompts": prompts })
}

async fn create_claw(server: &TestServer, input: Value) -> Value {
    let (status, body) = server.req("POST", "/api/claws", Some(input)).await;
    assert_eq!(status, TStatus::CREATED, "claw create failed: {body}");
    body
}

/// Attach to a started claw's session once the runtime reports both ids.
async fn claw_socket(server: &TestServer, id: &str) -> Client {
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let (_, body) = server.req("GET", &format!("/api/claws/{id}"), None).await;
        let conn = body["status"]["connectionId"].as_str().map(str::to_string);
        let sess = body["status"]["sessionId"].as_str().map(str::to_string);
        if let (Some(conn), Some(sess)) = (conn, sess) {
            return server.connect_ws(&conn, &sess, Some(0)).await;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "claw {id} never reported a live session: {body}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// `(seq, type)` for the logged events a client received, in arrival order.
fn event_log(client: &Client) -> Vec<(u64, String)> {
    client
        .seen
        .iter()
        .filter(|v| v["type"] != "ready" && v["type"] != "pong")
        .filter_map(|v| Some((v["seq"].as_u64()?, v["type"].as_str()?.to_string())))
        .collect()
}

/// Poll until `predicate` holds on the claw row, else panic at the deadline.
async fn wait_status<F>(server: &TestServer, id: &str, mut predicate: F, what: &str) -> Value
where
    F: FnMut(&Value) -> bool,
{
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let (_, body) = server.req("GET", &format!("/api/claws/{id}"), None).await;
        if predicate(&body) {
            return body;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{what}: last row {body}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

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
    async fn drain(&mut self) {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(400);
        while let Ok(Some(Ok(frame))) = tokio::time::timeout_at(deadline, self.socket.next()).await
        {
            if let TMessage::Text(text) = frame {
                self.seen.push(serde_json::from_str(&text).unwrap());
            }
        }
    }

    /// Concatenated `message_chunk` text, in arrival order.
    fn message_text(&self) -> String {
        self.all("message_chunk")
            .iter()
            .filter_map(|v| v["text"].as_str())
            .collect()
    }

    fn all(&self, ty: &str) -> Vec<&Value> {
        self.seen.iter().filter(|v| v["type"] == ty).collect()
    }

    async fn close(mut self) {
        let _ = self.socket.close(None).await;
    }
}

// ---- minimal HTTP client (same rationale as the sibling suites) -------------
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

// ---- E9–E13: CRUD over /api/claws -------------------------------------------

#[tokio::test]
async fn e9_create_lists_and_reads_a_claw() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[("a.rs", "fn main() {}\n")]).await;

    let created = create_claw(&server, claw_input("review-bot", "mock-chunks", &project)).await;
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["name"], "review-bot");
    assert_eq!(created["agentId"], "mock-chunks");
    assert_eq!(
        created["permissionMode"], "auto_approve",
        "documented default"
    );
    assert_eq!(created["enabled"], true);

    let (_, list) = server.req("GET", "/api/claws", None).await;
    let listed = list
        .as_array()
        .expect("list must be an array")
        .iter()
        .find(|c| c["id"] == id.as_str())
        .expect("created claw must appear in the list")
        .clone();
    assert_eq!(listed["name"], "review-bot");
    assert_eq!(
        listed["status"]["state"], "stopped",
        "fresh claw is stopped"
    );

    let (_, row) = server.req("GET", &format!("/api/claws/{id}"), None).await;
    assert_eq!(row["id"], id.as_str());
    assert_eq!(row["status"]["scheduleCount"], 0);
}

#[tokio::test]
async fn e10_unknown_agent_or_project_is_404() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[]).await;

    let mut input = claw_input("bad-agent", "no-such-agent", &project);
    let (status, body) = server.req("POST", "/api/claws", Some(input.clone())).await;
    assert_eq!(status, TStatus::NOT_FOUND);
    assert_eq!(body["error"], "agent", "agent errors carry the agent group");

    input = claw_input("bad-project", "mock-chunks", "no-such-project");
    let (status, body) = server.req("POST", "/api/claws", Some(input)).await;
    assert_eq!(status, TStatus::NOT_FOUND);
    assert_eq!(body["error"], "project");
}

#[tokio::test]
async fn e11_unknown_permission_mode_is_400() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[]).await;

    let mut input = claw_input("bad-mode", "mock-chunks", &project);
    input["permissionMode"] = json!("yolo");
    let (status, body) = server.req("POST", "/api/claws", Some(input)).await;
    assert_eq!(status, TStatus::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "claw");
    assert!(
        body["detail"].as_str().unwrap().contains("yolo"),
        "the error must name the rejected mode: {body}"
    );

    // The dead Telegram mode is refused by name, not silently downgraded —
    // a user must not believe remote approval was armed.
    let mut input = claw_input("telegram", "mock-chunks", &project);
    input["permissionMode"] = json!("ask_via_telegram");
    let (status, body) = server.req("POST", "/api/claws", Some(input)).await;
    assert_eq!(status, TStatus::BAD_REQUEST);
    assert!(
        body["detail"]
            .as_str()
            .unwrap()
            .contains("ask_via_telegram"),
        "must name telegram: {body}"
    );
}

#[tokio::test]
async fn e12_put_replaces_whole_definition() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[]).await;

    let created = create_claw(&server, claw_input("v1", "mock-chunks", &project)).await;
    let id = created["id"].as_str().unwrap().to_string();

    let mut input = claw_input("v2", "mock-permission", &project);
    input["schedules"] = json!([{ "cron": "0 9 * * *", "prompts": ["morning"] }]);
    let (status, body) = server
        .req("PUT", &format!("/api/claws/{id}"), Some(input))
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(body["id"], id.as_str(), "PUT keeps the id");
    assert_eq!(body["name"], "v2");
    assert_eq!(body["agentId"], "mock-permission");
    assert_eq!(body["status"]["scheduleCount"], 1);
    assert!(
        !body["status"]["scheduleDescriptions"]
            .as_array()
            .unwrap()
            .is_empty(),
        "descriptions echo per schedule"
    );
}

#[tokio::test]
async fn e13_delete_removes_then_404s() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[]).await;

    let created = create_claw(&server, claw_input("gone", "mock-chunks", &project)).await;
    let id = created["id"].as_str().unwrap().to_string();

    let (status, _) = server
        .req("DELETE", &format!("/api/claws/{id}"), None)
        .await;
    assert_eq!(status, TStatus::NO_CONTENT);

    let (status, _) = server.req("GET", &format!("/api/claws/{id}"), None).await;
    assert_eq!(status, TStatus::NOT_FOUND, "second read is a 404");
}

// ---- E14–E18: persistence, start/stop, spawn failure -------------------------

#[tokio::test]
async fn e14_definitions_survive_a_server_restart() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[]).await;

    let created = create_claw(&server, claw_input("survivor", "mock-chunks", &project)).await;
    let id = created["id"].as_str().unwrap().to_string();

    // Drop the first server; its Drop keeps the data dir because the second
    // server is about to reuse it — so clear cleanup before dropping.
    let data_dir = server.data_dir.clone();
    let token = server.token.clone();
    server.cleanup.clear();
    drop(server);

    let second = TestServer::from_data_dir(data_dir, token).await;
    let (_, row) = second.req("GET", &format!("/api/claws/{id}"), None).await;
    assert_eq!(row["id"], id.as_str(), "same id after restart");
    assert_eq!(row["name"], "survivor");
    assert_eq!(
        row["status"]["state"], "stopped",
        "[INVENTED-10]: no resume"
    );
}

#[tokio::test]
async fn e15_list_filters_by_project() {
    let mut server = TestServer::start().await;
    let (p1, _) = server.fixture_project(&[]).await;
    let (p2, _) = server.fixture_project(&[]).await;

    create_claw(&server, claw_input("one", "mock-chunks", &p1)).await;
    create_claw(&server, claw_input("two", "mock-chunks", &p2)).await;

    let (_, list) = server
        .req("GET", &format!("/api/claws?projectId={p1}"), None)
        .await;
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 1, "{list}");
    assert_eq!(arr[0]["name"], "one");
    assert_eq!(arr[0]["projectId"], p1.as_str());
}

#[tokio::test]
async fn e16_bad_cron_in_put_leaves_old_definition_intact() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[]).await;

    let created = create_claw(&server, claw_input("original", "mock-chunks", &project)).await;
    let id = created["id"].as_str().unwrap().to_string();

    let mut input = claw_input("original", "mock-chunks", &project);
    input["schedules"] = json!([{ "cron": "not a cron at all", "prompts": ["x"] }]);
    let (status, body) = server
        .req("PUT", &format!("/api/claws/{id}"), Some(input))
        .await;
    assert_eq!(status, TStatus::BAD_REQUEST);
    assert_eq!(body["error"], "cron");
    assert!(
        body["detail"].as_str().unwrap().contains("schedule 0"),
        "the cron error names its schedule index: {body}"
    );

    // Validate-on-save: the stored definition must be untouched.
    let (_, row) = server.req("GET", &format!("/api/claws/{id}"), None).await;
    assert_eq!(row["name"], "original");
    assert_eq!(row["agentId"], "mock-chunks");
}

#[tokio::test]
async fn e17_start_spawns_connection_on_the_right_project() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[("a.rs", "fn main() {}\n")]).await;

    let created = create_claw(&server, claw_input("runner", "mock-chunks", &project)).await;
    let id = created["id"].as_str().unwrap().to_string();

    let (status, _) = server
        .req("POST", &format!("/api/claws/{id}/start"), None)
        .await;
    assert_eq!(status, TStatus::OK);

    let row = wait_status(
        &server,
        &id,
        |v| v["status"]["connectionId"].is_string(),
        "start",
    )
    .await;
    let conn_id = row["status"]["connectionId"].as_str().unwrap();
    assert_eq!(conn_id, format!("claw:{id}"));
    assert!(row["status"]["sessionId"].is_string());

    // The connection shows up in the plain ACP listing too.
    let (_, acp) = server.req("GET", "/api/acp", None).await;
    assert!(
        acp.as_array().unwrap().iter().any(|c| c["id"] == conn_id),
        "claw connection must be listed under /api/acp"
    );
    assert!(
        ["starting", "running", "idle"]
            .iter()
            .any(|s| row["status"]["state"] == *s),
        "live state, got {}",
        row["status"]["state"]
    );

    server
        .req("POST", &format!("/api/claws/{id}/stop"), None)
        .await;
}

#[tokio::test]
async fn e18_double_start_conflicts() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[]).await;

    let created = create_claw(&server, claw_input("busy", "mock-slow", &project)).await;
    let id = created["id"].as_str().unwrap().to_string();

    let (status, _) = server
        .req("POST", &format!("/api/claws/{id}/start"), None)
        .await;
    assert_eq!(status, TStatus::OK);

    // Wait until the runtime actually holds the connection, then the second
    // start must collide.
    wait_status(
        &server,
        &id,
        |v| v["status"]["state"] != "starting",
        "first start",
    )
    .await;
    let (status, body) = server
        .req("POST", &format!("/api/claws/{id}/start"), None)
        .await;
    assert_eq!(status, TStatus::CONFLICT, "{body}");
    assert_eq!(body["error"], "claw");

    server
        .req("POST", &format!("/api/claws/{id}/stop"), None)
        .await;
}

// ---- E19–E23: stop, spawn failure, permission policy -------------------------

#[tokio::test]
async fn e19_stop_kills_the_connection() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[]).await;

    let created = create_claw(&server, claw_input("haltable", "mock-slow", &project)).await;
    let id = created["id"].as_str().unwrap().to_string();
    server
        .req("POST", &format!("/api/claws/{id}/start"), None)
        .await;
    wait_status(
        &server,
        &id,
        |v| v["status"]["state"] != "starting",
        "start",
    )
    .await;

    let (status, _) = server
        .req("POST", &format!("/api/claws/{id}/stop"), None)
        .await;
    assert_eq!(status, TStatus::OK);

    let row = wait_status(
        &server,
        &id,
        |v| v["status"]["connectionId"].is_null(),
        "stop",
    )
    .await;
    assert_eq!(row["status"]["state"], "stopped");

    // The ACP listing must no longer carry the claw connection.
    let (_, acp) = server.req("GET", "/api/acp", None).await;
    assert!(
        !acp.as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == format!("claw:{id}")),
        "stopped claw must not linger in /api/acp"
    );
}

#[tokio::test]
async fn e20_stop_is_idempotent() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[]).await;

    let created = create_claw(&server, claw_input("calm", "mock-chunks", &project)).await;
    let id = created["id"].as_str().unwrap().to_string();

    // Stopping something that never started is fine — a UI toggle must not 500.
    let (status, body) = server
        .req("POST", &format!("/api/claws/{id}/stop"), None)
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    let (status, _) = server
        .req("POST", &format!("/api/claws/{id}/stop"), None)
        .await;
    assert_eq!(status, TStatus::OK);
}

#[tokio::test]
async fn e21_failed_spawn_lands_in_error_state() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[]).await;

    let mut input = claw_input("doomed", "missing", &project);
    input["keepAlive"] = json!(false);
    let created = create_claw(&server, input).await;
    let id = created["id"].as_str().unwrap().to_string();

    let (status, body) = server
        .req("POST", &format!("/api/claws/{id}/start"), None)
        .await;
    assert_eq!(status, TStatus::BAD_GATEWAY, "{body}");
    assert_eq!(body["error"], "agent");
    assert!(
        !body["detail"].as_str().unwrap_or("").is_empty(),
        "the 502 detail carries the spawn failure: {body}"
    );

    // And the runtime slot records the error state — the UI can show it without
    // re-deriving from the failed call.
    wait_status(
        &server,
        &id,
        |v| v["status"]["state"] == "error",
        "error state",
    )
    .await;

    // The placeholder is replaceable: a start with a valid agent recovers.
    let mut fix = claw_input("doomed", "mock-chunks", &project);
    fix["keepAlive"] = json!(false);
    let (status, _) = server
        .req("PUT", &format!("/api/claws/{id}"), Some(fix))
        .await;
    assert_eq!(status, TStatus::OK);
    let (status, _) = server
        .req("POST", &format!("/api/claws/{id}/start"), None)
        .await;
    assert_eq!(status, TStatus::OK);
    wait_status(
        &server,
        &id,
        |v| v["status"]["state"] != "starting",
        "recovery",
    )
    .await;
}

#[tokio::test]
async fn e22_deleting_a_running_claw_stops_it() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[]).await;

    let created = create_claw(&server, claw_input("hot", "mock-slow", &project)).await;
    let id = created["id"].as_str().unwrap().to_string();
    server
        .req("POST", &format!("/api/claws/{id}/start"), None)
        .await;
    wait_status(
        &server,
        &id,
        |v| v["status"]["state"] != "starting",
        "start",
    )
    .await;

    let (status, _) = server
        .req("DELETE", &format!("/api/claws/{id}"), None)
        .await;
    assert_eq!(status, TStatus::NO_CONTENT);

    // The delete path must tear down the connection before answering.
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let (_, acp) = server.req("GET", "/api/acp", None).await;
        if !acp
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == format!("claw:{id}"))
        {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "deleted claw's connection still alive"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn e23_auto_approve_answers_permissions_itself() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[("a.rs", "fn main() {}\n")]).await;

    // A schedule is what drives the turns ([INVENTED-4]: no skill means the
    // runtime opens nothing on its own), so give the claw a fire-per-second one.
    let mut input = claw_input("self-serve", "mock-permission", &project);
    input["schedules"] = json!([every_second(json!(["go"]))]);
    let created = create_claw(&server, input).await;
    let id = created["id"].as_str().unwrap().to_string();
    server
        .req("POST", &format!("/api/claws/{id}/start"), None)
        .await;

    // Attach to the claw session and watch it drive itself through a turn.
    let mut client = claw_socket(&server, &id).await;
    let turn = client.wait_for(|v| v["type"] == "turn_complete").await;
    let _ = turn;

    let text = client.message_text();
    assert!(
        text.contains("selected:allow"),
        "auto_approve picked the allow option; got: {text}"
    );

    // Both sides of the exchange are logged for the transcript, but nobody
    // human answered anything.
    let log = event_log(&client);
    assert!(
        log.iter().any(|(_, t)| t == "permission_request"),
        "request logged: {log:?}"
    );
    assert!(
        log.iter().any(|(_, t)| t == "permission_resolved"),
        "resolution logged: {log:?}"
    );
    client.close().await;
    server
        .req("POST", &format!("/api/claws/{id}/stop"), None)
        .await;
}

// ---- E24–E28: permission policy continued + skills ---------------------------

#[tokio::test]
async fn e24_deny_all_rejects_and_still_logs() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[]).await;

    let mut input = claw_input("paranoid", "mock-permission", &project);
    input["permissionMode"] = json!("deny_all");
    input["schedules"] = json!([every_second(json!(["go"]))]);
    let created = create_claw(&server, input).await;
    let id = created["id"].as_str().unwrap().to_string();
    server
        .req("POST", &format!("/api/claws/{id}/start"), None)
        .await;

    let mut client = claw_socket(&server, &id).await;
    client.wait_for(|v| v["type"] == "turn_complete").await;

    let text = client.message_text();
    assert!(
        text.contains("selected:reject"),
        "deny_all must pick the reject option; got: {text}"
    );
    // Rejection is still an answer: both frames land in the transcript.
    let log = event_log(&client);
    assert!(log.iter().any(|(_, t)| t == "permission_request"));
    assert!(log.iter().any(|(_, t)| t == "permission_resolved"));
    client.close().await;
    server
        .req("POST", &format!("/api/claws/{id}/stop"), None)
        .await;
}

#[tokio::test]
async fn e25_ask_via_ui_parks_until_someone_answers() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[]).await;

    let mut input = claw_input("parks", "mock-permission", &project);
    input["permissionMode"] = json!("ask_via_ui");
    input["schedules"] = json!([every_second(json!(["go"]))]);
    let created = create_claw(&server, input).await;
    let id = created["id"].as_str().unwrap().to_string();
    server
        .req("POST", &format!("/api/claws/{id}/start"), None)
        .await;

    let mut client = claw_socket(&server, &id).await;
    let request = client.wait_for(|v| v["type"] == "permission_request").await;
    let request_id = request["requestId"].as_str().unwrap().to_string();

    // Long enough that the next `* * * * * *`-style trigger would have fired if
    // skipIfRunning were broken; short enough to keep the suite quick. Exactly
    // ONE parked request proves the busy tick was skipped, not queued.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    client.drain().await;
    assert_eq!(
        client.all("permission_request").len(),
        1,
        "skipIfRunning must park, not stack requests"
    );

    // A human answers with the agent's own first option.
    let options = request["options"].as_array().unwrap();
    client
        .send(json!({
            "type": "permission_response",
            "requestId": request_id,
            "optionId": options[0]["optionId"],
        }))
        .await;
    client.wait_for(|v| v["type"] == "turn_complete").await;

    let text = client.message_text();
    assert!(
        text.contains("selected:allow") || text.contains("cancelled"),
        "human answer echoed back: {text}"
    );

    // The next trigger resumes: a NEW parked request with a fresh id.
    let second = client
        .wait_for(|v| v["type"] == "permission_request" && v["requestId"] != request_id.as_str())
        .await;
    assert_ne!(
        second["requestId"].as_str().unwrap(),
        request_id,
        "resume issues a fresh request id"
    );

    client.close().await;
    server
        .req("POST", &format!("/api/claws/{id}/stop"), None)
        .await;
}

#[tokio::test]
async fn e26_chat_keeps_ask_policy_beside_auto_approve_claw() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[]).await;

    // The claw answers its own permissions…
    let mut input = claw_input("robot", "mock-permission", &project);
    input["schedules"] = json!([every_second(json!(["go"]))]);
    let created = create_claw(&server, input).await;
    let claw_id = created["id"].as_str().unwrap().to_string();
    server
        .req("POST", &format!("/api/claws/{claw_id}/start"), None)
        .await;

    // …while a plain chat connection on a DIFFERENT connection keeps ask-via-ui.
    server
        .req(
            "POST",
            "/api/acp/spawn",
            Some(json!({ "agentId": "mock-permission", "projectId": project })),
        )
        .await;
    let (_, acp) = server.req("GET", "/api/acp", None).await;
    let entries = acp.as_array().unwrap();
    assert!(
        entries.len() >= 2,
        "chat spawn and claw spawn are separate connections: {acp}"
    );
    let chat = entries
        .iter()
        .find(|c| c["id"] != format!("claw:{claw_id}"))
        .unwrap();
    let chat_id = chat["id"].as_str().unwrap().to_string();
    // The listing carries session *ids*, not a single sessionId — open one on
    // the chat connection the same way the UI does.
    let (status, session) = server
        .req(
            "POST",
            &format!("/api/projects/{project}/sessions"),
            Some(json!({ "connectionId": chat_id })),
        )
        .await;
    assert_eq!(status, TStatus::CREATED, "{session}");
    let chat_session = session["id"].as_str().unwrap().to_string();
    let mut chat_client = server.connect_ws(&chat_id, &chat_session, Some(0)).await;

    chat_client
        .send(json!({ "type": "prompt", "text": "go" }))
        .await;
    let request = chat_client
        .wait_for(|v| v["type"] == "permission_request")
        .await;
    let _request_id = request["requestId"].as_str().unwrap();

    // Nobody answers on purpose — the chat side parks forever until a human or
    // the timeout, proving the claw's auto_approve did not leak into chat.
    tokio::time::sleep(Duration::from_millis(500)).await;
    chat_client.drain().await;
    assert_eq!(
        chat_client.all("permission_resolved").len(),
        0,
        "chat permission must stay parked"
    );

    // Meanwhile the claw self-served.
    let mut claw_client = claw_socket(&server, &claw_id).await;
    claw_client.wait_for(|v| v["type"] == "turn_complete").await;
    assert!(claw_client.message_text().contains("selected:allow"));

    chat_client.close().await;
    claw_client.close().await;
    server
        .req("POST", &format!("/api/claws/{claw_id}/stop"), None)
        .await;
}

#[tokio::test]
async fn e28_skills_route_reports_frontmatter_fields() {
    let mut server = TestServer::start().await;
    const GOOD: &str = "---\n\
description: Review pull requests\n\
license: MIT\n\
compatibility: auggie\n\
allowedTools: Read, Glob\n\
metadata:\n  team: core\n\
---\n\
You are a code review agent.\n";
    let (project, _) = server
        .fixture_project_with_skills(&[(".claude/skills/review-pr/SKILL.md", GOOD)])
        .await;

    let (status, list) = server
        .req("GET", &format!("/api/projects/{project}/skills"), None)
        .await;
    assert_eq!(status, TStatus::OK);
    // Discovery also walks the real $HOME directories, so the array can carry
    // ambient skills — assert on the fixture row, not on the total.
    let s = list
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "review-pr")
        .expect("fixture skill must be discovered: {list}");
    assert_eq!(s["source"], "workspace");
    assert_eq!(s["description"], "Review pull requests");
    assert_eq!(s["license"], "MIT");
    assert_eq!(s["compatibility"], "auggie");
    assert_eq!(s["allowedTools"], "Read, Glob");
    assert_eq!(
        s["metadata"]["team"], "core",
        "metadata stays structured JSON"
    );
    assert_eq!(s["prompt"], "You are a code review agent.");
}

#[tokio::test]
async fn e29_skill_without_frontmatter_is_listed_with_null_fields() {
    let mut server = TestServer::start().await;
    let (project, _) = server
        .fixture_project_with_skills(&[(".claude/skills/plain/SKILL.md", "Just do the thing.\n")])
        .await;

    let (status, list) = server
        .req("GET", &format!("/api/projects/{project}/skills"), None)
        .await;
    assert_eq!(status, TStatus::OK);
    // Ambient $HOME skills may share the array; find the fixture row by name.
    let s = list
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "plain")
        .expect("fixture skill must be discovered: {list}");
    assert_eq!(s["description"], Value::Null);
    assert_eq!(s["license"], Value::Null);
    assert_eq!(s["prompt"], "Just do the thing.");
}

// ---- E30–E33: skills edge cases ----------------------------------------------

#[tokio::test]
async fn e30_broken_skill_is_skipped_not_500() {
    let mut server = TestServer::start().await;
    const GOOD: &str = "---\ndescription: fine\n---\nbody\n";
    let (project, _) = server
        .fixture_project_with_skills(&[
            (
                ".claude/skills/broken/SKILL.md",
                "---\ndescription: [unclosed\n---\nx\n",
            ),
            (".claude/skills/good-one/SKILL.md", GOOD),
        ])
        .await;

    // The route must answer 200 with the survivors — one bad file never takes
    // down the whole dropdown. Ambient $HOME skills may join, so filter to the
    // two fixture names before asserting on the outcome.
    let (status, list) = server
        .req("GET", &format!("/api/projects/{project}/skills"), None)
        .await;
    assert_eq!(status, TStatus::OK);
    let fixture: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["name"].as_str())
        .filter(|n| *n == "broken" || *n == "good-one")
        .collect();
    assert_eq!(fixture, vec!["good-one"], "{list}");
}

#[tokio::test]
async fn e31_skills_of_unknown_project_404() {
    let server = TestServer::start().await;
    let (status, body) = server
        .req("GET", "/api/projects/no-such-project/skills", None)
        .await;
    assert_eq!(status, TStatus::NOT_FOUND);
    assert_eq!(body["error"], "project");
}

// ---- E34–E38: scheduling for real --------------------------------------------

#[tokio::test]
async fn e34_schedule_fires_prompts_for_real() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[]).await;

    let mut input = claw_input("clockwork", "mock-chunks", &project);
    input["schedules"] = json!([every_second(json!(["tick"]))]);
    let created = create_claw(&server, input).await;
    let id = created["id"].as_str().unwrap().to_string();

    server
        .req("POST", &format!("/api/claws/{id}/start"), None)
        .await;
    let mut client = claw_socket(&server, &id).await;

    // Three turns in 15s is the floor — a slower box may deliver more, never
    // fewer unless the scheduler is broken.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while client.all("turn_complete").len() < 3 {
        let frame = tokio::time::timeout_at(deadline, client.socket.next())
            .await
            .expect("timed out waiting for 3 scheduled turns")
            .expect("socket closed")
            .expect("websocket error");
        if let TMessage::Text(text) = frame {
            let value: Value = serde_json::from_str(&text).unwrap();
            client.seen.push(value.clone());
        }
    }
    assert!(
        !client.message_text().is_empty(),
        "the schedule's prompt text came back as chunks"
    );

    drop(client);
    let row = wait_status(
        &server,
        &id,
        |v| v["status"]["lastRunAt"].is_string(),
        "lastRunAt",
    )
    .await;
    assert!(row["status"]["lastRunAt"].is_string());

    server
        .req("POST", &format!("/api/claws/{id}/stop"), None)
        .await;
}

#[tokio::test]
async fn e35_busy_tick_is_skipped_when_skip_if_running() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[]).await;

    // `slow` holds each turn open long enough that several ticks land mid-turn.
    let mut input = claw_input("patient", "mock-slow", &project);
    input["schedules"] = json!([every_second(json!(["go"]))]);
    input["skipIfRunning"] = json!(true);
    let created = create_claw(&server, input).await;
    let id = created["id"].as_str().unwrap().to_string();

    server
        .req("POST", &format!("/api/claws/{id}/start"), None)
        .await;
    wait_status(
        &server,
        &id,
        |v| v["status"]["state"] != "starting",
        "start",
    )
    .await;

    // Give it a few ticks; then nothing may have gone wrong and no restart may
    // have happened — a skipped tick is silent by design.
    tokio::time::sleep(Duration::from_secs(4)).await;
    let (_, row) = server.req("GET", &format!("/api/claws/{id}"), None).await;
    assert_eq!(row["status"]["restarts"], 0, "{row}");
    assert_eq!(row["status"]["lastError"], Value::Null);

    server
        .req("POST", &format!("/api/claws/{id}/stop"), None)
        .await;
}

#[tokio::test]
async fn e36_disabled_claw_never_fires() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[]).await;

    let mut input = claw_input("dormant", "mock-chunks", &project);
    input["enabled"] = json!(false);
    input["schedules"] = json!([every_second(json!(["never"]))]);
    let created = create_claw(&server, input).await;
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["enabled"], false);
    assert_eq!(
        created["status"]["nextRunAt"],
        Value::Null,
        "a disabled claw has no next run"
    );

    // Starting still works — enabled only gates triggers.
    server
        .req("POST", &format!("/api/claws/{id}/start"), None)
        .await;
    let row = wait_status(
        &server,
        &id,
        |v| v["status"]["connectionId"].is_string(),
        "start",
    )
    .await;
    assert!(row["status"]["sessionId"].is_string());

    tokio::time::sleep(Duration::from_secs(3)).await;
    let (_, row) = server.req("GET", &format!("/api/claws/{id}"), None).await;
    assert_eq!(
        row["status"]["lastRunAt"],
        Value::Null,
        "disabled claw must not have run"
    );
    assert_eq!(row["status"]["nextRunAt"], Value::Null);

    server
        .req("POST", &format!("/api/claws/{id}/stop"), None)
        .await;
}

// ---- E37–E42: schedule selection, keep-alive, cascade, token gate -------------

#[tokio::test]
async fn e38_disabled_schedule_is_excluded_from_next_run() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[]).await;

    // A: every schedule disabled → no next run at all.
    let mut input = claw_input("all-off", "mock-chunks", &project);
    input["schedules"] = json!([{
        "cron": "* * * * * *",
        "prompts": ["x"],
        "enabled": false,
    }]);
    let created = create_claw(&server, input).await;
    assert_eq!(created["status"]["nextRunAt"], Value::Null, "{created}");
    assert_eq!(
        created["status"]["scheduleCount"], 0,
        "scheduleCount counts enabled schedules only"
    );

    // B: one disabled every-second + one enabled annual — the next run must
    // come from the enabled one alone.
    let mut input = claw_input("mixed", "mock-chunks", &project);
    input["schedules"] = json!([
        { "cron": "* * * * * *", "prompts": ["x"], "enabled": false },
        { "cron": "0 0 1 1 *", "prompts": ["new year"], "enabled": true },
    ]);
    let created = create_claw(&server, input).await;
    assert_eq!(created["status"]["scheduleCount"], 1);
    let next = created["status"]["nextRunAt"]
        .as_str()
        .expect("nextRunAt set");
    assert!(
        next.starts_with("2027-01-01"),
        "annual cron is the only live schedule; got {next}"
    );
}

#[tokio::test]
async fn e40_keep_alive_gives_up_after_three_restarts() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[]).await;

    let mut input = claw_input("phoenix", "mock-die_after_handshake", &project);
    input["keepAlive"] = json!(true);
    input["schedules"] = json!([every_second(json!(["go"]))]);
    let created = create_claw(&server, input).await;
    let id = created["id"].as_str().unwrap().to_string();

    server
        .req("POST", &format!("/api/claws/{id}/start"), None)
        .await;

    // Timeline: fire ~0s, die, backoff 1s, die, backoff 2s, die, backoff 4s,
    // die, give up ≈ 12–13s. Poll to 45s so a loaded box still passes.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    loop {
        let (_, row) = server.req("GET", &format!("/api/claws/{id}"), None).await;
        if row["status"]["state"] == "error" && row["status"]["restarts"] == 3 {
            let err = row["status"]["lastError"].as_str().unwrap_or("");
            assert!(
                err.contains("giving up after 3"),
                "the give-up message names the cap: {err}"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "keepAlive never gave up: {}",
            row["status"]
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    server
        .req("POST", &format!("/api/claws/{id}/stop"), None)
        .await;
}

#[tokio::test]
async fn e41_without_keep_alive_death_is_immediate_error() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[]).await;

    let mut input = claw_input("fragile", "mock-die_after_handshake", &project);
    input["keepAlive"] = json!(false);
    input["schedules"] = json!([every_second(json!(["go"]))]);
    let created = create_claw(&server, input).await;
    let id = created["id"].as_str().unwrap().to_string();

    server
        .req("POST", &format!("/api/claws/{id}/start"), None)
        .await;

    // No retries: the first death is final, well inside the keep-alive test's
    // own backoff window.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let (_, row) = server.req("GET", &format!("/api/claws/{id}"), None).await;
        if row["status"]["state"] == "error" {
            assert_eq!(
                row["status"]["restarts"], 0,
                "no restart without keepAlive: {row}"
            );
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "death never surfaced: {}",
            row["status"]
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

#[tokio::test]
async fn e42_deleting_project_takes_its_claws_down() {
    let mut server = TestServer::start().await;
    let (project, _) = server.fixture_project(&[]).await;

    let created = create_claw(&server, claw_input("hostage", "mock-slow", &project)).await;
    let id = created["id"].as_str().unwrap().to_string();
    server
        .req("POST", &format!("/api/claws/{id}/start"), None)
        .await;
    wait_status(
        &server,
        &id,
        |v| v["status"]["state"] != "starting",
        "start",
    )
    .await;

    let (status, _) = server
        .req("DELETE", &format!("/api/projects/{project}"), None)
        .await;
    assert_eq!(status, TStatus::NO_CONTENT);

    // The cascade must have removed the definition AND stopped its connection.
    let deadline = tokio::time::Instant::now() + TIMEOUT;
    loop {
        let (status, _) = server.req("GET", &format!("/api/claws/{id}"), None).await;
        let (_, acp) = server.req("GET", "/api/acp", None).await;
        let gone = status == TStatus::NOT_FOUND;
        let conn_down = !acp
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == format!("claw:{id}"));
        if gone && conn_down {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "project delete left claw behind: gone={gone} conn_down={conn_down}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn e43_every_new_route_requires_the_token() {
    let server = TestServer::start().await;

    let paths: Vec<(&str, &str)> = vec![
        ("GET", "/api/claws"),
        ("POST", "/api/claws"),
        ("GET", "/api/claws/some-id"),
        ("PUT", "/api/claws/some-id"),
        ("DELETE", "/api/claws/some-id"),
        ("POST", "/api/claws/some-id/start"),
        ("POST", "/api/claws/some-id/stop"),
        ("GET", "/api/projects/some-id/skills"),
    ];
    for (method, path) in paths {
        let body = if method == "POST" || method == "PUT" {
            Some(json!({}))
        } else {
            None
        };
        let (status, resp) = server
            .client
            .request_raw(method, &server.url(path), None, body)
            .await;
        assert_eq!(status, TStatus::UNAUTHORIZED, "{method} {path}: {resp}");
    }
}
