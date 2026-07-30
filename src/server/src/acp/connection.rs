//! One agent process, driven as an actor (SPEC-003 §5.1 / §5.2).
//!
//! Why an actor and not a plain handle: `connect_with(transport, |conn| async {…})`
//! owns the `ConnectionTo<Agent>` for the closure's lifetime and closes the
//! connection when the closure returns. Spec ADE needs to call `session/prompt`
//! from an HTTP/WS request that arrives *later*, and the connection cannot be
//! handed out of the closure. So the closure never finishes on its own: it runs a
//! command loop reading [`AcpCommand`] off an `mpsc` channel, and callers post
//! commands instead of holding the connection.
//!
//! Corrected against the crate's real 2.0.0 API (SPEC-003 §2.1): there is no
//! `LocalSet` and nothing here is `!Send`, so this is an ordinary `tokio::spawn`.
//!
//! Concurrency rule that matters: `session/prompt` is long-lived, so it is
//! dispatched to a child task via `ConnectionTo::spawn`. Awaiting it inline would
//! deadlock cancellation — the loop would be blocked on the very turn that the
//! pending `Cancel` command is meant to stop.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, mpsc, oneshot};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::schema::v1::{
    CancelNotification, ClientCapabilities, ContentBlock, FileSystemCapabilities,
    InitializeRequest, InitializeResponse, NewSessionRequest, PromptRequest, ReadTextFileRequest,
    RequestPermissionRequest, SessionNotification, TextContent, WriteTextFileRequest,
};
use agent_client_protocol::{Client, ConnectionTo};

use super::agent::AcpAgentEntry;
use super::event::{AcpEvent, SessionState};
use super::fs_bridge;
use super::log::{EventLog, LoggedEvent, Replay};
use super::permission::{PendingPermissions, PermissionError};

/// How often expired permission requests are swept ([INVENTED-6]).
const PERMISSION_SWEEP_INTERVAL: Duration = Duration::from_secs(10);

/// Sweep cadence for a pair of timeouts.
///
/// Derived rather than fixed so a shortened timeout is actually observed on time:
/// a 1s timeout swept every 10s would take 10s to fire. Both deadlines are checked
/// on the same tick, so the cadence follows whichever is tighter. Quarter-period
/// keeps the worst-case overshoot proportional, and the floor stops a near-zero
/// timeout from turning the sweep into a busy loop.
fn sweep_interval(limits: AcpLimits) -> Duration {
    (limits.permission_timeout.min(limits.idle_timeout) / 4)
        .max(Duration::from_millis(100))
        .min(PERMISSION_SWEEP_INTERVAL)
}

/// Broadcast backlog per session. A slow socket that falls further behind than
/// this gets `Lagged` and re-replays from the log, so the bound only costs a
/// round-trip, never correctness.
const BROADCAST_CAPACITY: usize = 256;

/// Kill a connection nobody is watching after this long ([INVENTED-10]).
///
/// An agent process costs hundreds of MB, so an abandoned one is a real leak. The
/// window is long enough that closing a tab and coming back does not lose the
/// agent (A21 stays true); only genuine abandonment reaps it.
pub const ACP_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Env override for [`ACP_IDLE_TIMEOUT`], in seconds. Exists because verifying the
/// reaper by hand otherwise means waiting half an hour (SPEC-003 §8 step 9).
const IDLE_TIMEOUT_ENV: &str = "SPEC_ADE_ACP_IDLE_SECS";

/// The two timeouts a connection ages on.
///
/// Passed in rather than read from a global at the point of use so a test can
/// shorten them without touching process state: the acceptance criteria for both
/// (A11 permission timeout, [INVENTED-10] idle reaper) are only observable
/// end-to-end — the crate exposes no public `Responder` constructor, so a parked
/// permission cannot exist without a live connection — and a suite that had to
/// `set_var` to shorten a timeout could not run its tests in parallel.
#[derive(Debug, Clone, Copy)]
pub struct AcpLimits {
    /// A parked permission request is auto-cancelled after this long.
    pub permission_timeout: Duration,
    /// A connection with no session and no attached socket is reaped after this.
    pub idle_timeout: Duration,
}

