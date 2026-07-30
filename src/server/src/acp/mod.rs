//! ACP orchestration — spawn agent subprocesses and relay JSON-RPC over stdio.
//!
//! Responsibility (docs/analysis/03-acp-protocol.md, 04 §3): drive the
//! `agent-client-protocol` 2.0 connection to an agent subprocess. JSON-RPC 2.0
//! over stdio (newline-delimited). Source of truth for the protocol lives at
//! docs/references/agent-client-protocol/.
//!
//! Roadmap: Pha 3 (07-build-roadmap.md).
//!
//! Key design points:
//! - The client MUST also implement the server side of JSON-RPC: agents call
//!   back into `fs/*`, `terminal/*`, and `session/request_permission`.
//! - `session/prompt` is a long-running request; progress arrives as
//!   `session/update` notifications correlated by `sessionId`. Do NOT await a
//!   single response.
//! - Spec ADE adds its own layer on top: a monotonic event-log `seq` enabling
//!   replay via `?after_seq=N` (ACP has no built-in replay).
//!
//! CORRECTION vs the analysis docs (SPEC-003 §2.1, verified against the pinned
//! crate's source): ACP 2.0 futures are **not** `!Send` — `Role` is `Send + Sync`
//! and handler futures are `+ Send`, so there is no `LocalSet` and no dedicated
//! thread. A connection is an ordinary `tokio::spawn`. `02 §ACP threading` was
//! written against an earlier API and is stale.

pub mod agent;
pub mod connection;
pub mod event;
pub mod fs_bridge;
pub mod log;
pub mod permission;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub use connection::{AcpConnection, AcpError, AcpLimits};

/// Registry of live agent connections.
///
/// One process serves many sessions ([INVENTED-1]): ACP allows it, and a process
/// per session would cost hundreds of MB each. Keyed by Spec ADE's own connection
/// id, not a pid — a pid is reused by the OS and means nothing after exit.
#[derive(Clone, Default)]
pub struct AcpManager {
    inner: Arc<Mutex<HashMap<String, AcpConnection>>>,
    /// Spec ADE's own session records (§10 [INVENTED-9]: RAM only this phase, no
    /// `acp-history/` persistence — the format belongs to SPEC-004's chat UI).
    sessions: Arc<Mutex<HashMap<String, SessionInfo>>>,
    /// Timeouts every connection spawned here inherits.
    limits: AcpLimits,
}

/// One row of `GET /api/acp`.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionSummary {
    pub id: String,
    pub agent_id: String,
    pub project_id: String,
    pub state: &'static str,
    pub session_count: usize,
    pub session_ids: Vec<String>,
}

