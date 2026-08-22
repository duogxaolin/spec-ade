//! The live half of Claws (SPEC-007 §5.3): one task per running Claw.
//!
//! Structure mirrors [`crate::git::GitWatchers`] — a `std::sync::Mutex` HashMap
//! keyed by claw id, each value holding the task handle plus the status the REST
//! layer reads. Three rules are load-bearing:
//!
//! - **No polling interval** ([INVENTED-12]). The loop wakes on
//!   `sleep_until(next fire)`; a daily Claw sleeps ~86 400× longer than it runs.
//!   A Claw with no enabled schedule parks on `pending().await` forever —
//!   `abort()` is the exit. The only repeated wake is a 30-day cap that exists
//!   so a pathological pattern (Feb-29-only) cannot overflow the timer.
//! - **The map lock never crosses an `.await`** (SPEC-007 §9.3). Every critical
//!   section is a lookup/insert/remove; the guard drops before anything awaits.
//! - **Status outlives the task.** A Claw that ended in `error` keeps its map
//!   entry so `GET /api/claws` still reports `lastError` (E21/E39/E40). Only a
//!   new `start` replaces the entry.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::RecvError;

use crate::acp::AcpError;
use crate::acp::AcpManager;
use crate::acp::SessionInfo;
use crate::acp::agent::AcpAgentEntry;
use crate::acp::connection::{AcpConnection, WatcherGuard};
use crate::acp::event::{AcpEvent, SessionState};
use crate::acp::log::LoggedEvent;
use crate::acp::policy::PermissionPolicy;
use crate::claws::{ClawDefinition, ClawError, ClawState, ClawStatus, PermissionMode};
use crate::settings::Settings;

use super::skill;

/// keepAlive ceiling (`claws.mdx:50`). Three restarts per streak, counted per
/// death, reset by a completed turn or a manual start ([INVENTED-9]).
const MAX_RESTARTS: u32 = 3;

/// Upper bound on one `sleep_until` stretch.
///
/// Not a poll: normal crons never hit it (largest legal gap is days). It exists
/// so a pathological pattern — `0 0 29 2 *` can be years away — cannot push a
/// `tokio` Instant anywhere near overflow. Waking early just recomputes.
const MAX_SLEEP: Duration = Duration::from_secs(60 * 60 * 24 * 30);

/// Live event stream of the Claw's session.
type Events = Receiver<LoggedEvent>;

/// Status shared between the task and the map reader.
///
/// A mutex rather than channels because the reader polls whenever a `GET` lands
/// and must see the **final** state even after the task is gone.
#[derive(Debug)]
struct SharedStatus {
    state: ClawState,
    connection_id: Option<String>,
    session_id: Option<String>,
    restarts: u32,
    last_run_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
}

impl SharedStatus {
    fn starting() -> Self {
        Self {
            state: ClawState::Starting,
            connection_id: None,
            session_id: None,
            // A manual start begins a fresh streak ([INVENTED-9]); a brand-new
            // SharedStatus carries that by construction.
            restarts: 0,
            last_run_at: None,
            last_error: None,
        }
    }
}

type SharedStatusHandle = Arc<Mutex<SharedStatus>>;

/// Everything the loop needs to talk to one live connection.
struct Live {
    conn: AcpConnection,
    session: SessionInfo,
    events: Events,
    /// Held for the connection's lifetime: while it lives the connection is not
    /// idle-reaped ([SPEC-003 INVENTED-10]). Dropping it on any exit path —
    /// including `abort()` — is the whole point of RAII here.
    _watcher: WatcherGuard,
}

/// One running (or terminally-failed) Claw.
struct ClawTask {
    /// Killed by `.abort()` — the only stop mechanism. There is no graceful
    /// channel because every stop (user, cascade, shutdown) means "now".
    handle: tokio::task::JoinHandle<()>,
    shared: SharedStatusHandle,
    project_id: String,
}

/// Registry of running Claws. Lives in `AppState`.
#[derive(Clone, Default)]
pub struct ClawRuntime {
    inner: Arc<Mutex<HashMap<String, ClawTask>>>,
}

