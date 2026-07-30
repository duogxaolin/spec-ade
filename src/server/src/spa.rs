//! Embedded SPA serving — the built Vue frontend baked into the binary.
//!
//! Architecture (docs/analysis/02-architecture.md §single-binary, deep-dive 02
//! §3): one binary serves the SPA and the API on a single origin (no CORS,
//! simpler Tauri/PWA). The frontend is built to `src/web/dist` and embedded here
//! via `rust-embed`; any unmatched non-`/api` route falls back to `index.html`
//! so Vue Router's history mode works (06 §SPA fallback).
//!
//! BUILD ROBUSTNESS: `rust-embed`'s derive reads the `folder` at compile time.
//! If `src/web/dist` is missing (frontend not built yet), the macro errors and
//! breaks `cargo build`. To keep the backend independently buildable, a
//! committed placeholder (`src/web/dist/.gitkeep` + a fallback index) guarantees
//! the folder always exists. See `build.rs`, which creates a minimal
//! `index.html` if the real build output is absent. Producing the real SPA:
//! `cd src/web && npm install && npm run build`.

use axum::{
    body::Body,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

/// Compile-time embedding of the built frontend.
///
/// The path is relative to this crate's `Cargo.toml` (`src/server`), so it
/// points at the sibling `src/web/dist`. `build.rs` guarantees the folder and an
/// `index.html` exist even before the frontend is built.
#[derive(RustEmbed)]
#[folder = "../web/dist"]
struct Assets;

/// Serve an embedded asset by path, or fall back to `index.html`.
///
/// Routing contract:
/// - An exact asset hit (e.g. `/assets/index-abc.js`) is served with its
///   guessed MIME type.
/// - Anything else (client-side routes like `/projects/42`) returns
///   `index.html` so the SPA router can handle it.
/// - `/api/*` never reaches here — those routes are matched earlier.
pub async fn serve(uri: Uri) -> Response {
    // Strip the leading '/'. Root ('/') maps to index.html.
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match Assets::get(path) {
        Some(content) => asset_response(path, content.data.into_owned()),
        None => match Assets::get("index.html") {
            Some(index) => asset_response("index.html", index.data.into_owned()),
            // Only possible if even the placeholder index is missing — a broken
            // build. Report it explicitly rather than a blank 404.
            None => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "SPA assets missing: build the frontend (cd src/web && npm run build)",
            )
                .into_response(),
        },
    }
}

/// Build a `200 OK` response for an embedded asset with a guessed content type.
fn asset_response(path: &str, data: Vec<u8>) -> Response {
    let mime = mime_for(path);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .body(Body::from(data))
        .expect("static response is always valid")
}

/// Minimal extension → MIME map covering Vite's output. Avoids a `mime_guess`
/// dependency for Phase 0; extend as needed when the SPA grows.
fn mime_for(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "map" => "application/json",
        "wasm" => "application/wasm",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}
