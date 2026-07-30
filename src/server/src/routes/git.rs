//! Git routes — status/diff/log and mutations, plus an SSE watch stream.
//!
//! Contract: docs/analysis/06-api-contract.md §Git.
//!   SSE  /api/projects/{id}/git/watch   — after 3 failures, fall back to polling
//!   GET  /api/projects/{id}/git/status | diff?path= | log
//!   POST /api/projects/{id}/git/stage | commit | branch | merge
//!
//! Roadmap Pha 5 (07-build-roadmap.md). Logic lives in `crate::git`.
//! Split (02-architecture.md / 04 §4): git2 for reads (spawn_blocking), git CLI for
//! mutations so the user's credential helper + hooks are inherited.

// TODO(phase-5): router() wiring reads (git2) + mutations (CLI) + SSE watch.
