//! Per-session event log with replay (SPEC-003 §3.3 / §5.6).
//!
//! ACP has no replay: `session/update` notifications are fire-and-forget, so a
//! browser that reloads mid-turn would lose everything already streamed. Spec ADE
//! therefore keeps its own log and exposes `?after_seq=N`, mirroring the
//! `pty::scrollback` contract from SPEC-001.
//!
//! Difference from scrollback: `seq` counts **events**, not bytes, because an ACP
//! event is atomic — half a `tool_call` is meaningless, so there is nothing to
//! split. Byte accounting is still tracked to bound memory, since one event can
//! carry a large `tool_call` payload.

use std::collections::VecDeque;

use super::event::AcpEvent;

/// Keep at most this many events per session ([INVENTED-2]).
pub const ACP_LOG_MAX_EVENTS: usize = 5000;
/// …or this many bytes of serialized payload, whichever hits first.
pub const ACP_LOG_MAX_BYTES: usize = 8 << 20;

/// One logged event: the payload plus the `seq` assigned on append.
#[derive(Debug, Clone)]
pub struct LoggedEvent {
    pub seq: u64,
    pub event: AcpEvent,
    /// Serialized size, cached so pruning doesn't re-serialize.
    size: usize,
}

/// Result of a replay request.
#[derive(Debug)]
pub struct Replay {
    /// Events to send, oldest first.
    pub events: Vec<LoggedEvent>,
    /// `Some(from_seq)` when older events were already pruned — the caller MUST
    /// tell the client so the UI knows it has a hole rather than silently
    /// rendering a stream that is missing its middle.
    pub truncated_from: Option<u64>,
    /// `seq` of the newest event held; the client's new cursor.
    pub end_seq: u64,
}

/// Append-only log of one session's events, pruned from the front.
#[derive(Debug)]
pub struct EventLog {
    events: VecDeque<LoggedEvent>,
    /// `seq` of the last event ever appended. Never reset, never reused.
    end_seq: u64,
    bytes: usize,
    max_events: usize,
    max_bytes: usize,
}

impl Default for EventLog {
    fn default() -> Self {
        Self::new()
    }
}

impl EventLog {
    pub fn new() -> Self {
        Self::with_limits(ACP_LOG_MAX_EVENTS, ACP_LOG_MAX_BYTES)
    }

    /// Explicit limits — the seam tests use to exercise pruning with a handful
    /// of events instead of 5000.
    pub fn with_limits(max_events: usize, max_bytes: usize) -> Self {
        Self {
            events: VecDeque::new(),
            end_seq: 0,
            bytes: 0,
            max_events: max_events.max(1),
            max_bytes,
        }
    }

    /// `seq` of the newest event (0 when empty).
    pub fn end_seq(&self) -> u64 {
        self.end_seq
    }

