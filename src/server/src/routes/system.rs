//! System routes — host metrics + process control.
//!
//! Contract: docs/analysis/06-api-contract.md §System.
//!   GET  /api/system/metrics    — [DOCS] CPU/mem/process, poll 3s
//!   POST /api/system/kill/{pid} — [PROPOSED]
//!
//! Roadmap Pha 6 (07-build-roadmap.md). Backed by `sysinfo` 0.39 with an
//! optional `nvml-wrapper` GPU feature (04 §7). Keep ONE long-lived `System`
//! instance and refresh on a timer; CPU% needs two refreshes >= ~200ms apart.

// TODO(phase-6): router() with GET /metrics and POST /kill/{pid}.
