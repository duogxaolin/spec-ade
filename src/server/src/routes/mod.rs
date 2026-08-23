//! HTTP/WS/SSE route handlers for the Spec ADE REST + realtime surface.
//!
//! Endpoint contract: docs/analysis/06-api-contract.md. Three transports:
//! REST (CRUD), WebSocket (terminal I/O + chat streaming), SSE (git watch).
//! Every route sits behind the auth middleware assembled in `main::build_router`
//! (06 §Auth — token required even on localhost; PTY/ACP-over-WS is RCE-prone).
//!
//! Each submodule owns one resource group. They are declared here as stubs and
//! wired into the top-level router per their roadmap phase
//! (docs/analysis/07-build-roadmap.md).

// error — the `{error, detail}` JSON shape shared by every handler (SPEC-002 §3.6).
pub mod error;
// settings — GET/PUT /api/settings (partial update Option<Option<T>>).
// Implemented: SPEC-002.
pub mod settings;
// layout — GET/PUT /api/layout (opaque per-project pane trees + presets).
// Implemented: SPEC-008.
pub mod layout;
// projects — CRUD + GET /api/projects/{id}/tree (ignore-crate walk).
// Implemented: SPEC-002.
pub mod projects;
// sessions — POST/GET/DELETE session lifecycle under a project.
// Implemented: SPEC-003.
pub mod sessions;
// claws — CRUD + start/stop autonomous agents + GET /api/projects/{id}/skills.
// Implemented: SPEC-007.
pub mod claws;
// acp — POST /api/acp/spawn + WS /api/acp/{id}/ws (?after_seq=N replay).
// Implemented: SPEC-003.
pub mod acp;
// terminals — POST/GET/DELETE /api/terminals + WS /api/terminals/{id}/ws (PTY).
// Implemented: SPEC-001.
pub mod terminals;
// TODO(phase-5): git       — status/diff/log/stage/commit/branch/merge + SSE watch.
pub mod git;
// TODO(phase-6): search    — WS/SSE ripgrep stream under a project.
pub mod search;
// TODO(phase-6): system    — GET /api/system/metrics (poll 3s) + POST /api/system/kill/{pid}.
pub mod system;
