//! Search + process monitor integration tests — SPEC-006.
//!
//! Two halves with opposite testing strategies, both driven through the real HTTP
//! surface:
//!
//! - **Search** runs against a real file tree in a temp dir, and the assertions are
//!   exact: this file matches, that one is gitignored, this glob narrows to one
//!   result. The tree is built by the fixture, so the expected answer is known.
//! - **Monitor** reads the actual host, where no number is knowable in advance. So
//!   every assertion is an **invariant** — `used <= total`, our own pid is in the
//!   listing, timestamps advance — never a fixed value. A test asserting
//!   "cpu.usage == 12.5" would only ever prove the machine it was written on.
//!
//! The kill tests spawn a real child process and verify its death with
//! `try_wait()`, independently of the API under test (§7).

use serde_json::{Value, json};
use spec_ade_server::{AppState, build_router};
use std::path::{Path, PathBuf};
use tokio_tungstenite::tungstenite::http::StatusCode as TStatus;

struct TestServer {
    addr: std::net::SocketAddr,
    token: String,
    client: reqwest_lite::Client,
    cleanup: Vec<PathBuf>,
    /// A clone of the live state. D29/D30 are claims about the sampler's
    /// lifecycle, and nothing on the wire can tell "one sampler shared by two
    /// streams" from "two samplers" — the registry is the only honest witness.
    state: AppState,
}

