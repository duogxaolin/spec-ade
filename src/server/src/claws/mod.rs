//! Claws — autonomous agents that run a Skill on a cron schedule (SPEC-007).
//!
//! This module is the **pure** half: the persisted definition, the request DTO,
//! the validation that turns one into the other, and the read-only runtime view.
//! Nothing here knows about Axum or ACP, so every rule below is provable by a
//! unit test (SPEC-007 §5.1, same shape as `search/mod.rs`).
//!
//! Layout:
//! - [`cron`]    — the `croner` wrapper (§3.3, the phase's central decision).
//! - [`skill`]   — 8-directory `SKILL.md` discovery (§5.4).
//! - [`runtime`] — the live half: one task per running Claw (§5.3).
//!
//! Field names and defaults come from the product docs
//! (`docs/spec-ade-clone/docs/core-concepts/claws.mdx:14-35`) and are kept
//! verbatim. The two deviations are marked in SPEC-007 §4: `skipIfRunning` is
//! added ([INVENTED-8]) and `ask_via_telegram` is refused ([INVENTED-6]).

pub mod cron;
pub mod runtime;
pub mod skill;

pub use runtime::ClawRuntime;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Bounds for `name`. Rejected, never truncated — a silently shortened name is a
/// different Claw than the one the user thought they saved.
pub const NAME_MAX: usize = 100;

/// How a Claw answers the agent's `session/request_permission` (SPEC-007 §5.2).
///
/// `ask_via_telegram` is deliberately **absent**: `05-gaps:81` marks it ❌ Bỏ, and
/// accepting it by quietly downgrading to `ask_via_ui` would let a user believe
/// remote approval was armed when nothing of the sort exists ([INVENTED-6]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Answer with the first allow-shaped option the agent offered.
    AutoApprove,
    /// Answer with the first reject-shaped option. The mode promises read-only.
    DenyAll,
    /// Park the request and wait for a human — the SPEC-003 behaviour.
    AskViaUi,
}

impl Default for PermissionMode {
    /// `auto_approve`, matching `claws.mdx:56`. §9.1: this is execution
    /// authority, not a UI preference — the UI must say so.
    fn default() -> Self {
        Self::AutoApprove
    }
}

impl PermissionMode {
    /// Parse the wire value, naming the accepted set on failure.
    ///
    /// Hand-rolled rather than leaning on serde so an unknown mode surfaces as a
    /// 400 with our error envelope instead of Axum's JSON-rejection shape — and
    /// so `ask_via_telegram` can be refused by name.
    pub fn parse(raw: &str) -> Result<Self, ClawError> {
        match raw {
            "auto_approve" => Ok(Self::AutoApprove),
            "deny_all" => Ok(Self::DenyAll),
            "ask_via_ui" => Ok(Self::AskViaUi),
            "ask_via_telegram" => Err(ClawError::Invalid(
                "permissionMode 'ask_via_telegram' is not supported: no Telegram \
                 transport exists, and downgrading it to 'ask_via_ui' would hide that"
                    .to_string(),
            )),
            other => Err(ClawError::Invalid(format!(
                "unknown permissionMode '{other}' (expected auto_approve, deny_all, or ask_via_ui)"
            ))),
        }
    }

    /// The wire value, for round-tripping in error messages and logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AutoApprove => "auto_approve",
            Self::DenyAll => "deny_all",
            Self::AskViaUi => "ask_via_ui",
        }
    }
}

/// One cron entry on a Claw (`claws.mdx:63-70`).
///
/// `cron` is stored as the **string the user typed**, not a compiled pattern:
/// `settings.json` has to round-trip it, and re-rendering from the pattern would
/// show a normalised form nobody wrote. It is re-parsed on demand — parsing is
/// microseconds and it keeps one source of truth.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClawSchedule {
    /// Human label shown in the UI. Optional — the cron itself is the identity.
    #[serde(default)]
    pub label: Option<String>,
    pub cron: String,
    /// Sent one after another when the schedule fires.
    #[serde(default)]
    pub prompts: Vec<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
}

impl ClawSchedule {
    /// The compiled pattern, or the parse error.
    pub fn schedule(&self) -> Result<cron::Schedule, cron::CronError> {
        cron::Schedule::parse(&self.cron)
    }
}

