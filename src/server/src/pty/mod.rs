//! PTY management — spawn shells and bridge their I/O to WebSocket clients.
//!
//! Spec: `docs/specs/SPEC-001-terminal.md`. Design notes: deep-dive 02 §1.
//!
//! CRITICAL — blocking-in-async (02 §traps): `portable-pty`'s reader, writer and
//! `Child::wait` are all blocking. Each terminal therefore owns three dedicated
//! `std::thread`s bridged to tokio through channels. Never touch these APIs from
//! an async task.
//!
//! Ownership model (SPEC-001 §4 [INVENTED-4]): a `Terminal` outlives any single
//! WebSocket. Closing the socket does not kill the shell — a page reload
//! re-attaches and replays scrollback. Shells die only when they exit or when
//! `DELETE /api/terminals/{id}` kills them.
//!
//! Data flow:
//! ```text
//!  PTY ──read(blocking)──> reader thread ──mpsc──> pump task ──broadcast──> WS clients
//!                                                     └── scrollback (replay)
//!  WS clients ──mpsc──> writer thread ──write(blocking)──> PTY
//!  Child::wait(blocking) ──watch──> pump task ──> Exit event
//! ```
//! The pump task is the single writer of both the scrollback and the broadcast
//! channel, which is what keeps `seq` gap-free and lets a re-attaching client
//! splice into the stream without losing or duplicating a byte.

pub mod osc7;
pub mod scrollback;
pub mod shell_integration;
pub mod utf8;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, watch};

use scrollback::Scrollback;

/// PTY read buffer (deep-dive 02 §6: 8 KiB, vs sshx's 4 KiB).
const READ_BUF: usize = 8 * 1024;
/// Largest payload in one outbound WS frame. Only replay coalescing can reach
/// this; live chunks are bounded by `READ_BUF`.
const WS_CHUNK_MAX: usize = 64 * 1024;
/// Capacity of the reader→pump and WS→writer channels, in chunks.
const CHANNEL_CAPACITY: usize = 256;
/// Broadcast backlog. A slow client that falls further behind than this gets a
/// `Lagged` error, which the WS handler recovers from by replaying from the
/// scrollback — so this bounds memory, not correctness.
const BROADCAST_CAPACITY: usize = 1024;
/// How long an incomplete trailing UTF-8 sequence may be held before we give up
/// and forward it anyway (SPEC-001 §4 [INVENTED-9]).
const UTF8_FLUSH_AFTER: Duration = Duration::from_millis(50);
/// Fallback terminal size when the client doesn't supply one.
const DEFAULT_ROWS: u16 = 24;
const DEFAULT_COLS: u16 = 80;

/// Errors surfaced to the REST/WS layer.
#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    #[error("terminal not found")]
    NotFound,
    #[error("cwd is not a directory: {0}")]
    BadCwd(String),
    #[error("pty error: {0}")]
    Pty(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Options for `PtyManager::spawn` (SPEC-001 §4 [INVENTED-1]).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpawnOptions {
    /// Working directory. Must exist and be a directory.
    pub cwd: Option<String>,
    pub rows: Option<u16>,
    pub cols: Option<u16>,
    /// Program to run. Defaults to the user's login shell.
    pub shell: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment variables, applied after our own defaults so a caller
    /// can override `TERM` if it needs to.
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// Snapshot of a terminal for the REST layer.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalInfo {
    pub id: String,
    pub pid: Option<u32>,
    pub rows: u16,
    pub cols: u16,
    pub cwd: String,
    pub alive: bool,
    /// Exit code once dead; `None` while alive or when killed by a signal.
    pub exit_code: Option<u32>,
    /// Bytes of output produced so far — a fresh client can use this as its
    /// starting cursor if it does not want history.
    pub seq: u64,
}

/// How a shell finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitInfo {
    /// Exit code, or `None` if terminated by a signal.
    pub code: Option<u32>,
    pub signal: Option<String>,
}

/// One event on a terminal's output stream, in emission order.
#[derive(Debug, Clone)]
pub enum TerminalEvent {
    /// Raw PTY output. `seq_end` is the stream offset just past this chunk.
    Output { data: Bytes, seq_end: u64 },
    /// Working directory parsed out of an OSC 7 sequence.
    Cwd(String),
    /// The shell exited. Always the last event.
    Exit(ExitInfo),
}

