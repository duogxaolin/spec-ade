//! Claws routes — autonomous agents that run skills on a schedule.
//!
//! Contract: docs/analysis/06-api-contract.md §Claws.
//!   GET/POST         /api/claws            — list (+ runtime status) / create
//!   PUT/DELETE       /api/claws/{id}       — update / delete
//!   POST             /api/claws/{id}/start — start
//!   POST             /api/claws/{id}/stop  — stop
//!
//! Roadmap Pha 7 (07-build-roadmap.md): tokio-cron-scheduler 0.15 drives runs.
//! GOTCHA: its cron has a seconds field (6-7 fields), unlike crontab's 5 —
//! "0 9 * * *" copied from crontab is wrong. SKILL.md discovery scans 8 priority
//! dirs (workspace > user); parse frontmatter with gray_matter/serde_yaml, skip
//! malformed files with a warning instead of crashing discovery.
//! Permission mode maps to ACP session/request_permission
//! (auto_approve/deny_all/ask_via_ui/ask_via_telegram). keepAlive: restart <= 3x.

// TODO(phase-7): router() + handlers + scheduler wiring.