/// A persisted Claw (SPEC-007 §3.1). Lives in `settings.claws`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClawDefinition {
    /// Server-generated; clients never send it on `POST`.
    pub id: String,
    pub name: String,
    /// Must name an entry in `settings.acp_agents`.
    pub agent_id: String,
    /// Must name a registered project.
    pub project_id: String,
    /// `None` = a Claw that only runs its schedule prompts ([INVENTED-4]).
    #[serde(default)]
    pub skill: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Start at server boot ([INVENTED-5]).
    #[serde(default)]
    pub auto_start: bool,
    /// Restart a dead agent, at most 3 times (`claws.mdx:50`).
    #[serde(default = "yes")]
    pub keep_alive: bool,
    /// Spawn a fresh connection per trigger instead of reusing the session.
    #[serde(default)]
    pub restart_on_trigger: bool,
    #[serde(default)]
    pub permission_mode: PermissionMode,
    /// Skip a tick that lands while the session is still prompting ([INVENTED-8]).
    #[serde(default = "yes")]
    pub skip_if_running: bool,
    #[serde(default)]
    pub schedules: Vec<ClawSchedule>,
}

impl ClawDefinition {
    /// Earliest next fire across the enabled schedules, or `None` when the Claw
    /// is disabled, has no schedule, or every pattern is in a pinned past year.
    ///
    /// Computed per call from an explicit `now` ([INVENTED-11]) — a cached value
    /// could only ever be stale, and the argument keeps this testable.
    pub fn next_run_at(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        if !self.enabled {
            return None;
        }
        self.schedules
            .iter()
            .filter(|s| s.enabled)
            .filter_map(|s| s.schedule().ok())
            .filter_map(|s| s.next_after(now))
            .min()
    }

    /// Number of enabled schedules — what `scheduleCount` reports.
    pub fn enabled_schedule_count(&self) -> usize {
        self.schedules.iter().filter(|s| s.enabled).count()
    }

    /// One human rendering per schedule, in order, for the `POST`/`PUT` echo
    /// (§3.3, deliverable #4). A schedule that somehow fails to re-parse renders
    /// as the raw string rather than panicking — the value is diagnostic, and a
    /// stored definition is already known to have parsed once.
    pub fn schedule_descriptions(&self) -> Vec<String> {
        self.schedules
            .iter()
            .map(|s| match s.schedule() {
                Ok(parsed) => parsed.describe(),
                Err(_) => s.cron.clone(),
            })
            .collect()
    }
}

/// The `POST`/`PUT` body. `PUT` is a full replace (§3.2), so this is the whole
/// definition minus `id`.
///
/// `permissionMode` arrives as a `String` on purpose: see [`PermissionMode::parse`].
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClawInput {
    pub name: String,
    pub agent_id: String,
    pub project_id: String,
    #[serde(default)]
    pub skill: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub auto_start: bool,
    #[serde(default = "yes")]
    pub keep_alive: bool,
    #[serde(default)]
    pub restart_on_trigger: bool,
    #[serde(default = "default_permission_mode")]
    pub permission_mode: String,
    #[serde(default = "yes")]
    pub skip_if_running: bool,
    #[serde(default)]
    pub schedules: Vec<ClawSchedule>,
}

fn yes() -> bool {
    true
}

fn default_permission_mode() -> String {
    PermissionMode::default().as_str().to_string()
}

impl ClawInput {
    /// Validate everything checkable without touching settings, and stamp `id`.
    ///
    /// Agent and project existence are **not** checked here — they need the
    /// settings document, and mirroring `routes/acp.rs::spawn`'s order (agent,
    /// then project, then the rest) keeps the 404s consistent across the API.
    /// The route calls those first, then this.
    pub fn into_definition(self, id: String) -> Result<ClawDefinition, ClawError> {
        let name = self.name.trim().to_string();
        if name.is_empty() {
            return Err(ClawError::Invalid("name must not be empty".to_string()));
        }
        if name.chars().count() > NAME_MAX {
            return Err(ClawError::Invalid(format!(
                "name must be at most {NAME_MAX} characters"
            )));
        }

        let permission_mode = PermissionMode::parse(&self.permission_mode)?;

        let skill = self
            .skill
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let mut schedules = Vec::with_capacity(self.schedules.len());
        for (index, raw) in self.schedules.into_iter().enumerate() {
            // Validate on save (`claws.mdx:70`) — a bad cron means the Claw is
            // not created at all, so there is never a stored schedule that the
            // runtime would have to refuse later.
            let parsed = cron::Schedule::parse(&raw.cron)
                .map_err(|e| ClawError::Cron { index, detail: e.0 })?;
            let prompts: Vec<String> = raw
                .prompts
                .into_iter()
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect();
            if raw.enabled && prompts.is_empty() {
                return Err(ClawError::Invalid(format!(
                    "schedule {index} is enabled but has no prompts to send"
                )));
            }
            schedules.push(ClawSchedule {
                label: raw
                    .label
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty()),
                // Store the normalised source, so what round-trips is exactly
                // what the parser accepted.
                cron: parsed.source().to_string(),
                prompts,
                enabled: raw.enabled,
            });
        }