impl std::fmt::Debug for ClawRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClawRuntime")
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl ClawRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a Claw: bring up the ACP connection synchronously (so a bad agent
    /// surfaces as [`ClawError::Spawn`] — the 502 of E21 — to the caller), then
    /// hand scheduling to a background task.
    ///
    /// A live task for the same id is [`ClawError::Conflict`] (E18); a
    /// *finished* one (ended in `error`) is replaceable.
    pub async fn start(
        &self,
        def: &ClawDefinition,
        agent: AcpAgentEntry,
        root: PathBuf,
        acp: &AcpManager,
    ) -> Result<(), ClawError> {
        {
            let guard = lock(&self.inner);
            if let Some(task) = guard.get(&def.id)
                && !task.handle.is_finished()
            {
                return Err(ClawError::Conflict(format!(
                    "claw {} is already running",
                    def.id
                )));
            }
            // Guard dropped here, before the first `.await` (§9.3).
        }

        let shared: SharedStatusHandle = Arc::new(Mutex::new(SharedStatus::starting()));
        let live = match connect_fresh(def, &agent, &root, acp, &shared).await {
            Ok(live) => live,
            Err(reason) => {
                // E21: a failed spawn must stay visible in `GET /api/claws` as
                // `state = error`, so the entry is stored even though there is no
                // loop to run. An instantly-finished placeholder keeps the slot
                // replaceable by the next `start`.
                set_error(&shared, reason.clone());
                lock(&self.inner).insert(
                    def.id.clone(),
                    ClawTask {
                        handle: tokio::spawn(async {}),
                        shared,
                        project_id: def.project_id.clone(),
                    },
                );
                return Err(ClawError::Spawn(reason));
            }
        };

        let handle = tokio::spawn(run_loop(
            def.clone(),
            agent.clone(),
            root,
            acp.clone(),
            live,
            shared.clone(),
        ));

        // Inserting over a racing second start must not orphan the loser's task:
        // abort whatever sat in the slot before us.
        let id = def.id.clone();
        let previous = lock(&self.inner).insert(
            id.clone(),
            ClawTask {
                handle,
                shared,
                project_id: def.project_id.clone(),
            },
        );
        if let Some(old) = previous {
            old.handle.abort();
            let _ = acp.kill(&connection_id_for(&id)).await;
        }
        Ok(())
    }

    /// Start every enabled `autoStart` Claw (SPEC-007 §5.7, [INVENTED-5]).
    ///
    /// Best-effort: one broken Claw logs a warning and does not stop the rest —
    /// boot must succeed with a hand-written settings file.
    pub async fn autostart(&self, settings: &Settings, acp: &AcpManager) {
        for def in settings.claws.iter().filter(|c| c.auto_start && c.enabled) {
            let Some(agent) = settings
                .acp_agents
                .iter()
                .find(|a| a.id == def.agent_id)
                .cloned()
            else {
                tracing::warn!("claw {} autostart: unknown agent {}", def.id, def.agent_id);
                continue;
            };
            let Some(project) = settings.projects.iter().find(|p| p.id == def.project_id) else {
                tracing::warn!(
                    "claw {} autostart: unknown project {}",
                    def.id,
                    def.project_id
                );
                continue;
            };
            if let Err(e) = self
                .start(def, agent, PathBuf::from(&project.path), acp)
                .await
            {
                tracing::warn!("claw {} autostart failed: {e}", def.id);
            }
        }
    }

    /// Stop `id`: abort the loop, kill the connection, remove the entry — the
    /// removal happens under one map lock before anything awaits (§5.3).
    ///
    /// Idempotent by construction: no entry is still a successful stop (E20).
    pub async fn stop(&self, id: &str, acp: &AcpManager) {
        let task = lock(&self.inner).remove(id);
        if let Some(task) = task {
            task.handle.abort();
            let _ = acp.kill(&connection_id_for(id)).await;
        }
    }

    /// Stop every Claw of one project — the delete cascade (SPEC-007 §5.6).
    pub async fn stop_project(&self, project_id: &str, acp: &AcpManager) {
        let doomed: Vec<(String, ClawTask)> = {
            let mut guard = lock(&self.inner);
            let ids: Vec<String> = guard
                .iter()
                .filter(|(_, t)| t.project_id == project_id)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| guard.remove(&id).map(|task| (id, task)))
                .collect()
        };
        for (id, task) in doomed {
            task.handle.abort();
            let _ = acp.kill(&connection_id_for(&id)).await;
        }
    }

    /// Stop everything — graceful shutdown (SPEC-007 §5.7).
    pub async fn stop_all(&self, acp: &AcpManager) {
        let doomed: Vec<(String, ClawTask)> = lock(&self.inner).drain().collect();
        for (id, task) in doomed {
            task.handle.abort();
            let _ = acp.kill(&connection_id_for(&id)).await;
        }
    }

    /// The read-only view merged into a `GET /api/claws` row.
    ///
    /// `nextRunAt` is recomputed from the definition on every call
    /// ([INVENTED-11]) — a cached value could only ever be stale.
    pub fn status(&self, def: &ClawDefinition) -> ClawStatus {
        let mut status = {
            let guard = lock(&self.inner);
            match guard.get(&def.id) {
                Some(task) => {
                    let s = lock(&task.shared);
                    ClawStatus {
                        state: s.state,
                        connection_id: s.connection_id.clone(),
                        session_id: s.session_id.clone(),
                        restarts: s.restarts,
                        last_run_at: s.last_run_at,
                        last_error: s.last_error.clone(),
                        next_run_at: None,
                        schedule_count: def.enabled_schedule_count(),
                        schedule_descriptions: def.schedule_descriptions(),
                    }
                }
                // No entry = never started, or stopped: both report stopped.
                None => ClawStatus {
                    state: ClawState::Stopped,
                    connection_id: None,
                    session_id: None,
                    restarts: 0,
                    last_run_at: None,
                    last_error: None,
                    next_run_at: None,
                    schedule_count: def.enabled_schedule_count(),
                    schedule_descriptions: def.schedule_descriptions(),
                },
            }
        };
        status.next_run_at = def.next_run_at(Utc::now());
        status
    }
}

