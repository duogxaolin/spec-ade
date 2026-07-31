//! Phase 5 integration tests — SPEC-005 (git integration).
//!
//! These run against **real git repositories** in temp dirs, driven through the
//! real HTTP surface. That is the point: the whole spec is about matching what
//! `git` actually does, so a mocked git would only test our assumptions.
//!
//! The fixture sets `user.email`, `user.name`, `commit.gpgsign=false` and
//! `core.hooksPath` **locally** in each repo. Without that, a machine whose global
//! config signs commits or points hooks somewhere fails every commit test, and the
//! failure looks like our bug (§7).

use serde_json::{Value, json};
use spec_ade_server::{AppState, build_router};
use std::path::{Path, PathBuf};
use std::process::Command;
use tokio_tungstenite::tungstenite::http::StatusCode as TStatus;

struct TestServer {
    addr: std::net::SocketAddr,
    token: String,
    client: reqwest_lite::Client,
    cleanup: Vec<PathBuf>,
    /// A clone of the state the router holds. C37/C38 are claims about the watcher
    /// registry, and nothing on the wire can distinguish "one watcher shared by two
    /// streams" from "two watchers" — the only honest witness is the registry
    /// itself. `AppState` is `Clone` and shares its inner `Arc`s, so this observes
    /// the live server rather than a copy.
    state: AppState,
}