impl AcpLimits {
    /// Defaults, with both env overrides applied.
    pub fn from_env() -> Self {
        Self {
            permission_timeout: super::permission::permission_timeout(),
            idle_timeout: env_duration(IDLE_TIMEOUT_ENV, ACP_IDLE_TIMEOUT),
        }
    }
}

impl Default for AcpLimits {
    fn default() -> Self {
        Self::from_env()
    }
}

/// Read a `Duration` in whole seconds from `var`, falling back to `default`.
///
/// A malformed value warns and degrades rather than refusing to start: the
/// override exists for convenience during manual verification, and a typo in it
/// should not take the server down.
pub(super) fn env_duration(var: &str, default: Duration) -> Duration {
    match std::env::var(var) {
        Ok(v) => match v.parse::<u64>() {
            Ok(secs) => Duration::from_secs(secs),
            Err(e) => {
                tracing::warn!("{var}={v:?} is not a number ({e}); using the default");
                default
            }
        },
        Err(_) => default,
    }
}

/// Number of sockets currently attached, across all sessions of a connection.
///
/// Shared between every [`AcpConnection`] clone and the actor's sweep loop.
/// [INVENTED-10]'s trigger is "0 session, 0 WS": in practice a WS cannot attach
/// to begin with unless a session already exists (§3.2 requires `sessionId`), so
/// this only ever matters while [`Sessions`] is still empty — it exists mainly so
/// the condition is checked exactly as specified rather than assumed away.
#[derive(Clone, Default)]
struct Watchers(Arc<AtomicUsize>);

impl Watchers {
    fn attach(&self) -> WatcherGuard {
        self.0.fetch_add(1, Ordering::SeqCst);
        WatcherGuard(self.0.clone())
    }

    fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

/// RAII: a socket counts as attached for exactly as long as this is alive.
///
/// Held by the caller of [`AcpConnection::attach`] (the WS bridge) — dropping it
/// (socket closed) decrements the watcher count the idle reaper checks.
pub struct WatcherGuard(Arc<AtomicUsize>);

impl Drop for WatcherGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Errors surfaced to the REST/WS layer.
#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    /// The agent subprocess could not be started, or died before `initialize`
    /// completed. Maps to 502 with the gathered stderr — the failure is in an
    /// external process, not a bug in this server.
    #[error("agent failed to start: {0}")]
    Spawn(String),
    /// The actor is gone (process exited, connection killed).
    #[error("connection is closed")]
    Closed,
    /// No such session on this connection.
    #[error("no session {0}")]
    NoSession(String),
    /// A turn is already running ([INVENTED-4]): prompts are never queued, because
    /// a silent queue makes a user think their message was lost.
    #[error("a turn is already in progress for this session")]
    Busy,
    /// The agent rejected the call.
    #[error("agent error: {0}")]
    Agent(String),
    #[error(transparent)]
    Permission(#[from] PermissionError),
}

/// Commands the actor accepts.
enum AcpCommand {
    NewSession {
        cwd: PathBuf,
        reply: oneshot::Sender<Result<String, AcpError>>,
    },
    Prompt {
        session_id: String,
        text: String,
        reply: oneshot::Sender<Result<(), AcpError>>,
    },
    Cancel {
        session_id: String,
    },
    Shutdown,
}

/// Per-session state: the event log, the live-event fan-out, and whether a turn
/// is running.
struct SessionSlot {
    /// Project root, used as the `fs/*` sandbox and the session `cwd`.
    root: PathBuf,
    log: Mutex<EventLog>,
    tx: broadcast::Sender<LoggedEvent>,
    /// Turn state. `Mutex<SessionState>` rather than an atomic so the
    /// check-and-set in [`SessionSlot::begin_turn`] is one critical section — two
    /// concurrent prompts must not both see `Idle`.
    state: Mutex<SessionState>,
}

impl SessionSlot {
    fn new(root: PathBuf) -> Self {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            root,
            log: Mutex::new(EventLog::new()),
            tx,
            state: Mutex::new(SessionState::Idle),
        }
    }

