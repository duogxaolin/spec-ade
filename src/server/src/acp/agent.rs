//! Agent catalogue (SPEC-003 §3.4).
//!
//! An entry names an executable plus its argv, kept in `settings.json`. Read-only
//! in this phase: adding an agent means editing the file, because the management
//! UI belongs to a later spec and a half-built CRUD surface would be dead code.
//!
//! SECURITY: `command` and `args` are stored **separately** and handed to
//! `AcpAgentConfig` as a program plus an argv vector. There is deliberately no
//! path here that parses a user-supplied string into a command line —
//! `AcpAgent::from_str` does shell-style splitting, and feeding it a value that
//! came in over HTTP would create an injection surface for no benefit.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use agent_client_protocol::{AcpAgent, AcpAgentConfig};

/// One configured agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AcpAgentEntry {
    /// Stable key used by `POST /api/acp/spawn`.
    pub id: String,
    /// Label for the UI.
    pub name: String,
    /// Executable to run. Resolved via `PATH` by the OS, not a shell.
    pub command: String,
    /// Arguments, already split — never a single string to be re-parsed.
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment for the child, on top of the server's own.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl AcpAgentEntry {
    /// Build the launch config for this entry.
    pub fn to_acp_agent(&self) -> AcpAgent {
        let config = AcpAgentConfig::new(&self.command)
            .args(self.args.iter().cloned())
            .envs(self.env.clone());
        AcpAgent::new(config)
    }
}

/// Catalogue seeded on first run.
///
/// These mirror the crate's own `AcpAgent::claude_agent()` / `codex()` presets,
/// but written out as command + args rather than built from their shell strings —
/// so the stored shape is the same one a user-added entry has, and the spawn path
/// has exactly one code path to test.
pub fn default_agents() -> Vec<AcpAgentEntry> {
    vec![
        AcpAgentEntry {
            id: "claude".to_string(),
            name: "Claude Code".to_string(),
            command: "npx".to_string(),
            args: vec![
                "-y".to_string(),
                "@agentclientprotocol/claude-agent-acp@latest".to_string(),
            ],
            env: BTreeMap::new(),
        },
        AcpAgentEntry {
            id: "codex".to_string(),
            name: "Codex".to_string(),
            command: "npx".to_string(),
            args: vec![
                "-y".to_string(),
                "@agentclientprotocol/codex-acp@latest".to_string(),
            ],
            env: BTreeMap::new(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_crate_presets() {
        // If the crate's preset command line changes, the seed is stale and the
        // default agents would silently point at the wrong package.
        let agents = default_agents();
        let claude = &agents[0];
        let preset = AcpAgent::claude_agent();
        assert_eq!(claude.command, preset.config().command().to_string_lossy());
        assert_eq!(claude.args, preset.config().arguments());

        let codex = &agents[1];
        let preset = AcpAgent::codex();
        assert_eq!(codex.command, preset.config().command().to_string_lossy());
        assert_eq!(codex.args, preset.config().arguments());
    }

    #[test]
    fn args_are_passed_through_verbatim() {
        // An arg containing shell metacharacters must reach the child as ONE
        // argument, not be re-split. This is the injection guard.
        let entry = AcpAgentEntry {
            id: "x".into(),
            name: "x".into(),
            command: "/bin/echo".into(),
            args: vec!["a b; rm -rf /".into()],
            env: BTreeMap::new(),
        };
        let agent = entry.to_acp_agent();
        assert_eq!(agent.config().arguments(), &["a b; rm -rf /".to_string()]);
    }

    #[test]
    fn entry_round_trips_through_json_in_camel_case() {
        let entry = &default_agents()[0];
        let json = serde_json::to_value(entry).unwrap();
        assert_eq!(json["id"], "claude");
        assert_eq!(json["command"], "npx");
        let back: AcpAgentEntry = serde_json::from_value(json).unwrap();
        assert_eq!(&back, entry);
    }

    #[test]
    fn args_and_env_default_when_absent() {
        // A hand-edited settings.json with only the required keys must load.
        let entry: AcpAgentEntry =
            serde_json::from_str(r#"{"id":"a","name":"A","command":"my-agent"}"#).unwrap();
        assert!(entry.args.is_empty());
        assert!(entry.env.is_empty());
    }
}
