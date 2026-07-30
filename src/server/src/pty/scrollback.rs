//! Scrollback ring buffer — output history for reconnect/replay.
//!
//! SPEC-001 §3.3/§5.2. A terminal outlives any single WebSocket (a page reload
//! must not lose the shell), so output is appended here as it is produced and
//! replayed to whichever client attaches next.
//!
//! `seq` is the running count of output bytes since spawn. A client tracks how
//! many bytes it has consumed and reconnects with `?after_seq=N`; we hand back
//! the tail from `N`. Sizes follow sshx (`runner.rs:15-17`), which deep-dive 02
//! §5.1 recommends adopting: keep ~8 MiB, prune once past 12 MiB, so pruning is
//! amortized instead of running on every append.

use bytes::Bytes;

use super::utf8::prev_char_boundary;

/// Keep at least this much history for replay.
pub const ROLLING_BYTES: usize = 8 << 20;
/// Prune down to `ROLLING_BYTES` once the buffer exceeds this.
pub const PRUNE_BYTES: usize = 12 << 20;

/// Ring buffer of output chunks tagged with a byte-offset range.
#[derive(Debug)]
pub struct Scrollback {
    /// Chunks in emission order. Each is exactly as it was broadcast.
    chunks: std::collections::VecDeque<Bytes>,
    /// Byte offset of the first byte still held (rises as we prune).
    start_seq: u64,
    /// Byte offset one past the last byte held — total bytes ever produced.
    end_seq: u64,
    /// Cached `chunks.iter().map(len).sum()`.
    bytes: usize,
    rolling: usize,
    prune: usize,
}

/// Result of a replay request.
#[derive(Debug, PartialEq, Eq)]
pub struct Replay {
    /// Chunks to send, in order.
    pub chunks: Vec<Bytes>,
    /// Offset of the first byte returned. Greater than the requested `after_seq`
    /// when history was already pruned — the caller must tell the client so it
    /// knows the stream has a hole.
    pub from_seq: u64,
    /// Offset one past the last byte returned; the client's new cursor.
    pub end_seq: u64,
}

impl Default for Scrollback {
    fn default() -> Self {
        Self::new()
    }
}

impl Scrollback {
    pub fn new() -> Self {
        Self::with_limits(ROLLING_BYTES, PRUNE_BYTES)
    }

    /// Construct with explicit limits — used by tests to exercise pruning
    /// without allocating megabytes.
    pub fn with_limits(rolling: usize, prune: usize) -> Self {
        debug_assert!(
            prune >= rolling,
            "prune threshold must be >= rolling target"
        );
        Self {
            chunks: std::collections::VecDeque::new(),
            start_seq: 0,
            end_seq: 0,
            bytes: 0,
            rolling,
            prune,
        }
    }

    /// Offset one past the newest byte (total produced since spawn).
    pub fn end_seq(&self) -> u64 {
        self.end_seq
    }

    /// Offset of the oldest byte still available for replay.
    pub fn start_seq(&self) -> u64 {
        self.start_seq
    }

    /// Append a chunk and return its ending offset.
    ///
    /// Empty chunks are ignored so they can't create zero-length entries that
    /// complicate slicing.
    pub fn append(&mut self, chunk: Bytes) -> u64 {
        if chunk.is_empty() {
            return self.end_seq;
        }
        self.end_seq += chunk.len() as u64;
        self.bytes += chunk.len();
        self.chunks.push_back(chunk);
        if self.bytes > self.prune {
            self.prune_front();
        }
        self.end_seq
    }

    /// Drop whole chunks from the front until at or below `rolling`.
    ///
    /// Only whole chunks are dropped, so the remaining data stays chunk-aligned
    /// (and therefore character-aligned, since the pump only appends aligned
    /// chunks). `start_seq` rises accordingly.
    fn prune_front(&mut self) {
        while self.bytes > self.rolling {
            match self.chunks.pop_front() {
                Some(front) => {
                    self.bytes -= front.len();
                    self.start_seq += front.len() as u64;
                }
                None => break,
            }
        }
    }

