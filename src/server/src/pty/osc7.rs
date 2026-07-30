//! OSC 7 sniffing — track the shell's working directory from its output stream.
//!
//! Background (deep-dive 02 §5.3): OSC 7 is emitted *by the shell*, not by the
//! terminal — `ESC ] 7 ; file://<host><path> BEL` (or terminated by ST, `ESC \`)
//! on every directory change, provided the rc file installs the hook (zsh
//! `chpwd`, bash `PROMPT_COMMAND`). We spawn a login shell, so rc files are
//! sourced. None of the reference repos parse this, so the scanner below is ours.
//!
//! Two hard requirements:
//! - **Chunk-boundary safe**: the escape can be split across PTY reads, so this
//!   is a resumable state machine fed byte-by-byte, never a per-read regex.
//! - **Non-consuming**: every byte still goes to xterm.js untouched. We only
//!   observe. Stripping escapes would violate the raw-passthrough contract
//!   (§5.2) — xterm.js swallows OSC 7 itself, harmlessly.

/// Cap on the accumulated payload. A shell that emits an unterminated OSC would
/// otherwise grow this buffer without bound; 4 KiB is far beyond any real path.
const MAX_PAYLOAD: usize = 4096;

/// Position within an OSC 7 escape sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Not in an escape sequence.
    Idle,
    /// Saw `ESC`.
    Esc,
    /// Saw `ESC ]`, now reading the numeric command.
    Command,
    /// Saw `ESC ] 7 ;`, accumulating the payload.
    Payload,
    /// Saw `ESC` while in `Payload` — either ST (`ESC \`) ends it, or the
    /// sequence was interrupted by a new escape.
    PayloadEsc,
}

/// Incremental OSC 7 scanner. Feed it every output byte; it yields a path each
/// time a complete `ESC ] 7 ; file://... (BEL|ST)` sequence is seen.
#[derive(Debug)]
pub struct Scanner {
    state: State,
    /// Digits of the OSC command number, e.g. `7` (or `133` for prompt marks,
    /// which we ignore).
    command: String,
    payload: Vec<u8>,
}

impl Default for Scanner {
    fn default() -> Self {
        Self::new()
    }
}

impl Scanner {
    pub fn new() -> Self {
        Self {
            state: State::Idle,
            command: String::new(),
            payload: Vec::new(),
        }
    }

    /// Feed one chunk; returns every path completed within it.
    ///
    /// A chunk may complete a sequence that started in an earlier chunk, and may
    /// leave a new one half-read for the next call — that state lives in `self`.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        for &b in chunk {
            if let Some(path) = self.step(b) {
                out.push(path);
            }
        }
        out
    }

    /// Advance the machine by one byte, returning a path if one just completed.
    fn step(&mut self, b: u8) -> Option<String> {
        const ESC: u8 = 0x1B;
        const BEL: u8 = 0x07;

        match self.state {
            State::Idle => {
                if b == ESC {
                    self.state = State::Esc;
                }
                None
            }
            State::Esc => {
                match b {
                    b']' => {
                        self.state = State::Command;
                        self.command.clear();
                    }
                    // `ESC ESC` restarts; anything else is some other escape
                    // (CSI, charset select, …) we don't care about.
                    ESC => {}
                    _ => self.state = State::Idle,
                }
                None
            }
            State::Command => {
                match b {
                    b'0'..=b'9' if self.command.len() < 4 => {
                        self.command.push(b as char);
                    }
                    b';' => {
                        if self.command == "7" {
                            self.state = State::Payload;
                            self.payload.clear();
                        } else {
                            // Another OSC (title, prompt marks…). Not ours, and
                            // we must not treat its payload as a path.
                            self.state = State::Idle;
                        }
                    }
                    ESC => self.state = State::Esc,
                    // Non-numeric, non-separator → not an OSC we can read.
                    _ => self.state = State::Idle,
                }
                None
            }
            State::Payload => {
                match b {
                    BEL => {
                        self.state = State::Idle;
                        return parse_file_url(&std::mem::take(&mut self.payload));
                    }
                    ESC => self.state = State::PayloadEsc,
                    _ => {
                        if self.payload.len() >= MAX_PAYLOAD {
                            // Runaway sequence — drop it rather than buffer more.
                            self.payload.clear();
                            self.state = State::Idle;
                        } else {
                            self.payload.push(b);
                        }
                    }
                }
                None
            }
            State::PayloadEsc => {
                match b {
                    // ST terminator: `ESC \`.
                    b'\\' => {
                        self.state = State::Idle;
                        return parse_file_url(&std::mem::take(&mut self.payload));
                    }
                    // A fresh `ESC ]` starts a new OSC: abandon this payload.
                    b']' => {
                        self.payload.clear();
                        self.state = State::Command;
                        self.command.clear();
                    }
                    // Any other escape aborts the sequence.
                    _ => {
                        self.payload.clear();
                        self.state = State::Idle;
                    }
                }
                None
            }
        }
    }
}

/// Parse an OSC 7 payload (`file://<host><path>`) into an absolute path.
///
/// Returns `None` for anything that isn't a `file://` URL with an absolute path —
/// a shell may emit other schemes, and a relative path would be meaningless to
/// the frontend breadcrumb.
fn parse_file_url(payload: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(payload);
    let rest = text.strip_prefix("file://")?;
    // Everything up to the first '/' is the host (often empty or the hostname);
    // the path starts at that '/'.
    let path_start = rest.find('/')?;
    let raw_path = &rest[path_start..];
    let path = percent_decode_path(raw_path);
    path.starts_with('/').then_some(path)
}