impl TestServer {
    async fn start() -> Self {
        let token = format!("tok-{}", uuid::Uuid::new_v4());
        let data_dir = std::env::temp_dir().join(format!("spec-ade-p5-{}", uuid::Uuid::new_v4()));
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

    /// Register a directory as a project, returning its id.
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

    /// A fresh git repo with one commit ("init"), registered as a project.
    async fn git_project(&mut self, files: &[(&str, &str)]) -> (String, PathBuf) {
        let root = fresh_dir();
        git_init(&root);
        for (rel, content) in files {
            write(&root, rel, content);
        }
        git(&root, &["add", "--all"]);
        git(&root, &["commit", "-m", "init"]);
        let id = self.register(&root).await;
        (id, root)
    }

    /// Open an SSE stream. Dropping the returned value closes the socket, which is
    /// how the server learns its last subscriber left.
    async fn sse(&self, path: &str) -> reqwest_lite::SseStream {
        reqwest_lite::SseStream::open(&self.url(path), &self.token).await
    }

    /// A plain directory with no repository, registered as a project.
    async fn plain_project(&mut self) -> (String, PathBuf) {
        let root = fresh_dir();
        write(&root, "a.txt", "hello");
        let id = self.register(&root).await;
        (id, root)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        for dir in &self.cleanup {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

// ---- fixture helpers -------------------------------------------------------

fn fresh_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("spec-ade-git-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    // Canonical, so the path matches what the server stores (macOS /tmp symlink).
    dir.canonicalize().unwrap()
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

/// Run `git` in `root`, asserting success.
fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git must be on PATH for phase5 tests");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Run `git` allowing failure — for setting up conflicts.
fn git_may_fail(root: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .current_dir(root)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `git init` with every setting a commit needs, pinned locally.
fn git_init(root: &Path) {
    git(root, &["init", "--initial-branch=main"]);
    git(root, &["config", "user.email", "test@spec-ade.invalid"]);
    git(root, &["config", "user.name", "Spec ADE Test"]);
    // A machine with `commit.gpgsign=true` globally would otherwise fail every
    // commit test with a signing error.
    git(root, &["config", "commit.gpgsign", "false"]);
    // And one with a global `core.hooksPath` would run somebody else's hooks.
    git(root, &["config", "core.hooksPath", ".githooks-none"]);
}

/// Find a status entry by path, or fail with the whole list for context.
fn entry<'a>(status: &'a Value, path: &str) -> &'a Value {
    status["entries"]
        .as_array()
        .expect("entries array")
        .iter()
        .find(|e| e["path"] == path)
        .unwrap_or_else(|| panic!("no entry for {path} in {}", status["entries"]))
}

fn has_entry(status: &Value, path: &str) -> bool {
    status["entries"]
        .as_array()
        .map(|list| list.iter().any(|e| e["path"] == path))
        .unwrap_or(false)
}

// ---- status (C1–C7) --------------------------------------------------------

#[tokio::test]
async fn status_groups_every_kind_of_change() {
    let mut server = TestServer::start().await;
    let (id, root) = server
        .git_project(&[("tracked.txt", "one\n"), ("deleted.txt", "gone\n")])
        .await;

    write(&root, "tracked.txt", "one\ntwo\n"); // modified, unstaged
    write(&root, "staged.txt", "new\n");
    git(&root, &["add", "staged.txt"]); // staged, new
    write(&root, "untracked.txt", "loose\n"); // untracked
    std::fs::remove_file(root.join("deleted.txt")).unwrap(); // deleted, unstaged

    let (status, body) = server
        .req("GET", &format!("/api/projects/{id}/git/status"), None)
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(body["isRepo"], true);
    assert_eq!(body["head"]["branch"], "main");
    assert_eq!(body["head"]["detached"], false);
    assert!(body["head"]["oid"].as_str().unwrap().len() == 40);
    assert_eq!(body["state"], "clean");

    // C2/C3: index and worktree are reported on separate axes.
    assert_eq!(entry(&body, "staged.txt")["index"], "new");
    assert_eq!(entry(&body, "staged.txt")["worktree"], "none");
    assert_eq!(entry(&body, "staged.txt")["staged"], true);

    assert_eq!(entry(&body, "tracked.txt")["index"], "none");
    assert_eq!(entry(&body, "tracked.txt")["worktree"], "modified");
    assert_eq!(entry(&body, "tracked.txt")["staged"], false);

    assert_eq!(entry(&body, "untracked.txt")["worktree"], "new");
    assert_eq!(entry(&body, "deleted.txt")["worktree"], "deleted");

    // C6: counts match the groups the panel renders.
    assert_eq!(body["counts"]["staged"], 1);
    assert_eq!(body["counts"]["untracked"], 1);
    assert_eq!(body["counts"]["changed"], 2, "modified + deleted");
    assert_eq!(body["counts"]["conflicted"], 0);
}

#[tokio::test]
async fn a_file_staged_then_edited_again_appears_on_both_axes() {
    // git's `MM`: the reason `index` and `worktree` are separate fields (C3, C42).
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;

    write(&root, "a.txt", "two\n");
    git(&root, &["add", "a.txt"]);
    write(&root, "a.txt", "three\n");

    let (_, body) = server
        .req("GET", &format!("/api/projects/{id}/git/status"), None)
        .await;
    let a = entry(&body, "a.txt");
    assert_eq!(a["index"], "modified");
    assert_eq!(a["worktree"], "modified");
    // It is genuinely in both groups, so it counts in both.
    assert_eq!(body["counts"]["staged"], 1);
    assert_eq!(body["counts"]["changed"], 1);
}

#[tokio::test]
async fn gitignored_files_are_absent_from_status() {
    // C4. `.gitignore` itself is a tracked file and must still show.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;

    write(&root, ".gitignore", "secret.env\nbuild/\n");
    write(&root, "secret.env", "TOKEN=nope\n");
    write(&root, "build/out.js", "compiled\n");
    write(&root, "visible.txt", "shown\n");

    let (_, body) = server
        .req("GET", &format!("/api/projects/{id}/git/status"), None)
        .await;

    assert!(!has_entry(&body, "secret.env"), "ignored file leaked");
    assert!(!has_entry(&body, "build/out.js"), "ignored dir leaked");
    assert!(has_entry(&body, "visible.txt"));
    assert!(has_entry(&body, ".gitignore"), ".gitignore is tracked");
}

#[tokio::test]
async fn a_new_directory_lists_its_files_not_just_itself() {
    // Without `recurse_untracked_dirs` this is one entry "sub/", which cannot be
    // staged file by file.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    write(&root, "sub/one.txt", "1\n");
    write(&root, "sub/two.txt", "2\n");

    let (_, body) = server
        .req("GET", &format!("/api/projects/{id}/git/status"), None)
        .await;
    assert!(has_entry(&body, "sub/one.txt"));
    assert!(has_entry(&body, "sub/two.txt"));
    assert!(
        !has_entry(&body, "sub/"),
        "directory reported instead of files"
    );
}

#[tokio::test]
async fn a_project_without_a_repository_answers_is_repo_false() {
    // C5: information, not a red error. A 4xx here would make the panel shout at
    // the user for the normal case of a plain directory.
    let mut server = TestServer::start().await;
    let (id, _root) = server.plain_project().await;

    let (status, body) = server
        .req("GET", &format!("/api/projects/{id}/git/status"), None)
        .await;
    assert_eq!(status, TStatus::OK, "must not be an error: {body}");
    assert_eq!(body["isRepo"], false);
    assert_eq!(body["head"], Value::Null);
    assert!(body["entries"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_repository_with_no_commits_reports_its_unborn_branch() {
    // The panel opens before the first commit; that must not be an error.
    let mut server = TestServer::start().await;
    let root = fresh_dir();
    git_init(&root);
    write(&root, "a.txt", "one\n");
    let id = server.register(&root).await;

    let (status, body) = server
        .req("GET", &format!("/api/projects/{id}/git/status"), None)
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(body["isRepo"], true);
    assert_eq!(body["head"]["branch"], "main");
    assert_eq!(body["head"]["oid"], Value::Null, "no commit yet");
    assert!(has_entry(&body, "a.txt"));
}

#[tokio::test]
async fn status_does_not_write_the_git_index() {
    // C40: `update_index(false)`. A GET that rewrites `.git/index` races the
    // user's own `git` in a terminal.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    write(&root, "a.txt", "changed\n");

    let index = root.join(".git").join("index");
    let before = std::fs::metadata(&index).unwrap().modified().unwrap();

    for _ in 0..3 {
        let (status, _) = server
            .req("GET", &format!("/api/projects/{id}/git/status"), None)
            .await;
        assert_eq!(status, TStatus::OK);
    }

    let after = std::fs::metadata(&index).unwrap().modified().unwrap();
    assert_eq!(before, after, "GET status must not rewrite .git/index");
}

#[tokio::test]
async fn a_detached_head_is_reported_as_detached() {
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    write(&root, "a.txt", "two\n");
    git(&root, &["commit", "-am", "second"]);
    git(&root, &["checkout", "--detach", "HEAD~1"]);

    let (_, body) = server
        .req("GET", &format!("/api/projects/{id}/git/status"), None)
        .await;
    assert_eq!(body["head"]["detached"], true);
    assert_eq!(body["head"]["branch"], Value::Null);
    assert!(body["head"]["oid"].as_str().unwrap().len() == 40);
    // No branch means no upstream to compare against.
    assert_eq!(body["upstream"], Value::Null);
}

// ---- diff (C8–C10) ---------------------------------------------------------

#[tokio::test]
async fn diff_returns_whole_documents_for_the_merge_view() {
    // C8 + [INVENTED-4]: `unifiedMergeView` needs full `original`/`new` documents,
    // not only a patch.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\ntwo\n")]).await;
    write(&root, "a.txt", "one\ntwo\nthree\n");

    let (status, body) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/git/diff?path=a.txt"),
            None,
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(body["oldText"], "one\ntwo\n");
    assert_eq!(body["newText"], "one\ntwo\nthree\n");
    assert_eq!(body["added"], 1);
    assert_eq!(body["removed"], 0);
    assert_eq!(body["binary"], false);
    assert_eq!(body["truncated"], false);
    assert_eq!(
        body["worktreeOid"].as_str().map(str::len),
        Some(40),
        "an unstaged diff carries the exact worktree snapshot id"
    );
    assert!(
        body["patch"].as_str().unwrap().contains("+three"),
        "patch text missing the added line: {}",
        body["patch"]
    );
}

#[tokio::test]
async fn staged_and_unstaged_diffs_are_different_views() {
    // C9: with `staged=true` the diff is HEAD→index; otherwise index→worktree.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "base\n")]).await;

    write(&root, "a.txt", "staged\n");
    git(&root, &["add", "a.txt"]);
    write(&root, "a.txt", "worktree\n");

    let (_, staged) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/git/diff?path=a.txt&staged=true"),
            None,
        )
        .await;
    assert_eq!(staged["oldText"], "base\n");
    assert_eq!(staged["newText"], "staged\n");
    assert_eq!(staged["staged"], true);

    let (_, unstaged) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/git/diff?path=a.txt"),
            None,
        )
        .await;
    assert_eq!(unstaged["oldText"], "staged\n");
    assert_eq!(unstaged["newText"], "worktree\n");
    assert_eq!(unstaged["staged"], false);
}

#[tokio::test]
async fn an_untracked_file_diffs_as_all_additions() {
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    write(&root, "fresh.txt", "line1\nline2\n");

    let (status, body) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/git/diff?path=fresh.txt"),
            None,
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(body["oldText"], "", "no previous version");
    assert_eq!(body["newText"], "line1\nline2\n");
    assert_eq!(body["added"], 2);
}

#[tokio::test]
async fn a_binary_diff_says_binary_and_ships_no_content() {
    // C10: shipping megabytes of lossy bytes helps nobody.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    std::fs::write(root.join("blob.bin"), [0x00, 0x01, 0x02, 0xFF, 0x00]).unwrap();
    git(&root, &["add", "blob.bin"]);

    let (status, body) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/git/diff?path=blob.bin&staged=true"),
            None,
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(body["binary"], true);
    assert_eq!(body["oldText"], "");
    assert_eq!(body["newText"], "");
}

#[tokio::test]
async fn a_path_escaping_the_project_is_refused_everywhere() {
    // §5.4: the guard is on every path-taking endpoint, not just the file API.
    let mut server = TestServer::start().await;
    let (id, _root) = server.git_project(&[("a.txt", "one\n")]).await;

    for endpoint in ["diff", "blame", "blob", "conflict"] {
        let (status, body) = server
            .req(
                "GET",
                &format!("/api/projects/{id}/git/{endpoint}?path=../../etc/passwd"),
                None,
            )
            .await;
        assert_eq!(
            status,
            TStatus::BAD_REQUEST,
            "{endpoint} accepted a traversal: {body}"
        );
        assert_eq!(body["error"], "path");
    }

    // C17: `.git/…` is a *well-formed* path that is refused, so it is 403 rather
    // than 400 — the same line the file API draws (SPEC-002 `PathError::Escapes`).
    // `.git/config` in particular holds remote URLs, which can carry credentials.
    for endpoint in ["diff", "blame", "blob", "conflict"] {
        let (status, body) = server
            .req(
                "GET",
                &format!("/api/projects/{id}/git/{endpoint}?path=.git/config"),
                None,
            )
            .await;
        assert_eq!(status, TStatus::FORBIDDEN, "{endpoint}: {body}");
        assert_eq!(body["error"], "path");
    }

    // Nested at depth, and in a submodule's own `.git`, too.
    let (status, _) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/git/blob?path=sub/.git/config"),
            None,
        )
        .await;
    assert_eq!(status, TStatus::FORBIDDEN);
}

// ---- log and commit detail (C11–C12) ---------------------------------------

#[tokio::test]
async fn log_returns_newest_first_with_author_and_parents() {
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    write(&root, "a.txt", "two\n");
    git(
        &root,
        &["commit", "-am", "second commit\n\nWith a body line.\n"],
    );

    let (status, body) = server
        .req("GET", &format!("/api/projects/{id}/git/log"), None)
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    let commits = body["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0]["summary"], "second commit");
    assert_eq!(commits[0]["body"], "With a body line.");
    assert_eq!(commits[1]["summary"], "init");
    assert_eq!(commits[0]["author"]["email"], "test@spec-ade.invalid");
    assert_eq!(commits[0]["author"]["name"], "Spec ADE Test");
    assert!(commits[0]["author"]["time"].as_i64().unwrap() > 0);
    assert_eq!(commits[0]["short"].as_str().unwrap().len(), 7);
    // The newest commit's parent is the older one.
    assert_eq!(commits[0]["parents"][0], commits[1]["oid"]);
    assert!(commits[1]["parents"].as_array().unwrap().is_empty());
    assert_eq!(body["nextBefore"], Value::Null, "history is exhausted");
}

#[tokio::test]
async fn log_pages_by_cursor_without_repeating_or_skipping() {
    // C11 + [INVENTED-7]: `before=<oid>` rather than an offset, so a commit
    // arriving mid-paging cannot shift the window.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "0\n")]).await;
    for n in 1..=5 {
        write(&root, "a.txt", &format!("{n}\n"));
        git(&root, &["commit", "-am", &format!("commit {n}")]);
    }

    let (_, page1) = server
        .req("GET", &format!("/api/projects/{id}/git/log?limit=2"), None)
        .await;
    let first: Vec<String> = page1["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["oid"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(first.len(), 2);
    let cursor = page1["nextBefore"]
        .as_str()
        .expect("more history")
        .to_string();

    let (_, page2) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/git/log?limit=2&before={cursor}"),
            None,
        )
        .await;
    let second: Vec<String> = page2["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["oid"].as_str().unwrap().to_string())
        .collect();

    // The cursor is the first commit of page 2 — no gap, no duplicate.
    assert_eq!(second[0], cursor);
    for oid in &second {
        assert!(!first.contains(oid), "commit {oid} repeated across pages");
    }
}

#[tokio::test]
async fn log_can_be_filtered_to_one_path() {
    let mut server = TestServer::start().await;
    let (id, root) = server
        .git_project(&[("a.txt", "a\n"), ("b.txt", "b\n")])
        .await;
    write(&root, "a.txt", "a2\n");
    git(&root, &["commit", "-am", "touch a"]);
    write(&root, "b.txt", "b2\n");
    git(&root, &["commit", "-am", "touch b"]);

    let (_, body) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/git/log?path=b.txt"),
            None,
        )
        .await;
    let summaries: Vec<&str> = body["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["summary"].as_str().unwrap())
        .collect();
    assert!(summaries.contains(&"touch b"));
    assert!(!summaries.contains(&"touch a"), "filter leaked other paths");
}

#[tokio::test]
async fn commit_detail_lists_files_with_line_counts() {
    // C12.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    write(&root, "a.txt", "one\ntwo\n");
    write(&root, "new.txt", "fresh\n");
    git(&root, &["add", "--all"]);
    git(&root, &["commit", "-m", "add and modify"]);

    let (_, log) = server
        .req("GET", &format!("/api/projects/{id}/git/log?limit=1"), None)
        .await;
    let oid = log["commits"][0]["oid"].as_str().unwrap().to_string();

    let (status, body) = server
        .req("GET", &format!("/api/projects/{id}/git/commit/{oid}"), None)
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(body["commit"]["summary"], "add and modify");

    let files = body["files"].as_array().unwrap();
    let a = files.iter().find(|f| f["path"] == "a.txt").expect("a.txt");
    assert_eq!(a["change"], "modified");
    assert_eq!(a["added"], 1);
    let new = files
        .iter()
        .find(|f| f["path"] == "new.txt")
        .expect("new.txt");
    assert_eq!(new["change"], "added");
    assert_eq!(new["added"], 1);
}

#[tokio::test]
async fn an_unknown_commit_or_bad_oid_is_404() {
    let mut server = TestServer::start().await;
    let (id, _root) = server.git_project(&[("a.txt", "one\n")]).await;

    for oid in [
        "0123456789abcdef0123456789abcdef01234567", // well-formed, absent
        "not-an-oid",
    ] {
        let (status, body) = server
            .req("GET", &format!("/api/projects/{id}/git/commit/{oid}"), None)
            .await;
        assert_eq!(status, TStatus::NOT_FOUND, "oid {oid}: {body}");
    }
}

// ---- branches (C13) --------------------------------------------------------

#[tokio::test]
async fn branches_lists_locals_and_marks_the_current_one() {
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    git(&root, &["branch", "feature/x"]);

    let (status, body) = server
        .req("GET", &format!("/api/projects/{id}/git/branches"), None)
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(body["current"], "main");

    let local = body["local"].as_array().unwrap();
    assert_eq!(local.len(), 2);
    // Sorted, so the order is stable for the UI and for this assertion.
    assert_eq!(local[0]["name"], "feature/x");
    assert_eq!(local[0]["current"], false);
    assert_eq!(local[1]["name"], "main");
    assert_eq!(local[1]["current"], true);
    assert_eq!(local[1]["upstream"], Value::Null);
    assert!(body["remote"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_tracking_branch_reports_its_upstream_and_ahead_count() {
    // C13: the branch bar shows ahead/behind, so the numbers have to be real.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;

    let remote = fresh_dir();
    git(&remote, &["init", "--bare", "--initial-branch=main"]);
    server.cleanup.push(remote.clone());
    git(
        &root,
        &["remote", "add", "origin", &remote.display().to_string()],
    );
    git(&root, &["push", "-u", "origin", "main"]);

    // One commit that the remote does not have yet.
    write(&root, "a.txt", "two\n");
    git(&root, &["commit", "-am", "ahead by one"]);

    let (_, body) = server
        .req("GET", &format!("/api/projects/{id}/git/branches"), None)
        .await;
    let main = body["local"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "main")
        .expect("main");
    assert_eq!(main["upstream"], "origin/main");
    assert_eq!(main["ahead"], 1);
    assert_eq!(main["behind"], 0);

    let remotes: Vec<&str> = body["remote"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap())
        .collect();
    assert!(
        remotes.contains(&"origin/main"),
        "remote branches: {remotes:?}"
    );

    // The same tracking info rides on `status`, which is what the bar reads.
    let (_, status) = server
        .req("GET", &format!("/api/projects/{id}/git/status"), None)
        .await;
    assert_eq!(status["upstream"]["name"], "origin/main");
    assert_eq!(status["upstream"]["ahead"], 1);
}

// ---- blame and blob (C14–C15) ----------------------------------------------

#[tokio::test]
async fn blame_attributes_each_line_to_the_commit_that_wrote_it() {
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "first\n")]).await;
    write(&root, "a.txt", "first\nsecond\n");
    git(&root, &["commit", "-am", "add second line"]);

    let (status, body) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/git/blame?path=a.txt"),
            None,
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    let lines = body["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["line"], 1);
    assert_eq!(lines[0]["summary"], "init");
    assert_eq!(lines[1]["line"], 2);
    assert_eq!(lines[1]["summary"], "add second line");
    assert_eq!(lines[1]["author"], "Spec ADE Test");
    assert_ne!(lines[0]["oid"], lines[1]["oid"], "two different commits");
    assert_eq!(lines[0]["short"].as_str().unwrap().len(), 7);
}

#[tokio::test]
async fn blame_on_an_untracked_file_is_404_not_an_empty_list() {
    // An empty list would render as a blame gutter full of blanks, which reads as
    // "nobody wrote this" rather than "this file has no history".
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    write(&root, "loose.txt", "never committed\n");

    let (status, body) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/git/blame?path=loose.txt"),
            None,
        )
        .await;
    assert_eq!(status, TStatus::NOT_FOUND, "{body}");
    assert_eq!(body["error"], "notFound");
}

#[tokio::test]
async fn blob_reads_head_index_worktree_and_a_commit() {
    // C15: the diff viewer's "original" side comes from here, so all four
    // revisions have to be reachable.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "committed\n")]).await;
    let head_oid = git(&root, &["rev-parse", "HEAD"]).trim().to_string();

    write(&root, "a.txt", "indexed\n");
    git(&root, &["add", "a.txt"]);
    write(&root, "a.txt", "on disk\n");

    for (rev, expected) in [
        ("", "committed\n"),
        ("HEAD", "committed\n"),
        ("index", "indexed\n"),
        ("worktree", "on disk\n"),
        (head_oid.as_str(), "committed\n"),
    ] {
        let (status, body) = server
            .req(
                "GET",
                &format!("/api/projects/{id}/git/blob?path=a.txt&rev={rev}"),
                None,
            )
            .await;
        assert_eq!(status, TStatus::OK, "rev {rev:?}: {body}");
        assert_eq!(body["content"], expected, "rev {rev:?}");
        assert_eq!(body["binary"], false);
    }
}

#[tokio::test]
async fn blob_is_404_when_the_version_has_no_such_path() {
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    write(&root, "later.txt", "not committed\n");

    // Exists on disk, absent from HEAD.
    let (status, _) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/git/blob?path=later.txt&rev=HEAD"),
            None,
        )
        .await;
    assert_eq!(status, TStatus::NOT_FOUND);

    // An unparseable rev is 404 too — there is no such version to return.
    let (status, _) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/git/blob?path=a.txt&rev=nonsense"),
            None,
        )
        .await;
    assert_eq!(status, TStatus::NOT_FOUND);
}

// ---- stage / unstage (C16–C17) ---------------------------------------------

#[tokio::test]
async fn staging_returns_the_new_status_in_the_same_response() {
    // C16 + [INVENTED-6]: no second round-trip to learn what changed.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    write(&root, "a.txt", "two\n");
    write(&root, "new.txt", "fresh\n");

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/stage"),
            Some(json!({ "paths": ["a.txt", "new.txt"] })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(entry(&body, "a.txt")["index"], "modified");
    assert_eq!(entry(&body, "a.txt")["worktree"], "none");
    assert_eq!(entry(&body, "new.txt")["index"], "new");
    assert_eq!(body["counts"]["staged"], 2);
    assert_eq!(body["counts"]["untracked"], 0);
}

#[tokio::test]
async fn staging_a_deletion_records_it_rather_than_failing() {
    // Plain `git add <path>` cannot stage the removal of a file that is already
    // gone on older git, which is why the implementation passes `--all`.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("gone.txt", "bye\n")]).await;
    std::fs::remove_file(root.join("gone.txt")).unwrap();

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/stage"),
            Some(json!({ "paths": ["gone.txt"] })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(entry(&body, "gone.txt")["index"], "deleted");
}

#[tokio::test]
async fn unstaging_puts_the_change_back_in_the_worktree_group() {
    // C17: unstage must not throw the edit away, only move it between groups.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    write(&root, "a.txt", "edited\n");
    git(&root, &["add", "a.txt"]);

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/stage"),
            Some(json!({ "paths": ["a.txt"], "unstage": true })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(entry(&body, "a.txt")["index"], "none");
    assert_eq!(entry(&body, "a.txt")["worktree"], "modified");
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "edited\n",
        "unstage must not touch the file"
    );
}

#[tokio::test]
async fn stage_content_updates_only_the_index_document() {
    let mut server = TestServer::start().await;
    let base = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";
    let worktree = "one\nTWO\nthree\nfour\nfive\nsix\nseven\neight\nNINE\nten\n";
    let selected = "one\nTWO\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";
    let (id, root) = server.git_project(&[("a.txt", base)]).await;
    write(&root, "a.txt", worktree);

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/stage-content"),
            Some(json!({ "path": "a.txt", "content": selected })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(
        git(&root, &["show", ":a.txt"]),
        selected,
        "only the selected hunk entered the index"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        worktree,
        "staging a hunk must not rewrite the worktree"
    );
    let a = entry(&body, "a.txt");
    assert_eq!(a["index"], "modified");
    assert_eq!(a["worktree"], "modified");
}

#[tokio::test]
async fn unstage_content_updates_only_the_index_document() {
    let mut server = TestServer::start().await;
    let base = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";
    let staged = "one\nTWO\nthree\nfour\nfive\nsix\nseven\neight\nNINE\nten\n";
    let remaining = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nNINE\nten\n";
    let (id, root) = server.git_project(&[("a.txt", base)]).await;
    write(&root, "a.txt", staged);
    git(&root, &["add", "a.txt"]);

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/unstage-content"),
            Some(json!({ "path": "a.txt", "content": remaining, "exists": true })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(
        git(&root, &["show", ":a.txt"]),
        remaining,
        "only the selected staged hunk left the index"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        staged,
        "unstaging a hunk must not rewrite the worktree"
    );
    assert_eq!(entry(&body, "a.txt")["index"], "modified");
    assert_eq!(entry(&body, "a.txt")["worktree"], "modified");
}

#[tokio::test]
async fn unstage_content_removes_a_new_file_from_the_index_only() {
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    write(&root, "fresh.txt", "new work\n");
    git(&root, &["add", "fresh.txt"]);

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/unstage-content"),
            Some(json!({ "path": "fresh.txt", "content": "", "exists": false })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert!(
        git(&root, &["ls-files", "--error-unmatch", "a.txt"]).contains("a.txt"),
        "the existing index remains usable"
    );
    assert!(
        !git_may_fail(&root, &["ls-files", "--error-unmatch", "fresh.txt"]),
        "the new path left the index"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("fresh.txt")).unwrap(),
        "new work\n",
        "unstaging a new file must preserve its only copy"
    );
    assert_eq!(entry(&body, "fresh.txt")["worktree"], "new");
}

#[tokio::test]
async fn discard_content_updates_only_the_worktree_document() {
    let mut server = TestServer::start().await;
    let base = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";
    let indexed = "one\nTWO\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";
    let worktree = "one\nTWO\nthree\nfour\nfive\nsix\nseven\neight\nNINE\nten\n";
    let (id, root) = server.git_project(&[("a.txt", base)]).await;
    write(&root, "a.txt", indexed);
    git(&root, &["add", "a.txt"]);
    write(&root, "a.txt", worktree);

    let (status, diff) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/git/diff?path=a.txt"),
            None,
        )
        .await;
    assert_eq!(status, TStatus::OK, "{diff}");
    let expected_oid = diff["worktreeOid"]
        .as_str()
        .expect("worktree diff oid")
        .to_string();

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/discard-content"),
            Some(json!({
                "path": "a.txt",
                "content": indexed,
                "expectedOid": expected_oid,
            })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(
        git(&root, &["show", ":a.txt"]),
        indexed,
        "discarding a hunk must not rewrite the index"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        indexed,
        "only the selected worktree hunk was discarded"
    );
    assert_eq!(entry(&body, "a.txt")["index"], "modified");
    assert_eq!(entry(&body, "a.txt")["worktree"], "none");
}

#[tokio::test]
async fn discard_content_refuses_a_stale_worktree_snapshot() {
    let mut server = TestServer::start().await;
    let indexed = "one\nTWO\nthree\n";
    let shown = "one\nTWO\nTHREE\n";
    let external = "edited outside the diff view\n";
    let (id, root) = server.git_project(&[("a.txt", "one\ntwo\nthree\n")]).await;
    write(&root, "a.txt", indexed);
    git(&root, &["add", "a.txt"]);
    write(&root, "a.txt", shown);

    let (status, diff) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/git/diff?path=a.txt"),
            None,
        )
        .await;
    assert_eq!(status, TStatus::OK, "{diff}");
    let stale_oid = diff["worktreeOid"]
        .as_str()
        .expect("worktree diff oid")
        .to_string();

    write(&root, "a.txt", external);
    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/discard-content"),
            Some(json!({
                "path": "a.txt",
                "content": indexed,
                "expectedOid": stale_oid,
            })),
        )
        .await;

    assert_eq!(status, TStatus::CONFLICT, "{body}");
    assert_eq!(body["error"], "blocked");
    assert!(
        body["detail"]
            .as_str()
            .unwrap()
            .contains("refresh before discarding"),
        "{body}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        external,
        "the external edit must not be overwritten"
    );
    assert_eq!(
        git(&root, &["show", ":a.txt"]),
        indexed,
        "a refused worktree write must not touch the index"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn content_mutations_preserve_executable_mode_and_refuse_symlink_escape() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let mut server = TestServer::start().await;
    let (id, root) = server
        .git_project(&[("script.sh", "#!/bin/sh\necho old\n")])
        .await;
    std::fs::set_permissions(
        root.join("script.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    git(&root, &["add", "script.sh"]);
    git(&root, &["commit", "-m", "make executable"]);
    write(&root, "script.sh", "#!/bin/sh\necho new\n");

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/stage-content"),
            Some(json!({ "path": "script.sh", "content": "#!/bin/sh\necho selected\n" })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert!(
        git(&root, &["ls-files", "-s", "script.sh"]).starts_with("100755 "),
        "partial staging must preserve the executable bit"
    );

    let outside = fresh_dir();
    server.cleanup.push(outside.clone());
    write(&outside, "outside.txt", "do not touch\n");
    symlink(outside.join("outside.txt"), root.join("escape.txt")).unwrap();
    for endpoint in ["stage-content", "unstage-content", "discard-content"] {
        let body = match endpoint {
            "unstage-content" => {
                json!({ "path": "escape.txt", "content": "evil\n", "exists": true })
            }
            "discard-content" => json!({
                "path": "escape.txt",
                "content": "evil\n",
                "expectedOid": "0000000000000000000000000000000000000000",
            }),
            _ => json!({ "path": "escape.txt", "content": "evil\n" }),
        };
        let (status, response) = server
            .req(
                "POST",
                &format!("/api/projects/{id}/git/{endpoint}"),
                Some(body),
            )
            .await;
        assert_eq!(status, TStatus::FORBIDDEN, "{endpoint}: {response}");
    }
    assert_eq!(
        std::fs::read_to_string(outside.join("outside.txt")).unwrap(),
        "do not touch\n"
    );
}

#[tokio::test]
async fn a_registered_repo_subdirectory_uses_project_relative_paths() {
    let mut server = TestServer::start().await;
    let repo = fresh_dir();
    git_init(&repo);
    write(&repo, "outside.txt", "outside\n");
    write(&repo, "sub/a.txt", "one\n");
    git(&repo, &["add", "--all"]);
    git(&repo, &["commit", "-m", "init"]);
    let project = repo.join("sub");
    let id = server.register(&project).await;
    // `register` records only the project root; removing its parent is what cleans
    // the repository's `.git` directory and the sibling fixture file too.
    server.cleanup.push(repo.clone());

    write(&repo, "outside.txt", "outside changed\n");
    write(&project, "a.txt", "one\ntwo\n");
    let (status, body) = server
        .req("GET", &format!("/api/projects/{id}/git/status"), None)
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert!(has_entry(&body, "a.txt"));
    assert!(
        !has_entry(&body, "sub/a.txt"),
        "wire paths must be project-relative"
    );
    assert!(
        !has_entry(&body, "outside.txt"),
        "status must not leak the parent repository"
    );

    let (status, diff) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/git/diff?path=a.txt"),
            None,
        )
        .await;
    assert_eq!(status, TStatus::OK, "{diff}");
    assert_eq!(diff["oldText"], "one\n");
    assert_eq!(diff["newText"], "one\ntwo\n");

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/stage-content"),
            Some(json!({ "path": "a.txt", "content": "one\nselected\n" })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(git(&repo, &["show", ":sub/a.txt"]), "one\nselected\n");
    assert_eq!(git(&repo, &["show", ":outside.txt"]), "outside\n");
    assert_eq!(
        std::fs::read_to_string(project.join("a.txt")).unwrap(),
        "one\ntwo\n",
        "index plumbing must not touch the project worktree"
    );

    // History is scoped and stripped the same way status and diff are. The `init`
    // commit added both `outside.txt` and `sub/a.txt`; the panel for `sub` must see
    // exactly one file, named as the project sees it.
    let (_, log) = server
        .req("GET", &format!("/api/projects/{id}/git/log?limit=1"), None)
        .await;
    let oid = log["commits"][0]["oid"].as_str().unwrap().to_string();
    let (status, detail) = server
        .req("GET", &format!("/api/projects/{id}/git/commit/{oid}"), None)
        .await;
    assert_eq!(status, TStatus::OK, "{detail}");
    let paths: Vec<&str> = detail["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        vec!["a.txt"],
        "commit detail must be scoped to the project and project-relative"
    );
    assert_eq!(detail["files"][0]["added"], 1);
}

#[tokio::test]
async fn stage_refuses_an_empty_list_and_an_escaping_path() {
    let mut server = TestServer::start().await;
    let (id, _root) = server.git_project(&[("a.txt", "one\n")]).await;

    for paths in [json!([]), json!(["../outside.txt"])] {
        let (status, body) = server
            .req(
                "POST",
                &format!("/api/projects/{id}/git/stage"),
                Some(json!({ "paths": paths })),
            )
            .await;
        assert_eq!(status, TStatus::BAD_REQUEST, "paths {paths}: {body}");
        assert_eq!(body["error"], "path");
    }
}

#[tokio::test]
async fn mutations_refuse_git_internal_paths_without_running_git() {
    // C33: staging `.git/index` would corrupt the repository, so the guard is on
    // the mutation side too — and it runs before `git` does.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    let head_before = git(&root, &["rev-parse", "HEAD"]);

    for (path, body) in [
        (
            format!("/api/projects/{id}/git/stage"),
            json!({ "paths": [".git/index"] }),
        ),
        (
            format!("/api/projects/{id}/git/discard"),
            json!({ "paths": [".git/config"] }),
        ),
        (
            format!("/api/projects/{id}/git/resolve"),
            json!({ "path": ".git/HEAD", "content": "ref: refs/heads/evil\n" }),
        ),
        (
            format!("/api/projects/{id}/git/stage-content"),
            json!({ "path": ".git/index", "content": "evil\n" }),
        ),
        (
            format!("/api/projects/{id}/git/unstage-content"),
            json!({ "path": ".git/index", "content": "evil\n", "exists": true }),
        ),
        (
            format!("/api/projects/{id}/git/discard-content"),
            json!({
                "path": ".git/config",
                "content": "evil\n",
                "expectedOid": "0000000000000000000000000000000000000000",
            }),
        ),
    ] {
        let (status, response) = server.req("POST", &path, Some(body)).await;
        assert_eq!(status, TStatus::FORBIDDEN, "{path}: {response}");
        assert_eq!(response["error"], "path");
    }

    // Nothing ran, so nothing moved.
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(
        git(&root, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "main",
        "the resolve attempt must not have rewritten HEAD"
    );
}

// ---- commit (C18–C21) ------------------------------------------------------

#[tokio::test]
async fn committing_the_index_leaves_a_clean_tree() {
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    write(&root, "a.txt", "two\n");
    git(&root, &["add", "a.txt"]);

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/commit"),
            Some(json!({ "message": "a subject line\n\nand a body paragraph." })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(body["counts"]["staged"], 0);
    assert!(body["entries"].as_array().unwrap().is_empty());

    let (_, log) = server
        .req("GET", &format!("/api/projects/{id}/git/log?limit=1"), None)
        .await;
    assert_eq!(log["commits"][0]["summary"], "a subject line");
    assert_eq!(log["commits"][0]["body"], "and a body paragraph.");
}

#[tokio::test]
async fn a_commit_message_starting_with_a_dash_is_not_an_option() {
    // Why the message goes in on stdin via `--file -` instead of `-m`.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    write(&root, "a.txt", "two\n");
    git(&root, &["add", "a.txt"]);

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/commit"),
            Some(json!({ "message": "--amend is a fine thing to write about" })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");

    let (_, log) = server
        .req("GET", &format!("/api/projects/{id}/git/log"), None)
        .await;
    assert_eq!(
        log["commits"][0]["summary"],
        "--amend is a fine thing to write about"
    );
    assert_eq!(
        log["commits"].as_array().unwrap().len(),
        2,
        "a new commit, not an amended one"
    );
}

#[tokio::test]
async fn committing_with_nothing_staged_is_409_and_creates_no_commit() {
    // C19: `--allow-empty` is deliberately absent, so git's own refusal is what
    // the client sees.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    // An unstaged edit must not count as something to commit.
    write(&root, "a.txt", "unstaged\n");
    let before = git(&root, &["rev-parse", "HEAD"]);

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/commit"),
            Some(json!({ "message": "nothing here" })),
        )
        .await;
    assert_eq!(status, TStatus::CONFLICT, "{body}");
    assert_eq!(body["error"], "nothingToCommit");
    assert_eq!(
        git(&root, &["rev-parse", "HEAD"]),
        before,
        "HEAD must not move"
    );
}

#[tokio::test]
async fn an_empty_commit_message_is_refused_before_git_runs() {
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    write(&root, "a.txt", "two\n");
    git(&root, &["add", "a.txt"]);

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/commit"),
            Some(json!({ "message": "   \n  " })),
        )
        .await;
    assert_eq!(status, TStatus::CONFLICT, "{body}");
    assert_eq!(body["error"], "blocked");
}

#[tokio::test]
async fn amend_rewrites_the_last_commit_instead_of_adding_one() {
    // C21.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    write(&root, "a.txt", "two\n");
    git(&root, &["add", "a.txt"]);

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/commit"),
            Some(json!({ "message": "reworded init", "amend": true })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");

    let (_, log) = server
        .req("GET", &format!("/api/projects/{id}/git/log"), None)
        .await;
    let commits = log["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 1, "amend must not add a commit");
    assert_eq!(commits[0]["summary"], "reworded init");
}

#[tokio::test]
async fn a_failing_pre_commit_hook_blocks_the_commit_and_its_message_reaches_the_client() {
    // C20: the whole reason mutations go through the CLI. libgit2 would have
    // committed straight past this hook.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;

    let hooks = root.join(".githooks-none");
    std::fs::create_dir_all(&hooks).unwrap();
    write(
        &root,
        ".githooks-none/pre-commit",
        "#!/bin/sh\necho 'lint failed: tabs where spaces belong' >&2\nexit 1\n",
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            hooks.join("pre-commit"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }

    write(&root, "a.txt", "two\n");
    git(&root, &["add", "a.txt"]);
    let before = git(&root, &["rev-parse", "HEAD"]);

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/commit"),
            Some(json!({ "message": "should be blocked" })),
        )
        .await;

    // On a platform without executable hooks (or without a shell) git runs no
    // hook at all, and asserting a failure would be asserting the platform.
    #[cfg(unix)]
    {
        assert_eq!(status, TStatus::INTERNAL_SERVER_ERROR, "{body}");
        assert_eq!(body["error"], "git");
        assert!(
            body["detail"]
                .as_str()
                .unwrap()
                .contains("tabs where spaces belong"),
            "the hook's own message is the only useful diagnostic: {}",
            body["detail"]
        );
        assert_eq!(
            git(&root, &["rev-parse", "HEAD"]),
            before,
            "HEAD must not move when a hook refuses"
        );
    }
    #[cfg(not(unix))]
    {
        let _ = (status, body, before);
    }
}

// ---- discard (C22–C23) -----------------------------------------------------

#[tokio::test]
async fn discard_restores_the_file_from_head() {
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "original\n")]).await;
    write(&root, "a.txt", "wrecked\n");
    // Staged as well, to prove both axes are reset ([INVENTED-2]).
    git(&root, &["add", "a.txt"]);
    write(&root, "a.txt", "wrecked twice\n");

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/discard"),
            Some(json!({ "paths": ["a.txt"] })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert!(!has_entry(&body, "a.txt"), "file should be clean: {body}");
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "original\n"
    );
}

#[tokio::test]
async fn discard_refuses_untracked_files_and_leaves_them_on_disk() {
    // C23: `git clean -f` would delete work that exists nowhere else — no
    // reflog, no stash, nothing to recover from.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    write(&root, "precious.txt", "an hour of work\n");

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/discard"),
            Some(json!({ "paths": ["precious.txt"] })),
        )
        .await;
    assert_eq!(status, TStatus::CONFLICT, "{body}");
    assert_eq!(body["error"], "blocked");
    assert!(
        body["detail"].as_str().unwrap().contains("precious.txt"),
        "{}",
        body["detail"]
    );
    assert_eq!(
        std::fs::read_to_string(root.join("precious.txt")).unwrap(),
        "an hour of work\n",
        "the file must still be there"
    );
}

#[tokio::test]
async fn a_mixed_discard_refuses_wholesale_rather_than_half_applying() {
    // Half-applying would be the worst outcome: the user asked for one action and
    // would get an unpredictable subset of it.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("tracked.txt", "original\n")]).await;
    write(&root, "tracked.txt", "changed\n");
    write(&root, "untracked.txt", "new work\n");

    let (status, _) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/discard"),
            Some(json!({ "paths": ["tracked.txt", "untracked.txt"] })),
        )
        .await;
    assert_eq!(status, TStatus::CONFLICT);
    assert_eq!(
        std::fs::read_to_string(root.join("tracked.txt")).unwrap(),
        "changed\n",
        "the tracked file must not have been restored either"
    );
}

// ---- branch, checkout (C24–C26) --------------------------------------------

#[tokio::test]
async fn creating_a_branch_optionally_switches_to_it() {
    let mut server = TestServer::start().await;
    let (id, _root) = server.git_project(&[("a.txt", "one\n")]).await;

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/branch"),
            Some(json!({ "name": "feature/no-switch" })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(body["head"]["branch"], "main", "created but not switched");

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/branch"),
            Some(json!({ "name": "feature/switch", "checkout": true })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(body["head"]["branch"], "feature/switch");
}

#[tokio::test]
async fn a_branch_can_start_from_an_older_commit() {
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    let first = git(&root, &["rev-parse", "HEAD"]).trim().to_string();
    write(&root, "a.txt", "two\n");
    git(&root, &["commit", "-am", "second"]);

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/branch"),
            Some(json!({ "name": "from-first", "startPoint": first })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(
        git(&root, &["rev-parse", "from-first"]).trim(),
        first,
        "branch should point at the older commit"
    );
}

#[tokio::test]
async fn an_option_shaped_branch_name_is_refused() {
    // The security-relevant one: `--` cannot protect a ref position, so the name
    // itself is validated (§5.4).
    let mut server = TestServer::start().await;
    let (id, _root) = server.git_project(&[("a.txt", "one\n")]).await;

    for name in [
        "--upload-pack=curl evil.example",
        "-f",
        "has space",
        "bad..name",
        "",
    ] {
        let (status, body) = server
            .req(
                "POST",
                &format!("/api/projects/{id}/git/branch"),
                Some(json!({ "name": name })),
            )
            .await;
        assert_eq!(status, TStatus::BAD_REQUEST, "name {name:?}: {body}");
        assert_eq!(body["error"], "path");
    }
}

#[tokio::test]
async fn checkout_switches_a_clean_tree() {
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    git(&root, &["branch", "other"]);

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/checkout"),
            Some(json!({ "target": "other" })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(body["head"]["branch"], "other");
    assert_eq!(body["head"]["detached"], false);
}

#[tokio::test]
async fn checkout_is_blocked_by_a_dirty_tree_and_allowed_with_force() {
    // C26 + [INVENTED-11]: plain `git checkout` would carry the edit across, and
    // with agents writing files that means edits landing on the wrong branch.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    git(&root, &["branch", "other"]);
    write(&root, "a.txt", "local edit\n");

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/checkout"),
            Some(json!({ "target": "other" })),
        )
        .await;
    assert_eq!(status, TStatus::CONFLICT, "{body}");
    assert_eq!(body["error"], "blocked");
    assert_eq!(
        git(&root, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "main",
        "still on main"
    );

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/checkout"),
            Some(json!({ "target": "other", "force": true })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(body["head"]["branch"], "other");
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "one\n",
        "force discards the edit, as asked"
    );
}

#[tokio::test]
async fn untracked_files_do_not_block_a_checkout() {
    // They are carried nowhere by a switch, so treating them as dirty would block
    // a perfectly safe operation.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;
    git(&root, &["branch", "other"]);
    write(&root, "scratch.txt", "notes\n");

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/checkout"),
            Some(json!({ "target": "other" })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert!(root.join("scratch.txt").exists(), "and it survives");
}

#[tokio::test]
async fn checking_out_an_unknown_ref_reports_gits_own_refusal() {
    let mut server = TestServer::start().await;
    let (id, _root) = server.git_project(&[("a.txt", "one\n")]).await;

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/checkout"),
            Some(json!({ "target": "no-such-branch" })),
        )
        .await;
    // git says "pathspec … did not match", which `classify` maps to notFound.
    assert!(
        status == TStatus::NOT_FOUND || status == TStatus::INTERNAL_SERVER_ERROR,
        "unexpected {status}: {body}"
    );
    assert!(!body["detail"].as_str().unwrap().is_empty());
}

// ---- merge, conflict, resolve (C27–C31) ------------------------------------

/// Two branches whose edits to `a.txt` cannot be merged automatically.
///
/// Returns the project id and root, sitting on `main` with `feature` ready to be
/// merged in.
async fn conflicting_branches(server: &mut TestServer) -> (String, PathBuf) {
    let (id, root) = server.git_project(&[("a.txt", "base\n")]).await;
    git(&root, &["checkout", "-b", "feature"]);
    write(&root, "a.txt", "theirs\n");
    git(&root, &["commit", "-am", "feature edit"]);
    git(&root, &["checkout", "main"]);
    write(&root, "a.txt", "ours\n");
    git(&root, &["commit", "-am", "main edit"]);
    (id, root)
}

#[tokio::test]
async fn a_clean_merge_brings_the_other_branch_in() {
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "base\n")]).await;
    git(&root, &["checkout", "-b", "feature"]);
    write(&root, "b.txt", "from feature\n");
    git(&root, &["add", "b.txt"]);
    git(&root, &["commit", "-m", "add b"]);
    git(&root, &["checkout", "main"]);

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/merge"),
            Some(json!({ "from": "feature" })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(body["state"], "clean");
    assert_eq!(body["counts"]["conflicted"], 0);
    assert!(root.join("b.txt").exists(), "the merged file is present");
}

#[tokio::test]
async fn no_ff_forces_a_merge_commit() {
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "base\n")]).await;
    git(&root, &["checkout", "-b", "feature"]);
    write(&root, "b.txt", "from feature\n");
    git(&root, &["add", "b.txt"]);
    git(&root, &["commit", "-m", "add b"]);
    git(&root, &["checkout", "main"]);

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/merge"),
            Some(json!({ "from": "feature", "noFf": true })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");

    let (_, log) = server
        .req("GET", &format!("/api/projects/{id}/git/log?limit=1"), None)
        .await;
    assert_eq!(
        log["commits"][0]["parents"].as_array().unwrap().len(),
        2,
        "a merge commit has two parents: {}",
        log["commits"][0]
    );
}

#[tokio::test]
async fn merging_an_ancestor_says_up_to_date_rather_than_claiming_a_merge() {
    // §9 #9: git exits 0 here, so without the distinction the UI would announce a
    // merge that never happened.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "base\n")]).await;
    git(&root, &["branch", "behind-main"]);
    write(&root, "a.txt", "moved on\n");
    git(&root, &["commit", "-am", "second"]);

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/merge"),
            Some(json!({ "from": "behind-main" })),
        )
        .await;
    assert_eq!(status, TStatus::CONFLICT, "{body}");
    assert_eq!(body["error"], "upToDate");
}

#[tokio::test]
async fn a_conflicting_merge_succeeds_into_the_conflicted_state() {
    // C28: the conflicted index is not a failure to undo — it is the state the
    // 3-way editor reads.
    let mut server = TestServer::start().await;
    let (id, root) = conflicting_branches(&mut server).await;

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/merge"),
            Some(json!({ "from": "feature" })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(body["state"], "merge");
    assert_eq!(body["counts"]["conflicted"], 1);
    assert_eq!(entry(&body, "a.txt")["conflicted"], true);
    assert!(
        std::fs::read_to_string(root.join("a.txt"))
            .unwrap()
            .contains("<<<<<<<"),
        "git wrote its markers into the worktree file"
    );
}

#[tokio::test]
async fn the_conflict_endpoint_returns_all_three_sides() {
    // C30: read from the index's conflict stages, so each side is exactly what
    // git recorded rather than a re-parse of the marker text.
    let mut server = TestServer::start().await;
    let (id, _root) = conflicting_branches(&mut server).await;
    server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/merge"),
            Some(json!({ "from": "feature" })),
        )
        .await;

    let (status, body) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/git/conflict?path=a.txt"),
            None,
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(body["base"], "base\n");
    assert_eq!(body["ours"], "ours\n");
    assert_eq!(body["theirs"], "theirs\n");
    assert_eq!(body["binary"], false);
}

#[tokio::test]
async fn asking_for_the_conflict_of_a_clean_file_is_404() {
    let mut server = TestServer::start().await;
    let (id, _root) = server.git_project(&[("a.txt", "one\n")]).await;

    let (status, body) = server
        .req(
            "GET",
            &format!("/api/projects/{id}/git/conflict?path=a.txt"),
            None,
        )
        .await;
    assert_eq!(status, TStatus::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn resolving_writes_the_content_and_clears_the_conflict() {
    // C31 + [INVENTED-12]: the client sends the resolved text; the server never
    // picks a side, because guessing is how work disappears silently.
    let mut server = TestServer::start().await;
    let (id, root) = conflicting_branches(&mut server).await;
    server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/merge"),
            Some(json!({ "from": "feature" })),
        )
        .await;

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/resolve"),
            Some(json!({ "path": "a.txt", "content": "ours and theirs\n" })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(body["counts"]["conflicted"], 0);
    assert_eq!(entry(&body, "a.txt")["conflicted"], false);
    assert_eq!(entry(&body, "a.txt")["staged"], true);
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "ours and theirs\n"
    );

    // And the merge can now be finished the ordinary way.
    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/commit"),
            Some(json!({ "message": "merge feature" })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(body["state"], "clean");
    assert_eq!(body["counts"]["conflicted"], 0);
}

#[tokio::test]
async fn aborting_a_merge_returns_to_the_pre_merge_state() {
    // C29.
    let mut server = TestServer::start().await;
    let (id, root) = conflicting_branches(&mut server).await;
    let before = git(&root, &["rev-parse", "HEAD"]);
    server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/merge"),
            Some(json!({ "from": "feature" })),
        )
        .await;

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/merge"),
            Some(json!({ "abort": true })),
        )
        .await;
    assert_eq!(status, TStatus::OK, "{body}");
    assert_eq!(body["state"], "clean");
    assert!(body["entries"].as_array().unwrap().is_empty());
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "ours\n",
        "our side is back"
    );
    assert_eq!(git(&root, &["rev-parse", "HEAD"]), before);
}

#[tokio::test]
async fn merging_something_that_is_not_a_ref_fails_without_touching_the_tree() {
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;

    let (status, body) = server
        .req(
            "POST",
            &format!("/api/projects/{id}/git/merge"),
            Some(json!({ "from": "not-a-branch" })),
        )
        .await;
    assert_ne!(status, TStatus::OK, "{body}");
    // Not a conflict: nothing was merged, so the repository is still clean.
    assert!(
        !git_may_fail(&root, &["rev-parse", "--verify", "MERGE_HEAD"]),
        "no merge should be in progress"
    );
}

// ---- watch, SSE (C32–C35) --------------------------------------------------

#[tokio::test]
async fn the_watch_stream_sends_the_current_status_then_changes() {
    // C32: first frame immediately (the panel must render without waiting a tick),
    // then a new frame when the repository changes.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;

    let mut sse = server.sse(&format!("/api/projects/{id}/git/watch")).await;

    let first = sse.next_event().await.expect("an initial status");
    assert_eq!(first.event, "status");
    let status: Value = serde_json::from_str(&first.data).expect("status json");
    assert_eq!(status["isRepo"], true);
    assert!(status["entries"].as_array().unwrap().is_empty());

    write(&root, "a.txt", "changed by somebody else\n");

    let second = sse.next_event().await.expect("a status after the edit");
    assert_eq!(second.event, "status");
    let status: Value = serde_json::from_str(&second.data).unwrap();
    assert_eq!(entry(&status, "a.txt")["worktree"], "modified");
}

#[tokio::test]
async fn an_idle_repository_sends_nothing_after_the_first_status() {
    // C33 + the fingerprint: an unchanged repo costs zero bytes on the wire, so
    // the only thing arriving during an idle window is a keep-alive comment.
    let mut server = TestServer::start().await;
    let (id, _root) = server.git_project(&[("a.txt", "one\n")]).await;

    let mut sse = server.sse(&format!("/api/projects/{id}/git/watch")).await;
    assert_eq!(sse.next_event().await.expect("initial").event, "status");

    // Well past several poll intervals.
    let extra = tokio::time::timeout(std::time::Duration::from_secs(5), sse.next_event()).await;
    assert!(
        extra.is_err(),
        "idle repo should send no further events, got {extra:?}"
    );
}

#[tokio::test]
async fn a_project_that_is_not_a_repository_still_streams_its_state() {
    // C35: "not a repository" is information the panel renders, not a reason to
    // kill the stream.
    let mut server = TestServer::start().await;
    let (id, root) = server.plain_project().await;

    let mut sse = server.sse(&format!("/api/projects/{id}/git/watch")).await;
    let first = sse.next_event().await.expect("initial status");
    let status: Value = serde_json::from_str(&first.data).unwrap();
    assert_eq!(status["isRepo"], false);

    // And it notices when the directory becomes a repository.
    git_init(&root);
    let next = sse.next_event().await.expect("status after git init");
    let status: Value = serde_json::from_str(&next.data).unwrap();
    assert_eq!(status["isRepo"], true);
    assert_eq!(status["head"]["branch"], "main");
    assert_eq!(status["head"]["oid"], Value::Null, "no commits yet");
}

#[tokio::test]
async fn watching_an_unknown_project_is_404_before_any_stream_opens() {
    let mut server = TestServer::start().await;
    let _ = server.git_project(&[("a.txt", "one\n")]).await;

    let (status, body) = server
        .req("GET", "/api/projects/does-not-exist/git/watch", None)
        .await;
    assert_eq!(status, TStatus::NOT_FOUND, "{body}");
}

/// Wait until the registry reports `want` live watchers, or give up.
///
/// Polling rather than a fixed sleep: the watcher notices its last receiver left
/// on its next tick, so the stop is one poll interval away, not instant.
async fn await_watchers(state: &AppState, want: usize) -> usize {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
    loop {
        let live = state.git.active_count().await;
        if live == want || std::time::Instant::now() >= deadline {
            return live;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn two_streams_on_one_project_share_a_single_watcher() {
    // C37: ten browser tabs on one project must not mean ten `git status` calls
    // per interval. Nothing on the wire can show this — both streams look correct
    // either way — so the registry is the witness.
    let mut server = TestServer::start().await;
    let (id, root) = server.git_project(&[("a.txt", "one\n")]).await;

    let mut first = server.sse(&format!("/api/projects/{id}/git/watch")).await;
    let mut second = server.sse(&format!("/api/projects/{id}/git/watch")).await;

    // Both are really subscribed, not just connected.
    assert_eq!(
        first.next_event().await.expect("first stream").event,
        "status"
    );
    assert_eq!(
        second.next_event().await.expect("second stream").event,
        "status"
    );
    assert_eq!(
        server.state.git.active_count().await,
        1,
        "two subscribers, one poll task"
    );

    // And one edit reaches both — sharing the watcher must not cost a client its
    // updates.
    write(&root, "a.txt", "two\n");
    for (label, stream) in [("first", &mut first), ("second", &mut second)] {
        let event = stream.next_event().await.unwrap_or_else(|| {
            panic!("{label} stream should see the edit");
        });
        let status: Value = serde_json::from_str(&event.data).unwrap();
        assert_eq!(entry(&status, "a.txt")["worktree"], "modified", "{label}");
    }
}

#[tokio::test]
async fn the_watcher_stops_when_the_last_stream_closes() {
    // C38: a closed tab must not leave a `git status` running forever. Dropping
    // the stream closes the socket, which is exactly what a closed tab does.
    let mut server = TestServer::start().await;
    let (id, _root) = server.git_project(&[("a.txt", "one\n")]).await;

    let mut first = server.sse(&format!("/api/projects/{id}/git/watch")).await;
    let mut second = server.sse(&format!("/api/projects/{id}/git/watch")).await;
    assert_eq!(first.next_event().await.expect("first").event, "status");
    assert_eq!(second.next_event().await.expect("second").event, "status");
    assert_eq!(server.state.git.active_count().await, 1);

    // One of two closing keeps the watcher alive: the other tab still wants it.
    // A fixed wait, not a poll — the claim is that nothing happens, and the only
    // way to see nothing happen is to let more than one poll interval pass.
    drop(second);
    tokio::time::sleep(std::time::Duration::from_millis(3500)).await;
    assert_eq!(
        server.state.git.active_count().await,
        1,
        "one subscriber remains, so the watcher must keep polling"
    );

    drop(first);
    assert_eq!(
        await_watchers(&server.state, 0).await,
        0,
        "no subscribers left, so the poll task must stop"
    );

    // Reconnecting works: a finished watcher is replaced, not reused, so the new
    // stream gets a live task instead of a dead channel.
    let mut again = server.sse(&format!("/api/projects/{id}/git/watch")).await;
    assert_eq!(
        again
            .next_event()
            .await
            .expect("status after reconnect")
            .event,
        "status"
    );
    assert_eq!(server.state.git.active_count().await, 1);
}

#[tokio::test]
async fn every_git_endpoint_needs_the_token() {
    // The same auth layer as every other route (SPEC-002), asserted here because
    // a git panel leaking a repository's contents unauthenticated would be the
    // worst version of this bug.
    let mut server = TestServer::start().await;
    let (id, _root) = server.git_project(&[("a.txt", "one\n")]).await;

    for (method, path) in [
        ("GET", format!("/api/projects/{id}/git/status")),
        ("GET", format!("/api/projects/{id}/git/log")),
        ("GET", format!("/api/projects/{id}/git/watch")),
        ("POST", format!("/api/projects/{id}/git/commit")),
    ] {
        let (status, _) = server
            .client
            .request_raw(method, &server.url(&path), None, None)
            .await;
        assert_eq!(status, TStatus::UNAUTHORIZED, "{method} {path}");
    }
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

    /// One `event:`/`data:` frame off an SSE stream.
    #[derive(Debug)]
    pub struct SseEvent {
        pub event: String,
        pub data: String,
    }

    /// An open SSE connection.
    ///
    /// `Client::send` cannot read this: it does `read_to_end`, and an SSE stream
    /// never ends. This reads incrementally instead, decoding the chunked
    /// framing axum uses, and the socket closes when the value is dropped —
    /// which is also what makes the server's "no subscribers left" path testable.
    pub struct SseStream {
        stream: tokio::net::TcpStream,
        /// Undecoded bytes from the socket.
        raw: Vec<u8>,
        /// Decoded body text not yet parsed into a frame.
        body: String,
    }

    impl SseStream {
        /// Open the stream, consuming the response head. Panics unless the server
        /// answered `200 text/event-stream`.
        pub async fn open(url: &str, token: &str) -> Self {
            let (authority, path) = split_url(url);
            let mut stream = tokio::net::TcpStream::connect(authority).await.unwrap();

            let request = format!(
                "GET {path} HTTP/1.1\r\nHost: {authority}\r\n{TOKEN_HEADER}: {token}\r\nAccept: text/event-stream\r\n\r\n"
            );
            stream.write_all(request.as_bytes()).await.unwrap();

            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            // Read until the head is complete — it can arrive split across reads.
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

        /// The next event, or `None` if the server closed the stream.
        ///
        /// Keep-alive comment frames (`:keep-alive`) are skipped: they carry no
        /// information a test can assert on.
        pub async fn next_event(&mut self) -> Option<SseEvent> {
            loop {
                if let Some(frame) = self.take_frame() {
                    match parse_frame(&frame) {
                        Some(event) => return Some(event),
                        // A comment-only frame — keep reading.
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

        /// Move complete HTTP chunks out of `raw` and into `body`.
        ///
        /// axum streams the response chunked, so the payload arrives as
        /// `<hex-len>\r\n<bytes>\r\n` and the SSE frames sit inside it.
        fn decode_chunks(&mut self) {
            loop {
                let Some(sep) = find(&self.raw, b"\r\n") else {
                    return;
                };
                let header = String::from_utf8_lossy(&self.raw[..sep]).into_owned();
                // Chunk extensions after `;` are legal and irrelevant here.
                let size_text = header.split(';').next().unwrap_or("").trim();
                let Ok(size) = usize::from_str_radix(size_text, 16) else {
                    panic!("bad chunk header: {header:?}");
                };
                // `size + 2` for the CRLF that terminates the chunk data.
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

        /// Split off the next `\n\n`-terminated frame, if one is complete.
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
            // Anything else (`:comment`, `id:`, `retry:`) is not asserted on.
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
