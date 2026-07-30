//! Phase 2 integration tests — SPEC-002 (projects, file tree, files, settings).
//!
//! Same shape as phase1: a real server on an ephemeral port, because the auth
//! and Origin layers plus the spawn_blocking seams are exactly what an
//! in-process router test wouldn't exercise.
//!
//! Each test gets its own data dir (via `AppState::with_data_dir`) and its own
//! project fixture dir, so tests are independent and machine-global gitignore
//! state can't leak in.

use serde_json::{Value, json};
use spec_ade_server::{AppState, build_router};
use tokio_tungstenite::tungstenite::http::StatusCode as TStatus;

struct TestServer {
    addr: std::net::SocketAddr,
    token: String,
    client: reqwest_lite::Client,
    /// Temp dirs to remove when the test ends.
    cleanup: Vec<std::path::PathBuf>,
}

impl TestServer {
    async fn start() -> Self {
        let token = format!("tok-{}", uuid::Uuid::new_v4());
        let data_dir = std::env::temp_dir().join(format!("spec-ade-p2-{}", uuid::Uuid::new_v4()));
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

    /// Create a project fixture dir with the given files and register it.
    ///
    /// `files` maps relative path → content; parent dirs are created. Returns
    /// `(project_id, fixture_root)`.
    async fn fixture_project(&mut self, files: &[(&str, &str)]) -> (String, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("spec-ade-fix-{}", uuid::Uuid::new_v4()));
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
        // The canonical root (macOS: /tmp → /private/tmp).
        let canonical = root.canonicalize().unwrap();
        (body["id"].as_str().unwrap().to_string(), canonical)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        for dir in &self.cleanup {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

// ---- project CRUD ----------------------------------------------------------

#[tokio::test]
async fn project_crud_roundtrip_and_persistence() {
    let mut server = TestServer::start().await;
    let (id, root) = server.fixture_project(&[("a.txt", "hello")]).await;

    // List includes it with the canonical path.
    let (status, list) = server.req("GET", "/api/projects", None).await;
    assert_eq!(status, TStatus::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], id.as_str());
    assert_eq!(list[0]["path"], root.display().to_string());

    // Update: set name+icon; null icon clears it; absent fields keep.
    let (status, updated) = server
        .req(
            "PUT",
            &format!("/api/projects/{id}"),
            Some(json!({ "name": "renamed", "icon": "🦀" })),
        )
        .await;
    assert_eq!(status, TStatus::OK);
    assert_eq!(updated["name"], "renamed");
    assert_eq!(updated["icon"], "🦀");

    let (_, updated) = server
        .req(
            "PUT",
            &format!("/api/projects/{id}"),
            Some(json!({ "icon": null })),
        )
        .await;
    assert_eq!(updated["icon"], Value::Null);
    assert_eq!(updated["name"], "renamed", "absent field must be kept");

    // Persistence: the settings.json on disk carries the project.
    let (_, list) = server.req("GET", "/api/projects", None).await;
    assert_eq!(list[0]["name"], "renamed");

    // Delete → gone; second delete → 404.
    let (status, _) = server
        .req("DELETE", &format!("/api/projects/{id}"), None)
        .await;
    assert_eq!(status, TStatus::NO_CONTENT);
    let (_, list) = server.req("GET", "/api/projects", None).await;
    assert!(list.as_array().unwrap().is_empty());
    let (status, _) = server
        .req("DELETE", &format!("/api/projects/{id}"), None)
        .await;
    assert_eq!(status, TStatus::NOT_FOUND);
}

#[tokio::test]
async fn project_rejects_missing_file_and_duplicate_paths() {
    let mut server = TestServer::start().await;

    let (status, _) = server
        .req(
            "POST",
            "/api/projects",
            Some(json!({ "path": "/no/such/dir" })),
        )
        .await;
    assert_eq!(status, TStatus::BAD_REQUEST);

    // A file is not a directory.
    let (status, _) = server
        .req(
            "POST",
            "/api/projects",
            Some(json!({ "path": "/etc/hosts" })),
        )
        .await;
    assert_eq!(status, TStatus::BAD_REQUEST);

    // Duplicate (registered via its canonical path, re-posted uncanonicalized).
    let (id, root) = server.fixture_project(&[("x", "y")]).await;
    let (status, body) = server
        .req(
            "POST",
            "/api/projects",
            Some(json!({ "path": root.display().to_string() })),
        )
        .await;
    assert_eq!(status, TStatus::CONFLICT, "body: {body}");
    assert_eq!(body["existingId"], id.as_str());
}

// ---- tree ------------------------------------------------------------------

#[tokio::test]
async fn tree_lists_direct_children_sorted_dirs_first() {
    let mut server = TestServer::start().await;
    let (id, _root) = server
        .fixture_project(&[
            ("zed.txt", ""),
            ("Apple.txt", ""),
            ("dir_b/inner.txt", ""),
            ("dir_a/inner.txt", ""),
        ])
        .await;

    let (status, tree) = server
        .req("GET", &format!("/api/projects/{id}/tree"), None)
        .await;
    assert_eq!(status, TStatus::OK);
    let names: Vec<&str> = tree["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["dir_a", "dir_b", "Apple.txt", "zed.txt"]);
    assert_eq!(tree["truncated"], false);

    // Listing a subdirectory: paths are root-relative.
    let (_, sub) = server
        .req("GET", &format!("/api/projects/{id}/tree?path=dir_a"), None)
        .await;
    assert_eq!(sub["entries"][0]["path"], "dir_a/inner.txt");

    // A file path is a 400; a missing one is a 404.
    let (status, _) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/tree?path=zed.txt"),
            None,
        )
        .await;
    assert_eq!(status, TStatus::BAD_REQUEST);
    let (status, _) = server
        .req("GET", &format!("/api/projects/{id}/tree?path=nope"), None)
        .await;
    assert_eq!(status, TStatus::NOT_FOUND);
}

#[tokio::test]
async fn root_gitignore_applies_to_nested_dirs_and_hidden_files_show() {
    let mut server = TestServer::start().await;
    // Patterns are deliberately odd (`*.adelog`, `adedist/`) instead of the
    // obvious `*.log` / `dist/`: the walker runs with `require_git(false)` and
    // `git_global(true)`, so a host whose global excludes file lists `*.log`
    // would make the "must be ignored" assertions pass without the fixture's
    // own `.gitignore` doing anything (SPEC-002 §9 — no dependence on the host's
    // global gitignore).
    let (id, _root) = server
        .fixture_project(&[
            (".gitignore", "adedist/\n*.adelog\n"),
            (".env", "SECRET=1"),
            ("src/app.adelog", "ignored"),
            ("src/keep.txt", "kept"),
            ("src/adedist/bundle.js", "ignored"),
        ])
        .await;

    // Root listing shows hidden files (.env, .gitignore itself).
    let (_, tree) = server
        .req("GET", &format!("/api/projects/{id}/tree"), None)
        .await;
    let names: Vec<&str> = tree["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&".env"), "hidden files must show: {names:?}");
    assert!(names.contains(&".gitignore"));

    // The nested listing respects the ROOT .gitignore (parents(true)).
    let (_, sub) = server
        .req("GET", &format!("/api/projects/{id}/tree?path=src"), None)
        .await;
    let names: Vec<&str> = sub["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"keep.txt"));
    assert!(
        !names.contains(&"app.adelog"),
        "*.adelog must be ignored: {names:?}"
    );
    assert!(
        !names.contains(&"adedist"),
        "adedist/ must be ignored: {names:?}"
    );
}

#[tokio::test]
async fn git_and_node_modules_are_excluded_even_without_gitignore() {
    let mut server = TestServer::start().await;
    // Note: NO .gitignore in this fixture — the exclusion must be unconditional.
    let (id, _root) = server
        .fixture_project(&[
            (".git/config", "[core]"),
            ("node_modules/pkg/index.js", "x"),
            ("real.txt", "y"),
        ])
        .await;

    let (_, tree) = server
        .req("GET", &format!("/api/projects/{id}/tree"), None)
        .await;
    let names: Vec<&str> = tree["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["real.txt"], "got: {names:?}");
}

// ---- file read/write -------------------------------------------------------

#[tokio::test]
async fn read_write_roundtrip_with_rev() {
    let mut server = TestServer::start().await;
    let (id, root) = server.fixture_project(&[("f.txt", "original")]).await;

    let (status, read) = server
        .req("GET", &format!("/api/projects/{id}/file?path=f.txt"), None)
        .await;
    assert_eq!(status, TStatus::OK);
    assert_eq!(read["kind"], "text");
    assert_eq!(read["content"], "original");
    assert_eq!(read["eol"], "lf");
    let rev = read["rev"].as_str().unwrap().to_string();

    // Write with the current rev succeeds and bumps the rev.
    let (status, written) = server
        .req(
            "PUT",
            &format!("/api/projects/{id}/file?path=f.txt"),
            Some(json!({ "content": "updated", "rev": rev })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "write: {written}");
    assert_ne!(written["rev"].as_str().unwrap(), rev);
    assert_eq!(
        std::fs::read_to_string(root.join("f.txt")).unwrap(),
        "updated"
    );
}

#[tokio::test]
async fn stale_rev_is_rejected_and_disk_untouched() {
    let mut server = TestServer::start().await;
    let (id, root) = server.fixture_project(&[("f.txt", "v1")]).await;

    let (_, read) = server
        .req("GET", &format!("/api/projects/{id}/file?path=f.txt"), None)
        .await;
    let rev = read["rev"].as_str().unwrap().to_string();

    // External edit lands (content of a different length so the rev must change).
    std::fs::write(root.join("f.txt"), "externally-changed").unwrap();

    let (status, body) = server
        .req(
            "PUT",
            &format!("/api/projects/{id}/file?path=f.txt"),
            Some(json!({ "content": "clobber", "rev": rev })),
        )
        .await;
    assert_eq!(status, TStatus::CONFLICT, "body: {body}");
    assert!(body["currentRev"].is_string());
    assert_eq!(
        std::fs::read_to_string(root.join("f.txt")).unwrap(),
        "externally-changed",
        "a refused write must not touch the disk"
    );

    // Force overwrite (no rev) is the explicit escape hatch.
    let (status, _) = server
        .req(
            "PUT",
            &format!("/api/projects/{id}/file?path=f.txt"),
            Some(json!({ "content": "forced" })),
        )
        .await;
    assert_eq!(status, TStatus::OK);
    assert_eq!(
        std::fs::read_to_string(root.join("f.txt")).unwrap(),
        "forced"
    );
}

#[tokio::test]
async fn put_to_missing_path_is_404_and_binary_large_report_kinds() {
    let mut server = TestServer::start().await;
    let (id, root) = server.fixture_project(&[]).await;

    let (status, _) = server
        .req(
            "PUT",
            &format!("/api/projects/{id}/file?path=ghost.txt"),
            Some(json!({ "content": "x" })),
        )
        .await;
    assert_eq!(status, TStatus::NOT_FOUND, "creation goes through /entries");

    // Binary file: metadata only, no content field.
    std::fs::write(root.join("blob.png"), [0x89u8, 0x50, 0x00, 0xFF]).unwrap();
    let (status, body) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/file?path=blob.png"),
            None,
        )
        .await;
    assert_eq!(status, TStatus::OK);
    assert_eq!(body["kind"], "binary");
    assert_eq!(body["mime"], "image/png");
    assert!(
        body.get("content").is_none(),
        "binary must not carry content"
    );

    // Oversized file: kind tooLarge, correct size, no content.
    let big = vec![b'a'; (spec_ade_server::files::FILE_TEXT_MAX + 1) as usize];
    std::fs::write(root.join("huge.txt"), &big).unwrap();
    let (_, body) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/file?path=huge.txt"),
            None,
        )
        .await;
    assert_eq!(body["kind"], "tooLarge");
    assert_eq!(body["size"].as_u64().unwrap(), big.len() as u64);
    assert!(body.get("content").is_none());
}

#[tokio::test]
async fn crlf_bom_files_round_trip_byte_for_byte() {
    let mut server = TestServer::start().await;
    let (id, root) = server.fixture_project(&[]).await;
    let original: &[u8] = b"\xEF\xBB\xBFwindows line\r\nsecond\r\n";
    std::fs::write(root.join("crlf.txt"), original).unwrap();

    let (_, read) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/file?path=crlf.txt"),
            None,
        )
        .await;
    assert_eq!(read["kind"], "text");
    assert_eq!(read["eol"], "crlf");

    // Echo the content straight back (an unedited save) — bytes must not move.
    let (status, _) = server
        .req(
            "PUT",
            &format!("/api/projects/{id}/file?path=crlf.txt"),
            Some(json!({
                "content": read["content"],
                "rev": read["rev"],
            })),
        )
        .await;
    assert_eq!(status, TStatus::OK);
    assert_eq!(std::fs::read(root.join("crlf.txt")).unwrap(), original);
}

#[tokio::test]
async fn traversal_and_symlink_escape_are_refused() {
    let mut server = TestServer::start().await;
    let (id, root) = server.fixture_project(&[("inside.txt", "x")]).await;

    for bad in ["../outside.txt", "/etc/passwd", "a/../../b", "./x"] {
        let encoded = bad.replace('/', "%2F");
        let (status, body) = server
            .req(
                "GET",
                &format!("/api/projects/{id}/file?path={encoded}"),
                None,
            )
            .await;
        assert_eq!(status, TStatus::BAD_REQUEST, "path {bad:?}: {body}");
    }

    // Symlink pointing out of the root: 403, and unreadable through every route.
    let outside = std::env::temp_dir().join(format!("spec-ade-out-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "s3cret").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("evil")).unwrap();

    let (status, _) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/file?path=evil%2Fsecret.txt"),
            None,
        )
        .await;
    assert_eq!(status, TStatus::FORBIDDEN);

    let _ = std::fs::remove_dir_all(&outside);
}

// ---- entries ---------------------------------------------------------------

#[tokio::test]
async fn create_rename_delete_entries() {
    let mut server = TestServer::start().await;
    let (id, root) = server.fixture_project(&[("keep.txt", "")]).await;

    // Create a file and a dir.
    let (status, _) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/entries"),
            Some(json!({ "path": "new.txt", "kind": "file" })),
        )
        .await;
    assert_eq!(status, TStatus::CREATED);
    assert!(root.join("new.txt").is_file());

    let (status, _) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/entries"),
            Some(json!({ "path": "subdir", "kind": "dir" })),
        )
        .await;
    assert_eq!(status, TStatus::CREATED);
    assert!(root.join("subdir").is_dir());

    // Existing target → 409; missing parent → 404; root → 400.
    let (status, _) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/entries"),
            Some(json!({ "path": "keep.txt", "kind": "file" })),
        )
        .await;
    assert_eq!(status, TStatus::CONFLICT);
    let (status, _) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/entries"),
            Some(json!({ "path": "ghost/child.txt", "kind": "file" })),
        )
        .await;
    assert_eq!(status, TStatus::NOT_FOUND);
    let (status, _) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/entries"),
            Some(json!({ "path": "", "kind": "dir" })),
        )
        .await;
    assert_eq!(status, TStatus::BAD_REQUEST);

    // Rename; renaming onto an existing target → 409.
    let (status, _) = server
        .req(
            "PATCH",
            &format!("/api/projects/{id}/entries?path=new.txt"),
            Some(json!({ "newPath": "renamed.txt" })),
        )
        .await;
    assert_eq!(status, TStatus::OK);
    assert!(!root.join("new.txt").exists());
    assert!(root.join("renamed.txt").is_file());
    let (status, _) = server
        .req(
            "PATCH",
            &format!("/api/projects/{id}/entries?path=renamed.txt"),
            Some(json!({ "newPath": "keep.txt" })),
        )
        .await;
    assert_eq!(status, TStatus::CONFLICT);

    // Delete: non-empty dir needs recursive.
    std::fs::write(root.join("subdir/child.txt"), "x").unwrap();
    let (status, _) = server
        .req(
            "DELETE",
            &format!("/api/projects/{id}/entries?path=subdir"),
            None,
        )
        .await;
    assert_eq!(status, TStatus::CONFLICT);
    let (status, _) = server
        .req(
            "DELETE",
            &format!("/api/projects/{id}/entries?path=subdir&recursive=true"),
            None,
        )
        .await;
    assert_eq!(status, TStatus::NO_CONTENT);
    assert!(!root.join("subdir").exists());
}

