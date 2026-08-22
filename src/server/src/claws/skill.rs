//! `SKILL.md` discovery — the 8 directories, in priority order (SPEC-007 §5.4).
//!
//! A Skill is a prompt plus tooling config packaged in a `SKILL.md` file
//! (`docs/spec-ade-clone/docs/core-concepts/skills.mdx`). Discovery walks eight
//! fixed directories — four under the workspace, then the same four under `$HOME`
//! (`skills.mdx:15-24`) — and the **first** occurrence of a name wins, which is
//! exactly "workspace skills take precedence" (`skills.mdx:26`) because the walk
//! order *is* the priority order. No separate tie-break rule to keep in sync.
//!
//! Two rules that the tests pin, both from `04-module-tech-reference.md:71`:
//!
//! 1. **A broken file is skipped, never fatal.** YAML is whitespace-sensitive, so
//!    one bad `SKILL.md` in a user's home directory must not take out the whole
//!    dropdown. Every failure is a `tracing::warn!` and a `continue`.
//! 2. **No cache.** `skills.mdx:69` promises a new skill "appears automatically —
//!    no restart needed", and eight `read_dir` calls are not worth a staleness bug.
//!
//! This module is synchronous and knows nothing about Axum: the route wraps it in
//! `spawn_blocking` (§9.2).

use std::collections::BTreeMap;
use std::path::Path;

use gray_matter::Matter;
use gray_matter::engine::YAML;
use serde::{Deserialize, Serialize};

/// The four directory names checked under both the workspace and `$HOME`,
/// in the order `skills.mdx:15-24` lists them.
pub const SKILL_DIRS: [&str; 4] = [
    ".augment/skills",
    ".augment/skill",
    ".claude/skills",
    ".claude/skill",
];

/// Where a skill was found. Surfaced so the UI can say which copy won when a
/// name exists in both places (deliverable #11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillSource {
    Workspace,
    User,
}

/// The frontmatter block, exactly the keys documented at `skills.mdx:52-58`.
///
/// Every field is optional: `skills.mdx` calls only `description` "recommended",
/// and a `SKILL.md` with no frontmatter at all is a legitimate skill (E29).
#[derive(Debug, Default, Deserialize)]
struct Front {
    description: Option<String>,
    license: Option<String>,
    compatibility: Option<String>,
    #[serde(rename = "allowedTools", alias = "allowed_tools")]
    allowed_tools: Option<String>,
    metadata: Option<serde_json::Value>,
}

/// One discovered skill, as returned by `GET /api/projects/{id}/skills`
/// ([INVENTED-3], SPEC-007 §3.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    /// The name of the directory holding `SKILL.md` — the identity a Claw stores.
    pub name: String,
    pub source: SkillSource,
    /// Display path, so a user can tell two same-named skills apart.
    pub dir: String,
    pub description: Option<String>,
    pub license: Option<String>,
    pub compatibility: Option<String>,
    pub allowed_tools: Option<String>,
    pub metadata: Option<serde_json::Value>,
    /// The body after the frontmatter — what a Claw actually sends
    /// ([INVENTED-7]: as a `session/prompt`, not a slash command).
    pub prompt: String,
}

/// Discover every skill visible to `root`, workspace first, sorted by name.
///
/// `home` is passed in rather than read from the environment so a test can point
/// it at a `TempDir` without mutating process-global state — the same reasoning
/// as `AcpLimits`. `None` means "no `$HOME`": workspace only, and **not** an
/// error (E33), matching how `storage::config_dir` degrades.
pub fn discover(root: &Path, home: Option<&Path>) -> Vec<Skill> {
    let mut found: BTreeMap<String, Skill> = BTreeMap::new();

    let roots = [(root, SkillSource::Workspace)]
        .into_iter()
        .chain(home.map(|h| (h, SkillSource::User)));

    for (base, source) in roots {
        for rel in SKILL_DIRS {
            for skill in scan_dir(&base.join(rel), source) {
                // First writer wins. Because the walk visits the workspace before
                // `$HOME`, this *is* the precedence rule (E31) — the loser is
                // absent from the result entirely, as §3.4 requires.
                found.entry(skill.name.clone()).or_insert(skill);
            }
        }
    }

    // `BTreeMap` already orders by name; collecting keeps that guarantee explicit.
    found.into_values().collect()
}

/// Every readable skill directly under `dir`. A missing directory is the normal
/// case (most of the eight never exist) and is silently empty.
fn scan_dir(dir: &Path, source: SkillSource) -> Vec<Skill> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!("skill discovery: cannot read {}: {e}", dir.display());
            return Vec::new();
        }
    };

    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let manifest = path.join("SKILL.md");
        if !manifest.is_file() {
            continue;
        }
        match parse_skill(&manifest, name, source, &path) {
            Ok(skill) => out.push(skill),
            Err(reason) => {
                // E30: warn and skip. Breaking the loop here would let one bad
                // file hide every skill that sorts after it.
                tracing::warn!("skill discovery: skipping {}: {reason}", manifest.display());
            }
        }
    }
    out
}