/// The ACP-side id every connection of this Claw carries (`claws.mdx:43`,
/// SPEC-007 §5.3) — constant across respawns, so a stop always finds the
/// current one.
fn connection_id_for(claw_id: &str) -> String {
    format!("claw:{claw_id}")
}

fn policy_for(mode: PermissionMode) -> PermissionPolicy {
    match mode {
        PermissionMode::AutoApprove => PermissionPolicy::AutoApprove,
        PermissionMode::DenyAll => PermissionPolicy::DenyAll,
        PermissionMode::AskViaUi => PermissionPolicy::AskViaUi,
    }
}

fn set_state(shared: &SharedStatusHandle, state: ClawState) {
    lock(shared).state = state;
}

fn set_error(shared: &SharedStatusHandle, message: String) {
    let mut s = lock(shared);
    s.state = ClawState::Error;
    s.last_error = Some(message);
}

/// A completed turn resets the restart streak ([INVENTED-9]) and counts as a
/// run — including the opening skill turn.
fn note_healthy_turn(shared: &SharedStatusHandle, at: DateTime<Utc>) {
    let mut s = lock(shared);
    s.restarts = 0;
    s.last_run_at = Some(at);
}

/// Spawn the connection, open a session, attach to its event stream.
///
/// Every failure path tears down what it managed to build, so a half-alive
/// connection never lingers in [`AcpManager`].
async fn connect_fresh(
    def: &ClawDefinition,
    agent: &AcpAgentEntry,
    root: &Path,
    acp: &AcpManager,
    shared: &SharedStatusHandle,
) -> Result<Live, String> {
    set_state(shared, ClawState::Starting);
    let conn = acp
        .spawn_with(
            agent,
            &def.project_id,
            policy_for(def.permission_mode),
            Some(connection_id_for(&def.id)),
        )
        .await
        .map_err(|e| e.to_string())?;

    let session = match acp
        .create_session(&conn, &def.project_id, root.to_path_buf())
        .await
    {
        Ok(s) => s,
        Err(e) => {
            let _ = acp.kill(&conn.id).await;
            return Err(e.to_string());
        }
    };

    let (_replay, events, _state, watcher) = match conn.attach(&session.agent_session_id, 0) {
        Ok(attached) => attached,
        Err(e) => {
            let _ = acp.kill(&conn.id).await;
            return Err(format!("cannot attach to claw session: {e}"));
        }
    };

    {
        let mut s = lock(shared);
        s.connection_id = Some(conn.id.clone());
        s.session_id = Some(session.id.clone());
    }

    Ok(Live {
        conn,
        session,
        events,
        _watcher: watcher,
    })
}