/// Mutable per-terminal metadata.
#[derive(Debug)]
struct TerminalState {
    rows: u16,
    cols: u16,
    cwd: String,
}

/// A live (or recently dead) PTY plus everything needed to talk to it.
pub struct Terminal {
    pub id: String,
    pub pid: Option<u32>,
    state: Mutex<TerminalState>,
    scrollback: Mutex<Scrollback>,
    /// Fan-out to every attached WebSocket.
    events: broadcast::Sender<TerminalEvent>,
    /// Into the writer thread.
    input_tx: mpsc::Sender<Bytes>,
    /// Kept for `resize` (`ioctl(TIOCSWINSZ)`); not used for I/O.
    master: Mutex<Box<dyn MasterPty + Send>>,
    /// Split off the `Child` so we can kill while the waiter thread blocks in
    /// `wait()` (`portable-pty` lib.rs:154-157).
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    /// `Some` once the shell has exited.
    exit: watch::Receiver<Option<ExitInfo>>,
    integration: shell_integration::Integration,
}

impl std::fmt::Debug for Terminal {
    /// Hand-written because the `dyn MasterPty` field isn't `Debug`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terminal")
            .field("id", &self.id)
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

/// What a newly attached client needs to splice into the stream.
pub struct Attachment {
    /// Live events from the moment of attach onward.
    pub events: broadcast::Receiver<TerminalEvent>,
    /// History to send first.
    pub replay: Vec<Bytes>,
    /// Offset of the first replayed byte. Greater than the requested `after_seq`
    /// when history had been pruned — the caller reports the gap to the client.
    pub from_seq: u64,
    /// Offset just past the replayed bytes; the client's cursor afterwards.
    pub seq: u64,
    /// Set if the shell had already exited before this client attached.
    pub exit: Option<ExitInfo>,
}

impl Terminal {
    /// Attach a client, atomically pairing a history snapshot with a live
    /// subscription.
    ///
    /// The lock ordering is the crux of SPEC-001 §5.1: subscribe *while* holding
    /// the scrollback lock. The pump needs that same lock to append, so every
    /// chunk is either already in `replay` or still to come on `events` — never
    /// both, never neither.
    pub fn attach(&self, after_seq: Option<u64>) -> Attachment {
        let (events, replay) = {
            let scrollback = lock(&self.scrollback);
            let events = self.events.subscribe();
            let replay = scrollback.replay_from(after_seq.unwrap_or(0));
            (events, replay)
        };

        Attachment {
            events,
            replay: coalesce(replay.chunks),
            from_seq: replay.from_seq,
            seq: replay.end_seq,
            exit: self.exit.borrow().clone(),
        }
    }

    /// Queue bytes for the PTY. Errors only if the writer thread is gone
    /// (i.e. the shell is dead).
    pub async fn write_input(&self, data: Bytes) -> Result<(), PtyError> {
        self.input_tx
            .send(data)
            .await
            .map_err(|_| PtyError::NotFound)
    }

    /// Apply a new window size — `ioctl(TIOCSWINSZ)`, which signals `SIGWINCH`
    /// to the child. Non-blocking, so it's safe to call from an async task.
    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), PtyError> {
        // Guard against a client sending 0 — a zero-sized winsize makes curses
        // apps misbehave.
        let rows = rows.max(1);
        let cols = cols.max(1);
        lock(&self.master)
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| PtyError::Pty(e.to_string()))?;
        let mut state = lock(&self.state);
        state.rows = rows;
        state.cols = cols;
        Ok(())
    }

    /// Signal the child to terminate. The waiter thread then observes the exit
    /// and the pump emits `Exit`.
    pub fn kill(&self) -> Result<(), PtyError> {
        lock(&self.killer).kill().map_err(PtyError::Io)
    }

    pub fn is_alive(&self) -> bool {
        self.exit.borrow().is_none()
    }

    pub fn info(&self) -> TerminalInfo {
        let state = lock(&self.state);
        let exit = self.exit.borrow().clone();
        TerminalInfo {
            id: self.id.clone(),
            pid: self.pid,
            rows: state.rows,
            cols: state.cols,
            cwd: state.cwd.clone(),
            alive: exit.is_none(),
            exit_code: exit.and_then(|e| e.code),
            seq: lock(&self.scrollback).end_seq(),
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.integration.cleanup();
    }
}

