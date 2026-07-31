//! Search routes — streaming content search per project.
//!
//! Spec: `docs/specs/SPEC-006-search-monitor.md` §3.1.
//!
//! ```text
//! SSE /api/projects/{id}/search  ?query=&regex=&case=&word=&glob=&path=&maxResults=
//! ```
//!
//! SSE rather than WebSocket ([SPEC-006 INVENTED-1]): the traffic is entirely
//! one-directional, and closing an `EventSource` is a cancellation the server can
//! actually observe. A new query is a new stream; the old one is closed by the
//! client and the walk quits on its own (§5.4).
//!
//! Everything that can be wrong with a *request* — empty query, bad regex, bad
//! glob, escaping path, unknown project — is answered as a normal HTTP error
//! **before** the stream opens. Once the stream is open, a failure is one `error`
//! event about one file and the walk continues.

use std::convert::Infallible;
use std::time::Duration;

use axum::{
    Router,
    extract::{Path, Query, RawQuery, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    routing::get,
};
use serde::Deserialize;
use tokio_stream::{Stream, StreamExt, wrappers::ReceiverStream};

use crate::AppState;
use crate::routes::error::ApiError;
use crate::routes::projects::project_root;
use crate::search::{SearchError, SearchQuery, engine};

pub fn router() -> Router<AppState> {
    Router::new().route("/projects/{id}/search", get(search))
}

impl From<SearchError> for ApiError {
    fn from(e: SearchError) -> Self {
        match e {
            // The client fixes its own input in all three cases, so 400 with a
            // message the search box can show inline.
            SearchError::EmptyQuery | SearchError::BadPattern(_) | SearchError::BadGlob(_) => {
                ApiError::new(StatusCode::BAD_REQUEST, "search", e.to_string())
            }
            // Same mapping the file API uses for the same input (SPEC-002 §3.6):
            // `Escapes` is a refusal (403), the rest are malformed input (400).
            SearchError::Path(crate::files::PathError::Escapes) => {
                ApiError::new(StatusCode::FORBIDDEN, "path", e.to_string())
            }
            SearchError::Path(_) => ApiError::new(StatusCode::BAD_REQUEST, "path", e.to_string()),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchParams {
    query: String,
    #[serde(default)]
    regex: bool,
    /// Case-**sensitive** when true; default false = case-insensitive.
    #[serde(default)]
    case: bool,
    #[serde(default)]
    word: bool,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    max_results: Option<usize>,
}

/// `SSE …/search` (D1–D21).
async fn search(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(params): Query<SearchParams>,
    RawQuery(raw): RawQuery,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let root = project_root(&state, &id)?;

    // `glob` is repeatable (`?glob=*.rs&glob=!*.min.js`), which `serde` cannot
    // express through `Query<T>` — it keeps only the last value. Parsing the raw
    // string is the only way to see them all.
    let globs = repeated_param(raw.as_deref(), "glob");

    let query = SearchQuery::new(
        &params.query,
        params.regex,
        params.case,
        params.word,
        globs,
        params.path,
        params.max_results,
    )?;

    // A subdirectory scope goes through the same guard as every other client path,
    // so `?path=../../etc` is a 403 here exactly as it is in the file API (§5.3).
    if let Some(rel) = &query.path {
        crate::files::resolve(&root, rel).map_err(SearchError::Path)?;
    }

    // Compile before the stream opens: a typo'd regex must be a readable 400, not
    // an `error` frame on a stream the client already committed to.
    engine::validate(&query)?;

    let (tx, rx) = engine::channel();
    // `spawn_blocking`: `WalkParallel::run` owns a thread pool and returns only
    // when the walk finishes. On a runtime worker it would stall every other
    // request on the server.
    tokio::task::spawn_blocking(move || engine::run_blocking(&root, query, tx));

    let stream = ReceiverStream::new(rx).map(|event| {
        let name = event.name();
        Ok(Event::default()
            .event(name)
            .json_data(&event)
            .unwrap_or_else(|e| {
                // Serializing our own DTO cannot fail in practice; if it does,
                // say so rather than dropping the frame silently.
                Event::default()
                    .event("error")
                    .data(format!("serialize failed: {e}"))
            }))
    });

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

/// Collect every occurrence of `key` from a raw query string.
///
/// Split out and tested because the percent-decoding is easy to get wrong: a glob
/// like `!*.min.js` survives a URL round-trip only if `+` and `%xx` are both
/// handled, and getting it wrong silently changes which files are searched.
fn repeated_param(raw: Option<&str>, key: &str) -> Vec<String> {
    let Some(raw) = raw else {
        return Vec::new();
    };
    form_urlencoded::parse(raw.as_bytes())
        .filter(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_every_occurrence_of_a_repeated_param() {
        let raw = "query=x&glob=*.rs&glob=%21*.min.js&case=true";
        assert_eq!(
            repeated_param(Some(raw), "glob"),
            vec!["*.rs".to_string(), "!*.min.js".to_string()]
        );
    }

    #[test]
    fn returns_nothing_when_the_param_is_absent() {
        assert!(repeated_param(Some("query=x"), "glob").is_empty());
        assert!(repeated_param(None, "glob").is_empty());
    }

    #[test]
    fn decodes_a_percent_encoded_glob() {
        // `**/src/*.rs` is the shape a real include glob takes, and every one of
        // those characters is percent-encoded by the browser.
        let raw = "glob=%2A%2A%2Fsrc%2F%2A.rs";
        assert_eq!(repeated_param(Some(raw), "glob"), vec!["**/src/*.rs"]);
    }
}