/// Read and parse one `SKILL.md`.
///
/// The `Err` is a plain `String`: it is only ever logged, and giving it a type
/// would invite callers to branch on a failure whose only correct handling is
/// "skip this file".
fn parse_skill(
    manifest: &Path,
    name: &str,
    source: SkillSource,
    dir: &Path,
) -> Result<Skill, String> {
    let raw = std::fs::read_to_string(manifest).map_err(|e| e.to_string())?;
    let parsed = Matter::<YAML>::new()
        .parse::<Front>(&raw)
        .map_err(|e| format!("invalid frontmatter: {e}"))?;
    // `data` is `None` when the file has no frontmatter — a valid skill whose
    // metadata fields are all null and whose prompt is the whole file (E29).
    let front = parsed.data.unwrap_or_default();

    Ok(Skill {
        name: name.to_string(),
        source,
        dir: dir.display().to_string(),
        description: front.description,
        license: front.license,
        compatibility: front.compatibility,
        allowed_tools: front.allowed_tools,
        metadata: front.metadata,
        prompt: parsed.content.trim().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write `SKILL.md` for `name` under `base/rel`.
    fn write_skill(base: &Path, rel: &str, name: &str, body: &str) {
        let dir = base.join(rel).join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), body).unwrap();
    }

    const GOOD: &str = "---\n\
description: Review pull requests\n\
license: MIT\n\
compatibility: auggie, claude\n\
allowedTools: Read, Glob, Grep\n\
metadata:\n  team: core\n\
---\n\
You are a code review agent.\n";

    #[test]
    fn parses_every_documented_frontmatter_field() {
        // E28.
        let tmp = tempfile::tempdir().unwrap();
        write_skill(tmp.path(), ".claude/skills", "review-pr", GOOD);

        let skills = discover(tmp.path(), None);
        assert_eq!(skills.len(), 1);
        let s = &skills[0];
        assert_eq!(s.name, "review-pr");
        assert_eq!(s.source, SkillSource::Workspace);
        assert_eq!(s.description.as_deref(), Some("Review pull requests"));
        assert_eq!(s.license.as_deref(), Some("MIT"));
        assert_eq!(s.compatibility.as_deref(), Some("auggie, claude"));
        assert_eq!(s.allowed_tools.as_deref(), Some("Read, Glob, Grep"));
        assert_eq!(
            s.metadata,
            Some(serde_json::json!({ "team": "core" })),
            "metadata must survive as structured JSON, not a string"
        );
        assert_eq!(s.prompt, "You are a code review agent.");
    }

    #[test]
    fn a_file_without_frontmatter_is_still_a_skill() {
        // E29. The whole file is the prompt.
        let tmp = tempfile::tempdir().unwrap();
        write_skill(
            tmp.path(),
            ".claude/skills",
            "plain",
            "Just do the thing.\n",
        );

        let skills = discover(tmp.path(), None);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, None);
        assert_eq!(skills[0].prompt, "Just do the thing.");
    }

    #[test]
    fn a_broken_file_is_skipped_and_the_others_survive() {
        // E30 — the assertion that goes red if `warn + continue` becomes `return`.
        let tmp = tempfile::tempdir().unwrap();
        write_skill(
            tmp.path(),
            ".claude/skills",
            "aaa-broken",
            "---\ndescription: [unclosed\n---\nbody\n",
        );
        write_skill(tmp.path(), ".claude/skills", "zzz-good", GOOD);

        let skills = discover(tmp.path(), None);
        assert_eq!(
            skills.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["zzz-good"],
            "the broken skill must be dropped and the good one kept"
        );
    }

    #[test]
    fn workspace_beats_user_for_the_same_name() {
        // E31.
        let ws = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write_skill(
            ws.path(),
            ".claude/skills",
            "dup",
            "---\ndescription: from workspace\n---\nW\n",
        );
        write_skill(
            home.path(),
            ".claude/skills",
            "dup",
            "---\ndescription: from home\n---\nH\n",
        );

        let skills = discover(ws.path(), Some(home.path()));
        assert_eq!(
            skills.len(),
            1,
            "the loser must be absent, not listed twice"
        );
        assert_eq!(skills[0].source, SkillSource::Workspace);
        assert_eq!(skills[0].description.as_deref(), Some("from workspace"));
    }

    #[test]
    fn all_four_workspace_variants_are_scanned() {
        // E32. One skill per directory name, so a dropped entry in SKILL_DIRS
        // shows up as a missing name rather than a silently identical count.
        let tmp = tempfile::tempdir().unwrap();
        for (i, rel) in SKILL_DIRS.iter().enumerate() {
            write_skill(tmp.path(), rel, &format!("s{i}"), "body\n");
        }
        let names: Vec<_> = discover(tmp.path(), None)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["s0", "s1", "s2", "s3"]);
    }

    #[test]
    fn user_dirs_are_scanned_too_and_marked() {
        let ws = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        write_skill(home.path(), ".augment/skills", "mine", "body\n");

        let skills = discover(ws.path(), Some(home.path()));
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].source, SkillSource::User);
    }

    #[test]
    fn no_home_means_workspace_only_and_no_error() {
        // E33.
        let tmp = tempfile::tempdir().unwrap();
        write_skill(tmp.path(), ".claude/skills", "only", "body\n");
        assert_eq!(discover(tmp.path(), None).len(), 1);
    }

    #[test]
    fn results_are_sorted_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        for name in ["zeta", "alpha", "mid"] {
            write_skill(tmp.path(), ".claude/skills", name, "body\n");
        }
        let names: Vec<_> = discover(tmp.path(), None)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, vec!["alpha", "mid", "zeta"]);
    }

    #[test]
    fn a_directory_without_skill_md_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".claude/skills/empty")).unwrap();
        std::fs::write(tmp.path().join(".claude/skills/loose.md"), "x").unwrap();
        assert!(discover(tmp.path(), None).is_empty());
    }

    #[test]
    fn missing_directories_are_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(discover(&tmp.path().join("nope"), None).is_empty());
    }
}
