//! Git state watcher (SPEC-005 §5.6, [SPEC-005 INVENTED-8]).
//!
//! One background task per watched project polls the status every
//! [`POLL_INTERVAL`] and broadcasts it **only when a fingerprint changes**, so an
//! idle repository costs one `git status` per interval and zero bytes on the wire.
//!
//! Why polling instead of the `notify` crate:
//!
//! - it would have to watch both `.git/` and the whole worktree, and `.git/` churns
//!   constantly (`index.lock`, `ORIG_HEAD`, refs) — every event needs a status call
//!   anyway to know whether anything user-visible changed;
//! - macOS FSEvents coalesces, so a debounce of roughly this length is required
//!   regardless. Same latency, more moving parts;
//! - a recursive watcher on a large repo dies silently past `ulimit -n`, and a
//!   watcher that stops working without saying so is worse than a slow one.
//!
//! `BroadcastStream` `Lagged` is benign here, unlike the PTY stream in SPEC-001:
//! the payload is *state*, not an event log, so a subscriber that fell behind wants
//! the newest value and nothing else. Dropped intermediate values are exactly right.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, broadcast};

use super::repo::{self, GitStatus};

/// How often the poll runs ([SPEC-005 INVENTED-8]).
///
/// 1.5s keeps the ≤2s freshness the spec promises (C32) with headroom for a status
/// call on a large repository.
const POLL_INTERVAL: Duration = Duration::from_millis(1500);

/// Consecutive failures before the watcher gives up and lets clients fall back to
/// manual refresh (C34).
const MAX_FAILURES: u32 = 3;

/// Buffer depth. Small on purpose: only the newest value matters, so a deep buffer
/// would just hold stale states alive.
const CHANNEL_CAPACITY: usize = 8;

/// What a subscriber receives.
#[derive(Debug, Clone)]
pub enum WatchEvent {
    /// New status — send it to the client as an SSE `status` event.
    Status(Arc<GitStatus>),
    /// The watcher gave up after [`MAX_FAILURES`]; the client should switch to
    /// polling `GET …/git/status` itself (C34).
    Stopped { reason: String },
}

/// A live watcher for one project root.
struct Watcher {
    tx: broadcast::Sender<WatchEvent>,
    /// Kills the poll task on drop.
    _task: tokio::task::JoinHandle<()>,
}

/// Registry of watchers, one per project root. Lives in `AppState`.
#[derive(Clone, Default)]
pub struct GitWatchers {
    inner: Arc<Mutex<HashMap<PathBuf, Watcher>>>,
}

impl std::fmt::Debug for GitWatchers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GitWatchers")
    }
}

impl GitWatchers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribe to `root`, starting the poll task if this is the first subscriber.
    ///
    /// Late subscribers share the running task, so ten browser tabs on one project
    /// still cost one `git status` per interval (C33).
    pub async fn subscribe(&self, root: &Path) -> broadcast::Receiver<WatchEvent> {
        let mut map = self.inner.lock().await;

        // A watcher whose task has finished (it hit MAX_FAILURES) must be replaced,
        // not reused: subscribing to its channel would never yield anything.
        if let Some(existing) = map.get(root)
            && !existing._task.is_finished()
        {
            return existing.tx.subscribe();
        }

        let (tx, rx) = broadcast::channel(CHANNEL_CAPACITY);
        let task = tokio::spawn(poll_loop(root.to_path_buf(), tx.clone()));
        map.insert(root.to_path_buf(), Watcher { tx, _task: task });
        rx
    }

    /// Stop watching `root` — called when a project is removed.
    pub async fn stop(&self, root: &Path) {
        if let Some(watcher) = self.inner.lock().await.remove(root) {
            watcher._task.abort();
        }
    }

    /// Stop every watcher. Used on shutdown and in tests.
    pub async fn stop_all(&self) {
        for (_, watcher) in self.inner.lock().await.drain() {
            watcher._task.abort();
        }
    }

    /// How many roots currently have a **live** poll task.
    ///
    /// Public because C37/C38 are claims about watcher lifecycle, and the only
    /// honest way to assert "one watcher for two subscribers" or "the watcher
    /// stopped" is to ask the registry. Registered-but-finished entries are not
    /// counted: a task that returned after its last receiver left is stopped, and
    /// reporting it as alive would let the leak C38 guards against pass.
    pub async fn active_count(&self) -> usize {
        self.inner
            .lock()
            .await
            .values()
            .filter(|w| !w._task.is_finished())
            .count()
    }
}

/// Cheap value that changes whenever anything the client renders changes.
///
/// Comparing whole `GitStatus` values would work too, but this is a single
/// allocation-free-ish string built once per poll rather than a deep compare of a
/// vector of DTOs, and it makes "what counts as a change" explicit.
fn fingerprint(status: &GitStatus) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(64 + status.entries.len() * 24);

    let _ = write!(out, "{}|{}", status.is_repo, status.state);
    if let Some(head) = &status.head {
        let _ = write!(
            out,
            "|{}|{}|{}",
            head.branch.as_deref().unwrap_or(""),
            head.detached,
            head.oid.as_deref().unwrap_or("")
        );
    }
    if let Some(up) = &status.upstream {
        // ahead/behind are in the branch bar, so a fetch that moves the upstream
        // must invalidate the fingerprint (C36).
        let _ = write!(out, "|{}|{}|{}", up.name, up.ahead, up.behind);
    }
    for entry in &status.entries {
        let _ = write!(
            out,
            "\n{}|{}|{}|{}",
            entry.path, entry.index, entry.worktree, entry.conflicted
        );
    }
    out
}