    /// `seq` of the oldest event still held (0 when empty).
    pub fn start_seq(&self) -> u64 {
        self.events.front().map_or(0, |e| e.seq)
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Append an event, assign it the next `seq`, and return the stored copy.
    ///
    /// The returned clone is what gets broadcast to attached sockets, so the
    /// broadcast and the log always agree on `seq` — a socket can dedupe by it.
    pub fn append(&mut self, event: AcpEvent) -> LoggedEvent {
        self.end_seq += 1;
        // A payload that can't serialize would be a programming error in
        // `event.rs`, not a runtime condition; estimate rather than panic so one
        // bad event can't take down a live session.
        let size = serde_json::to_vec(&event).map_or(256, |v| v.len());
        let logged = LoggedEvent {
            seq: self.end_seq,
            event,
            size,
        };
        self.bytes += size;
        self.events.push_back(logged.clone());
        self.prune_front();
        logged
    }

    /// Drop oldest events until both limits hold.
    ///
    /// Always leaves at least one event: an empty log after appending would make
    /// `start_seq()` lie about what is available.
    fn prune_front(&mut self) {
        while self.events.len() > self.max_events
            || (self.bytes > self.max_bytes && self.events.len() > 1)
        {
            match self.events.pop_front() {
                Some(front) => self.bytes -= front.size,
                None => break,
            }
        }
    }

    /// Everything with `seq > after_seq`.
    ///
    /// - `after_seq >= end_seq` → nothing (client is current or ahead).
    /// - `after_seq + 1 < start_seq` → the requested next event is gone;
    ///   `truncated_from` reports where the stream actually resumes.
    pub fn replay_from(&self, after_seq: u64) -> Replay {
        if after_seq >= self.end_seq {
            return Replay {
                events: Vec::new(),
                truncated_from: None,
                end_seq: self.end_seq,
            };
        }

        let events: Vec<LoggedEvent> = self
            .events
            .iter()
            .filter(|e| e.seq > after_seq)
            .cloned()
            .collect();

        // A gap exists when the first event we can hand back is not the one the
        // client asked for next.
        let truncated_from = match events.first() {
            Some(first) if first.seq > after_seq + 1 => Some(first.seq),
            // The whole tail was pruned: nothing to hand back even though the
            // cursor is behind. Report the gap at the log's current head.
            None => Some(self.end_seq),
            Some(_) => None,
        };

        Replay {
            events,
            truncated_from,
            end_seq: self.end_seq,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(text: &str) -> AcpEvent {
        AcpEvent::MessageChunk { text: text.into() }
    }

    fn texts(replay: &Replay) -> Vec<String> {
        replay
            .events
            .iter()
            .map(|e| match &e.event {
                AcpEvent::MessageChunk { text } => text.clone(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn seq_starts_at_one_and_is_monotonic() {
        let mut log = EventLog::new();
        assert_eq!(log.end_seq(), 0, "an empty log has produced nothing");
        assert_eq!(log.append(chunk("a")).seq, 1);
        assert_eq!(log.append(chunk("b")).seq, 2);
        assert_eq!(log.end_seq(), 2);
        assert_eq!(log.start_seq(), 1);
    }

    #[test]
    fn replay_from_zero_returns_everything() {
        let mut log = EventLog::new();
        log.append(chunk("a"));
        log.append(chunk("b"));

        let r = log.replay_from(0);
        assert_eq!(texts(&r), ["a", "b"]);
        assert_eq!(r.truncated_from, None);
        assert_eq!(r.end_seq, 2);
    }

    #[test]
    fn every_cursor_yields_the_exact_remaining_tail() {
        // The reconnect invariant: no event lost, no event duplicated.
        let mut log = EventLog::new();
        let all: Vec<String> = (0..10).map(|i| i.to_string()).collect();
        for t in &all {
            log.append(chunk(t));
        }
        for cursor in 0..=all.len() {
            let r = log.replay_from(cursor as u64);
            assert_eq!(texts(&r), all[cursor..], "cursor {cursor}");
            assert_eq!(r.truncated_from, None, "nothing was pruned");
        }
    }

    #[test]
    fn replay_at_or_past_end_is_empty_and_not_truncated() {
        let mut log = EventLog::new();
        log.append(chunk("a"));
        for cursor in [1, 2, 999] {
            let r = log.replay_from(cursor);
            assert!(r.events.is_empty(), "cursor {cursor}");
            assert_eq!(r.truncated_from, None, "being current is not a gap");
            assert_eq!(r.end_seq, 1);
        }
    }

    #[test]
    fn event_count_limit_prunes_oldest_and_raises_start_seq() {
        let mut log = EventLog::with_limits(3, usize::MAX);
        for i in 0..5 {
            log.append(chunk(&i.to_string()));
        }
        assert_eq!(log.len(), 3);
        assert_eq!(log.end_seq(), 5, "seq must not be reused after pruning");
        assert_eq!(log.start_seq(), 3);
        assert_eq!(texts(&log.replay_from(0)), ["2", "3", "4"]);
    }

    #[test]
    fn byte_limit_prunes_but_always_keeps_one_event() {
        // A single event bigger than the whole budget must still be replayable —
        // otherwise a large tool_call would vanish and the client would show a
        // gap with nothing after it.
        let mut log = EventLog::with_limits(usize::MAX, 8);
        log.append(chunk(&"x".repeat(500)));
        assert_eq!(log.len(), 1);
        log.append(chunk(&"y".repeat(500)));
        assert_eq!(log.len(), 1, "the newest event is kept");
        assert_eq!(log.start_seq(), 2);
    }

    #[test]
    fn stale_cursor_after_prune_reports_the_gap() {
        let mut log = EventLog::with_limits(3, usize::MAX);
        for i in 0..5 {
            log.append(chunk(&i.to_string()));
        }
        // Client had seq 1; events 2 and 3 are gone, so the stream resumes at 3.
        let r = log.replay_from(1);
        assert_eq!(r.truncated_from, Some(3), "must report where data resumes");
        assert_eq!(texts(&r), ["2", "3", "4"]);

        // A cursor exactly one behind the head is NOT a gap.
        let r = log.replay_from(2);
        assert_eq!(r.truncated_from, None);
        assert_eq!(texts(&r), ["2", "3", "4"]);
    }

    #[test]
    fn a_cursor_behind_a_fully_pruned_log_still_reports_a_gap() {
        // Pathological but reachable: max_events=1 and the client is far behind.
        let mut log = EventLog::with_limits(1, usize::MAX);
        for i in 0..4 {
            log.append(chunk(&i.to_string()));
        }
        let r = log.replay_from(1);
        assert_eq!(r.truncated_from, Some(4));
        assert_eq!(texts(&r), ["3"]);
    }
}