    /// Append to the log and fan out to attached sockets.
    ///
    /// Both happen under the log lock so `seq` order and broadcast order agree; a
    /// socket that subscribes before replaying can then dedupe purely by `seq`.
    /// The lock is a `std::sync::Mutex` and `broadcast::Sender::send` does not
    /// await, so nothing is held across a suspension point.
    fn emit(&self, event: AcpEvent) -> u64 {
        let logged = {
            let mut log = lock(&self.log);
            log.append(event)
        };
        let seq = logged.seq;
        // `Err` only means "no subscribers"; the log still has it for replay.
        let _ = self.tx.send(logged);
        seq
    }

    fn replay(&self, after_seq: u64) -> Replay {
        lock(&self.log).replay_from(after_seq)
    }

    fn state(&self) -> SessionState {
        *lock(&self.state)
    }

    fn set_state(&self, state: SessionState) {
        *lock(&self.state) = state;
    }

    /// Claim the turn, or report why not. One critical section, so two prompts
    /// racing cannot both win.
    fn begin_turn(&self) -> Result<(), AcpError> {
        let mut guard = lock(&self.state);
        match *guard {
            SessionState::Idle => {
                *guard = SessionState::Prompting;
                Ok(())
            }
            SessionState::Prompting => Err(AcpError::Busy),
            SessionState::Closed => Err(AcpError::Closed),
        }
    }
}

/// Sessions of one connection, shared between the actor and the HTTP layer.
#[derive(Clone, Default)]
struct Sessions {
    /// Keyed by the **agent's** session id: that is what arrives on every
    /// `session/update`, so correlating on anything else would need a second map.
    inner: Arc<Mutex<HashMap<String, Arc<SessionSlot>>>>,
}

impl Sessions {
    fn get(&self, id: &str) -> Option<Arc<SessionSlot>> {
        lock(&self.inner).get(id).cloned()
    }

    fn insert(&self, id: String, slot: Arc<SessionSlot>) {
        lock(&self.inner).insert(id, slot);
    }

    fn ids(&self) -> Vec<String> {
        lock(&self.inner).keys().cloned().collect()
    }

    fn len(&self) -> usize {
        lock(&self.inner).len()
    }

    fn all(&self) -> Vec<Arc<SessionSlot>> {
        lock(&self.inner).values().cloned().collect()
    }
}

/// Handle to a running agent connection. Cloneable; the actor lives as long as
/// any command can still be sent plus the process itself.
#[derive(Clone)]
pub struct AcpConnection {
    pub id: String,
    pub agent_id: String,
    pub project_id: String,
    /// What the agent reported at `initialize`, echoed to the client so the UI can
    /// enable features the agent actually supports.
    pub agent_capabilities: serde_json::Value,
    /// The agent's own name/version (§8 step 1 checks this shows the real agent).
    /// `null` when the agent did not send one — the schema still has it optional.
    pub agent_info: serde_json::Value,
    cmd_tx: mpsc::Sender<AcpCommand>,
    sessions: Sessions,
    permissions: PendingPermissions,
    /// Ring of the agent's stderr ([INVENTED-11]): when an agent misbehaves this
    /// is often the only explanation available.
    stderr: StderrRing,
    /// Attached WebSocket count, for the idle reaper ([INVENTED-10]).
    watchers: Watchers,
    /// Set once the process is gone, so `GET /api/acp` can drop it.
    closed: Arc<std::sync::atomic::AtomicBool>,
}

/// Bounded stderr capture ([INVENTED-11]).
#[derive(Clone, Default)]
pub struct StderrRing {
    inner: Arc<Mutex<String>>,
}

/// Keep this much agent stderr per connection.
pub const STDERR_RING_BYTES: usize = 256 * 1024;