/// The poll task: read status, compare fingerprint, broadcast on change.
async fn poll_loop(root: PathBuf, tx: broadcast::Sender<WatchEvent>) {
    let mut last: Option<String> = None;
    let mut failures = 0u32;
    // `MissedTickBehavior::Delay` (the default) is what we want: a slow status call
    // must not cause a burst of catch-up ticks.
    let mut ticker = tokio::time::interval(POLL_INTERVAL);

    loop {
        ticker.tick().await;

        // No subscribers left: stop polling. A new subscriber gets a fresh task
        // because `subscribe` replaces finished watchers.
        if tx.receiver_count() == 0 {
            return;
        }

        let path = root.clone();
        let read = tokio::task::spawn_blocking(move || {
            if repo::is_repo(&path) {
                repo::status(&path)
            } else {
                // Not a repository is a valid state, not a failure: a project can
                // become one while the panel is open (C5, C35).
                Ok(GitStatus::not_a_repo())
            }
        })
        .await;

        let status = match read {
            Ok(Ok(status)) => status,
            Ok(Err(e)) => {
                failures += 1;
                if failures >= MAX_FAILURES {
                    let _ = tx.send(WatchEvent::Stopped {
                        reason: format!("git status failed {failures} times: {e}"),
                    });
                    return;
                }
                continue;
            }
            // The blocking task panicked or was cancelled — treat as a failure and
            // let the same threshold apply.
            Err(e) => {
                failures += 1;
                if failures >= MAX_FAILURES {
                    let _ = tx.send(WatchEvent::Stopped {
                        reason: format!("git watcher task failed: {e}"),
                    });
                    return;
                }
                continue;
            }
        };

        // Any success resets the counter: three *consecutive* failures is the gate,
        // so a transient `index.lock` collision does not eventually kill a healthy
        // watcher.
        failures = 0;

        let print = fingerprint(&status);
        if last.as_deref() == Some(print.as_str()) {
            continue;
        }
        last = Some(print);

        // `send` fails only when every receiver is gone, which the check above
        // already handles; either way the next tick returns.
        if tx.send(WatchEvent::Status(Arc::new(status))).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::repo::{HeadInfo, StatusEntryDto, UpstreamInfo};

    fn base() -> GitStatus {
        GitStatus {
            is_repo: true,
            head: Some(HeadInfo {
                branch: Some("main".into()),
                detached: false,
                oid: Some("abc123".into()),
            }),
            upstream: None,
            state: "clean",
            entries: Vec::new(),
            counts: Default::default(),
        }
    }

    fn entry(path: &str, index: &'static str, worktree: &'static str) -> StatusEntryDto {
        StatusEntryDto {
            path: path.into(),
            orig_path: None,
            index,
            worktree,
            conflicted: false,
            staged: index != "none",
        }
    }

    #[test]
    fn identical_status_has_identical_fingerprint() {
        assert_eq!(fingerprint(&base()), fingerprint(&base()));
    }

    #[test]
    fn a_new_commit_changes_the_fingerprint() {
        let mut after = base();
        after.head.as_mut().unwrap().oid = Some("def456".into());
        assert_ne!(fingerprint(&base()), fingerprint(&after));
    }

    #[test]
    fn staging_the_same_file_changes_the_fingerprint() {
        // The case a naive "count the entries" fingerprint would miss: staging
        // moves a file between groups without changing how many there are (C36).
        let mut before = base();
        before.entries.push(entry("a.txt", "none", "modified"));
        let mut after = base();
        after.entries.push(entry("a.txt", "modified", "none"));
        assert_ne!(fingerprint(&before), fingerprint(&after));
    }

    #[test]
    fn upstream_movement_changes_the_fingerprint() {
        let mut before = base();
        before.upstream = Some(UpstreamInfo {
            name: "origin/main".into(),
            ahead: 0,
            behind: 0,
        });
        let mut after = before.clone();
        after.upstream.as_mut().unwrap().behind = 2;
        assert_ne!(fingerprint(&before), fingerprint(&after));
    }

    #[test]
    fn conflict_resolution_changes_the_fingerprint() {
        let mut before = base();
        let mut conflicted = entry("a.txt", "none", "modified");
        conflicted.conflicted = true;
        before.entries.push(conflicted);
        let mut after = base();
        after.entries.push(entry("a.txt", "none", "modified"));
        assert_ne!(fingerprint(&before), fingerprint(&after));
    }

    #[test]
    fn branch_switch_and_repo_state_change_the_fingerprint() {
        let mut renamed = base();
        renamed.head.as_mut().unwrap().branch = Some("feature".into());
        assert_ne!(fingerprint(&base()), fingerprint(&renamed));

        let mut merging = base();
        merging.state = "merge";
        assert_ne!(fingerprint(&base()), fingerprint(&merging));
    }

    #[tokio::test]
    async fn subscribers_to_one_root_share_a_single_watcher() {
        let watchers = GitWatchers::new();
        let dir = std::env::temp_dir().join("spec-ade-watch-share");
        std::fs::create_dir_all(&dir).unwrap();

        let _a = watchers.subscribe(&dir).await;
        let _b = watchers.subscribe(&dir).await;
        assert_eq!(
            watchers.active_count().await,
            1,
            "one task for two subscribers"
        );

        watchers.stop(&dir).await;
        assert_eq!(watchers.active_count().await, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_non_repository_reports_state_rather_than_failing() {
        // C35: a project that is not a repo must yield `isRepo: false` on the
        // stream, not silently stop it.
        let watchers = GitWatchers::new();
        let dir = std::env::temp_dir().join("spec-ade-watch-norepo");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut rx = watchers.subscribe(&dir).await;
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("watcher should emit within 5s")
            .expect("channel open");

        match event {
            WatchEvent::Status(status) => assert!(!status.is_repo),
            WatchEvent::Stopped { reason } => panic!("should not stop: {reason}"),
        }

        watchers.stop_all().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
