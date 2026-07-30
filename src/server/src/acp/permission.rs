//! Parked permission requests (SPEC-003 §5.4).
//!
//! `session/request_permission` is a JSON-RPC **request**: the agent blocks on the
//! answer. But the answer comes from a human, over a WebSocket, seconds later. So
//! the `Responder` is stored here and answered out of band. This works because
//! `Responder::send_fn` is `Box<dyn FnOnce + Send>` — the responder is not tied to
//! the handler's stack frame.
//!
//! Policy this phase ([INVENTED-5]): **always ask**. There is no allow-list yet, so
//! failing closed is the only honest behaviour — auto-approving would grant the
//! agent whatever it asked for while telling the user nothing.
//!
//! Two ways a parked request ends without a human: the timeout
//! ([`ACP_PERMISSION_TIMEOUT`]) and connection teardown. Both answer `Cancelled`,
//! which is a documented outcome in the schema — the agent handles it as "the user
//! went away", not as a protocol error. Leaving it unanswered would wedge the
//! agent's turn forever.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agent_client_protocol::Responder;
use agent_client_protocol::schema::v1::{
    PermissionOptionId, RequestPermissionOutcome, RequestPermissionResponse,
    SelectedPermissionOutcome,
};

/// A parked request is auto-cancelled after this long ([INVENTED-6]).
pub const ACP_PERMISSION_TIMEOUT: Duration = Duration::from_secs(300);

/// Env override for [`ACP_PERMISSION_TIMEOUT`], in seconds. Same purpose as the
/// idle-reaper override: shorten the wait during manual verification (SPEC-003 §8)
/// without recompiling. Tests inject
/// [`AcpLimits`](super::connection::AcpLimits) instead, so they can run in
/// parallel.
const PERMISSION_TIMEOUT_ENV: &str = "SPEC_ADE_ACP_PERMISSION_SECS";

/// The default timeout, honouring the env override.
pub fn permission_timeout() -> Duration {
    super::connection::env_duration(PERMISSION_TIMEOUT_ENV, ACP_PERMISSION_TIMEOUT)
}

/// Why answering a permission request failed.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PermissionError {
    /// No such `requestId`: already answered, timed out, or never existed.
    #[error("no pending permission request {0}")]
    Unknown(String),
    /// The chosen `optionId` was not among the ones the agent offered.
    #[error("option {option} is not offered by request {request}")]
    NotOffered { request: String, option: String },
}

/// One parked request.
struct Pending {
    responder: Responder<RequestPermissionResponse>,
    /// Options the agent offered. An answer is checked against this rather than
    /// forwarded blind — sending an `optionId` the agent never listed would make
    /// it fail a request the user *did* answer.
    options: Vec<String>,
    /// Session this belongs to, so teardown can cancel just that session's.
    session_id: String,
    parked_at: Instant,
}

/// Registry of permission requests awaiting a human answer.
///
/// `std::sync::Mutex` (not tokio's): every critical section is a map lookup, and
/// the guard is dropped before any `.await`.
#[derive(Clone, Default)]
pub struct PendingPermissions {
    inner: Arc<Mutex<HashMap<String, Pending>>>,
}

/// What happened to a parked request, for the `permission_resolved` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Selected(String),
    Cancelled,
}

impl Resolution {
    pub fn as_str(&self) -> &str {
        match self {
            Resolution::Selected(_) => "selected",
            Resolution::Cancelled => "cancelled",
        }
    }
}