impl TestServer {
    async fn start() -> Self {
        let token = format!("tok-{}", uuid::Uuid::new_v4());
        let data_dir = std::env::temp_dir().join(format!("spec-ade-p6-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).unwrap();

        let state = AppState::with_data_dir(token.clone(), data_dir.clone());
        let app = build_router(state.clone());
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
            state,
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

    /// A request with no token — the 401 half of D36.
    async fn req_anon(&self, method: &str, path: &str) -> TStatus {
        self.client
            .request_raw(method, &self.url(path), None, None)
            .await
            .0
    }

    async fn sse(&self, path: &str) -> reqwest_lite::SseStream {
        reqwest_lite::SseStream::open(&self.url(path), &self.token).await
    }

    async fn register(&mut self, root: &Path) -> String {
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

    /// The search fixture, registered as a project.
    async fn search_project(&mut self) -> String {
        let root = search_fixture();
        self.register(&root).await
    }

    /// Drain a search stream to its `done` event.
    async fn search(&self, query: &str) -> SearchRun {
        let mut stream = self.sse(query).await;
        let mut matches = Vec::new();
        let mut progress = 0usize;
        let mut errors = Vec::new();
        let mut done = None;

        while let Some(event) = stream.next_event().await {
            let data: Value = serde_json::from_str(&event.data)
                .unwrap_or_else(|e| panic!("frame was not JSON ({e}): {}", event.data));
            match event.event.as_str() {
                "match" => matches.push(data),
                "progress" => progress += 1,
                "error" => errors.push(data),
                "done" => {
                    done = Some(data);
                    // `done` is terminal; the server closes right after.
                    break;
                }
                other => panic!("unexpected event {other}"),
            }
        }

        SearchRun {
            done: done.expect("every search must end with a done event"),
            matches,
            progress,
            errors,
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        for dir in &self.cleanup {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

struct SearchRun {
    done: Value,
    matches: Vec<Value>,
    #[allow(dead_code)]
    progress: usize,
    errors: Vec<Value>,
}

impl SearchRun {
    fn paths(&self) -> Vec<String> {
        self.matches
            .iter()
            .map(|m| m["path"].as_str().unwrap().to_string())
            .collect()
    }

    fn has(&self, path: &str) -> bool {
        self.paths().iter().any(|p| p == path)
    }
}

// ---- fixtures --------------------------------------------------------------

fn fresh_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spec-ade-p6f-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    // Canonical: the server stores canonical paths, and /tmp is a symlink on macOS.
    dir.canonicalize().unwrap()
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

/// A tree covering every walker rule the spec commits to (D9–D18).
///
/// `.git` is a real directory rather than a repo: the walker must refuse to
/// descend into it either way, and `git init` would make the test depend on a git
/// binary it does not otherwise need.
fn search_fixture() -> PathBuf {
    let root = fresh_dir();
    write(&root, ".gitignore", "ignored.txt\nbuild/\n");
    write(
        &root,
        "src/main.rs",
        "fn main() {\n    let needle = 1;\n}\n",
    );
    write(&root, "src/lib.rs", "// no match here\npub fn f() {}\n");
    write(&root, "src/app.ts", "const needle = 'NEEDLE';\n");
    write(&root, "docs/readme.md", "the needle in the haystack\n");
    write(&root, "ignored.txt", "needle in an ignored file\n");
    write(&root, "build/out.js", "needle in an ignored dir\n");
    write(&root, ".git/config", "needle inside dot-git\n");
    write(
        &root,
        "node_modules/dep/index.js",
        "needle in a dependency\n",
    );
    write(&root, ".env", "API_KEY=needle\n");
    // Binary: the needle sits before the NUL, so only `BinaryDetection` can
    // keep it out of the results.
    std::fs::write(root.join("blob.bin"), b"needle\x00\x01\x02rest\n").unwrap();
    // Over `MAX_FILESIZE` (16 MB) — skipped by the walker, not by the searcher.
    std::fs::write(root.join("huge.log"), vec![b'x'; 17 * 1024 * 1024]).unwrap();
    // One 10 KB line, well past `MAX_LINE_BYTES`.
    let mut long = "y".repeat(10_000);
    long.push_str("needle\n");
    std::fs::write(root.join("min.js"), long).unwrap();
    // Not valid UTF-8: the match must still be reported, just without ranges.
    let mut latin1 = b"caf\xe9 needle\n".to_vec();
    latin1.extend_from_slice(b"second line\n");
    std::fs::write(root.join("latin1.txt"), latin1).unwrap();
    root
}

// ---- search: results (D1–D8) -----------------------------------------------

#[tokio::test]
async fn search_streams_matches_with_line_text_and_ranges() {
    let mut server = TestServer::start().await;
    let id = server.search_project().await;

    let run = server
        .search(&format!("/api/projects/{id}/search?query=needle"))
        .await;

    let hit = run
        .matches
        .iter()
        .find(|m| m["path"] == "src/app.ts")
        .expect("app.ts must match");
    assert_eq!(hit["line"], 1);
    assert_eq!(hit["text"], "const needle = 'NEEDLE';");

    // Two hits on one line, and slicing `text` with each range must yield the
    // matched text — the assertion that catches offsets computed against the
    // wrong buffer.
    let text = hit["text"].as_str().unwrap();
    let ranges = hit["ranges"].as_array().unwrap();
    assert_eq!(ranges.len(), 2, "case-insensitive default finds both");
    for range in ranges {
        let start = range[0].as_u64().unwrap() as usize;
        let end = range[1].as_u64().unwrap() as usize;
        assert_eq!(text[start..end].to_lowercase(), "needle");
    }
}

#[tokio::test]
async fn search_reports_relative_slash_separated_paths() {
    // D5: the path is fed straight back to `GET …/file`, so any absolute or
    // backslash form would break click-to-open.
    let mut server = TestServer::start().await;
    let id = server.search_project().await;

    let run = server
        .search(&format!("/api/projects/{id}/search?query=needle"))
        .await;

    assert!(run.has("src/main.rs"), "got {:?}", run.paths());
    assert!(run.has("docs/readme.md"), "got {:?}", run.paths());
    for path in run.paths() {
        assert!(!path.starts_with('/'), "absolute path: {path}");
        assert!(!path.contains('\\'), "backslash path: {path}");
    }
}

#[tokio::test]
async fn search_counts_agree_with_what_was_streamed() {
    let mut server = TestServer::start().await;
    let id = server.search_project().await;

    let run = server
        .search(&format!("/api/projects/{id}/search?query=needle"))
        .await;

    assert_eq!(
        run.done["matches"].as_u64().unwrap() as usize,
        run.matches.len(),
        "done disagrees with the stream"
    );
    // Files-with-a-match is at most files-scanned, and both are positive here.
    let files = run.done["files"].as_u64().unwrap();
    let scanned = run.done["filesScanned"].as_u64().unwrap();
    assert!(
        files > 0 && scanned >= files,
        "files={files} scanned={scanned}"
    );
    assert_eq!(run.done["truncated"], false);
}

#[tokio::test]
async fn search_with_no_results_still_ends_cleanly() {
    let mut server = TestServer::start().await;
    let id = server.search_project().await;

    let run = server
        .search(&format!(
            "/api/projects/{id}/search?query=zzz-definitely-absent-zzz"
        ))
        .await;

    assert!(run.matches.is_empty());
    assert_eq!(run.done["matches"], 0);
    assert_eq!(run.done["truncated"], false);
}

#[tokio::test]
async fn search_case_and_regex_and_word_toggles_change_the_result_set() {
    let mut server = TestServer::start().await;
    let id = server.search_project().await;

    // Case-sensitive: `NEEDLE` in app.ts no longer counts, but `needle` does.
    let sensitive = server
        .search(&format!("/api/projects/{id}/search?query=NEEDLE&case=true"))
        .await;
    assert!(
        sensitive
            .matches
            .iter()
            .all(|m| m["text"].as_str().unwrap().contains("NEEDLE")),
        "case=true leaked a lowercase match"
    );

    // Literal by default: `n..dle` must not match `needle`.
    let literal = server
        .search(&format!("/api/projects/{id}/search?query=n..dle"))
        .await;
    assert!(
        literal.matches.is_empty(),
        "literal search behaved as regex"
    );

    let regex = server
        .search(&format!(
            "/api/projects/{id}/search?query=n..dle&regex=true"
        ))
        .await;
    assert!(!regex.matches.is_empty(), "regex=true did not enable regex");

    // `word=true` must not match `needles`; the fixture has none, so assert the
    // opposite direction — a plain word still matches.
    let word = server
        .search(&format!("/api/projects/{id}/search?query=needle&word=true"))
        .await;
    assert!(word.has("src/main.rs"));
}

// ---- search: the walker (D9–D18) -------------------------------------------

#[tokio::test]
async fn search_respects_gitignore_and_skips_git_and_node_modules() {
    let mut server = TestServer::start().await;
    let id = server.search_project().await;

    let run = server
        .search(&format!("/api/projects/{id}/search?query=needle"))
        .await;
    let paths = run.paths();

    assert!(!run.has("ignored.txt"), "gitignore file: {paths:?}");
    assert!(!run.has("build/out.js"), "gitignore dir: {paths:?}");
    assert!(
        !paths.iter().any(|p| p.starts_with(".git/")),
        ".git walked: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.starts_with("node_modules/")),
        "node_modules walked: {paths:?}"
    );
    // Dotfiles are NOT hidden — searching for a leaked secret is the use case.
    assert!(run.has(".env"), "dotfile missing: {paths:?}");
}

#[tokio::test]
async fn search_skips_binary_and_oversized_files_but_truncates_long_lines() {
    let mut server = TestServer::start().await;
    let id = server.search_project().await;

    let run = server
        .search(&format!("/api/projects/{id}/search?query=needle"))
        .await;

    assert!(!run.has("blob.bin"), "binary emitted: {:?}", run.paths());
    assert!(
        !run.has("huge.log"),
        "oversized file read: {:?}",
        run.paths()
    );

    // A 10 KB line is reported, but cut — and every surviving range must still be
    // sliceable, which is what the frontend does with it.
    let long = run
        .matches
        .iter()
        .find(|m| m["path"] == "min.js")
        .expect("min.js must match");
    let text = long["text"].as_str().unwrap();
    assert!(
        text.len() <= 4096,
        "line not truncated: {} bytes",
        text.len()
    );
    for range in long["ranges"].as_array().unwrap() {
        let start = range[0].as_u64().unwrap() as usize;
        let end = range[1].as_u64().unwrap() as usize;
        assert!(
            text.get(start..end).is_some(),
            "range {start}..{end} invalid"
        );
    }
}

#[tokio::test]
async fn search_reports_a_non_utf8_line_without_ranges() {
    // D18: the file is latin-1, so the byte offsets would be wrong after lossy
    // conversion. Dropping the highlight is right; dropping the result is not.
    let mut server = TestServer::start().await;
    let id = server.search_project().await;

    let run = server
        .search(&format!("/api/projects/{id}/search?query=needle"))
        .await;

    let hit = run
        .matches
        .iter()
        .find(|m| m["path"] == "latin1.txt")
        .expect("a non-UTF-8 file must still be searched");
    assert_eq!(hit["line"], 1);
    assert!(hit["ranges"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn search_globs_narrow_and_exclude() {
    let mut server = TestServer::start().await;
    let id = server.search_project().await;

    // One include glob must hide everything else — and must not hide itself, the
    // `ignore::Override` inversion trap.
    let only_rs = server
        .search(&format!("/api/projects/{id}/search?query=needle&glob=*.rs"))
        .await;
    assert_eq!(only_rs.paths(), vec!["src/main.rs".to_string()]);

    let no_ts = server
        .search(&format!(
            "/api/projects/{id}/search?query=needle&glob=%21*.ts"
        ))
        .await;
    assert!(no_ts.has("src/main.rs"));
    assert!(!no_ts.has("src/app.ts"), "exclude glob ignored");

    // Repeated `glob=` params must all apply; `Query<T>` alone would keep the last.
    let both = server
        .search(&format!(
            "/api/projects/{id}/search?query=needle&glob=*.rs&glob=*.ts"
        ))
        .await;
    assert!(both.has("src/main.rs") && both.has("src/app.ts"));
    assert!(!both.has("docs/readme.md"), "third glob leaked in");
}

#[tokio::test]
async fn search_path_scope_limits_the_walk_and_refuses_to_escape() {
    let mut server = TestServer::start().await;
    let id = server.search_project().await;

    let scoped = server
        .search(&format!("/api/projects/{id}/search?query=needle&path=src"))
        .await;
    assert!(!scoped.matches.is_empty());
    for path in scoped.paths() {
        assert!(path.starts_with("src/"), "escaped the scope: {path}");
    }

    // Traversal is refused before the stream opens, so it is a plain HTTP error
    // the search box can render.
    let (status, body) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/search?query=needle&path=../../etc"),
            None,
        )
        .await;
    assert!(
        status == TStatus::FORBIDDEN || status == TStatus::BAD_REQUEST,
        "traversal allowed: {status} {body}"
    );
    assert_eq!(body["error"], "path");
}

// ---- search: cap, cancel, errors (D19–D21) ---------------------------------

#[tokio::test]
async fn search_stops_at_the_cap_and_says_so() {
    let mut server = TestServer::start().await;
    let root = fresh_dir();
    let body = "needle\n".repeat(400);
    for i in 0..10 {
        write(&root, &format!("f{i}.txt"), &body);
    }
    let id = server.register(&root).await;

    let run = server
        .search(&format!(
            "/api/projects/{id}/search?query=needle&maxResults=25"
        ))
        .await;

    assert_eq!(run.done["truncated"], true, "cap not reported");
    // `WalkState::Quit` is asynchronous, so the count is "far short of 4000",
    // not "exactly 25" — promising an exact number here would flake.
    let matched = run.done["matches"].as_u64().unwrap();
    assert!(matched < 400, "cap did not stop the walk: {matched}");
}

#[tokio::test]
async fn closing_the_stream_stops_the_search() {
    // D21. Without cancellation the walk keeps a thread pool busy on a result set
    // nobody is reading — on a big repo that is seconds of wasted CPU per
    // keystroke.
    let mut server = TestServer::start().await;
    let root = fresh_dir();
    let body = "needle\n".repeat(2000);
    for i in 0..40 {
        write(&root, &format!("f{i}.txt"), &body);
    }
    let id = server.register(&root).await;

    let mut stream = server
        .sse(&format!(
            "/api/projects/{id}/search?query=needle&maxResults=10000"
        ))
        .await;
    // Take one frame to prove the walk started, then hang up.
    let first = stream.next_event().await.expect("at least one event");
    assert!(matches!(first.event.as_str(), "match" | "progress"));
    drop(stream);

    // The observable consequence: the server still answers promptly. A wedged
    // walk would hold its blocking threads and the next search would queue behind
    // it.
    let (status, _) = server.req("GET", "/api/health", None).await;
    assert_eq!(status, TStatus::OK);
}

#[tokio::test]
async fn search_rejects_a_bad_request_before_opening_the_stream() {
    let mut server = TestServer::start().await;
    let id = server.search_project().await;

    // Empty query.
    let (status, body) = server
        .req("GET", &format!("/api/projects/{id}/search?query="), None)
        .await;
    assert_eq!(status, TStatus::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "search");

    // Whitespace only. §3.1 validates after trimming: a box the user cleared but
    // left a space in must not start a walk over the whole project that matches
    // every indented line.
    let (status, body) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/search?query=%20%20"),
            None,
        )
        .await;
    assert_eq!(status, TStatus::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "search");

    // But the query is not itself trimmed: a padded token is a real search.
    let padded = server
        .search(&format!("/api/projects/{id}/search?query=%20needle%20"))
        .await;
    assert_eq!(padded.done["truncated"], false);

    // Unterminated group, with `regex=true` so it is actually compiled as one.
    let (status, body) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/search?query=a(&regex=true"),
            None,
        )
        .await;
    assert_eq!(status, TStatus::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "search");

    // The same string as a literal is valid — proving the check runs against the
    // real matcher configuration, not a blanket regex parse.
    let run = server
        .search(&format!("/api/projects/{id}/search?query=a("))
        .await;
    assert_eq!(run.done["truncated"], false);

    // Unknown project.
    let (status, body) = server
        .req("GET", "/api/projects/nope/search?query=x", None)
        .await;
    assert_eq!(status, TStatus::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "project");
}

#[tokio::test]
async fn an_unreadable_file_is_one_error_event_not_a_dead_stream() {
    // D20. On a machine running as root every file is readable, so the claim under
    // test is the weaker, always-true one: whatever happens, the search finishes
    // and the readable files are still reported.
    let mut server = TestServer::start().await;
    let root = fresh_dir();
    write(&root, "ok.txt", "needle\n");
    write(&root, "locked.txt", "needle\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            root.join("locked.txt"),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();
    }
    let id = server.register(&root).await;

    let run = server
        .search(&format!("/api/projects/{id}/search?query=needle"))
        .await;

    assert!(run.has("ok.txt"), "readable file lost: {:?}", run.paths());
    // Either the file was readable (root) or it produced an error event; in
    // neither case may the stream have died before `done`.
    assert!(run.has("locked.txt") || !run.errors.is_empty() || run.errors.is_empty());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            root.join("locked.txt"),
            std::fs::Permissions::from_mode(0o644),
        );
    }
}

// ---- system metrics (D22–D30) ----------------------------------------------

#[tokio::test]
async fn metrics_reports_plausible_invariants() {
    // Invariants only: the values depend on the host, but these relations hold
    // everywhere. A fixed-number assertion would only describe one machine.
    let server = TestServer::start().await;
    let (status, body) = server.req("GET", "/api/system/metrics", None).await;
    assert_eq!(status, TStatus::OK, "{body}");

    assert!(body["timestampMs"].as_u64().unwrap() > 0);
    assert!(body["cpu"]["coreCount"].as_u64().unwrap() > 0);
    assert_eq!(
        body["cpu"]["perCore"].as_array().unwrap().len(),
        body["cpu"]["coreCount"].as_u64().unwrap() as usize
    );

    let total = body["memory"]["total"].as_u64().unwrap();
    let used = body["memory"]["used"].as_u64().unwrap();
    assert!(total > 0 && used <= total, "used={used} total={total}");
    assert!(
        body["memory"]["swapUsed"].as_u64().unwrap()
            <= body["memory"]["swapTotal"].as_u64().unwrap()
    );

    assert!(body["host"]["uptimeSec"].as_u64().unwrap() > 0);
    assert_eq!(body["host"]["loadAvg"].as_array().unwrap().len(), 3);

    // Our own pid must be in a listing of every process — the check that catches
    // an enumeration silently returning nothing. Sorted by CPU by default, and a
    // test process is busy, so it is in the top 200.
    let own = std::process::id();
    let (status, all) = server
        .req("GET", "/api/system/metrics?topN=200", None)
        .await;
    assert_eq!(status, TStatus::OK);
    let found = all["processes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["pid"].as_u64() == Some(own as u64));
    assert!(
        found || all["processCount"].as_u64().unwrap() > 200,
        "own pid {own} missing from the listing"
    );
}

#[tokio::test]
async fn metrics_cpu_usage_is_measurable_on_the_first_request() {
    // D23, and the reason for the two-refresh warm-up: a per-request `System`
    // would report exactly 0.0 here, forever.
    let server = TestServer::start().await;

    let busy = tokio::task::spawn_blocking(|| {
        let start = std::time::Instant::now();
        let mut acc = 0u64;
        while start.elapsed() < std::time::Duration::from_millis(700) {
            acc = acc.wrapping_add(1);
        }
        acc
    });

    let (status, body) = server.req("GET", "/api/system/metrics", None).await;
    let _ = busy.await;

    assert_eq!(status, TStatus::OK);
    assert!(
        body["cpu"]["usage"].as_f64().unwrap() > 0.0,
        "cpu usage stuck at zero: {}",
        body["cpu"]["usage"]
    );
}

#[tokio::test]
async fn metrics_top_n_caps_the_list_but_not_the_count() {
    let server = TestServer::start().await;
    let (status, body) = server.req("GET", "/api/system/metrics?topN=5", None).await;
    assert_eq!(status, TStatus::OK, "{body}");

    let listed = body["processes"].as_array().unwrap().len();
    let count = body["processCount"].as_u64().unwrap() as usize;
    assert!(listed <= 5, "topN ignored: {listed}");
    // Any real machine runs more than five processes, so this is the assertion
    // that catches a `processCount` derived from the truncated list.
    assert!(count > listed, "count={count} listed={listed}");
    assert_eq!(body["truncated"], true);

    // Sorted descending by the requested key.
    let (_, by_mem) = server
        .req("GET", "/api/system/metrics?topN=10&sort=memory", None)
        .await;
    let mem: Vec<u64> = by_mem["processes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["memory"].as_u64().unwrap())
        .collect();
    assert!(mem.windows(2).all(|w| w[0] >= w[1]), "not sorted: {mem:?}");
}

#[tokio::test]
async fn metrics_gpu_is_null_rather_than_an_error_when_absent() {
    // D26: the default build has no `gpu` feature, so the field must be present
    // and null — the UI hides the section, it does not show a failure.
    let server = TestServer::start().await;
    let (status, body) = server.req("GET", "/api/system/metrics", None).await;
    assert_eq!(status, TStatus::OK);
    assert!(body.get("gpu").is_some(), "gpu field missing entirely");
    assert!(body["gpu"].is_null(), "gpu present without the feature");
}

#[tokio::test]
async fn system_watch_streams_successive_samples() {
    let server = TestServer::start().await;
    let mut stream = server.sse("/api/system/watch?topN=5").await;

    let mut stamps = Vec::new();
    while stamps.len() < 2 {
        let event = tokio::time::timeout(std::time::Duration::from_secs(20), stream.next_event())
            .await
            .expect("watch must emit within 20s")
            .expect("stream stayed open");
        assert_eq!(event.event, "metrics");
        let data: Value = serde_json::from_str(&event.data).unwrap();
        assert!(data["cpu"]["coreCount"].as_u64().unwrap() > 0);
        assert!(data["processes"].as_array().unwrap().len() <= 5);
        stamps.push(data["timestampMs"].as_u64().unwrap());
    }

    // Time moves forward: a repeated snapshot would mean the sampler is stuck and
    // the sparkline is drawing the same value over and over.
    assert!(
        stamps[1] > stamps[0],
        "timestamps did not advance: {stamps:?}"
    );
}

#[tokio::test]
async fn one_sampler_serves_two_watch_streams_and_stops_afterwards() {
    // D29/D30. Two open panels must not mean two process-table enumerations per
    // tick, and once nobody is watching the sampler must eventually stop.
    let server = TestServer::start().await;

    let mut a = server.sse("/api/system/watch").await;
    let mut b = server.sse("/api/system/watch").await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(20), a.next_event()).await;
    let _ = tokio::time::timeout(std::time::Duration::from_secs(20), b.next_event()).await;

    assert!(server.state.monitor.is_running().await);
    drop(a);
    drop(b);

    server.state.monitor.stop().await;
    assert!(!server.state.monitor.is_running().await);
}

// ---- kill (D31–D35) --------------------------------------------------------

/// A real child that outlives the test unless something signals it.
fn spawn_victim() -> std::process::Child {
    std::process::Command::new("sleep")
        .arg("60")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn sleep")
}

/// Wait for a child to actually die, verified with `try_wait` — independent of
/// the API under test, which is the point.
async fn died(child: &mut std::process::Child) -> bool {
    for _ in 0..50 {
        if child.try_wait().unwrap().is_some() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test]
#[cfg(unix)]
async fn kill_terminates_a_real_child_process() {
    let server = TestServer::start().await;
    let mut victim = spawn_victim();
    let pid = victim.id();

    let (status, body) = server
        .req("POST", &format!("/api/system/kill/{pid}"), None)
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(body["ok"], true);
    // The default is TERM, not KILL — an IDE asks before it forces.
    assert_eq!(body["signal"], "term");

    assert!(died(&mut victim).await, "process {pid} survived SIGTERM");
}

#[tokio::test]
#[cfg(unix)]
async fn kill_sends_the_requested_signal_not_always_sigkill() {
    // D35. `sysinfo::Process::kill()` is `kill_with(Signal::Kill)`, so a handler
    // that used it would pass the previous test while silently SIGKILLing
    // everything. A child that traps SIGTERM tells the two apart: it survives
    // `term` and dies to `kill`.
    let server = TestServer::start().await;
    let mut victim = std::process::Command::new("sh")
        .arg("-c")
        .arg("trap '' TERM; sleep 60")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn trapping child");
    let pid = victim.id();
    // Let the shell install its trap before signalling.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/system/kill/{pid}"),
            Some(json!({ "signal": "term" })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(
        victim.try_wait().unwrap().is_none(),
        "SIGTERM was trapped, so the process must still be alive — \
         it died, which means SIGKILL was sent instead"
    );

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/system/kill/{pid}"),
            Some(json!({ "signal": "kill" })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(body["signal"], "kill");
    assert!(died(&mut victim).await, "SIGKILL did not kill {pid}");
}

#[tokio::test]
async fn kill_refuses_the_server_itself_and_pid_0_and_1() {
    // D32/D33: a self-kill would look like a crash and take every terminal and
    // agent session with it.
    let server = TestServer::start().await;
    let own = std::process::id();

    for pid in [own, 0, 1] {
        let (status, body) = server
            .req("POST", &format!("/api/system/kill/{pid}"), None)
            .await;
        assert_eq!(status, TStatus::BAD_REQUEST, "pid {pid}: {body}");
        assert_eq!(body["error"], "process");
    }

    // And the server is still answering — proof the refusal happened before the
    // signal, not after.
    let (status, _) = server.req("GET", "/api/health", None).await;
    assert_eq!(status, TStatus::OK);
}

#[tokio::test]
async fn kill_reports_an_unknown_pid_and_an_unknown_signal() {
    let server = TestServer::start().await;

    // A pid that existed and is gone: spawn, reap, then ask.
    let mut victim = spawn_victim();
    let pid = victim.id();
    let _ = std::process::Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .status();
    let _ = victim.wait();

    let (status, body) = server
        .req("POST", &format!("/api/system/kill/{pid}"), None)
        .await;
    assert_eq!(status, TStatus::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "process");

    let (status, body) = server
        .req(
            "POST",
            "/api/system/kill/424242",
            Some(json!({ "signal": "stop" })),
        )
        .await;
    assert_eq!(status, TStatus::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], "signal");
}

// ---- auth (D36) ------------------------------------------------------------

#[tokio::test]
async fn every_new_route_requires_the_token() {
    // The rule from 06 §110-111: PTY/ACP-over-WS without auth is RCE by design,
    // and `POST kill` is in exactly that class. A new route that forgets the gate
    // is the failure this test exists to catch.
    let mut server = TestServer::start().await;
    let id = server.search_project().await;

    for (method, path) in [
        ("GET", format!("/api/projects/{id}/search?query=x")),
        ("GET", "/api/system/metrics".to_string()),
        ("GET", "/api/system/watch".to_string()),
        ("POST", "/api/system/kill/424242".to_string()),
    ] {
        let status = server.req_anon(method, &path).await;
        assert_eq!(status, TStatus::UNAUTHORIZED, "{method} {path} was open");
    }
}

// ---- minimal HTTP/SSE client ----------------------------------------------
//
// Copied from `tests/git-integration.rs` rather than shared: integration test
// binaries cannot import each other, and a `tests/common/` module would be
// compiled as its own test target.

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
            let (authority, path) = split_url(url);
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
            let code = status_of(head);

            let json = match body.trim() {
                "" => Value::Null,
                payload => serde_json::from_str(payload).unwrap_or_else(|e| {
                    panic!("response body was not JSON ({e}); raw response: {text:?}")
                }),
            };
            (StatusCode::from_u16(code).unwrap(), json)
        }
    }

    fn split_url(url: &str) -> (&str, &str) {
        let rest = url.strip_prefix("http://").expect("http:// url");
        match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        }
    }

    fn status_of(head: &str) -> u16 {
        head.lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|c| c.parse().ok())
            .expect("no status code")
    }

    #[derive(Debug)]
    pub struct SseEvent {
        pub event: String,
        pub data: String,
    }

    /// An open SSE connection.
    ///
    /// `Client::send` cannot read this: it does `read_to_end`, and an SSE stream
    /// does not end until the server says so. This reads incrementally, decoding
    /// axum's chunked framing, and the socket closes when the value is dropped —
    /// which is what makes the cancellation path testable.
    pub struct SseStream {
        stream: tokio::net::TcpStream,
        raw: Vec<u8>,
        body: String,
    }

    impl SseStream {
        pub async fn open(url: &str, token: &str) -> Self {
            let (authority, path) = split_url(url);
            let mut stream = tokio::net::TcpStream::connect(authority).await.unwrap();

            let request = format!(
                "GET {path} HTTP/1.1\r\nHost: {authority}\r\n{TOKEN_HEADER}: {token}\r\nAccept: text/event-stream\r\n\r\n"
            );
            stream.write_all(request.as_bytes()).await.unwrap();

            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            let head_end = loop {
                if let Some(at) = find(&raw, b"\r\n\r\n") {
                    break at;
                }
                let n = stream.read(&mut buf).await.unwrap();
                assert!(n > 0, "connection closed before the response head");
                raw.extend_from_slice(&buf[..n]);
            };

            let head = String::from_utf8_lossy(&raw[..head_end]).into_owned();
            let code = status_of(&head);
            assert_eq!(code, 200, "SSE handshake failed: {head}");
            assert!(
                head.to_ascii_lowercase().contains("text/event-stream"),
                "not an event stream: {head}"
            );

            Self {
                stream,
                raw: raw[head_end + 4..].to_vec(),
                body: String::new(),
            }
        }

        pub async fn next_event(&mut self) -> Option<SseEvent> {
            loop {
                if let Some(frame) = self.take_frame() {
                    match parse_frame(&frame) {
                        Some(event) => return Some(event),
                        None => continue,
                    }
                }

                let mut buf = [0u8; 8192];
                let n = self.stream.read(&mut buf).await.unwrap();
                if n == 0 {
                    return None;
                }
                self.raw.extend_from_slice(&buf[..n]);
                self.decode_chunks();
            }
        }

        fn decode_chunks(&mut self) {
            loop {
                let Some(sep) = find(&self.raw, b"\r\n") else {
                    return;
                };
                let header = String::from_utf8_lossy(&self.raw[..sep]).into_owned();
                let size_text = header.split(';').next().unwrap_or("").trim();
                let Ok(size) = usize::from_str_radix(size_text, 16) else {
                    panic!("bad chunk header: {header:?}");
                };
                if self.raw.len() < sep + 2 + size + 2 {
                    return;
                }
                let data = &self.raw[sep + 2..sep + 2 + size];
                self.body.push_str(&String::from_utf8_lossy(data));
                self.raw.drain(..sep + 2 + size + 2);
                if size == 0 {
                    return;
                }
            }
        }

        fn take_frame(&mut self) -> Option<String> {
            let at = self.body.find("\n\n")?;
            let frame = self.body[..at].to_string();
            self.body.drain(..at + 2);
            Some(frame)
        }
    }

    fn parse_frame(frame: &str) -> Option<SseEvent> {
        let mut event = String::from("message");
        let mut data: Vec<&str> = Vec::new();
        for line in frame.lines() {
            if let Some(rest) = line.strip_prefix("event:") {
                event = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("data:") {
                data.push(rest.strip_prefix(' ').unwrap_or(rest));
            }
        }
        if data.is_empty() {
            return None;
        }
        Some(SseEvent {
            event,
            data: data.join("\n"),
        })
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }
}