impl StderrRing {
    fn push_line(&self, line: &str) {
        let mut buf = lock(&self.inner);
        buf.push_str(line);
        buf.push('\n');
        if buf.len() > STDERR_RING_BYTES {
            // Drop from the front at a char boundary so the ring stays valid
            // UTF-8 and can be returned as a plain JSON string.
            let cut = buf.len() - STDERR_RING_BYTES;
            let cut = (cut..buf.len())
                .find(|i| buf.is_char_boundary(*i))
                .unwrap_or(buf.len());
            let tail = buf[cut..].to_string();
            *buf = tail;
        }
    }

    pub fn snapshot(&self) -> String {
        lock(&self.inner).clone()
    }
}

impl AcpConnection {
    /// Spawn an agent and complete the ACP handshake.
    ///
    /// Returns only once `initialize` has answered, so a 201 from
    /// `POST /api/acp/spawn` means the agent is genuinely usable. A process that
    /// dies first yields [`AcpError::Spawn`] carrying its stderr.
    pub async fn spawn(
        entry: &AcpAgentEntry,
        project_id: &str,
        limits: AcpLimits,
    ) -> Result<Self, AcpError> {
        let id = uuid::Uuid::new_v4().to_string();
        let (cmd_tx, cmd_rx) = mpsc::channel::<AcpCommand>(32);
        let sessions = Sessions::default();
        let permissions = PendingPermissions::new();
        let stderr = StderrRing::default();
        let watchers = Watchers::default();
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let agent = entry.to_acp_agent().with_debug({
            let stderr = stderr.clone();
            move |line, direction| {
                if matches!(direction, agent_client_protocol::LineDirection::Stderr) {
                    stderr.push_line(line);
                }
            }
        });

        // The handshake result comes back over a oneshot so this function can
        // report a spawn failure instead of returning a handle to a dead process.
        let (ready_tx, ready_rx) = oneshot::channel::<Result<InitializeResponse, AcpError>>();

        let task = ConnectionTask {
            sessions: sessions.clone(),
            permissions: permissions.clone(),
            watchers: watchers.clone(),
            closed: closed.clone(),
            limits,
        };
        tokio::spawn(task.run(agent, cmd_rx, ready_tx));

        let init = match ready_rx.await {
            Ok(Ok(init)) => init,
            Ok(Err(e)) => return Err(e),
            // The task died without reporting — treat as a spawn failure and
            // surface whatever stderr was captured.
            Err(_) => {
                let captured = stderr.snapshot();
                return Err(AcpError::Spawn(if captured.is_empty() {
                    "agent exited before the handshake completed".to_string()
                } else {
                    captured
                }));
            }
        };

        Ok(Self {
            id,
            agent_id: entry.id.clone(),
            project_id: project_id.to_string(),
            agent_capabilities: serde_json::to_value(&init.agent_capabilities)
                .unwrap_or(serde_json::Value::Null),
            agent_info: serde_json::to_value(&init.agent_info).unwrap_or(serde_json::Value::Null),
            cmd_tx,
            sessions,
            permissions,
            stderr,
            watchers,
            closed,
        })
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    pub fn session_ids(&self) -> Vec<String> {
        self.sessions.ids()
    }

    pub fn stderr(&self) -> String {
        self.stderr.snapshot()
    }

    pub fn permissions(&self) -> &PendingPermissions {
        &self.permissions
    }

    /// Open a session rooted at `root` (the project directory).
    pub async fn new_session(&self, root: PathBuf) -> Result<String, AcpError> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(AcpCommand::NewSession { cwd: root, reply })
            .await
            .map_err(|_| AcpError::Closed)?;
        rx.await.map_err(|_| AcpError::Closed)?
    }

    /// Send a prompt, opening a turn.
    pub async fn prompt(&self, session_id: &str, text: String) -> Result<(), AcpError> {
        let (reply, rx) = oneshot::channel();
        self.cmd_tx
            .send(AcpCommand::Prompt {
                session_id: session_id.to_string(),
                text,
                reply,
            })
            .await
            .map_err(|_| AcpError::Closed)?;
        rx.await.map_err(|_| AcpError::Closed)?
    }