/// What one `session/prompt` round-trip ended in.
enum TurnOutcome {
    /// The agent reached Idle again.
    Completed,
    /// The connection or session is gone; the string becomes `lastError`.
    Dead(String),
}

/// Send one prompt and wait for the turn to end.
///
/// Waiting is mandatory, not stylistic: `conn.prompt` resolves when the turn
/// *begins*, so firing a schedule's prompts back-to-back would feed every one
/// after the first into `AcpError::Busy` — exactly the swallowed-prompt bug
/// §9.6 warns about.
async fn run_turn(
    conn: &AcpConnection,
    session: &SessionInfo,
    events: &mut Events,
    text: String,
) -> TurnOutcome {
    if let Err(e) = conn.prompt(&session.agent_session_id, text).await {
        return match e {
            // Unreachable behind the sequential loop, but a manual prompt on the
            // same session could collide: dropping ours beats tearing down.
            AcpError::Busy => {
                tracing::warn!("claw session {}: prompt arrived while busy", session.id);
                TurnOutcome::Completed
            }
            other => TurnOutcome::Dead(other.to_string()),
        };
    }
    loop {
        match events.recv().await {
            Ok(logged) => match logged.event {
                AcpEvent::SessionState {
                    state: SessionState::Idle,
                } => return settle(conn, events).await,
                AcpEvent::SessionState {
                    state: SessionState::Closed,
                } => return TurnOutcome::Dead("session closed".to_string()),
                AcpEvent::ConnectionClosed { reason } => return TurnOutcome::Dead(reason),
                _ => {}
            },
            // Capacity is 256 and the Claw is the sole consumer; the next state
            // event still resolves the turn.
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => {
                return TurnOutcome::Dead("event stream closed".to_string());
            }
        }
    }
}

/// Decide whether an Idle really means "turn completed".
///
/// When the agent process dies mid-turn, two tasks race to report it:
/// `start_turn`'s failure path sees the slot still in `Prompting` (teardown has
/// not flipped it yet) and dutifully emits `SessionState::Idle`, while
/// `ConnectionTask::run` teardown emits `Closed` + `ConnectionClosed`. If we
/// declared `Completed` on the first Idle we would win the race exactly
/// backwards — `note_healthy_turn` would reset the restart streak on every
/// death, and a keepAlive Claw would respawn forever without ever reaching
/// [`MAX_RESTARTS`] ([INVENTED-9] integrity). So an Idle is only trusted after a
/// bounded sweep of the already-buffered stream: a death surfaces its
/// `ConnectionClosed`/`Closed` within a few milliseconds, a live agent surfaces
/// nothing, and the sweep costs one Idle-wait of ~150 ms at most.
async fn settle(conn: &AcpConnection, events: &mut Events) -> TurnOutcome {
    const SWEEP: Duration = Duration::from_millis(150);
    const STEP: Duration = Duration::from_millis(25);

    let deadline = tokio::time::Instant::now() + SWEEP;
    loop {
        match events.try_recv() {
            Ok(logged) => match logged.event {
                AcpEvent::SessionState {
                    state: SessionState::Closed,
                } => return TurnOutcome::Dead("session closed".to_string()),
                AcpEvent::ConnectionClosed { reason } => return TurnOutcome::Dead(reason),
                // A second Idle (or anything else) changes nothing.
                _ => {}
            },
            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                return TurnOutcome::Dead("event stream closed".to_string());
            }
            // Empty or lagged: wait a step, unless the sweep is exhausted —
            // then the connection itself is the tiebreaker.
            Err(_) => {
                if tokio::time::Instant::now() >= deadline {
                    return if conn.is_closed() {
                        TurnOutcome::Dead("connection closed".to_string())
                    } else {
                        TurnOutcome::Completed
                    };
                }
                tokio::time::sleep(STEP).await;
            }
        }
    }
}