        Ok(ClawDefinition {
            id,
            name,
            agent_id: self.agent_id,
            project_id: self.project_id,
            skill,
            enabled: self.enabled,
            auto_start: self.auto_start,
            keep_alive: self.keep_alive,
            restart_on_trigger: self.restart_on_trigger,
            permission_mode,
            skip_if_running: self.skip_if_running,
            schedules,
        })
    }
}

/// Lifecycle state (`claws.mdx:39-48`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ClawState {
    /// Not running — the state every Claw starts in after a server restart
    /// ([INVENTED-10]).
    #[default]
    Stopped,
    /// The ACP connection is being spawned.
    Starting,
    /// A turn is in flight.
    Running,
    /// Connected, waiting for the next trigger.
    Idle,
    /// Gave up: spawn failed, or keepAlive exhausted its 3 retries.
    Error,
}

/// The read-only runtime view merged into every `GET /api/claws` row
/// ("list all claw definitions **with runtime status**", `claws.mdx:76`).
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ClawStatus {
    pub state: ClawState,
    pub connection_id: Option<String>,
    pub session_id: Option<String>,
    /// Restarts in the current streak; reset by a completed turn or a manual
    /// start ([INVENTED-9]).
    pub restarts: u32,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub schedule_count: usize,
    /// `describe()` per schedule, in definition order (§3.3).
    pub schedule_descriptions: Vec<String>,
}