    /// Cancel the running turn. `session/cancel` is a notification, so there is
    /// nothing to await — the turn ends via `turn_complete { cancelled }`.
    pub async fn cancel(&self, session_id: &str) -> Result<(), AcpError> {
        if self.sessions.get(session_id).is_none() {
            return Err(AcpError::NoSession(session_id.to_string()));
        }
        self.cmd_tx
            .send(AcpCommand::Cancel {
                session_id: session_id.to_string(),
            })
            .await
            .map_err(|_| AcpError::Closed)
    }

    /// Answer a parked permission request.
    pub fn respond_permission(
        &self,
        session_id: &str,
        request_id: &str,
        option_id: Option<&str>,
    ) -> Result<(), AcpError> {
        match option_id {
            Some(option) => self.permissions.select(request_id, option)?,
            None => self.permissions.cancel(request_id)?,
        }
        if let Some(slot) = self.sessions.get(session_id) {
            slot.emit(AcpEvent::PermissionResolved {
                request_id: request_id.to_string(),
                outcome: match option_id {
                    Some(_) => "selected".to_string(),
                    None => "cancelled".to_string(),
                },
            });
        }
        Ok(())
    }

    /// Replay + live subscription for one session's socket.
    ///
    /// Subscribing **before** replaying closes the window where an event emitted
    /// between the two steps would be missed by both; duplicates are filtered by
    /// `seq` in the socket loop. The returned guard counts this socket toward the
    /// idle reaper ([INVENTED-10]) for as long as the caller holds it.
    pub fn attach(
        &self,
        session_id: &str,
        after_seq: u64,
    ) -> Result<
        (
            Replay,
            broadcast::Receiver<LoggedEvent>,
            SessionState,
            WatcherGuard,
        ),
        AcpError,
    > {
        let slot = self
            .sessions
            .get(session_id)
            .ok_or_else(|| AcpError::NoSession(session_id.to_string()))?;
        let rx = slot.tx.subscribe();
        let replay = slot.replay(after_seq);
        Ok((replay, rx, slot.state(), self.watchers.attach()))
    }

    /// Re-read the log after a `Lagged` — the socket lost its place in the
    /// broadcast, but the log is still authoritative.
    pub fn replay(&self, session_id: &str, after_seq: u64) -> Result<Replay, AcpError> {
        let slot = self
            .sessions
            .get(session_id)
            .ok_or_else(|| AcpError::NoSession(session_id.to_string()))?;
        Ok(slot.replay(after_seq))
    }

    /// Terminate the agent. Idempotent: an already-dead actor is success, since
    /// the caller's goal (no process) already holds.
    pub async fn shutdown(&self) {
        let _ = self.cmd_tx.send(AcpCommand::Shutdown).await;
    }
}

/// State the background task owns while the connection runs.
struct ConnectionTask {
    sessions: Sessions,
    permissions: PendingPermissions,
    watchers: Watchers,
    closed: Arc<std::sync::atomic::AtomicBool>,
    limits: AcpLimits,
}