/// Collapse everything already buffered on the stream into `(busy, dead)`.
///
/// The Claw is the only writer of prompts on its connection, so "busy" tracked
/// here is exactly "a turn is in flight" (E36) — no peeking into the session
/// slot needed.
fn drain(events: &mut Events) -> (bool, Option<String>) {
    let mut busy = false;
    let mut dead = None;
    while let Ok(logged) = events.try_recv() {
        match logged.event {
            AcpEvent::SessionState {
                state: SessionState::Prompting,
            } => busy = true,
            AcpEvent::SessionState {
                state: SessionState::Idle,
            } => busy = false,
            AcpEvent::SessionState {
                state: SessionState::Closed,
            } => dead = Some("session closed".to_string()),
            AcpEvent::ConnectionClosed { reason } => dead = Some(reason),
            _ => {}
        }
    }
    (busy, dead)
}

/// Deliver the opening skill prompt ([INVENTED-7]): the SKILL.md **body** as a
/// plain `session/prompt`, never a slash command — the pinned ACP schema has no
/// client→agent command channel, and `/review-pr` to an agent that never learnt
/// the word is just a wasted chat bubble.
///
/// Sent on *every* fresh connection (initial start, keepAlive restart,
/// restartOnTrigger respawn): a respawned agent has no memory, and the skill is
/// its entire briefing.
async fn open_skill(
    def: &ClawDefinition,
    live: &mut Live,
    root: &Path,
    shared: &SharedStatusHandle,
) -> Result<(), String> {
    let Some(skill_name) = &def.skill else {
        return Ok(()); // A prompts-only Claw opens with nothing ([INVENTED-4]).
    };

    let text = opening_prompt(root, skill_name).await?;
    set_state(shared, ClawState::Running);
    match run_turn(&live.conn, &live.session, &mut live.events, text).await {
        TurnOutcome::Completed => {
            note_healthy_turn(shared, Utc::now());
            Ok(())
        }
        TurnOutcome::Dead(reason) => Err(format!("opening skill turn failed: {reason}")),
    }
}

/// Look up a skill by name and return its prompt body.
///
/// Discovery walks eight directories — blocking filesystem work stays off the
/// async thread (SPEC-007 §9.2), same as every SPEC-002 file read.
async fn opening_prompt(root: &Path, name: &str) -> Result<String, String> {
    let root = root.to_path_buf();
    let name = name.to_string();
    tokio::task::spawn_blocking(move || -> Result<String, String> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        skill::discover(&root, home.as_deref())
            .into_iter()
            .find(|s| s.name == name)
            .map(|s| s.prompt)
            .ok_or_else(|| format!("skill '{name}' not found"))
    })
    .await
    .map_err(|e| format!("skill lookup failed: {e}"))?
}

/// keepAlive: turn one death into either a restarted connection or a terminal
/// `error` (E39/E40). `reason` is the failure being reacted to.
///
/// The ceiling and the backoff are both load-bearing (§9.4): the cap alone
/// would still burn three spawns in 30 ms against an agent that dies on start,
/// so each attempt waits 1s/2s/4s — doubling because a fast-crashing agent
/// stays fast-crashing.
async fn respawn_chain(
    def: &ClawDefinition,
    agent: &AcpAgentEntry,
    root: &Path,
    acp: &AcpManager,
    shared: &SharedStatusHandle,
    mut reason: String,
) -> Option<Live> {
    if !def.keep_alive {
        // E40: immediate error, streak untouched.
        set_error(shared, reason);
        return None;
    }
    loop {
        let restarts = lock(shared).restarts;
        if restarts >= MAX_RESTARTS {
            set_error(
                shared,
                format!("{reason}; giving up after {MAX_RESTARTS} restarts"),
            );
            return None;
        }
        let backoff = Duration::from_secs(1u64 << restarts.min(2));
        tracing::warn!(
            "claw {}: {reason}; restarting in {backoff:?} ({}/{MAX_RESTARTS})",
            def.id,
            restarts + 1
        );
        set_state(shared, ClawState::Starting);
        tokio::time::sleep(backoff).await;
        lock(shared).restarts += 1;

        match connect_fresh(def, agent, root, acp, shared).await {
            Ok(mut live) => {
                // The fresh agent needs its briefing again; a death during the
                // opening turn is just another death for this chain.
                match open_skill(def, &mut live, root, shared).await {
                    Ok(()) => {
                        set_state(shared, ClawState::Idle);
                        return Some(live);
                    }
                    Err(next) => reason = next,
                }
            }
            Err(next) => reason = next,
        }
    }
}