impl PendingPermissions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Park a responder and return the id the client answers with.
    ///
    /// The id is a fresh UUID, deliberately *not* the JSON-RPC request id: that is
    /// transport-internal, and exposing it would let a client reference messages
    /// it was never told about.
    pub fn park(
        &self,
        session_id: &str,
        options: Vec<String>,
        responder: Responder<RequestPermissionResponse>,
    ) -> String {
        let request_id = uuid::Uuid::new_v4().to_string();
        lock(&self.inner).insert(
            request_id.clone(),
            Pending {
                responder,
                options,
                session_id: session_id.to_string(),
                parked_at: Instant::now(),
            },
        );
        request_id
    }

    /// Answer with the user's choice.
    ///
    /// An unknown `option_id` leaves the request parked so the user can answer
    /// again — dropping it would strand the agent, and forwarding it would make the
    /// agent reject a decision the user actually made.
    pub fn select(&self, request_id: &str, option_id: &str) -> Result<(), PermissionError> {
        let pending = {
            let mut guard = lock(&self.inner);
            let entry = guard
                .get(request_id)
                .ok_or_else(|| PermissionError::Unknown(request_id.to_string()))?;
            if !entry.options.iter().any(|o| o == option_id) {
                return Err(PermissionError::NotOffered {
                    request: request_id.to_string(),
                    option: option_id.to_string(),
                });
            }
            guard
                .remove(request_id)
                .expect("just checked it is present")
        };

        respond(
            pending,
            RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                PermissionOptionId::from(option_id.to_string()),
            )),
        );
        Ok(())
    }

    /// Answer `Cancelled` — the user dismissed the prompt.
    pub fn cancel(&self, request_id: &str) -> Result<(), PermissionError> {
        let pending = lock(&self.inner)
            .remove(request_id)
            .ok_or_else(|| PermissionError::Unknown(request_id.to_string()))?;
        respond(pending, RequestPermissionOutcome::Cancelled);
        Ok(())
    }

    /// Cancel everything parked for one session (turn cancelled, session closed).
    ///
    /// Returns the ids cancelled so the caller can log a `permission_resolved`
    /// event for each, letting a reattaching client drop stale prompts.
    pub fn cancel_session(&self, session_id: &str) -> Vec<String> {
        let drained: Vec<(String, Pending)> = {
            let mut guard = lock(&self.inner);
            let ids: Vec<String> = guard
                .iter()
                .filter(|(_, p)| p.session_id == session_id)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| guard.remove(&id).map(|p| (id, p)))
                .collect()
        };
        drained
            .into_iter()
            .map(|(id, pending)| {
                respond(pending, RequestPermissionOutcome::Cancelled);
                id
            })
            .collect()
    }

    /// Cancel everything parked, whatever the session — connection teardown.
    pub fn cancel_all(&self) -> Vec<String> {
        let drained: Vec<(String, Pending)> = lock(&self.inner).drain().collect();
        drained
            .into_iter()
            .map(|(id, pending)| {
                respond(pending, RequestPermissionOutcome::Cancelled);
                id
            })
            .collect()
    }

    /// Cancel every request parked longer than `timeout` ([INVENTED-6]).
    ///
    /// Called from a periodic sweep rather than one timer per request: a sweep
    /// cannot leak a task if the request is answered first.
    pub fn cancel_expired(&self, timeout: Duration) -> Vec<String> {
        let drained: Vec<(String, Pending)> = {
            let mut guard = lock(&self.inner);
            let ids: Vec<String> = guard
                .iter()
                .filter(|(_, p)| p.parked_at.elapsed() >= timeout)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| guard.remove(&id).map(|p| (id, p)))
                .collect()
        };
        drained
            .into_iter()
            .map(|(id, pending)| {
                tracing::debug!("acp: permission request {id} expired; answering cancelled");
                respond(pending, RequestPermissionOutcome::Cancelled);
                id
            })
            .collect()
    }

    /// How many requests are parked. For assertions and `GET /api/acp`.
    pub fn len(&self) -> usize {
        lock(&self.inner).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Send an outcome, logging rather than propagating a send failure.
///
/// A failure here means the connection is already gone, so there is nobody left
/// to tell — and the caller (a WS message handler or a teardown path) has no
/// useful recovery.
fn respond(pending: Pending, outcome: RequestPermissionOutcome) {
    let response = RequestPermissionResponse::new(outcome);
    if let Err(e) = pending.responder.respond(response) {
        tracing::debug!("acp: could not deliver permission outcome: {e}");
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
