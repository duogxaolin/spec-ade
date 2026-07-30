//! Content probing — text/binary classification, EOL detection, display mime.
//!
//! SPEC-002 §5.4. Classification is content-based ([INVENTED-8]): a NUL byte in
//! the first 8 KiB, or invalid UTF-8 anywhere, means binary — extensions lie
//! (`.log` can be binary, extensionless files can be text). This mirrors the
//! git/ripgrep heuristic, which is cheap and right in practice.

/// How many leading bytes are scanned for NUL.
const NUL_SCAN_LIMIT: usize = 8 * 1024;

/// Line-ending flavour of a text file — display metadata only; content is
/// round-tripped verbatim ([INVENTED-15], never normalized).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eol {
    Lf,
    Crlf,
    Mixed,
}

impl Eol {
    pub fn as_str(self) -> &'static str {
        match self {
            Eol::Lf => "lf",
            Eol::Crlf => "crlf",
            Eol::Mixed => "mixed",
        }
    }
}

/// Classification verdict.
#[derive(Debug, PartialEq, Eq)]
pub enum Classified {
    Text { eol: Eol },
    Binary,
}

/// Classify file contents ([INVENTED-8]).
pub fn classify(bytes: &[u8]) -> Classified {
    let scan = &bytes[..bytes.len().min(NUL_SCAN_LIMIT)];
    if scan.contains(&0) {
        return Classified::Binary;
    }
    if std::str::from_utf8(bytes).is_err() {
        return Classified::Binary;
    }
    Classified::Text {
        eol: detect_eol(bytes),
    }
}

/// Count `\r\n` vs bare-`\n` line endings. No newlines at all reads as LF —
/// the ubiquitous default, and the value is display-only.
pub fn detect_eol(bytes: &[u8]) -> Eol {
    let mut crlf = 0usize;
    let mut bare_lf = 0usize;
    let mut prev_cr = false;
    for &b in bytes {
        match b {
            b'\n' if prev_cr => crlf += 1,
            b'\n' => bare_lf += 1,
            _ => {}
        }
        prev_cr = b == b'\r';
    }
    match (crlf, bare_lf) {
        (0, _) => Eol::Lf,
        (_, 0) => Eol::Crlf,
        _ => Eol::Mixed,
    }
}

/// Best-effort mime from the extension — used only in "can't open this"
/// notices, so a small table beats a dependency ([INVENTED-7]).
pub fn mime_for(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "woff" | "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        "js" | "mjs" | "cjs" => "text/javascript",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "md" | "markdown" => "text/markdown",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_and_multibyte_utf8_are_text() {
        assert_eq!(
            classify(b"hello\nworld\n"),
            Classified::Text { eol: Eol::Lf }
        );
        assert_eq!(
            classify("xin chào thế giới €🦀\n".as_bytes()),
            Classified::Text { eol: Eol::Lf }
        );
        // Empty file: text, LF by convention.
        assert_eq!(classify(b""), Classified::Text { eol: Eol::Lf });
    }

    #[test]
    fn nul_byte_and_invalid_utf8_are_binary() {
        assert_eq!(classify(b"abc\x00def"), Classified::Binary);
        assert_eq!(classify(&[0xFF, 0xFE, 0x41]), Classified::Binary);
        // Invalid UTF-8 beyond the NUL scan window is still caught.
        let mut big = vec![b'a'; NUL_SCAN_LIMIT + 10];
        big.push(0xFF);
        assert_eq!(classify(&big), Classified::Binary);
    }

    #[test]
    fn eol_detection_covers_all_flavours() {
        assert_eq!(detect_eol(b"a\nb\n"), Eol::Lf);
        assert_eq!(detect_eol(b"a\r\nb\r\n"), Eol::Crlf);
        assert_eq!(detect_eol(b"a\r\nb\n"), Eol::Mixed);
        assert_eq!(detect_eol(b"no newline"), Eol::Lf);
        // A bare \r is not a line ending — old-Mac files read as no-newline.
        assert_eq!(detect_eol(b"a\rb"), Eol::Lf);
    }

    #[test]
    fn mime_lookup_is_case_insensitive_with_fallback() {
        assert_eq!(mime_for("logo.PNG"), "image/png");
        assert_eq!(mime_for("noext"), "application/octet-stream");
        assert_eq!(mime_for("archive.tar"), "application/x-tar");
    }
}