impl AcpManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// A manager whose connections use `limits` instead of the defaults.
    ///
    /// The seam the two aging behaviours are tested through — see [`AcpLimits`].
    pub fn with_limits(limits: AcpLimits) -> Self {
        Self {
            limits,
            ..Self::default()
        }
    }

    /// Open a session on `conn` and register it under a fresh Spec ADE id.
    ///
    /// The two ids are kept apart on purpose (see [`SessionInfo`]): `agent_session_id`
    /// is whatever the agent chose to return, `id` is what the REST/WS API keys on.
    pub async fn create_session(
        &self,
        conn: &AcpConnection,
        project_id: &str,
        cwd: PathBuf,
    ) -> Result<SessionInfo, AcpError> {
        let agent_session_id = conn.new_session(cwd.clone()).await?;
        let info = SessionInfo {
            id: uuid::Uuid::new_v4().to_string(),
            project_id: project_id.to_string(),
            connection_id: conn.id.clone(),
            agent_session_id,
            created_at: now_rfc3339(),
            cwd,
        };
        lock(&self.sessions).insert(info.id.clone(), info.clone());
        Ok(info)
    }

    pub fn list_sessions(&self, project_id: &str) -> Vec<SessionInfo> {
        lock(&self.sessions)
            .values()
            .filter(|s| s.project_id == project_id)
            .cloned()
            .collect()
    }

    pub fn get_session(&self, id: &str) -> Option<SessionInfo> {
        lock(&self.sessions).get(id).cloned()
    }

    pub fn remove_session(&self, id: &str) -> Option<SessionInfo> {
        lock(&self.sessions).remove(id)
    }

    /// Spawn an agent and register it once the handshake succeeds.
    ///
    /// Registration happens **after** `initialize`, so a failed spawn never leaves
    /// a phantom row in `GET /api/acp` (SPEC-003 A2).
    pub async fn spawn(
        &self,
        entry: &agent::AcpAgentEntry,
        project_id: &str,
    ) -> Result<AcpConnection, AcpError> {
        let conn = AcpConnection::spawn(entry, project_id, self.limits).await?;
        lock(&self.inner).insert(conn.id.clone(), conn.clone());
        Ok(conn)
    }

    /// Look up a connection, treating a dead one as absent.
    ///
    /// A connection whose process exited is also unregistered here, so the reaping
    /// is driven by use rather than needing a sweeper task (A19).
    pub fn get(&self, id: &str) -> Option<AcpConnection> {
        let mut guard = lock(&self.inner);
        match guard.get(id) {
            Some(conn) if conn.is_closed() => {
                guard.remove(id);
                None
            }
            Some(conn) => Some(conn.clone()),
            None => None,
        }
    }

    /// Live connections, dropping any whose process has exited.
    pub fn list(&self) -> Vec<ConnectionSummary> {
        let mut guard = lock(&self.inner);
        guard.retain(|_, conn| !conn.is_closed());
        guard
            .values()
            .map(|conn| ConnectionSummary {
                id: conn.id.clone(),
                agent_id: conn.agent_id.clone(),
                project_id: conn.project_id.clone(),
                state: "ready",
                session_count: conn.session_count(),
                session_ids: conn.session_ids(),
            })
            .collect()
    }

    /// Find which connection owns an agent session id.
    pub fn find_by_session(&self, session_id: &str) -> Option<AcpConnection> {
        lock(&self.inner)
            .values()
            .find(|conn| conn.session_ids().iter().any(|s| s == session_id))
            .cloned()
    }

    /// Kill a connection and unregister it. `false` if there was no such id.
    pub async fn kill(&self, id: &str) -> bool {
        let conn = lock(&self.inner).remove(id);
        match conn {
            Some(conn) => {
                conn.shutdown().await;
                true
            }
            None => false,
        }
    }

    /// Kill every connection belonging to a project — used when the project is
    /// deregistered, so its agents don't outlive it holding a stale `cwd`.
    pub async fn kill_project(&self, project_id: &str) -> usize {
        let doomed: Vec<AcpConnection> = {
            let mut guard = lock(&self.inner);
            let ids: Vec<String> = guard
                .values()
                .filter(|c| c.project_id == project_id)
                .map(|c| c.id.clone())
                .collect();
            ids.into_iter().filter_map(|id| guard.remove(&id)).collect()
        };
        let count = doomed.len();
        for conn in doomed {
            conn.shutdown().await;
        }
        lock(&self.sessions).retain(|_, s| s.project_id != project_id);
        count
    }
}

/// Current time as RFC 3339 (UTC), with no extra date/time crate: the format is
/// simple enough to hand-render and one fewer dependency for one timestamp field.
fn now_rfc3339() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    rfc3339_from_unix(now.as_secs(), now.subsec_millis())
}

/// The pure part of [`now_rfc3339`], split out so the calendar math (civil-from-days,
/// Howard Hinnant's algorithm; no leap-second handling) is testable without
/// depending on the current time.
fn rfc3339_from_unix(secs: u64, millis: u32) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hour, min, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{min:02}:{sec:02}.{millis:03}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_renders_known_instants() {
        // Hand-rolled calendar math deserves fixed reference points rather than
        // trust. Values cross-checked against `date -u -r <epoch>`.
        assert_eq!(rfc3339_from_unix(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            rfc3339_from_unix(1_000_000_000, 5),
            "2001-09-09T01:46:40.005Z"
        );
        // A leap day — the case an off-by-one in the era math would break.
        assert_eq!(
            rfc3339_from_unix(1_709_164_800, 0),
            "2024-02-29T00:00:00.000Z"
        );
        // End of a century that is not a leap year (2100 is not).
        assert_eq!(
            rfc3339_from_unix(4_107_542_399, 999),
            "2100-02-28T23:59:59.999Z"
        );
    }

    #[test]
    fn now_agrees_with_the_pure_renderer() {
        // Guards against the two copies of the algorithm drifting apart.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        assert_eq!(
            rfc3339_from_unix(now.as_secs(), now.subsec_millis())[..17],
            now_rfc3339()[..17],
            "date+hour+minute must match"
        );
    }
}

/// A session as the REST layer reports it (SPEC-003 §3.1).
///
/// `id` is Spec ADE's key; `agent_session_id` is what the agent assigned. They are
/// kept distinct because an agent may reuse or format ids however it likes, and
/// the UI needs a key it owns.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: String,
    pub project_id: String,
    pub connection_id: String,
    pub agent_session_id: String,
    pub created_at: String,
    /// Project root the session runs in, and the `fs/*` sandbox root.
    pub cwd: PathBuf,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
