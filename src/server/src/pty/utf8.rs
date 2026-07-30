//! UTF-8 chunk-boundary handling for PTY output.
//!
//! The PTY reader hands us arbitrary byte splits, so a multi-byte character can
//! land across two reads (deep-dive 02 §5.1). Sending half a character would make
//! xterm.js render a replacement glyph for that write.
//!
//! SPEC-001 §4 [INVENTED-8]: we do NOT run the stream through a lossy decoder
//! (`encoding_rs`, as sshx does). A decoder rewrites invalid bytes to U+FFFD,
//! which breaks the raw-passthrough contract (§2.3/§5.2) — `cat`ing a binary file
//! would come out corrupted. Instead we *hold back* only a trailing byte sequence
//! that is a valid but incomplete UTF-8 prefix, and forward everything else
//! byte-for-byte. Invalid bytes are never held back: they can't be completed, so
//! waiting for more input would stall output forever.

/// Longest UTF-8 encoding, and therefore the most bytes we ever hold back
/// (a 4-byte sequence missing its last byte = 3 held bytes).
const MAX_UTF8_LEN: usize = 4;

/// Expected total length of the UTF-8 sequence a lead byte starts, or `None` if
/// `b` is not a lead byte (ASCII is handled by the caller; continuation bytes
/// `10xxxxxx` and the invalid `0xF8..=0xFF` return `None`).
fn utf8_seq_len(b: u8) -> Option<usize> {
    match b {
        0x00..=0x7F => Some(1),
        0xC2..=0xDF => Some(2),
        0xE0..=0xEF => Some(3),
        0xF0..=0xF4 => Some(4),
        // 0x80..=0xBF continuation, 0xC0/0xC1 overlong, 0xF5.. out of range.
        _ => None,
    }
}

/// Split point for `buf`: bytes `[..n]` are safe to send now, `buf[n..]` is an
/// incomplete UTF-8 sequence that must be prepended to the next read.
///
/// Returns `buf.len()` when nothing needs holding back (the common case).
///
/// The scan looks at most `MAX_UTF8_LEN` bytes back from the end, so cost is
/// constant regardless of chunk size.
pub fn split_incomplete_tail(buf: &[u8]) -> usize {
    let len = buf.len();
    // Walk back over trailing continuation bytes to find the sequence's lead.
    let mut i = len;
    let floor = len.saturating_sub(MAX_UTF8_LEN);
    while i > floor {
        let candidate = i - 1;
        let b = buf[candidate];
        if b & 0b1100_0000 == 0b1000_0000 {
            // Continuation byte — keep walking back toward the lead byte.
            i = candidate;
            continue;
        }
        // Not a continuation: this is where a sequence starts (or invalid).
        return match utf8_seq_len(b) {
            // Sequence needs more bytes than the buffer holds → hold it back.
            Some(need) if candidate + need > len => candidate,
            // Complete sequence, or a byte that can never be completed
            // (invalid lead / stray continuation) → send everything.
            _ => len,
        };
    }
    // Either the buffer is empty, or its last 4 bytes are all continuation
    // bytes — which cannot be a valid prefix (max sequence is 4 bytes total),
    // so there is nothing worth holding back.
    len
}