/// Everything that can go wrong with a Claw request.
///
/// Kept free of Axum so this module stays unit-testable; `routes/claws.rs` owns
/// the mapping to status codes and error groups (SPEC-007 §3.2).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClawError {
    /// 400, group `claw`.
    #[error("{0}")]
    Invalid(String),
    /// 400, group `cron`, plus the offending schedule index.
    #[error("schedule {index}: {detail}")]
    Cron { index: usize, detail: String },
    /// 404, group `agent`.
    #[error("unknown agent '{0}'")]
    UnknownAgent(String),
    /// 404, group `project`.
    #[error("unknown project '{0}'")]
    UnknownProject(String),
    /// 404, group `claw`.
    #[error("unknown claw '{0}'")]
    NotFound(String),
    /// 409, group `claw` — start while already running.
    #[error("{0}")]
    Conflict(String),
    /// 502, group `agent` — the spawn failed; the detail carries stderr.
    #[error("{0}")]
    Spawn(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn base() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).unwrap()
    }

    fn input() -> ClawInput {
        ClawInput {
            name: "review-bot".to_string(),
            agent_id: "claude".to_string(),
            project_id: "p1".to_string(),
            skill: None,
            enabled: true,
            auto_start: false,
            keep_alive: true,
            restart_on_trigger: false,
            permission_mode: "auto_approve".to_string(),
            skip_if_running: true,
            schedules: Vec::new(),
        }
    }

    fn schedule(cron: &str) -> ClawSchedule {
        ClawSchedule {
            label: None,
            cron: cron.to_string(),
            prompts: vec!["do a thing".to_string()],
            enabled: true,
        }
    }

    #[test]
    fn valid_input_becomes_a_definition() {
        // E9's pure half.
        let mut i = input();
        i.name = "  review-bot  ".to_string();
        i.schedules = vec![schedule("0 9 * * *")];
        let def = i.into_definition("id-1".to_string()).unwrap();
        assert_eq!(def.name, "review-bot", "name must be trimmed");
        assert_eq!(def.id, "id-1");
        assert_eq!(def.permission_mode, PermissionMode::AutoApprove);
        assert_eq!(def.enabled_schedule_count(), 1);
    }

    #[test]
    fn name_bounds_are_rejected_not_clamped() {
        let mut i = input();
        i.name = "   ".to_string();
        assert!(matches!(
            i.into_definition("x".into()),
            Err(ClawError::Invalid(_))
        ));

        let mut i = input();
        i.name = "a".repeat(NAME_MAX + 1);
        assert!(matches!(
            i.into_definition("x".into()),
            Err(ClawError::Invalid(_))
        ));

        // Exactly at the bound is fine — an off-by-one here would silently ban a
        // legal name.
        let mut i = input();
        i.name = "a".repeat(NAME_MAX);
        assert!(i.into_definition("x".into()).is_ok());
    }

    #[test]
    fn telegram_is_refused_by_name_not_downgraded() {
        // E11 / [INVENTED-6]. The message has to say *telegram*, otherwise a user
        // reading it would think they mistyped rather than picked a dead mode.
        let mut i = input();
        i.permission_mode = "ask_via_telegram".to_string();
        let err = i.into_definition("x".into()).unwrap_err();
        assert!(
            matches!(&err, ClawError::Invalid(m) if m.contains("ask_via_telegram")),
            "got {err:?}"
        );

        let mut i = input();
        i.permission_mode = "yolo".to_string();
        assert!(matches!(
            i.into_definition("x".into()),
            Err(ClawError::Invalid(_))
        ));
    }

    #[test]
    fn all_three_modes_round_trip() {
        for mode in ["auto_approve", "deny_all", "ask_via_ui"] {
            let parsed = PermissionMode::parse(mode).unwrap();
            assert_eq!(parsed.as_str(), mode);
        }
    }

    #[test]
    fn a_bad_cron_names_its_schedule_index() {
        // E3 — the index is what lets the UI mark the right row red.
        let mut i = input();
        i.schedules = vec![schedule("0 9 * * *"), schedule("0 9 * *")];
        match i.into_definition("x".into()).unwrap_err() {
            ClawError::Cron { index, detail } => {
                assert_eq!(index, 1);
                assert!(detail.contains("fields"), "unhelpful detail: {detail}");
            }
            other => panic!("expected a cron error, got {other:?}"),
        }
    }

    #[test]
    fn an_enabled_schedule_needs_prompts() {
        let mut s = schedule("0 9 * * *");
        s.prompts = vec!["  ".to_string()];
        let mut i = input();
        i.schedules = vec![s];
        assert!(matches!(
            i.into_definition("x".into()),
            Err(ClawError::Invalid(_))
        ));

        // Disabled schedules are exempt: a half-written row the user parked is
        // not an error until they switch it on.
        let mut s = schedule("0 9 * * *");
        s.prompts = Vec::new();
        s.enabled = false;
        let mut i = input();
        i.schedules = vec![s];
        assert!(i.into_definition("x".into()).is_ok());
    }

    #[test]
    fn next_run_at_uses_only_enabled_schedules() {
        // E38.
        let mut i = input();
        let mut every_five = schedule("*/5 * * * *");
        every_five.enabled = false;
        i.schedules = vec![schedule("0 9 * * *"), every_five];
        let def = i.into_definition("x".into()).unwrap();

        let next = def.next_run_at(base()).unwrap();
        assert_eq!(next.to_rfc3339(), "2026-11-01T09:00:00+00:00");
        assert_eq!(def.enabled_schedule_count(), 1);
    }

    #[test]
    fn a_disabled_claw_never_fires() {
        // E37.
        let mut i = input();
        i.enabled = false;
        i.schedules = vec![schedule("*/5 * * * *")];
        let def = i.into_definition("x".into()).unwrap();
        assert_eq!(def.next_run_at(base()), None);
    }

    #[test]
    fn descriptions_are_echoed_per_schedule() {
        // E8.
        let mut i = input();
        i.schedules = vec![schedule("0 9 * * *")];
        let def = i.into_definition("x".into()).unwrap();
        let described = def.schedule_descriptions();
        assert_eq!(described.len(), 1);
        assert!(described[0].contains("09:00"), "got {:?}", described[0]);
    }

    #[test]
    fn empty_skill_string_becomes_none() {
        // The form submits "" for "no skill"; storing that would make the runtime
        // look for a skill named "".
        let mut i = input();
        i.skill = Some("   ".to_string());
        assert_eq!(i.into_definition("x".into()).unwrap().skill, None);

        let mut i = input();
        i.skill = Some(" review-pr ".to_string());
        assert_eq!(
            i.into_definition("x".into()).unwrap().skill.as_deref(),
            Some("review-pr")
        );
    }

    #[test]
    fn defaults_match_the_product_docs() {
        // `claws.mdx:14-35` + §3.1. Deserializing the minimum body must produce
        // exactly the documented defaults, not serde's zero values.
        let json = r#"{"name":"n","agentId":"a","projectId":"p"}"#;
        let i: ClawInput = serde_json::from_str(json).unwrap();
        let def = i.into_definition("x".into()).unwrap();
        assert!(def.enabled);
        assert!(!def.auto_start);
        assert!(def.keep_alive);
        assert!(!def.restart_on_trigger);
        assert!(def.skip_if_running);
        assert_eq!(def.permission_mode, PermissionMode::AutoApprove);
        assert!(def.schedules.is_empty());
    }
}
