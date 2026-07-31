//! The search engine: `ignore::WalkParallel` + `grep-searcher`, funnelled into a
//! bounded channel.
//!
//! Spec: `docs/specs/SPEC-006-search-monitor.md` §5.2.
//!
//! Everything in this file is **blocking** — `WalkParallel::run` owns a thread
//! pool and returns only when the walk is finished. The caller wraps it in
//! `spawn_blocking`; nothing here touches the Tokio runtime except
//! `Sender::blocking_send`, which is the sanctioned way across that boundary.
//!
//! Four decisions carry the design (§5.2):
//!
//! 1. **`blocking_send`, not `try_send`.** `try_send` drops matches when the
//!    buffer is full and reports success — a silently incomplete search. Blocking
//!    applies real backpressure: a slow client slows the walk.
//! 2. **The channel is the cancellation token.** The client disconnecting drops
//!    the receiver; `tx.is_closed()` then flips, the walk closure returns
//!    `WalkState::Quit` and the sink returns `Ok(false)` (`grep-searcher`
//!    `sink.rs:118-124`: `Ok(false)` stops *this file* and still calls `finish`,
//!    while `Err` would bubble a fake error to the caller).
//! 3. **The cap is a shared `AtomicUsize`.** `WalkState::Quit` is documented as
//!    asynchronous (`ignore` `walk.rs:1318-1330`), so a handful of matches can
//!    still land after the decision to stop. `truncated` is therefore an honest
//!    boolean, never a promise of exactly N results.
//! 4. **Errors are per file.** A permission-denied directory emits one `error`
//!    event and the walk continues; a search that dies because one file was
//!    unreadable would be useless on any real machine.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use grep_regex::RegexMatcherBuilder;
// A hand-written `Sink` rather than `sinks::UTF8`: that helper hands back a
// `&str` with no match offsets, and the highlight ranges are the whole point of
// the DTO.
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::{WalkBuilder, WalkState, overrides::OverrideBuilder};
use tokio::sync::mpsc::Sender;

use super::{
    CHANNEL_CAPACITY, DoneEvent, FileErrorEvent, MAX_FILESIZE, MAX_LINE_BYTES, MatchEvent,
    PROGRESS_INTERVAL_MS, ProgressEvent, SearchError, SearchEvent, SearchQuery, truncate_line,
};