/// Round `idx` down to a UTF-8 character boundary within `buf`.
///
/// Used when slicing the scrollback for replay at a client-supplied byte offset
/// (SPEC-001 §5.2). Chunks stored in the scrollback are already character
/// aligned, so in practice `idx` lands on a boundary; this is the defensive path
/// for a client that sends a skewed `after_seq`.
///
/// Bytes that are not part of a valid sequence are treated as their own
/// boundaries — the raw stream may legitimately contain non-UTF-8 data, and
/// scanning past it would drop bytes.
pub fn prev_char_boundary(buf: &[u8], idx: usize) -> usize {
    let idx = idx.min(buf.len());
    // Start and end of the buffer are always boundaries. The end matters: a
    // caller slicing at `buf.len()` has no byte at `idx` to inspect.
    if idx == 0 || idx == buf.len() {
        return idx;
    }
    // If idx is not inside a multi-byte sequence, it's already a boundary.
    if buf[idx] & 0b1100_0000 != 0b1000_0000 {
        return idx;
    }
    let floor = idx.saturating_sub(MAX_UTF8_LEN - 1);
    let mut i = idx;
    while i > floor {
        i -= 1;
        if buf[i] & 0b1100_0000 != 0b1000_0000 {
            // Found the lead byte. If its sequence really covers idx, the
            // boundary is at the lead; otherwise idx follows a broken sequence
            // and stands on its own.
            return match utf8_seq_len(buf[i]) {
                Some(need) if i + need > idx => i,
                _ => idx,
            };
        }
    }
    // No lead byte within reach — stray continuation bytes; idx is a boundary.
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_and_empty_never_hold_back() {
        assert_eq!(split_incomplete_tail(b""), 0);
        assert_eq!(split_incomplete_tail(b"hello"), 5);
        assert_eq!(split_incomplete_tail(b"ls -la\r\n"), 8);
    }

    #[test]
    fn complete_sequences_are_forwarded_whole() {
        // 2-byte (é), 3-byte (€), 4-byte (emoji) — all complete.
        for s in ["é", "€", "😀", "aé", "x€y", "hi 😀"] {
            let b = s.as_bytes();
            assert_eq!(split_incomplete_tail(b), b.len(), "input {s:?}");
        }
    }

    #[test]
    fn incomplete_tail_is_held_back() {
        // "aé" with the last byte of é missing → hold back 1 byte, emit "a".
        let full = "aé".as_bytes();
        let truncated = &full[..full.len() - 1];
        assert_eq!(split_incomplete_tail(truncated), 1);

        // 3-byte € missing 1 and 2 bytes.
        let euro = "x€".as_bytes();
        assert_eq!(split_incomplete_tail(&euro[..euro.len() - 1]), 1);
        assert_eq!(split_incomplete_tail(&euro[..euro.len() - 2]), 1);

        // 4-byte emoji missing 1..=3 bytes.
        let emoji = "😀".as_bytes();
        for missing in 1..=3 {
            assert_eq!(
                split_incomplete_tail(&emoji[..emoji.len() - missing]),
                0,
                "missing {missing}"
            );
        }
    }

    #[test]
    fn split_then_rejoin_reconstructs_the_character() {
        // The pump's actual contract: hold back, prepend to the next read, and
        // the character must come out intact.
        let text = "cd /tmp && echo 日本語";
        let bytes = text.as_bytes();
        // Cut at every possible boundary; rejoining must always reproduce input.
        for cut in 0..=bytes.len() {
            let (first, second) = bytes.split_at(cut);
            let n = split_incomplete_tail(first);
            let mut emitted = first[..n].to_vec();
            let mut buf = first[n..].to_vec();
            buf.extend_from_slice(second);
            let n2 = split_incomplete_tail(&buf);
            emitted.extend_from_slice(&buf[..n2]);
            // At EOF the pump flushes the remainder.
            emitted.extend_from_slice(&buf[n2..]);
            assert_eq!(emitted, bytes, "cut at {cut}");
        }
    }

    #[test]
    fn invalid_bytes_are_never_held_back() {
        // Raw binary must pass through untouched (SPEC-001 §4 [INVENTED-8]) —
        // holding an uncompletable byte would stall output forever.
        assert_eq!(split_incomplete_tail(&[0xFF]), 1);
        assert_eq!(split_incomplete_tail(&[0x41, 0xFF, 0xFE]), 3);
        assert_eq!(split_incomplete_tail(&[0xC0]), 1); // overlong lead
        // A stray continuation byte with no lead in reach.
        assert_eq!(split_incomplete_tail(&[0x80, 0x80, 0x80, 0x80, 0x80]), 5);
    }

    #[test]
    fn boundary_rounds_down_into_a_sequence() {
        let bytes = "a€b".as_bytes(); // a=1, €=3 (idx 1..4), b=1
        assert_eq!(prev_char_boundary(bytes, 0), 0);
        assert_eq!(prev_char_boundary(bytes, 1), 1); // start of €
        assert_eq!(prev_char_boundary(bytes, 2), 1); // inside € → back to lead
        assert_eq!(prev_char_boundary(bytes, 3), 1); // inside € → back to lead
        assert_eq!(prev_char_boundary(bytes, 4), 4); // start of b
        assert_eq!(prev_char_boundary(bytes, 5), 5); // end
        // Past the end clamps.
        assert_eq!(prev_char_boundary(bytes, 99), bytes.len());
    }

    #[test]
    fn boundary_leaves_non_utf8_bytes_alone() {
        // 0xFF is not a lead byte, so a following 0x80 stands on its own — the
        // slice must not swallow raw bytes looking for a sequence start.
        let bytes = [0x41u8, 0xFF, 0x80, 0x42];
        assert_eq!(prev_char_boundary(&bytes, 2), 2);
        assert_eq!(prev_char_boundary(&bytes, 3), 3);
    }
}
