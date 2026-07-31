//! Content search — ripgrep's engine, driven from Axum.
//!
//! Spec: `docs/specs/SPEC-006-search-monitor.md` §3.1, §5.2.
//!
//! The central rule of this phase (04 §6): **stream each match, never collect**.
//! A search over a monorepo can produce more results than the process should hold,
//! and the user only ever looks at the first screenful — so every decision here is
//! about emitting early and stopping early.
//!
//! Layering mirrors `git`: `engine` is blocking and knows nothing about HTTP;
//! `routes::search` owns the SSE translation. The seam is a bounded
//! [`tokio::sync::mpsc`] channel — which is also the cancellation token
//! (§5.4): when the client disconnects the receiver drops, `tx.is_closed()`
//! flips, and the walk quits.

pub mod engine;

use serde::Serialize;

pub use engine::run_blocking;

/// Default result cap ([SPEC-006 INVENTED-7]).
pub const DEFAULT_MAX_RESULTS: usize = 2000;

/// Hard ceiling on `maxResults`, whatever the client asks for.
pub const MAX_MAX_RESULTS: usize = 10_000;

/// Longest line emitted, in bytes ([SPEC-006 INVENTED-3]).
///
/// A minified bundle is one 500 KB line; sending it would cost more than every
/// real result combined and no UI can render it.
pub const MAX_LINE_BYTES: usize = 4096;

/// Files bigger than this are skipped by the walker ([SPEC-006 INVENTED-7]).
pub const MAX_FILESIZE: u64 = 16 * 1024 * 1024;

/// How often a `progress` event is emitted, in milliseconds.
pub const PROGRESS_INTERVAL_MS: u64 = 250;

/// Channel depth between the walk's worker threads and the SSE task.
///
/// Bounded on purpose: a slow client must slow the *walk*, not grow a queue.
pub const CHANNEL_CAPACITY: usize = 256;

/// A validated search request.
#[derive(Debug, Clone)]
pub struct SearchQuery {
    /// Pattern. Literal unless `regex` is set.
    pub query: String,
    /// Treat `query` as a regular expression instead of a literal.
    pub regex: bool,
    /// Case-**sensitive** when true. Default false = case-insensitive, which is
    /// what an editor's search box does.
    pub case: bool,
    /// Match whole words only ([SPEC-006 INVENTED-4]).
    pub word: bool,
    /// Glob filters ([SPEC-006 INVENTED-5]). A plain glob whitelists; `!glob`
    /// excludes. This is exactly `ignore::Override`'s own polarity, so the
    /// strings pass through unchanged.
    pub globs: Vec<String>,
    /// Restrict the walk to this project-relative subdirectory
    /// ([SPEC-006 INVENTED-6]).
    pub path: Option<String>,
    /// Stop after this many matches ([SPEC-006 INVENTED-7]).
    pub max_results: usize,
}

/// What the client receives, one SSE event per variant.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum SearchEvent {
    Match(MatchEvent),
    Progress(ProgressEvent),
    Done(DoneEvent),
    /// A single file could not be read. Non-fatal: the walk continues.
    FileError(FileErrorEvent),
}

impl SearchEvent {
    /// SSE `event:` name. Part of the contract (§3.1).
    pub fn name(&self) -> &'static str {
        match self {
            SearchEvent::Match(_) => "match",
            SearchEvent::Progress(_) => "progress",
            SearchEvent::Done(_) => "done",
            SearchEvent::FileError(_) => "error",
        }
    }
}