/// Run a search to completion, sending events on `tx`.
///
/// Blocking. Returns nothing: everything the caller needs, including the terminal
/// [`SearchEvent::Done`], goes out on the channel.
pub fn run_blocking(root: &Path, query: SearchQuery, tx: Sender<SearchEvent>) {
    let started = Instant::now();

    let matcher = match RegexMatcherBuilder::new()
        // `fixed_strings` is what makes a literal search literal. Hand-escaping
        // the pattern instead would get `\Q`-style edge cases wrong and is the
        // classic bug here (§9 #5).
        .fixed_strings(!query.regex)
        // `case: false` (the default) means case-insensitive — an editor search
        // box, not `grep`.
        .case_insensitive(!query.case)
        .word(query.word)
        .line_terminator(Some(b'\n'))
        .build(&query.query)
    {
        Ok(m) => m,
        // Validated before the stream opened, so this is unreachable in practice;
        // if it does happen, say so on the stream rather than hanging the client.
        Err(e) => {
            let _ = tx.blocking_send(SearchEvent::FileError(FileErrorEvent {
                path: String::new(),
                detail: format!("invalid pattern: {e}"),
            }));
            return;
        }
    };

    let walk_root = match &query.path {
        Some(rel) => root.join(rel),
        None => root.to_path_buf(),
    };

    let mut builder = WalkBuilder::new(&walk_root);
    builder
        // Same ignore configuration as the file tree (SPEC-002 `files/tree.rs`),
        // so what the tree shows is what search sees. `hidden(false)` means
        // dotfiles are *not* hidden: `.env` and `.github` are searchable.
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true)
        .parents(true)
        // Without this, a project that is not a git repo loses every gitignore
        // rule — the trap SPEC-002 already hit.
        .require_git(false)
        .follow_links(false)
        // A 16 MB source file is a generated artifact or a checked-in blob; either
        // way, searching it costs more than the result is worth ([INVENTED-7]).
        .max_filesize(Some(MAX_FILESIZE))
        .filter_entry(|e| !matches!(e.file_name().to_str(), Some(".git" | "node_modules")));

    if !query.globs.is_empty() {
        // `ignore::Override` polarity is inverted relative to gitignore
        // (`overrides.rs:85-93`): a plain glob *whitelists*, `!glob` ignores. The
        // client's strings already use that convention, so they pass through.
        let mut overrides = OverrideBuilder::new(&walk_root);
        for glob in &query.globs {
            if overrides.add(glob).is_err() {
                let _ = tx.blocking_send(SearchEvent::FileError(FileErrorEvent {
                    path: String::new(),
                    detail: format!("invalid glob: {glob}"),
                }));
                return;
            }
        }
        match overrides.build() {
            Ok(o) => {
                builder.overrides(o);
            }
            Err(e) => {
                let _ = tx.blocking_send(SearchEvent::FileError(FileErrorEvent {
                    path: String::new(),
                    detail: format!("invalid glob: {e}"),
                }));
                return;
            }
        }
    }

    let matches = Arc::new(AtomicUsize::new(0));
    let files_scanned = Arc::new(AtomicUsize::new(0));
    let files_with_match = Arc::new(AtomicUsize::new(0));
    let truncated = Arc::new(AtomicBool::new(false));
    let max_results = query.max_results;
    let root = root.to_path_buf();

    // Progress is emitted from whichever worker notices the interval has elapsed;
    // the counters are atomics, so the numbers are consistent regardless of which.
    let last_progress = Arc::new(std::sync::Mutex::new(Instant::now()));

    builder.build_parallel().run(|| {
        let tx = tx.clone();
        let matcher = matcher.clone();
        let matches = Arc::clone(&matches);
        let files_scanned = Arc::clone(&files_scanned);
        let files_with_match = Arc::clone(&files_with_match);
        let truncated = Arc::clone(&truncated);
        let last_progress = Arc::clone(&last_progress);
        let root = root.clone();
        let mut searcher = SearcherBuilder::new()
            // Without this, `SinkMatch::line_number()` is always `None` and every
            // result points at line 0 (§9 #4).
            .line_number(true)
            // ripgrep's own rule: a NUL byte means binary, stop reading. Emitting
            // a binary file's "lines" would flood the UI with control characters.
            .binary_detection(BinaryDetection::quit(b'\x00'))
            .heap_limit(Some(MAX_LINE_BYTES * 64))
            .build();

        Box::new(move |entry| {
            // Cancellation, checked before any work: the receiver is gone, so
            // whatever we find has nowhere to go.
            if tx.is_closed() {
                return WalkState::Quit;
            }
            if matches.load(Ordering::Relaxed) >= max_results {
                truncated.store(true, Ordering::Relaxed);
                return WalkState::Quit;
            }

            let entry = match entry {
                Ok(e) => e,
                Err(e) => {
                    let _ = tx.blocking_send(SearchEvent::FileError(FileErrorEvent {
                        path: String::new(),
                        detail: e.to_string(),
                    }));
                    return WalkState::Continue;
                }
            };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                return WalkState::Continue;
            }

            files_scanned.fetch_add(1, Ordering::Relaxed);
            let rel = relative_display(&root, entry.path());

            let mut sink = MatchSink {
                tx: &tx,
                path: &rel,
                matcher: &matcher,
                matches: &matches,
                max_results,
                truncated: &truncated,
                found_here: false,
            };

            match searcher.search_path(&matcher, entry.path(), &mut sink) {
                Ok(()) => {
                    if sink.found_here {
                        files_with_match.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(e) => {
                    // One unreadable file is information, not a failure: report it
                    // and keep walking.
                    let _ = tx.blocking_send(SearchEvent::FileError(FileErrorEvent {
                        path: rel.clone(),
                        detail: e.to_string(),
                    }));
                }
            }

            // Progress keeps the counters honest on a long scan with few hits,
            // where the UI would otherwise look frozen.
            if let Ok(mut last) = last_progress.try_lock()
                && last.elapsed() >= Duration::from_millis(PROGRESS_INTERVAL_MS)
            {
                *last = Instant::now();
                let _ = tx.blocking_send(SearchEvent::Progress(ProgressEvent {
                    files_scanned: files_scanned.load(Ordering::Relaxed),
                    matches: matches.load(Ordering::Relaxed),
                }));
            }

            WalkState::Continue
        })
    });

    let _ = tx.blocking_send(SearchEvent::Done(DoneEvent {
        matches: matches.load(Ordering::Relaxed),
        files: files_with_match.load(Ordering::Relaxed),
        files_scanned: files_scanned.load(Ordering::Relaxed),
        truncated: truncated.load(Ordering::Relaxed),
        elapsed_ms: started.elapsed().as_millis() as u64,
    }));
}