    /// Collect everything after `after_seq`.
    ///
    /// - `after_seq >= end_seq` → nothing to replay (client is current or ahead).
    /// - `after_seq < start_seq` → history was pruned; returns what's left with
    ///   `from_seq > after_seq` so the caller can report the gap.
    /// - otherwise → the tail starting exactly at `after_seq`, splitting the
    ///   chunk that straddles it at a character boundary.
    pub fn replay_from(&self, after_seq: u64) -> Replay {
        if after_seq >= self.end_seq {
            return Replay {
                chunks: Vec::new(),
                from_seq: self.end_seq,
                end_seq: self.end_seq,
            };
        }

        let target = after_seq.max(self.start_seq);
        let mut chunks = Vec::new();
        let mut from_seq = target;
        // Offset of the chunk currently being examined.
        let mut cursor = self.start_seq;

        for chunk in &self.chunks {
            let chunk_end = cursor + chunk.len() as u64;
            if chunk_end <= target {
                // Entirely before the requested offset.
                cursor = chunk_end;
                continue;
            }
            if cursor >= target {
                chunks.push(chunk.clone());
            } else {
                // Straddles the offset — cut it. Rounding down keeps a
                // multi-byte character intact; the client already has the bytes
                // before `target`, and a boundary-aligned `after_seq` (the norm)
                // makes this a no-op.
                let split = (target - cursor) as usize;
                let split = prev_char_boundary(chunk, split);
                from_seq = cursor + split as u64;
                chunks.push(chunk.slice(split..));
            }
            cursor = chunk_end;
        }

        Replay {
            chunks,
            from_seq,
            end_seq: self.end_seq,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flatten(chunks: &[Bytes]) -> Vec<u8> {
        chunks.iter().flat_map(|c| c.iter().copied()).collect()
    }

    #[test]
    fn append_advances_seq_and_ignores_empty() {
        let mut sb = Scrollback::new();
        assert_eq!(sb.end_seq(), 0);
        assert_eq!(sb.append(Bytes::from_static(b"abc")), 3);
        assert_eq!(sb.append(Bytes::new()), 3, "empty chunk must not move seq");
        assert_eq!(sb.append(Bytes::from_static(b"de")), 5);
        assert_eq!(sb.start_seq(), 0);
    }

    #[test]
    fn replay_from_zero_returns_everything() {
        let mut sb = Scrollback::new();
        sb.append(Bytes::from_static(b"hello "));
        sb.append(Bytes::from_static(b"world"));

        let r = sb.replay_from(0);
        assert_eq!(flatten(&r.chunks), b"hello world");
        assert_eq!(r.from_seq, 0);
        assert_eq!(r.end_seq, 11);
    }

    #[test]
    fn replay_splits_the_straddling_chunk() {
        let mut sb = Scrollback::new();
        sb.append(Bytes::from_static(b"hello "));
        sb.append(Bytes::from_static(b"world"));

        // Cursor inside the first chunk.
        let r = sb.replay_from(3);
        assert_eq!(flatten(&r.chunks), b"lo world");
        assert_eq!(r.from_seq, 3);

        // Cursor exactly on a chunk boundary — no split needed.
        let r = sb.replay_from(6);
        assert_eq!(flatten(&r.chunks), b"world");
        assert_eq!(r.from_seq, 6);
    }

    #[test]
    fn replay_at_or_past_end_is_empty() {
        let mut sb = Scrollback::new();
        sb.append(Bytes::from_static(b"abc"));

        for cursor in [3, 4, 999] {
            let r = sb.replay_from(cursor);
            assert!(r.chunks.is_empty(), "cursor {cursor}");
            assert_eq!(r.from_seq, 3);
            assert_eq!(r.end_seq, 3);
        }
    }

    #[test]
    fn every_cursor_yields_the_exact_remaining_tail() {
        // The reconnect invariant: no byte lost, no byte duplicated.
        let mut sb = Scrollback::new();
        let full: Vec<u8> = (0..30u8).collect();
        for chunk in full.chunks(7) {
            sb.append(Bytes::copy_from_slice(chunk));
        }
        for cursor in 0..=full.len() {
            let r = sb.replay_from(cursor as u64);
            assert_eq!(flatten(&r.chunks), &full[cursor..], "cursor {cursor}");
            assert_eq!(r.from_seq, cursor.min(full.len()) as u64);
        }
    }

    #[test]
    fn prune_drops_oldest_and_raises_start_seq() {
        // Rolling 10 B, prune at 15 B: append 5 B chunks.
        let mut sb = Scrollback::with_limits(10, 15);
        for _ in 0..4 {
            sb.append(Bytes::from_static(b"abcde"));
        }
        // 20 B produced; over the 15 B threshold, pruned back to <= 10 B.
        assert_eq!(sb.end_seq(), 20);
        assert_eq!(sb.start_seq(), 10);

        let r = sb.replay_from(0);
        // The oldest 10 B are gone, so replay starts later than requested — the
        // caller must report this gap to the client.
        assert_eq!(r.from_seq, 10);
        assert_eq!(flatten(&r.chunks), b"abcdeabcde");
    }

    #[test]
    fn stale_cursor_after_prune_reports_the_gap() {
        let mut sb = Scrollback::with_limits(10, 15);
        for _ in 0..4 {
            sb.append(Bytes::from_static(b"abcde"));
        }
        // Client asks for byte 2, but history now starts at 10.
        let r = sb.replay_from(2);
        assert_eq!(r.from_seq, 10, "must report where data actually resumes");
        assert_eq!(r.end_seq, 20);
        assert_eq!(flatten(&r.chunks).len(), 10);
    }

    #[test]
    fn replay_never_splits_a_multibyte_character() {
        let mut sb = Scrollback::new();
        // "€" is 3 bytes at offsets 1..4.
        sb.append(Bytes::from("a€b".to_string()));

        // A client cursor landing inside the character rounds back to its start,
        // so the frame we send is never half a code point.
        let r = sb.replay_from(2);
        assert_eq!(r.from_seq, 1);
        assert_eq!(flatten(&r.chunks), "€b".as_bytes());
    }

    #[test]
    fn binary_output_is_preserved_byte_for_byte() {
        // Raw passthrough (SPEC-001 §4 [INVENTED-8]): invalid UTF-8 must survive.
        let mut sb = Scrollback::new();
        let binary = Bytes::from_static(&[0x00, 0xFF, 0xFE, 0x80, 0x41]);
        sb.append(binary.clone());
        assert_eq!(flatten(&sb.replay_from(0).chunks), binary.as_ref());
    }
}