/// Earliest enabled-schedule fire strictly after `now`, with that schedule's
/// prompts.
///
/// Mirrors [`ClawDefinition::next_run_at`] but keeps the winning schedule's
/// prompts — the loop needs both, and computing them separately invites the two
/// views to disagree across a boundary crossing.
fn next_trigger(def: &ClawDefinition, now: DateTime<Utc>) -> Option<(DateTime<Utc>, Vec<String>)> {
    if !def.enabled {
        return None; // E37: a disabled Claw is invisible to the scheduler.
    }
    def.schedules
        .iter()
        .filter(|s| s.enabled) // E38: disabled schedules never win.
        .filter_map(|s| {
            let parsed = s.schedule().ok()?;
            parsed.next_after(now).map(|at| (at, s.prompts.clone()))
        })
        .min_by_key(|(at, _)| *at)
}

/// Sleep length toward `at`, floored at zero and capped at [`MAX_SLEEP`].
fn capped_delay(at: DateTime<Utc>, now: DateTime<Utc>) -> Duration {
    at.signed_duration_since(now)
        .to_std()
        .unwrap_or(Duration::ZERO)
        .min(MAX_SLEEP)
}

/// Wait until `at`, tolerating the [`MAX_SLEEP`] cap by re-sleeping.
async fn sleep_until(at: DateTime<Utc>) {
    let mut delay = capped_delay(at, Utc::now());
    loop {
        tokio::time::sleep(delay).await;
        let now = Utc::now();
        if now >= at {
            return;
        }
        delay = capped_delay(at, now);
    }
}