/// The `Sink` that turns `grep-searcher` callbacks into channel sends.
struct MatchSink<'a> {
    tx: &'a Sender<SearchEvent>,
    path: &'a str,
    matcher: &'a grep_regex::RegexMatcher,
    matches: &'a AtomicUsize,
    max_results: usize,
    truncated: &'a AtomicBool,
    /// Whether this file produced at least one match — feeds the `files` count.
    found_here: bool,
}

impl Sink for MatchSink<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> std::io::Result<bool> {
        // `Ok(false)` — not `Err` — is how a `Sink` cancels (`sink.rs:118-124`).
        // Returning an error here would surface a fabricated I/O failure to the
        // caller for what is a perfectly normal client disconnect.
        if self.tx.is_closed() {
            return Ok(false);
        }
        if self.matches.load(Ordering::Relaxed) >= self.max_results {
            self.truncated.store(true, Ordering::Relaxed);
            return Ok(false);
        }

        let bytes = mat.bytes();
        let trimmed = strip_line_terminator(bytes);

        // Offsets are into the raw line, so collect them before the lossy
        // conversion — after it they would point at the wrong bytes. On a
        // non-UTF-8 line the ranges stay empty: lossy replacement shifts every
        // offset, and a wrong highlight is worse than none.
        let mut ranges: Vec<[usize; 2]> = Vec::new();
        if std::str::from_utf8(trimmed).is_ok() {
            use grep_matcher::Matcher as _;
            let _ = self.matcher.find_iter(trimmed, |m| {
                ranges.push([m.start(), m.end()]);
                true
            });
        }

        let text = truncate_line(String::from_utf8_lossy(trimmed).into_owned(), &mut ranges);

        let line = mat.line_number().unwrap_or(0);
        self.found_here = true;
        self.matches.fetch_add(1, Ordering::Relaxed);

        // `blocking_send` fails only when the receiver is gone, which is the same
        // cancellation the check above handles — stop this file either way.
        if self
            .tx
            .blocking_send(SearchEvent::Match(MatchEvent {
                path: self.path.to_string(),
                line,
                text,
                ranges,
            }))
            .is_err()
        {
            return Ok(false);
        }
        Ok(true)
    }
}

/// Drop a trailing `\n` and the `\r` before it. `SinkMatch::bytes` includes the
/// terminator, and shipping it would put a stray newline inside every result row.
fn strip_line_terminator(bytes: &[u8]) -> &[u8] {
    let mut end = bytes.len();
    if end > 0 && bytes[end - 1] == b'\n' {
        end -= 1;
        if end > 0 && bytes[end - 1] == b'\r' {
            end -= 1;
        }
    }
    &bytes[..end]
}