impl ConnectionTask {
    /// Build the client, handshake, then serve commands until told to stop.
    async fn run(
        self,
        agent: agent_client_protocol::AcpAgent,
        cmd_rx: mpsc::Receiver<AcpCommand>,
        ready_tx: oneshot::Sender<Result<InitializeResponse, AcpError>>,
    ) {
        let sessions = self.sessions.clone();
        let permissions = self.permissions.clone();
        let watchers = self.watchers.clone();
        let limits = self.limits;

        // Every handler captures only `Send` state: `Sessions` and
        // `PendingPermissions` are `Arc<Mutex<…>>` clones.
        let result = Client
            .builder()
            .name("spec-ade")
            .on_receive_notification(
                {
                    let sessions = sessions.clone();
                    async move |notif: SessionNotification, _conn| {
                        handle_session_update(&sessions, notif);
                        Ok(())
                    }
                },
                agent_client_protocol::on_receive_notification!(),
            )
            .on_receive_request(
                {
                    let sessions = sessions.clone();
                    let permissions = permissions.clone();
                    async move |req: RequestPermissionRequest, responder, _conn| {
                        handle_permission(&sessions, &permissions, req, responder);
                        Ok(())
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let sessions = sessions.clone();
                    async move |req: ReadTextFileRequest, responder, _conn| {
                        let root = session_root(&sessions, &req.session_id.to_string());
                        let result = match root {
                            Some(root) => tokio::task::spawn_blocking(move || {
                                fs_bridge::read_text_file(&root, &req)
                            })
                            .await
                            .unwrap_or_else(|e| {
                                Err(agent_client_protocol::Error::internal_error()
                                    .data(e.to_string()))
                            }),
                            None => Err(unknown_session(&req.session_id.to_string())),
                        };
                        responder.respond_with_result(result)
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .on_receive_request(
                {
                    let sessions = sessions.clone();
                    async move |req: WriteTextFileRequest, responder, _conn| {
                        let root = session_root(&sessions, &req.session_id.to_string());
                        let result = match root {
                            Some(root) => tokio::task::spawn_blocking(move || {
                                fs_bridge::write_text_file(&root, &req)
                            })
                            .await
                            .unwrap_or_else(|e| {
                                Err(agent_client_protocol::Error::internal_error()
                                    .data(e.to_string()))
                            }),
                            None => Err(unknown_session(&req.session_id.to_string())),
                        };
                        responder.respond_with_result(result)
                    }
                },
                agent_client_protocol::on_receive_request!(),
            )
            .connect_with(agent, {
                // Clones for the closure to consume: the originals stay behind for
                // the teardown block after `.await`.
                let loop_sessions = sessions.clone();
                let loop_permissions = permissions.clone();
                let loop_watchers = watchers.clone();
                async move |conn: ConnectionTo<agent_client_protocol::Agent>| {
                    // Advertise exactly what is implemented: `fs/*` yes, `terminal/*`
                    // no ([INVENTED-8]). Over-claiming would make the agent issue
                    // `terminal/create` calls that can only fail.
                    let init = InitializeRequest::new(ProtocolVersion::V1).client_capabilities(
                        ClientCapabilities::new()
                            .fs(FileSystemCapabilities::new()
                                .read_text_file(true)
                                .write_text_file(true))
                            .terminal(false),
                    );

                    let init_result = conn.send_request(init).block_task().await;
                    let init = match init_result {
                        Ok(init) => init,
                        Err(e) => {
                            let _ = ready_tx.send(Err(AcpError::Spawn(e.to_string())));
                            return Ok(());
                        }
                    };
                    if ready_tx.send(Ok(init)).is_err() {
                        // The spawner gave up; nothing will ever use this process.
                        return Ok(());
                    }

                    command_loop(
                        conn,
                        cmd_rx,
                        loop_sessions,
                        loop_permissions,
                        loop_watchers,
                        limits,
                    )
                    .await;
                    Ok(())
                }
            })
            .await;

        // Whatever ended the connection — clean shutdown, agent crash, transport
        // EOF — the process is gone now. Tell every attached socket and release
        // any agent still blocked on a permission answer.
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        let reason = match &result {
            Ok(()) => "agent connection closed".to_string(),
            Err(e) => e.to_string(),
        };
        if let Err(e) = &result {
            tracing::debug!("acp: connection ended with error: {e}");
        }
        for id in permissions.cancel_all() {
            // A socket that reattaches must not keep showing buttons for a
            // request the agent can no longer receive an answer to.
            for slot in sessions.all() {
                slot.emit(AcpEvent::PermissionResolved {
                    request_id: id.clone(),
                    outcome: "cancelled".to_string(),
                });
            }
        }
        for slot in sessions.all() {
            slot.set_state(SessionState::Closed);
            slot.emit(AcpEvent::SessionState {
                state: SessionState::Closed,
            });
            slot.emit(AcpEvent::ConnectionClosed {
                reason: reason.clone(),
            });
        }
    }
}

/// JSON-RPC error for a `sessionId` this connection does not know.
fn unknown_session(session_id: &str) -> agent_client_protocol::Error {
    agent_client_protocol::Error::invalid_params()
        .data(serde_json::json!({ "sessionId": session_id, "reason": "unknown session" }))
}

fn session_root(sessions: &Sessions, session_id: &str) -> Option<PathBuf> {
    sessions.get(session_id).map(|slot| slot.root.clone())
}

/// Translate and log one `session/update`.
fn handle_session_update(sessions: &Sessions, notif: SessionNotification) {
    let session_id = notif.session_id.to_string();
    let Some(slot) = sessions.get(&session_id) else {
        // An update for a session we never created. Log and drop: this is the
        // agent's bookkeeping problem, and closing the connection over it would
        // take down the sessions that *are* working.
        tracing::debug!("acp: session update for unknown session {session_id}");
        return;
    };
    // `None` = an unmodelled or empty variant, already logged at debug by
    // `translate`. Nothing is emitted, and the connection stays up.
    if let Some(event) = super::event::translate(&notif.update) {
        slot.emit(event);
    }
}

/// Park a permission request and tell the user about it.
fn handle_permission(
    sessions: &Sessions,
    permissions: &PendingPermissions,
    req: RequestPermissionRequest,
    responder: agent_client_protocol::Responder<
        agent_client_protocol::schema::v1::RequestPermissionResponse,
    >,
) {
    let session_id = req.session_id.to_string();
    let Some(slot) = sessions.get(&session_id) else {
        // Cannot ask a user who is not there. Failing closed (an error, not an
        // approval) is the whole point of [INVENTED-5].
        let _ = responder.respond_with_error(unknown_session(&session_id));
        return;
    };

    let options: Vec<super::event::PermissionOptionView> = req
        .options
        .iter()
        .map(|o| super::event::PermissionOptionView {
            option_id: o.option_id.to_string(),
            name: o.name.clone(),
            kind: serde_json::to_value(o.kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| "other".to_string()),
        })
        .collect();

    let request_id = permissions.park(
        &session_id,
        options.iter().map(|o| o.option_id.clone()).collect(),
        responder,
    );

    // Logged (not just broadcast) so a page reload still shows the pending
    // prompt — otherwise the agent waits on a question nobody can see.
    slot.emit(AcpEvent::PermissionRequest {
        request_id,
        tool_call: serde_json::to_value(&req.tool_call).unwrap_or(serde_json::Value::Null),
        options,
    });
}

/// Serve commands until `Shutdown`, the channel closes, or the transport dies.
async fn command_loop(
    conn: ConnectionTo<agent_client_protocol::Agent>,
    mut cmd_rx: mpsc::Receiver<AcpCommand>,
    sessions: Sessions,
    permissions: PendingPermissions,
    watchers: Watchers,
    limits: AcpLimits,
) {
    // Both timeouts are fixed for the connection's lifetime: a value changing under
    // a running connection would make its behaviour depend on when it was checked.
    let permission_after = limits.permission_timeout;
    let mut sweep = tokio::time::interval(sweep_interval(limits));
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let idle_after = limits.idle_timeout;
    // A connection is born idle: spawn happens before the first session exists, so
    // the clock has to start here or an agent nobody ever used would live forever.
    let mut idle_since = Some(Instant::now());

    loop {
        tokio::select! {
            // Sweeping in the loop (rather than one timer per request) means a
            // request answered normally cannot leave a task behind.
            _ = sweep.tick() => {
                for id in permissions.cancel_expired(permission_after) {
                    for slot in sessions.all() {
                        slot.emit(AcpEvent::PermissionResolved {
                            request_id: id.clone(),
                            outcome: "cancelled".to_string(),
                        });
                    }
                }

                // Idle reaper ([INVENTED-10]): no session AND no socket watching.
                // Tracked as "since when" rather than a countdown so activity
                // resets it, and so the check reuses the sweep tick instead of
                // needing a second timer.
                if sessions.len() == 0 && watchers.count() == 0 {
                    let since = *idle_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= idle_after {
                        tracing::info!(
                            "acp: reaping connection idle for {:?} (no sessions, no sockets)",
                            since.elapsed()
                        );
                        return;
                    }
                } else {
                    idle_since = None;
                }
            }
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else {
                    // Every handle dropped: nothing can ever prompt again.
                    return;
                };
                match cmd {
                    AcpCommand::Shutdown => return,
                    AcpCommand::NewSession { cwd, reply } => {
                        let result = new_session(&conn, &sessions, cwd).await;
                        let _ = reply.send(result);
                    }
                    AcpCommand::Prompt { session_id, text, reply } => {
                        let _ = reply.send(start_turn(&conn, &sessions, &session_id, text));
                    }
                    AcpCommand::Cancel { session_id } => {
                        // A notification: fire and forget. The turn ends when the
                        // agent answers its prompt with `cancelled`.
                        if let Err(e) = conn.send_notification(
                            CancelNotification::new(session_id.clone()),
                        ) {
                            tracing::debug!("acp: cancel notification failed: {e}");
                        }
                        // Any permission still parked for this session would block
                        // the very turn being cancelled, so release it now — the
                        // schema requires `Cancelled` for exactly this case.
                        for id in permissions.cancel_session(&session_id) {
                            if let Some(slot) = sessions.get(&session_id) {
                                slot.emit(AcpEvent::PermissionResolved {
                                    request_id: id,
                                    outcome: "cancelled".to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }
}

/// `session/new`, then register the slot before returning.
///
/// This runs in the command loop (not the dispatch loop), so `block_task()` is
/// the correct consumption mode here.
async fn new_session(
    conn: &ConnectionTo<agent_client_protocol::Agent>,
    sessions: &Sessions,
    cwd: PathBuf,
) -> Result<String, AcpError> {
    let response = conn
        .send_request(NewSessionRequest::new(cwd.clone()))
        .block_task()
        .await
        .map_err(|e| AcpError::Agent(e.to_string()))?;

    let session_id = response.session_id.to_string();
    let slot = Arc::new(SessionSlot::new(cwd));
    // Register before emitting so the first event has somewhere to go.
    sessions.insert(session_id.clone(), slot.clone());
    slot.emit(AcpEvent::SessionState {
        state: SessionState::Idle,
    });
    Ok(session_id)
}

/// Claim the turn and dispatch `session/prompt` to a child task.
///
/// Synchronous on purpose: it must not await the turn. Awaiting here would block
/// the command loop for the whole turn, so a `Cancel` arriving mid-turn could
/// never be processed — the deadlock SPEC-003 §5.2 calls out.
fn start_turn(
    conn: &ConnectionTo<agent_client_protocol::Agent>,
    sessions: &Sessions,
    session_id: &str,
    text: String,
) -> Result<(), AcpError> {
    let slot = sessions
        .get(session_id)
        .ok_or_else(|| AcpError::NoSession(session_id.to_string()))?;
    slot.begin_turn()?;
    slot.emit(AcpEvent::SessionState {
        state: SessionState::Prompting,
    });

    let request = PromptRequest::new(
        session_id.to_string(),
        vec![ContentBlock::Text(TextContent::new(text))],
    );

    let spawned = conn.spawn({
        let conn = conn.clone();
        let slot = slot.clone();
        async move {
            let result = conn.send_request(request).block_task().await;
            match result {
                Ok(response) => {
                    let stop_reason = serde_json::to_value(response.stop_reason)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_else(|| "end_turn".to_string());
                    slot.emit(AcpEvent::TurnComplete { stop_reason });
                }
                Err(e) => {
                    // The turn failed rather than ending. Report it and return to
                    // idle, so the user can retry instead of being stuck.
                    slot.emit(AcpEvent::Error {
                        message: format!("prompt failed: {e}"),
                    });
                }
            }
            // Only leave `Prompting` if the session is still alive; teardown may
            // already have moved it to `Closed`, and resurrecting it would tell
            // the UI a dead session is ready for input.
            if slot.state() == SessionState::Prompting {
                slot.set_state(SessionState::Idle);
                slot.emit(AcpEvent::SessionState {
                    state: SessionState::Idle,
                });
            }
            Ok(())
        }
    });

    if let Err(e) = spawned {
        // The connection is already going down; undo the claim so the state the
        // client sees matches reality.
        slot.set_state(SessionState::Idle);
        return Err(AcpError::Agent(e.to_string()));
    }
    Ok(())
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