/// Registry of live terminals, shared through `AppState`.
#[derive(Clone)]
pub struct PtyManager {
    terminals: Arc<Mutex<HashMap<String, Arc<Terminal>>>>,
    /// Scrollback sizing applied to every terminal this manager spawns.
    ///
    /// Configurable so a test can drive the prune/replay-gap path with a few
    /// hundred bytes instead of the 12 MiB the production threshold needs. The
    /// default is the production sizing, so nothing about a real run changes.
    limits: (usize, usize),
}

impl Default for PtyManager {
    fn default() -> Self {
        Self {
            terminals: Arc::default(),
            limits: (scrollback::ROLLING_BYTES, scrollback::PRUNE_BYTES),
        }
    }
}

impl PtyManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Manager whose terminals keep `rolling` bytes of history and prune once
    /// past `prune`. See [`PtyManager::limits`].
    pub fn with_scrollback_limits(rolling: usize, prune: usize) -> Self {
        Self {
            terminals: Arc::default(),
            limits: (rolling, prune),
        }
    }

    pub fn get(&self, id: &str) -> Option<Arc<Terminal>> {
        lock(&self.terminals).get(id).cloned()
    }

    /// Every terminal, newest-id-last (sorted for a stable API response).
    pub fn list(&self) -> Vec<TerminalInfo> {
        let mut infos: Vec<_> = lock(&self.terminals).values().map(|t| t.info()).collect();
        infos.sort_by(|a, b| a.id.cmp(&b.id));
        infos
    }

    /// Kill a terminal and drop it from the registry.
    ///
    /// Killing is best-effort: an already-dead shell yields an error we ignore,
    /// since the caller's intent (it's gone) is satisfied either way.
    pub fn remove(&self, id: &str) -> Result<(), PtyError> {
        let terminal = lock(&self.terminals).remove(id).ok_or(PtyError::NotFound)?;
        let _ = terminal.kill();
        Ok(())
    }

    /// Spawn a PTY plus its three blocking threads and the pump task.
    ///
    /// **Blocking**: `openpty` and `spawn_command` fork/exec, so call this from
    /// `tokio::task::spawn_blocking`. It must still run inside the runtime
    /// context — the pump is a `tokio::spawn`.
    pub fn spawn(&self, opts: SpawnOptions, data_dir: PathBuf) -> Result<TerminalInfo, PtyError> {
        let rows = opts.rows.unwrap_or(DEFAULT_ROWS).max(1);
        let cols = opts.cols.unwrap_or(DEFAULT_COLS).max(1);

        // Resolve + validate cwd before touching the PTY: a bad path should be a
        // 400, not a half-spawned shell.
        let cwd = resolve_cwd(opts.cwd.as_deref())?;

        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| PtyError::Pty(e.to_string()))?;

        let id = uuid::Uuid::new_v4().to_string();

        // `new_default_prog` runs the login shell (argv0 prefixed with `-`,
        // cmdbuilder.rs:526-533) so rc files are sourced — required for the OSC 7
        // hook to be installed.
        let (mut cmd, program) = match &opts.shell {
            Some(shell) => (CommandBuilder::new(shell), shell.clone()),
            None => {
                let builder = CommandBuilder::new_default_prog();
                let program = builder.get_shell();
                (builder, program)
            }
        };
        cmd.args(&opts.args);
        cmd.cwd(&cwd);
        // Advertise a capable terminal — xterm.js handles 256 colours and
        // truecolor (env borrowed from sshx `terminal/unix.rs:99-101`).
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("SPEC_ADE_TERMINAL_ID", &id);

        let integration = shell_integration::prepare(&program, &data_dir, &id);
        for (k, v) in &integration.env {
            cmd.env(k, v);
        }
        // Caller-supplied env last so it can override anything above.
        for (k, v) in &opts.env {
            cmd.env(k, v);
        }

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| PtyError::Pty(e.to_string()))?;
        let pid = child.process_id();
        let killer = child.clone_killer();

        // Drop our slave handle now. While we hold it the PTY has a writer other
        // than the child, so the master read would never see EOF when the shell
        // exits (unix.rs:93-106 maps EIO → Ok(0)).
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| PtyError::Pty(e.to_string()))?;
        // `take_writer` is valid exactly once (unix.rs:357-364); the writer
        // thread owns it for the rest of the terminal's life.
        let mut writer = pair
            .master
            .take_writer()
            .map_err(|e| PtyError::Pty(e.to_string()))?;

        let (raw_tx, raw_rx) = mpsc::channel::<Bytes>(CHANNEL_CAPACITY);
        let (input_tx, mut input_rx) = mpsc::channel::<Bytes>(CHANNEL_CAPACITY);
        let (events_tx, _) = broadcast::channel::<TerminalEvent>(BROADCAST_CAPACITY);
        let (exit_tx, exit_rx) = watch::channel::<Option<ExitInfo>>(None);

        // (A) reader thread: blocking read → mpsc.
        let reader_id = id.clone();
        std::thread::Builder::new()
            .name(format!("pty-read-{reader_id}"))
            .spawn(move || {
                let mut buf = [0u8; READ_BUF];
                loop {
                    match reader.read(&mut buf) {
                        // EOF (EIO on unix) — the shell is gone.
                        Ok(0) => break,
                        Ok(n) => {
                            if raw_tx
                                .blocking_send(Bytes::copy_from_slice(&buf[..n]))
                                .is_err()
                            {
                                // Pump dropped: terminal is being torn down.
                                break;
                            }
                        }
                        Err(e) => {
                            tracing::debug!("pty {reader_id} read ended: {e}");
                            break;
                        }
                    }
                }
            })?;

        // (B) writer thread: mpsc → blocking write.
        let writer_id = id.clone();
        std::thread::Builder::new()
            .name(format!("pty-write-{writer_id}"))
            .spawn(move || {
                while let Some(data) = input_rx.blocking_recv() {
                    if writer.write_all(&data).is_err() || writer.flush().is_err() {
                        break;
                    }
                }
                // Dropping the writer sends EOF to the slave (unix.rs:393-405).
            })?;

        // (C) waiter thread: blocking wait → watch.
        let waiter_id = id.clone();
        std::thread::Builder::new()
            .name(format!("pty-wait-{waiter_id}"))
            .spawn(move || {
                let info = match child.wait() {
                    Ok(status) => ExitInfo {
                        // `ExitStatus` reports code 1 alongside a signal name
                        // (lib.rs:182-187); a signalled shell has no meaningful
                        // exit code, so report `None`.
                        code: match status.signal() {
                            Some(_) => None,
                            None => Some(status.exit_code()),
                        },
                        signal: status.signal().map(str::to_string),
                    },
                    Err(e) => {
                        tracing::warn!("pty {waiter_id} wait failed: {e}");
                        ExitInfo {
                            code: None,
                            signal: None,
                        }
                    }
                };
                let _ = exit_tx.send(Some(info));
            })?;

        let terminal = Arc::new(Terminal {
            id: id.clone(),
            pid,
            state: Mutex::new(TerminalState {
                rows,
                cols,
                cwd: cwd.clone(),
            }),
            scrollback: Mutex::new(Scrollback::with_limits(self.limits.0, self.limits.1)),
            events: events_tx,
            input_tx,
            master: Mutex::new(pair.master),
            killer: Mutex::new(killer),
            exit: exit_rx.clone(),
            integration,
        });

        lock(&self.terminals).insert(id.clone(), Arc::clone(&terminal));
        let info = terminal.info();

        tokio::spawn(pump(Arc::clone(&terminal), raw_rx, exit_rx));

        Ok(info)
    }
}