// ---- settings --------------------------------------------------------------

#[tokio::test]
async fn settings_partial_update_semantics_and_persistence() {
    let server = TestServer::start().await;

    let (status, settings) = server.req("GET", "/api/settings", None).await;
    assert_eq!(status, TStatus::OK);
    assert_eq!(settings["editor"]["tabSize"], 2);
    assert!(
        settings.get("authToken").is_none() && settings.get("auth_token").is_none(),
        "token must never be exposed: {settings}"
    );

    // Set one field; others keep their values.
    let (status, updated) = server
        .req(
            "PUT",
            "/api/settings",
            Some(json!({ "editor": { "tabSize": 4 } })),
        )
        .await;
    assert_eq!(status, TStatus::OK);
    assert_eq!(updated["editor"]["tabSize"], 4);
    assert_eq!(updated["editor"]["fontSize"], 14, "absent field must keep");

    // null → back to default.
    let (_, updated) = server
        .req(
            "PUT",
            "/api/settings",
            Some(json!({ "editor": { "tabSize": null } })),
        )
        .await;
    assert_eq!(updated["editor"]["tabSize"], 2);

    // Out of range → 400 and nothing changes.
    let (status, _) = server
        .req(
            "PUT",
            "/api/settings",
            Some(json!({ "editor": { "fontSize": 100 } })),
        )
        .await;
    assert_eq!(status, TStatus::BAD_REQUEST);
    let (_, after) = server.req("GET", "/api/settings", None).await;
    assert_eq!(after["editor"]["fontSize"], 14);

    // Unknown editor key → 400; forbidden top-level keys → 403.
    let (status, _) = server
        .req(
            "PUT",
            "/api/settings",
            Some(json!({ "editor": { "theme": "light" } })),
        )
        .await;
    assert_eq!(status, TStatus::BAD_REQUEST);
    for forbidden in [json!({ "authToken": "hax" }), json!({ "projects": [] })] {
        let (status, _) = server.req("PUT", "/api/settings", Some(forbidden)).await;
        assert_eq!(status, TStatus::FORBIDDEN);
    }
}

// ---- gates -----------------------------------------------------------------

#[tokio::test]
async fn new_routes_require_token_and_origin() {
    let server = TestServer::start().await;

    // No token → 401 on every new surface.
    for path in ["/api/projects", "/api/settings"] {
        let (status, _) = server
            .client
            .request_raw("GET", &server.url(path), None, None)
            .await;
        assert_eq!(status, TStatus::UNAUTHORIZED, "{path} must demand a token");
    }

    // Hostile Origin → 403 even with a valid token.
    let (status, _) = server
        .client
        .request_with_origin(
            "GET",
            &server.url("/api/projects"),
            &server.token,
            "http://evil.example.com",
        )
        .await;
    assert_eq!(status, TStatus::FORBIDDEN);
}

// ---- minimal HTTP client (same rationale as phase1) ------------------------

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

        pub async fn request_with_origin(
            &self,
            method: &str,
            url: &str,
            token: &str,
            origin: &str,
        ) -> (StatusCode, Value) {
            self.send(method, url, Some(token), Some(origin), None)
                .await
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
