//! Layout persistence integration tests — SPEC-008 §3.3 / §8.1.
//!
//! Driven through the real HTTP surface. The server treats a pane tree as
//! OPAQUE JSON, so the assertions here are deliberately structure-blind: a tree
//! carries a nonsense nested field and we prove it survives a round-trip
//! byte-for-byte, never that the server understood it. The two guards that are
//! NOT opaque — the 256 KiB cap and the registered-project-key check — get their
//! own tests, as does the token gate every `/api/*` route must sit behind.

use serde_json::{Value, json};
use spec_ade_server::{AppState, build_router};
use std::path::PathBuf;
use tokio_tungstenite::tungstenite::http::StatusCode as TStatus;

struct TestServer {
    addr: std::net::SocketAddr,
    token: String,
    client: reqwest_lite::Client,
    data_dir: PathBuf,
    cleanup: Vec<PathBuf>,
}

impl TestServer {
    async fn start() -> Self {
        let token = format!("tok-{}", uuid::Uuid::new_v4());
        let data_dir = std::env::temp_dir().join(format!("spec-ade-s8-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).unwrap();

        let state = AppState::with_data_dir(token.clone(), data_dir.clone());
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

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    async fn req(&self, method: &str, path: &str, body: Option<Value>) -> (TStatus, Value) {
        self.client
            .request(method, &self.url(path), &self.token, body)
            .await
    }

    async fn req_anon(&self, method: &str, path: &str) -> TStatus {
        self.client
            .request_raw(method, &self.url(path), None, None)
            .await
            .0
    }

    async fn register(&mut self, root: &std::path::Path) -> String {
        let (status, body) = self
            .req(
                "POST",
                "/api/projects",
                Some(json!({ "path": root.display().to_string() })),
            )
            .await;
        assert_eq!(status, TStatus::CREATED, "register failed: {body}");
        self.cleanup.push(root.to_path_buf());
        body["id"].as_str().unwrap().to_string()
    }

    /// A registered project rooted at a fresh temp dir.
    async fn project(&mut self) -> String {
        let root = std::env::temp_dir().join(format!("spec-ade-s8-proj-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        self.register(&root).await
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        for dir in &self.cleanup {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// A pane tree with a nonsense nested field: if the server ever parsed the
/// grammar this key would be dropped, so its survival proves opacity.
fn sample_tree() -> Value {
    json!({
        "kind": "split",
        "direction": "horizontal",
        "ratio": 0.42,
        "first": { "kind": "leaf", "id": "L1", "tabs": [], "activeTabId": null },
        "second": { "kind": "leaf", "id": "L2", "tabs": [], "activeTabId": null },
        "__opaque_probe__": { "server": "must", "not": ["parse", "this"] }
    })
}

#[tokio::test]
async fn layout_defaults_are_empty() {
    let server = TestServer::start().await;
    let (status, body) = server.req("GET", "/api/layout", None).await;
    assert_eq!(status, TStatus::OK);
    assert_eq!(body["projectLayouts"], json!({}));
    assert_eq!(body["lastLayout"], Value::Null);
    assert_eq!(body["layoutPresets"], json!([]));
}

#[tokio::test]
async fn put_then_get_round_trips_an_opaque_tree() {
    let mut server = TestServer::start().await;
    let id = server.project().await;
    let tree = sample_tree();

    let (status, put) = server
        .req(
            "PUT",
            "/api/layout",
            Some(json!({ "projectLayouts": { &id: tree } })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "put failed: {put}");
    // The PUT echoes the stored document.
    assert_eq!(put["projectLayouts"][&id], sample_tree());

    let (status, got) = server.req("GET", "/api/layout", None).await;
    assert_eq!(status, TStatus::OK);
    // Byte-for-byte survival, probe field included.
    assert_eq!(got["projectLayouts"][&id], sample_tree());
    assert_eq!(
        got["projectLayouts"][&id]["__opaque_probe__"]["not"],
        json!(["parse", "this"])
    );
}

#[tokio::test]
async fn top_level_merge_preserves_absent_fields() {
    let mut server = TestServer::start().await;
    let id = server.project().await;

    server
        .req(
            "PUT",
            "/api/layout",
            Some(json!({ "projectLayouts": { &id: sample_tree() } })),
        )
        .await;

    // A second PUT carrying only lastLayout must not wipe projectLayouts.
    let leaf = json!({ "kind": "leaf", "id": "solo", "tabs": [], "activeTabId": null });
    let (status, _) = server
        .req("PUT", "/api/layout", Some(json!({ "lastLayout": leaf })))
        .await;
    assert_eq!(status, TStatus::OK);

    let (_, got) = server.req("GET", "/api/layout", None).await;
    assert_eq!(
        got["projectLayouts"][&id],
        sample_tree(),
        "projectLayouts lost"
    );
    assert_eq!(got["lastLayout"]["id"], json!("solo"));
}

#[tokio::test]
async fn last_layout_null_clears_while_absent_keeps() {
    let server = TestServer::start().await;
    let leaf = json!({ "kind": "leaf", "id": "x", "tabs": [], "activeTabId": null });

    server
        .req("PUT", "/api/layout", Some(json!({ "lastLayout": leaf })))
        .await;
    // Absent lastLayout keeps the stored value.
    server
        .req("PUT", "/api/layout", Some(json!({ "layoutPresets": [] })))
        .await;
    let (_, kept) = server.req("GET", "/api/layout", None).await;
    assert_eq!(kept["lastLayout"]["id"], json!("x"), "absent should keep");

    // Explicit null clears it.
    let (status, _) = server
        .req(
            "PUT",
            "/api/layout",
            Some(json!({ "lastLayout": Value::Null })),
        )
        .await;
    assert_eq!(status, TStatus::OK);
    let (_, cleared) = server.req("GET", "/api/layout", None).await;
    assert_eq!(cleared["lastLayout"], Value::Null, "null should clear");
}

#[tokio::test]
async fn unknown_project_key_is_400_group_layout() {
    let server = TestServer::start().await;
    let (status, body) = server
        .req(
            "PUT",
            "/api/layout",
            Some(json!({ "projectLayouts": { "ghost-project": sample_tree() } })),
        )
        .await;
    assert_eq!(status, TStatus::BAD_REQUEST);
    assert_eq!(body["error"], json!("layout"));
    assert!(
        body["detail"].as_str().unwrap().contains("ghost-project"),
        "detail should name the offending key: {body}"
    );
}

#[tokio::test]
async fn oversized_body_is_400_group_layout() {
    let server = TestServer::start().await;
    // A quarter-megabyte-plus opaque string trips the cap before any parse or
    // key check — lastLayout is opaque, so this is otherwise a valid body.
    let huge = "x".repeat(300 * 1024);
    let (status, body) = server
        .req("PUT", "/api/layout", Some(json!({ "lastLayout": huge })))
        .await;
    assert_eq!(status, TStatus::BAD_REQUEST);
    assert_eq!(body["error"], json!("layout"));
    assert!(
        body["detail"].as_str().unwrap().contains("exceeds cap"),
        "detail should mention the cap: {body}"
    );
}

#[tokio::test]
async fn presets_persist_verbatim() {
    let server = TestServer::start().await;
    let presets = json!([
        { "name": "Side by side", "tree": sample_tree() },
        { "name": "Single", "tree": { "kind": "leaf", "id": "s", "tabs": [], "activeTabId": null } }
    ]);
    let (status, _) = server
        .req(
            "PUT",
            "/api/layout",
            Some(json!({ "layoutPresets": presets })),
        )
        .await;
    assert_eq!(status, TStatus::OK);
    let (_, got) = server.req("GET", "/api/layout", None).await;
    assert_eq!(got["layoutPresets"], presets);
}

#[tokio::test]
async fn deleting_a_project_cascades_its_layout() {
    let mut server = TestServer::start().await;
    let id = server.project().await;
    server
        .req(
            "PUT",
            "/api/layout",
            Some(json!({ "projectLayouts": { &id: sample_tree() } })),
        )
        .await;

    let (status, _) = server
        .req("DELETE", &format!("/api/projects/{id}"), None)
        .await;
    assert_eq!(status, TStatus::NO_CONTENT);

    let (_, got) = server.req("GET", "/api/layout", None).await;
    assert!(
        got["projectLayouts"].get(&id).is_none(),
        "layout for the deleted project should be gone: {got}"
    );
}

#[tokio::test]
async fn a_saved_layout_survives_a_reload_from_disk() {
    let server = TestServer::start().await;
    let leaf = json!({ "kind": "leaf", "id": "persisted", "tabs": [], "activeTabId": null });
    server
        .req("PUT", "/api/layout", Some(json!({ "lastLayout": leaf })))
        .await;

    // A fresh state over the same data dir reads settings.json back — proof the
    // PUT reached disk, not just RAM.
    let reloaded = AppState::with_data_dir(server.token.clone(), server.data_dir.clone());
    let snap = reloaded.settings.snapshot();
    assert_eq!(snap.last_layout.unwrap()["id"], json!("persisted"));
}

#[tokio::test]
async fn layout_requires_the_token() {
    let server = TestServer::start().await;
    // Both verbs of the new route sit behind the gate; an anon request never
    // reaches the handler (SPEC-002 §3.5 security invariant).
    assert_eq!(
        server.req_anon("GET", "/api/layout").await,
        TStatus::UNAUTHORIZED
    );
    assert_eq!(
        server.req_anon("PUT", "/api/layout").await,
        TStatus::UNAUTHORIZED
    );
}

// A raw-TCP HTTP client, inlined per test file (not a shared dep) so each
// integration test binary stays self-contained — same pattern as the sibling
// suites (projects-files.rs, search-monitor.rs, …).
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
            self.send(method, url, Some(token), body).await
        }

        pub async fn request_raw(
            &self,
            method: &str,
            url: &str,
            token: Option<&str>,
            body: Option<Value>,
        ) -> (StatusCode, Value) {
            self.send(method, url, token, body).await
        }

        async fn send(
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
                request.push_str(&format!("{TOKEN_HEADER}: {token}\r\n"));
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