/// Project-relative, `/`-separated path — the shape `GET /api/projects/{id}/file`
/// takes, so a click-to-open needs no conversion (§5.3).
fn relative_display(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Convenience for the routes layer: the channel pair this engine expects.
pub fn channel() -> (
    Sender<SearchEvent>,
    tokio::sync::mpsc::Receiver<SearchEvent>,
) {
    tokio::sync::mpsc::channel(CHANNEL_CAPACITY)
}

/// Compile-check a pattern and its globs before the SSE stream opens.
///
/// Exists so a typo'd regex is a `400` with a readable message instead of an
/// `error` event on a stream the client already committed to (§3.1).
pub fn validate(query: &SearchQuery) -> Result<(), SearchError> {
    RegexMatcherBuilder::new()
        .fixed_strings(!query.regex)
        .case_insensitive(!query.case)
        .word(query.word)
        .line_terminator(Some(b'\n'))
        .build(&query.query)
        .map_err(|e| SearchError::BadPattern(e.to_string()))?;

    if !query.globs.is_empty() {
        let mut overrides = OverrideBuilder::new(PathBuf::from("."));
        for glob in &query.globs {
            overrides
                .add(glob)
                .map_err(|e| SearchError::BadGlob(format!("{glob}: {e}")))?;
        }
        overrides
            .build()
            .map_err(|e| SearchError::BadGlob(e.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(root: &Path, query: SearchQuery) -> Vec<SearchEvent> {
        // A real runtime, because `blocking_send` needs a live receiver and the
        // engine is blocking by construction.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (tx, mut rx) = channel();
            let root = root.to_path_buf();
            let handle = tokio::task::spawn_blocking(move || run_blocking(&root, query, tx));
            let mut out = Vec::new();
            while let Some(event) = rx.recv().await {
                out.push(event);
            }
            handle.await.unwrap();
            out
        })
    }

    fn matches_of(events: &[SearchEvent]) -> Vec<&MatchEvent> {
        events
            .iter()
            .filter_map(|e| match e {
                SearchEvent::Match(m) => Some(m),
                _ => None,
            })
            .collect()
    }

    fn done_of(events: &[SearchEvent]) -> &DoneEvent {
        events
            .iter()
            .find_map(|e| match e {
                SearchEvent::Done(d) => Some(d),
                _ => None,
            })
            .expect("every search ends with exactly one done event")
    }

    struct Fixture {
        dir: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A small tree covering the cases the walker configuration is *for*:
    /// a gitignored file, a `.git` directory, a dotfile, a binary, and two
    /// extensions.
    fn fixture(name: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("spec-ade-search-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules")).unwrap();
        std::fs::write(dir.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(dir.join("src/a.rs"), "fn needle() {}\nlet x = 1;\n").unwrap();
        std::fs::write(dir.join("src/b.ts"), "const needle = 2;\n").unwrap();
        std::fs::write(dir.join("ignored.txt"), "needle here\n").unwrap();
        std::fs::write(dir.join(".git/config"), "needle in git\n").unwrap();
        std::fs::write(dir.join("node_modules/dep.js"), "needle in dep\n").unwrap();
        std::fs::write(dir.join(".env"), "SECRET=needle\n").unwrap();
        std::fs::write(dir.join("bin.dat"), b"needle\x00\x01binary\n").unwrap();
        Fixture { dir }
    }

    fn query(q: &str) -> SearchQuery {
        SearchQuery::new(q, false, false, false, vec![], None, None).unwrap()
    }

    #[test]
    fn finds_literal_matches_and_reports_paths_relative() {
        let f = fixture("literal");
        let events = collect(&f.dir, query("needle"));
        let hits = matches_of(&events);

        let paths: Vec<&str> = hits.iter().map(|m| m.path.as_str()).collect();
        assert!(paths.contains(&"src/a.rs"), "got {paths:?}");
        assert!(paths.contains(&"src/b.ts"), "got {paths:?}");
        // Dotfiles are searchable: `hidden(false)` is deliberate, and a secret
        // leaking into `.env` is exactly what a user greps for.
        assert!(paths.contains(&".env"), "got {paths:?}");
        // Relative and `/`-separated, so the click-to-open path needs no fixing.
        assert!(!paths.iter().any(|p| p.starts_with('/')));
    }

    #[test]
    fn respects_gitignore_and_never_walks_git_or_node_modules() {
        let f = fixture("ignored");
        let events = collect(&f.dir, query("needle"));
        let paths: Vec<&str> = matches_of(&events)
            .iter()
            .map(|m| m.path.as_str())
            .collect();

        assert!(
            !paths.contains(&"ignored.txt"),
            "gitignore ignored: {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p.starts_with(".git/")),
            ".git walked: {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p.starts_with("node_modules/")),
            "node_modules walked: {paths:?}"
        );
    }

    #[test]
    fn skips_binary_files() {
        let f = fixture("binary");
        let events = collect(&f.dir, query("needle"));
        let paths: Vec<&str> = matches_of(&events)
            .iter()
            .map(|m| m.path.as_str())
            .collect();
        // The needle really is in `bin.dat`, before the NUL — `BinaryDetection`
        // is the only reason it does not show up.
        assert!(!paths.contains(&"bin.dat"), "binary emitted: {paths:?}");
    }

    #[test]
    fn ranges_point_at_the_match_inside_the_line() {
        let f = fixture("ranges");
        let events = collect(&f.dir, query("needle"));
        let hit = matches_of(&events)
            .into_iter()
            .find(|m| m.path == "src/b.ts")
            .expect("b.ts matches");

        assert_eq!(hit.line, 1);
        assert_eq!(hit.text, "const needle = 2;");
        assert_eq!(hit.ranges.len(), 1);
        let [start, end] = hit.ranges[0];
        // Slicing with the range must yield the query — the assertion that would
        // fail if offsets were computed against the terminator-bearing bytes.
        assert_eq!(&hit.text[start..end], "needle");
    }

    #[test]
    fn is_case_insensitive_by_default_and_sensitive_on_request() {
        let dir = std::env::temp_dir().join("spec-ade-search-case");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "Needle\nneedle\n").unwrap();
        let f = Fixture { dir };

        let insensitive = collect(&f.dir, query("needle"));
        assert_eq!(matches_of(&insensitive).len(), 2);

        let sensitive = SearchQuery::new("needle", false, true, false, vec![], None, None).unwrap();
        assert_eq!(matches_of(&collect(&f.dir, sensitive)).len(), 1);
    }

    #[test]
    fn a_literal_query_is_not_a_regex() {
        // The reason `fixed_strings` exists: `a.c` must not match `abc`.
        let dir = std::env::temp_dir().join("spec-ade-search-literal-dot");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "abc\na.c\n").unwrap();
        let f = Fixture { dir };

        let literal = collect(&f.dir, query("a.c"));
        assert_eq!(matches_of(&literal).len(), 1);
        assert_eq!(matches_of(&literal)[0].text, "a.c");

        let re = SearchQuery::new("a.c", true, false, false, vec![], None, None).unwrap();
        assert_eq!(matches_of(&collect(&f.dir, re)).len(), 2);
    }

    #[test]
    fn word_mode_refuses_a_substring() {
        let dir = std::env::temp_dir().join("spec-ade-search-word");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "needle\nneedles\n").unwrap();
        let f = Fixture { dir };

        let word = SearchQuery::new("needle", false, false, true, vec![], None, None).unwrap();
        let hits = collect(&f.dir, word);
        let hits = matches_of(&hits);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].text, "needle");
    }

    #[test]
    fn an_include_glob_narrows_and_a_bang_glob_excludes() {
        let f = fixture("globs");

        let only_rs = SearchQuery::new(
            "needle",
            false,
            false,
            false,
            vec!["*.rs".into()],
            None,
            None,
        )
        .unwrap();
        let paths: Vec<String> = matches_of(&collect(&f.dir, only_rs))
            .iter()
            .map(|m| m.path.clone())
            .collect();
        // The `Override` inversion trap: one whitelist glob must hide `.ts` and
        // `.env` but must NOT hide the `.rs` file itself.
        assert_eq!(paths, vec!["src/a.rs".to_string()]);

        let not_ts = SearchQuery::new(
            "needle",
            false,
            false,
            false,
            vec!["!*.ts".into()],
            None,
            None,
        )
        .unwrap();
        let paths: Vec<String> = matches_of(&collect(&f.dir, not_ts))
            .iter()
            .map(|m| m.path.clone())
            .collect();
        assert!(paths.contains(&"src/a.rs".to_string()), "got {paths:?}");
        assert!(!paths.contains(&"src/b.ts".to_string()), "got {paths:?}");
    }

    #[test]
    fn a_path_scope_limits_the_walk() {
        let f = fixture("scope");
        let scoped = SearchQuery::new(
            "needle",
            false,
            false,
            false,
            vec![],
            Some("src".into()),
            None,
        )
        .unwrap();
        let paths: Vec<String> = matches_of(&collect(&f.dir, scoped))
            .iter()
            .map(|m| m.path.clone())
            .collect();

        assert!(paths.iter().all(|p| p.starts_with("src/")), "got {paths:?}");
        // Still relative to the *project* root, not the scope — otherwise every
        // click-to-open in a scoped search would open the wrong file.
        assert!(paths.contains(&"src/a.rs".to_string()));
    }

    #[test]
    fn the_cap_stops_the_walk_and_reports_truncated() {
        let dir = std::env::temp_dir().join("spec-ade-search-cap");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let body = "needle\n".repeat(500);
        for i in 0..5 {
            std::fs::write(dir.join(format!("f{i}.txt")), &body).unwrap();
        }
        let f = Fixture { dir };

        let capped =
            SearchQuery::new("needle", false, false, false, vec![], None, Some(10)).unwrap();
        let events = collect(&f.dir, capped);
        let done = done_of(&events);

        assert!(done.truncated, "cap must be reported honestly");
        // `WalkState::Quit` is asynchronous, so the exact count is not promised —
        // only that it stopped far short of the 2500 available.
        assert!(
            done.matches < 500,
            "cap did not stop the walk: {} matches",
            done.matches
        );
    }

    #[test]
    fn a_long_line_is_truncated_but_still_reported() {
        let dir = std::env::temp_dir().join("spec-ade-search-longline");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut line = "x".repeat(MAX_LINE_BYTES * 2);
        line.push_str("needle\n");
        std::fs::write(dir.join("min.js"), line).unwrap();
        let f = Fixture { dir };

        let events = collect(&f.dir, query("needle"));
        let hits = matches_of(&events);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].text.len() <= MAX_LINE_BYTES);
        // The match itself is past the cut, so its range is dropped rather than
        // pointing outside the string.
        for r in &hits[0].ranges {
            assert!(hits[0].text.get(r[0]..r[1]).is_some());
        }
    }

    #[test]
    fn done_counts_files_not_just_matches() {
        let f = fixture("counts");
        let events = collect(&f.dir, query("needle"));
        let done = done_of(&events);
        let hits = matches_of(&events).len();

        assert_eq!(done.matches, hits, "done must agree with what was streamed");
        assert!(done.files >= 3, "a.rs, b.ts, .env at least: {}", done.files);
        assert!(done.files_scanned >= done.files);
        assert!(!done.truncated);
    }

    #[test]
    fn a_search_with_no_hits_still_ends_with_done() {
        let f = fixture("nohits");
        let events = collect(&f.dir, query("zzz-not-present-zzz"));
        assert!(matches_of(&events).is_empty());
        let done = done_of(&events);
        assert_eq!(done.matches, 0);
        assert!(!done.truncated);
    }

    #[test]
    fn validate_rejects_a_bad_regex_and_a_bad_glob() {
        let bad = SearchQuery::new("a(", true, false, false, vec![], None, None).unwrap();
        assert!(matches!(validate(&bad), Err(SearchError::BadPattern(_))));

        // The same string as a literal is perfectly valid — which is why the
        // check has to run against the actual matcher configuration.
        let ok = SearchQuery::new("a(", false, false, false, vec![], None, None).unwrap();
        assert!(validate(&ok).is_ok());

        let bad_glob =
            SearchQuery::new("x", false, false, false, vec!["[".into()], None, None).unwrap();
        assert!(matches!(validate(&bad_glob), Err(SearchError::BadGlob(_))));
    }

    #[test]
    fn strip_line_terminator_handles_crlf_and_bare_lines() {
        assert_eq!(strip_line_terminator(b"abc\n"), b"abc");
        assert_eq!(strip_line_terminator(b"abc\r\n"), b"abc");
        assert_eq!(strip_line_terminator(b"abc"), b"abc");
        assert_eq!(strip_line_terminator(b"\n"), b"");
        assert_eq!(strip_line_terminator(b""), b"");
    }

    #[test]
    fn dropping_the_receiver_stops_the_walk() {
        // The cancellation contract (§5.4): the channel *is* the token. Without a
        // live receiver the walk must return instead of blocking forever on a
        // full buffer.
        let dir = std::env::temp_dir().join("spec-ade-search-cancel");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let body = "needle\n".repeat(2000);
        for i in 0..20 {
            std::fs::write(dir.join(format!("f{i}.txt")), &body).unwrap();
        }
        let f = Fixture { dir };

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (tx, rx) = channel();
            let root = f.dir.clone();
            let q = SearchQuery::new("needle", false, false, false, vec![], None, Some(100_000))
                .unwrap();
            let handle = tokio::task::spawn_blocking(move || run_blocking(&root, q, tx));
            drop(rx);
            // Generous, but finite: without cancellation this never returns.
            tokio::time::timeout(Duration::from_secs(20), handle)
                .await
                .expect("walk must stop once the receiver is gone")
                .unwrap();
        });
    }
}
