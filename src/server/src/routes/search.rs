//! Search routes — streaming ripgrep-style content search per project.
//!
//! Contract: docs/analysis/06-api-contract.md §Search.
//!   WS/SSE /api/projects/{id}/search  — params: query, regex, case, glob
//!
//! Roadmap Pha 6 (07-build-roadmap.md). Backed by the `ignore` crate walker
//! (04 §6). Stream each match; never collect. WalkParallel callbacks run on
//! worker threads, so funnel matches through a channel back to async
//! (02-architecture.md blocking-in-async note).

// TODO(phase-6): router() exposing a streaming search endpoint (WS or SSE).