/// Percent-decode a URL *path*.
///
/// Deliberately distinct from query decoding: here `+` is a literal plus sign
/// (a legal filename character), not a space. Using form-encoding rules would
/// corrupt any directory whose name contains `+`.
fn percent_decode_path(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ESC ] 7 ; file://host<path> BEL`
    fn bel_seq(path: &str) -> Vec<u8> {
        format!("\x1b]7;file://myhost{path}\x07").into_bytes()
    }

    #[test]
    fn parses_bel_terminated_sequence() {
        let mut s = Scanner::new();
        assert_eq!(s.feed(&bel_seq("/Users/x/proj")), vec!["/Users/x/proj"]);
    }

    #[test]
    fn parses_st_terminated_sequence() {
        // ST is `ESC \` rather than BEL.
        let mut s = Scanner::new();
        let seq = b"\x1b]7;file://myhost/tmp\x1b\\";
        assert_eq!(s.feed(seq), vec!["/tmp"]);
    }

    #[test]
    fn parses_empty_host_form() {
        // `file:///tmp` — no hostname, path starts right after the third slash.
        let mut s = Scanner::new();
        assert_eq!(s.feed(b"\x1b]7;file:///tmp\x07"), vec!["/tmp"]);
    }

    #[test]
    fn survives_chunk_boundaries_at_every_position() {
        // The whole point of a state machine: a PTY read can cut the escape
        // anywhere (deep-dive 02 §5.3).
        let seq = bel_seq("/var/log");
        for cut in 0..=seq.len() {
            let mut s = Scanner::new();
            let mut found = s.feed(&seq[..cut]);
            found.extend(s.feed(&seq[cut..]));
            assert_eq!(found, vec!["/var/log"], "cut at {cut}");
        }
    }

    #[test]
    fn survives_byte_at_a_time_feeding() {
        let seq = bel_seq("/a/b/c");
        let mut s = Scanner::new();
        let mut found = Vec::new();
        for b in &seq {
            found.extend(s.feed(&[*b]));
        }
        assert_eq!(found, vec!["/a/b/c"]);
    }

    #[test]
    fn percent_decodes_and_keeps_plus_literal() {
        let mut s = Scanner::new();
        assert_eq!(
            s.feed(&bel_seq("/tmp/my%20dir/c%2B%2B")),
            vec!["/tmp/my dir/c++"]
        );
        // `+` must stay a plus sign, not become a space — this is a URL path,
        // not form encoding.
        let mut s = Scanner::new();
        assert_eq!(s.feed(&bel_seq("/tmp/a+b")), vec!["/tmp/a+b"]);
    }

    #[test]
    fn ignores_other_osc_commands() {
        let mut s = Scanner::new();
        // OSC 0 = window title, OSC 133 = prompt marks. Neither is a cwd.
        assert!(s.feed(b"\x1b]0;my title\x07").is_empty());
        assert!(s.feed(b"\x1b]133;A\x07").is_empty());
        // Still works afterwards — the machine returned to Idle cleanly.
        assert_eq!(s.feed(&bel_seq("/after")), vec!["/after"]);
    }

    #[test]
    fn ignores_non_file_scheme_and_relative_paths() {
        let mut s = Scanner::new();
        assert!(s.feed(b"\x1b]7;http://example.com/x\x07").is_empty());
        assert!(s.feed(b"\x1b]7;file://host\x07").is_empty()); // no path at all
    }

    #[test]
    fn ignores_plain_output_and_other_escapes() {
        let mut s = Scanner::new();
        assert!(
            s.feed(b"total 0\r\ndrwxr-xr-x  3 user  staff\r\n")
                .is_empty()
        );
        // CSI colour sequence, cursor movement.
        assert!(s.feed(b"\x1b[31mred\x1b[0m\x1b[2J").is_empty());
    }

    #[test]
    fn oversized_payload_is_dropped_not_buffered() {
        let mut s = Scanner::new();
        let mut junk = b"\x1b]7;file://host/".to_vec();
        junk.extend(std::iter::repeat_n(b'x', MAX_PAYLOAD + 100));
        junk.push(0x07);
        assert!(s.feed(&junk).is_empty());
        // And the scanner recovered.
        assert_eq!(s.feed(&bel_seq("/ok")), vec!["/ok"]);
    }

    #[test]
    fn interrupted_sequence_does_not_leak_into_the_next() {
        let mut s = Scanner::new();
        // Start an OSC 7, then interrupt with a CSI before terminating it.
        assert!(s.feed(b"\x1b]7;file://host/partial\x1b[0m").is_empty());
        assert_eq!(s.feed(&bel_seq("/real")), vec!["/real"]);
    }

    #[test]
    fn reports_every_sequence_in_one_chunk() {
        let mut s = Scanner::new();
        let mut buf = bel_seq("/one");
        buf.extend(b"some output\r\n");
        buf.extend(bel_seq("/two"));
        assert_eq!(s.feed(&buf), vec!["/one", "/two"]);
    }
}