/// The Claw task: open, then loop `sleep_until(next enabled fire)` forever
/// (SPEC-007 §5.3). Exits only via `abort()` or a terminal `error`.
async fn run_loop(
    def: ClawDefinition,
    agent: AcpAgentEntry,
    root: PathBuf,
    acp: AcpManager,
    mut live: Live,
    shared: SharedStatusHandle,
) {
    if let Err(reason) = open_skill(&def, &mut live, &root, &shared).await {
        let Some(restarted) = respawn_chain(&def, &agent, &root, &acp, &shared, reason).await
        else {
            return; // Terminal error recorded; the entry stays for `GET`.
        };
        live = restarted;
    }
    set_state(&shared, ClawState::Idle);

    'ticks: loop {
        // ---- wait for the next enabled occurrence ---------------------------
        let Some((at, prompts)) = next_trigger(&def, Utc::now()) else {
            // No enabled schedule: park forever. This Claw is manual-start-only;
            // `abort()` (via stop/cascade/shutdown) is the only exit, so no
            // wakeup is ever scheduled for it.
            loop {
                std::future::pending::<()>().await;
            }
        };
        sleep_until(at).await;

        // Absorb everything buffered while asleep — a death that happened
        // during the nap surfaces here instead of mid-prompt.
        let (busy, dead) = drain(&mut live.events);
        if let Some(reason) = dead {
            let Some(restarted) = respawn_chain(&def, &agent, &root, &acp, &shared, reason).await
            else {
                return;
            };
            live = restarted;
            // The connection just came back; fire on the next tick rather than
            // immediately hammering a freshly-born agent.
            continue 'ticks;
        }
        // [INVENTED-8]/§9.6: a tick landing on a live turn is skipped on
        // purpose — queuing it behind the running turn would burst later, and
        // sending it raw would be eaten by `Busy`.
        if busy && def.skip_if_running {
            tracing::info!("claw {}: tick skipped, a turn is still running", def.id);
            continue 'ticks;
        }

        // restartOnTrigger: every trigger gets a virgin connection instead of
        // the reused session (§3.1).
        if def.restart_on_trigger {
            live.conn.shutdown().await;
            match connect_fresh(&def, &agent, &root, &acp, &shared).await {
                Ok(mut fresh) => {
                    if let Err(reason) = open_skill(&def, &mut fresh, &root, &shared).await {
                        let Some(restarted) =
                            respawn_chain(&def, &agent, &root, &acp, &shared, reason).await
                        else {
                            return;
                        };
                        live = restarted;
                        continue 'ticks;
                    }
                    live = fresh;
                }
                Err(reason) => {
                    let Some(restarted) =
                        respawn_chain(&def, &agent, &root, &acp, &shared, reason).await
                    else {
                        return;
                    };
                    live = restarted;
                    continue 'ticks;
                }
            }
        }

        // Fire the tick's prompts, strictly one turn at a time.
        set_state(&shared, ClawState::Running);
        for prompt in prompts {
            match run_turn(&live.conn, &live.session, &mut live.events, prompt).await {
                TurnOutcome::Completed => note_healthy_turn(&shared, Utc::now()),
                TurnOutcome::Dead(reason) => {
                    let Some(restarted) =
                        respawn_chain(&def, &agent, &root, &acp, &shared, reason).await
                    else {
                        return;
                    };
                    live = restarted;
                    // Remaining prompts of this tick are abandoned; the next
                    // tick starts clean on the fresh connection.
                    continue 'ticks;
                }
            }
        }
        set_state(&shared, ClawState::Idle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claws::{ClawSchedule, ClawStatus};
    use chrono::TimeZone;

    fn base() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap()
    }

    fn definition(schedules: Vec<ClawSchedule>, enabled: bool) -> ClawDefinition {
        ClawDefinition {
            id: "c1".into(),
            name: "n".into(),
            agent_id: "a".into(),
            project_id: "p".into(),
            skill: None,
            enabled,
            auto_start: false,
            keep_alive: true,
            restart_on_trigger: false,
            permission_mode: PermissionMode::AutoApprove,
            skip_if_running: true,
            schedules,
        }
    }

    fn schedule(cron: &str, enabled: bool, prompts: &[&str]) -> ClawSchedule {
        ClawSchedule {
            label: None,
            cron: cron.into(),
            prompts: prompts.iter().map(|p| p.to_string()).collect(),
            enabled,
        }
    }

    #[test]
    fn next_trigger_carries_the_winning_prompts() {
        let def = definition(
            vec![
                schedule("0 10 * * *", true, &["late"]),
                schedule("0 9 * * *", true, &["early"]),
                // Disabled rows are excluded from both time and prompts (E38).
                schedule("0 1 * * *", false, &["asleep"]),
            ],
            true,
        );
        let (at, prompts) = next_trigger(&def, base()).expect("an enabled schedule exists");
        assert_eq!(at, Utc.with_ymd_and_hms(2026, 11, 1, 9, 0, 0).unwrap());
        assert_eq!(prompts, vec!["early".to_string()]);
    }

    #[test]
    fn a_disabled_claw_has_no_trigger_even_with_schedules() {
        // E37's pure half: `enabled: false` short-circuits before any cron math.
        let def = definition(vec![schedule("* * * * *", true, &["x"])], false);
        assert_eq!(next_trigger(&def, base()), None);
    }

    #[test]
    fn delays_floor_at_zero_and_cap_thirty_days() {
        let past = base() - chrono::Duration::hours(1);
        assert_eq!(capped_delay(past, base()), Duration::ZERO);

        let far = base() + chrono::Duration::days(365);
        assert_eq!(capped_delay(far, base()), MAX_SLEEP);

        let soon = base() + chrono::Duration::seconds(90);
        assert_eq!(capped_delay(soon, base()), Duration::from_secs(90));
    }

    #[test]
    fn status_of_an_unknown_claw_reports_stopped() {
        // The shape every Claw shows right after a server restart ([INVENTED-10]).
        let runtime = ClawRuntime::new();
        let def = definition(Vec::new(), true);
        let ClawStatus {
            state,
            connection_id,
            restarts,
            ..
        } = runtime.status(&def);
        assert_eq!(state, ClawState::Stopped);
        assert_eq!(connection_id, None);
        assert_eq!(restarts, 0);
    }
}