/// The single writer of scrollback + broadcast.
///
/// Runs until the reader thread stops (shell dead or terminal torn down), then
/// waits for the exit status and emits `Exit` last — so a client never sees the
/// exit before the output that preceded it.
async fn pump(
    terminal: Arc<Terminal>,
    mut raw_rx: mpsc::Receiver<Bytes>,
    mut exit_rx: watch::Receiver<Option<ExitInfo>>,
) {
    let mut scanner = osc7::Scanner::new();
    // Trailing bytes of an incomplete UTF-8 sequence, carried to the next read.
    let mut leftover: Vec<u8> = Vec::new();

    loop {
        let chunk = if leftover.is_empty() {
            raw_rx.recv().await
        } else {
            // Holding bytes back: don't wait indefinitely for the sequence to be
            // completed, or a stream ending mid-sequence would stall
            // (SPEC-001 §4 [INVENTED-9]).
            match tokio::time::timeout(UTF8_FLUSH_AFTER, raw_rx.recv()).await {
                Ok(chunk) => chunk,
                Err(_) => {
                    publish(
                        &terminal,
                        &mut scanner,
                        Bytes::from(std::mem::take(&mut leftover)),
                    );
                    continue;
                }
            }
        };

        match chunk {
            Some(chunk) => {
                // Splice held-back bytes in front so the sequence is whole.
                let buf = if leftover.is_empty() {
                    chunk
                } else {
                    let mut joined = std::mem::take(&mut leftover);
                    joined.extend_from_slice(&chunk);
                    Bytes::from(joined)
                };

                let split = utf8::split_incomplete_tail(&buf);
                if split < buf.len() {
                    leftover.extend_from_slice(&buf[split..]);
                }
                if split > 0 {
                    publish(&terminal, &mut scanner, buf.slice(..split));
                }
            }
            None => {
                // Reader finished. Flush whatever we were holding — at EOF an
                // incomplete sequence will never be completed.
                if !leftover.is_empty() {
                    publish(
                        &terminal,
                        &mut scanner,
                        Bytes::from(std::mem::take(&mut leftover)),
                    );
                }
                break;
            }
        }
    }

    // Wait for the waiter thread's verdict. The read loop usually ends first
    // (EOF precedes reaping), so this resolves almost immediately.
    let exit = loop {
        if let Some(info) = exit_rx.borrow_and_update().clone() {
            break info;
        }
        if exit_rx.changed().await.is_err() {
            // Waiter thread vanished without reporting — synthesize an unknown
            // exit so clients still get a terminal event.
            break ExitInfo {
                code: None,
                signal: None,
            };
        }
    };

    let _ = terminal.events.send(TerminalEvent::Exit(exit));
}

