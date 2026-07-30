//! Git integration — hybrid libgit2 (read) + git CLI (mutation).
//!
//! Responsibility (docs/analysis/02-architecture.md, 04 §4): use `git2` 0.21 for
//! fast reads (status/log/blame/diff) and shell out to the `git` CLI for
//! mutations (commit/merge/push) so user credential helpers and hooks apply.
//!
//! Roadmap: Pha 5 (07-build-roadmap.md).
//!
//! Gotchas:
//! - libgit2 blame/diff on large repos is slow and blocking → run under
//!   `spawn_blocking` (02 §traps).
//! - Emit changes via SSE `/api/projects/{id}/git/watch`; after 3 failures fall
//!   back to polling (06 §Git).
//! - Worktrees: CLI `git worktree add/list/remove` is more robust for heavy
//!   orchestration; one worktree per agent shares the object store.

// TODO(phase-5): read API over git2 (status/log/blame/diff) via spawn_blocking.
// TODO(phase-5): mutation API shelling out to git CLI (commit/merge/push).
// TODO(phase-5): SSE watcher with poll fallback.