/// One matching **line** ([SPEC-006 INVENTED-2]) — not one match. A line with
/// three hits is one event with three `ranges`, which is also how the UI renders it.
#[derive(Debug, Clone, Serialize)]
pub struct MatchEvent {
    /// Project-relative, `/`-separated — the same shape the file API takes, so a
    /// click-to-open needs no conversion (§5.3).
    pub path: String,
    /// 1-based line number.
    pub line: u64,
    /// The line, without its terminator, truncated to [`MAX_LINE_BYTES`].
    pub text: String,
    /// Highlight spans as `[start, end)` **byte** offsets into `text`.
    ///
    /// Empty when the line is not valid UTF-8: the offsets would be meaningless
    /// after lossy conversion, and a missing highlight beats a wrong one.
    pub ranges: Vec<[usize; 2]>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressEvent {
    pub files_scanned: usize,
    pub matches: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoneEvent {
    pub matches: usize,
    /// Number of files that contained at least one match.
    pub files: usize,
    pub files_scanned: usize,
    /// The cap was hit. **Not** a promise that exactly `maxResults` were sent:
    /// `WalkState::Quit` is asynchronous (ignore `walk.rs:1318-1330`), so a few
    /// in-flight matches may still arrive after the decision to stop (§5.2).
    pub truncated: bool,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileErrorEvent {
    pub path: String,
    pub detail: String,
}

/// Everything that can stop a search *before* the stream opens.
///
/// Once the stream is open there are no fatal errors left — a bad file becomes a
/// [`SearchEvent::FileError`] and the walk keeps going.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("query is required")]
    EmptyQuery,
    #[error("invalid pattern: {0}")]
    BadPattern(String),
    #[error("invalid glob: {0}")]
    BadGlob(String),
    #[error("{0}")]
    Path(#[from] crate::files::PathError),
}

impl SearchQuery {
    /// Validate raw query params.
    ///
    /// `max_results` is clamped rather than rejected: a client asking for a
    /// million results has a bug, and answering with 10 000 is more useful than a
    /// 400 it will not handle.
    pub fn new(
        query: &str,
        regex: bool,
        case: bool,
        word: bool,
        globs: Vec<String>,
        path: Option<String>,
        max_results: Option<usize>,
    ) -> Result<Self, SearchError> {
        let query = query.trim_end_matches(['\r', '\n']).to_string();
        // §3.1 validates on the *trimmed* value but searches the untrimmed one:
        // `"   "` is a mistake, while `" fn "` is a legitimate query for a padded
        // token, and trimming it would silently search for something else.
        if query.trim().is_empty() {
            return Err(SearchError::EmptyQuery);
        }
        let path = path
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty() && p != ".");
        Ok(Self {
            query,
            regex,
            case,
            word,
            globs: globs.into_iter().filter(|g| !g.trim().is_empty()).collect(),
            path,
            max_results: max_results
                .unwrap_or(DEFAULT_MAX_RESULTS)
                .clamp(1, MAX_MAX_RESULTS),
        })
    }
}

/// Cut `text` to at most [`MAX_LINE_BYTES`], never mid-codepoint, and drop or
/// clamp any highlight range that no longer fits.
///
/// Split out because it is pure and the off-by-one is the kind that ships: a
/// range clamped to `end > len` panics the moment the frontend slices with it.
pub(crate) fn truncate_line(mut text: String, ranges: &mut Vec<[usize; 2]>) -> String {
    if text.len() <= MAX_LINE_BYTES {
        return text;
    }
    let mut cut = MAX_LINE_BYTES;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    ranges.retain(|r| r[0] < cut);
    for r in ranges.iter_mut() {
        r[1] = r[1].min(cut);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_an_empty_query() {
        assert!(matches!(
            SearchQuery::new("", false, false, false, vec![], None, None),
            Err(SearchError::EmptyQuery)
        ));
        assert!(matches!(
            SearchQuery::new("\n", false, false, false, vec![], None, None),
            Err(SearchError::EmptyQuery)
        ));
        // §3.1 validates after trimming, so whitespace alone is refused — a user
        // who cleared the box and left a stray space gets a 400, not a walk over
        // the whole project that matches every indented line.
        assert!(matches!(
            SearchQuery::new("   ", false, false, false, vec![], None, None),
            Err(SearchError::EmptyQuery)
        ));
        // But the query itself is *not* trimmed: " fn " searches for a padded
        // token, and trimming it would search for something the user did not ask for.
        let padded = SearchQuery::new(" fn ", false, false, false, vec![], None, None).unwrap();
        assert_eq!(padded.query, " fn ");
    }

    #[test]
    fn clamps_max_results_into_range() {
        let q = SearchQuery::new("x", false, false, false, vec![], None, Some(999_999)).unwrap();
        assert_eq!(q.max_results, MAX_MAX_RESULTS);
        let q = SearchQuery::new("x", false, false, false, vec![], None, Some(0)).unwrap();
        assert_eq!(q.max_results, 1);
        let q = SearchQuery::new("x", false, false, false, vec![], None, None).unwrap();
        assert_eq!(q.max_results, DEFAULT_MAX_RESULTS);
    }

    #[test]
    fn drops_empty_globs_and_a_dot_path() {
        let q = SearchQuery::new(
            "x",
            false,
            false,
            false,
            vec!["*.rs".into(), "  ".into()],
            Some(".".into()),
            None,
        )
        .unwrap();
        assert_eq!(q.globs, vec!["*.rs".to_string()]);
        // `?path=.` means the project root, which is what an absent `path` means —
        // passing it through would send the walker to a "./" prefix for no reason.
        assert!(q.path.is_none());
    }

    #[test]
    fn truncate_line_keeps_short_lines_untouched() {
        let mut ranges = vec![[0usize, 3]];
        let out = truncate_line("hello".into(), &mut ranges);
        assert_eq!(out, "hello");
        assert_eq!(ranges, vec![[0, 3]]);
    }

    #[test]
    fn truncate_line_clamps_and_drops_ranges() {
        let long = "a".repeat(MAX_LINE_BYTES + 100);
        // One range straddling the cut, one entirely past it.
        let mut ranges = vec![
            [0, 2],
            [MAX_LINE_BYTES - 1, MAX_LINE_BYTES + 5],
            [MAX_LINE_BYTES + 10, MAX_LINE_BYTES + 20],
        ];
        let out = truncate_line(long, &mut ranges);
        assert_eq!(out.len(), MAX_LINE_BYTES);
        assert_eq!(ranges, vec![[0, 2], [MAX_LINE_BYTES - 1, MAX_LINE_BYTES]]);
        // Every surviving range must be sliceable — this is the assertion that
        // catches the off-by-one.
        for r in &ranges {
            assert!(out.get(r[0]..r[1]).is_some(), "range {r:?} not sliceable");
        }
    }

    #[test]
    fn truncate_line_never_splits_a_codepoint() {
        // "é" is two bytes; place one exactly across the cut so a naive
        // `truncate(MAX_LINE_BYTES)` would panic.
        let mut text = "a".repeat(MAX_LINE_BYTES - 1);
        text.push('é');
        text.push_str("tail");
        let mut ranges = Vec::new();
        let out = truncate_line(text, &mut ranges);
        assert_eq!(out.len(), MAX_LINE_BYTES - 1);
        assert!(out.ends_with('a'));
    }
}