/// Append to scrollback and broadcast, under one lock, plus OSC 7 sniffing.
///
/// Sniffing happens on the same byte stream that goes to the client — the bytes
/// are observed, never consumed (deep-dive 02 §5.2/§5.3: raw passthrough).
fn publish(terminal: &Terminal, scanner: &mut osc7::Scanner, data: Bytes) {
    let paths = scanner.feed(&data);

    let seq_end = {
        let mut scrollback = lock(&terminal.scrollback);
        let seq_end = scrollback.append(data.clone());
        // Send while holding the lock so an attaching client can't observe a
        // chunk that is in neither its snapshot nor its subscription.
        let _ = terminal
            .events
            .send(TerminalEvent::Output { data, seq_end });
        seq_end
    };
    debug_assert!(seq_end > 0);

    for path in paths {
        lock(&terminal.state).cwd = path.clone();
        let _ = terminal.events.send(TerminalEvent::Cwd(path));
    }
}

/// Merge replay chunks into frames of at most `WS_CHUNK_MAX`.
///
/// Scrollback holds thousands of 8 KiB reads; sending each as its own WS frame
/// would be needlessly chatty on reconnect. Chunk boundaries carry no meaning to
/// xterm.js, only order does.
fn coalesce(chunks: Vec<Bytes>) -> Vec<Bytes> {
    let mut out: Vec<Bytes> = Vec::new();
    let mut buf: Vec<u8> = Vec::new();
    for chunk in chunks {
        if buf.len() + chunk.len() > WS_CHUNK_MAX && !buf.is_empty() {
            out.push(Bytes::from(std::mem::take(&mut buf)));
        }
        if chunk.len() >= WS_CHUNK_MAX {
            out.push(chunk);
        } else {
            buf.extend_from_slice(&chunk);
        }
    }
    if !buf.is_empty() {
        out.push(Bytes::from(buf));
    }
    out
}

/// Validate and canonicalize a requested working directory.
///
/// Defaults to `$HOME`, then `/`. A missing or non-directory path is a client
/// error (`400`), not a spawn failure.
fn resolve_cwd(requested: Option<&str>) -> Result<String, PtyError> {
    let path = match requested {
        Some(p) if !p.trim().is_empty() => PathBuf::from(p),
        _ => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home),
            None => PathBuf::from("/"),
        },
    };
    let meta =
        std::fs::metadata(&path).map_err(|_| PtyError::BadCwd(path.display().to_string()))?;
    if !meta.is_dir() {
        return Err(PtyError::BadCwd(path.display().to_string()));
    }
    // Canonicalize so the reported cwd matches what OSC 7 will report (symlinks
    // resolved), keeping the frontend breadcrumb consistent.
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    Ok(path.display().to_string())
}

/// Lock a mutex, recovering from poisoning.
///
/// A panic in one terminal's handler must not permanently break that terminal
/// for everyone else. The guarded data is plain metadata with no invariant that
/// a mid-panic write could corrupt, so taking the inner value is safe.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
