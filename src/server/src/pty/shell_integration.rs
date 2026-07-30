//! Shell integration — make the shell emit OSC 7 so cwd tracking actually works.
//!
//! Why this exists (deep-dive 02 §5.3): OSC 7 is emitted by the *shell*, not the
//! terminal, and only if its rc file installs a hook. On a default macOS zsh or a
//! stock bash there is no such hook, so a scanner alone would never see a single
//! sequence. The deep-dive's conclusion is explicit: "đừng phụ thuộc rc file user
//! có sẵn hay không" — inject the hook ourselves.
//!
//! Strategy per shell, chosen to never break the user's own startup files:
//!
//! - **zsh**: point `ZDOTDIR` at a generated directory holding `.zshenv`,
//!   `.zprofile` and `.zshrc`. Each of ours re-points `ZDOTDIR` at the user's
//!   real directory just long enough to source the corresponding user file, then
//!   points it back so zsh keeps finding *our* remaining startup files. Our
//!   `.zshrc` runs last: it restores `ZDOTDIR` for good and installs a `chpwd`
//!   hook plus one initial emit. (Same shape as VS Code / iTerm2 shell
//!   integration.)
//! - **bash**: export `PROMPT_COMMAND` with our emit prepended. Bash reads it
//!   from the environment. A `.bashrc` that *assigns* `PROMPT_COMMAND=` instead
//!   of appending will drop the hook — an acceptable, documented degradation
//!   (SPEC-001 §4 [INVENTED-9] neighbourhood); everything else keeps working and
//!   the frontend simply shows the cwd from spawn time.
//! - **anything else** (fish, nu, a bare command): no injection. The scanner
//!   still reports OSC 7 if the program happens to emit it.
//!
//! The generated files are per-terminal and live under the data dir; they are
//! removed when the terminal is dropped.

use std::io;
use std::path::{Path, PathBuf};

/// Shell command emitting one OSC 7 sequence for the current directory.
///
/// `\033]7;file://$HOST$PWD\a` — `printf %s` on `$PWD` avoids mangling paths
/// that contain backslashes or `%`. Not percent-encoded: the scanner decodes
/// `%XX` but tolerates raw bytes, and encoding in POSIX shell would need a loop.
const EMIT_OSC7: &str = r#"printf '\033]7;file://%s%s\a' "${HOSTNAME:-localhost}" "$PWD""#;

/// What kind of shell we detected, and therefore how to inject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Zsh,
    Bash,
    Other,
}

/// Classify a program path/name. Matches on the file stem so `/bin/zsh`,
/// `/opt/homebrew/bin/zsh` and `-zsh` (login argv0) all resolve.
pub fn detect(program: &str) -> ShellKind {
    let stem = Path::new(program)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(program)
        .trim_start_matches('-');
    match stem {
        "zsh" => ShellKind::Zsh,
        "bash" | "sh" => ShellKind::Bash,
        _ => ShellKind::Other,
    }
}

/// Environment variables to inject, plus a scratch dir to clean up on drop.
#[derive(Debug, Default)]
pub struct Integration {
    /// `(key, value)` pairs to set on the `CommandBuilder`.
    pub env: Vec<(String, String)>,
    /// Generated `ZDOTDIR`, if any — deleted when the terminal goes away.
    pub scratch_dir: Option<PathBuf>,
}

impl Integration {
    /// Remove generated files. Best-effort: a leftover temp dir is harmless.
    pub fn cleanup(&self) {
        if let Some(dir) = &self.scratch_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Build the integration for `program`, writing any helper files under `root`.
///
/// Never fails the spawn: if the scratch files can't be written we log and return
/// an empty integration, because a terminal without cwd tracking is far better
/// than no terminal.
pub fn prepare(program: &str, root: &Path, terminal_id: &str) -> Integration {
    match detect(program) {
        ShellKind::Bash => Integration {
            env: vec![("PROMPT_COMMAND".to_string(), bash_prompt_command())],
            scratch_dir: None,
        },
        ShellKind::Zsh => match write_zdotdir(root, terminal_id) {
            Ok(integration) => integration,
            Err(e) => {
                tracing::warn!(
                    "shell integration: could not write ZDOTDIR ({e}); cwd tracking off"
                );
                Integration::default()
            }
        },
        ShellKind::Other => Integration::default(),
    }
}

/// `PROMPT_COMMAND` value: emit, then run whatever the user already had.
///
/// Prepending (rather than appending) means the emit still runs if the user's own
/// command fails.
fn bash_prompt_command() -> String {
    match std::env::var("PROMPT_COMMAND") {
        Ok(existing) if !existing.trim().is_empty() => format!("{EMIT_OSC7}; {existing}"),
        _ => EMIT_OSC7.to_string(),
    }
}

/// Write the three zsh startup files and return the env pointing at them.
fn write_zdotdir(root: &Path, terminal_id: &str) -> io::Result<Integration> {
    let dir = root.join("shell-integration").join(terminal_id);
    std::fs::create_dir_all(&dir)?;

    // The user's real ZDOTDIR — where their own startup files live.
    let user_zdotdir = std::env::var("ZDOTDIR")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOME").ok())
        .unwrap_or_default();

    // `.zshenv` and `.zprofile`: source the user's file, keeping ZDOTDIR pointed
    // at us so zsh continues through *our* startup sequence.
    std::fs::write(dir.join(".zshenv"), chain_snippet("zshenv"))?;
    std::fs::write(dir.join(".zprofile"), chain_snippet("zprofile"))?;
    // `.zshrc` runs last: source the user's, restore ZDOTDIR permanently, then
    // install the hook.
    std::fs::write(dir.join(".zshrc"), zshrc_contents())?;

    Ok(Integration {
        env: vec![
            ("ZDOTDIR".to_string(), dir.display().to_string()),
            ("SPEC_ADE_USER_ZDOTDIR".to_string(), user_zdotdir),
        ],
        scratch_dir: Some(dir),
    })
}

/// Source the user's `.<name>` from their real ZDOTDIR, then put ours back.
///
/// `[[ -f ... ]]` guards a missing file; `builtin source` avoids a user-defined
/// `source` function interfering.
fn chain_snippet(name: &str) -> String {
    format!(
        r#"# Generated by Spec ADE (SPEC-001 shell integration). Safe to delete.
if [[ -n "$SPEC_ADE_USER_ZDOTDIR" && -f "$SPEC_ADE_USER_ZDOTDIR/.{name}" ]]; then
  SPEC_ADE_OUR_ZDOTDIR="$ZDOTDIR"
  ZDOTDIR="$SPEC_ADE_USER_ZDOTDIR"
  builtin source "$SPEC_ADE_USER_ZDOTDIR/.{name}"
  ZDOTDIR="$SPEC_ADE_OUR_ZDOTDIR"
  unset SPEC_ADE_OUR_ZDOTDIR
fi
"#
    )
}

/// Our `.zshrc`: chain the user's, hand `ZDOTDIR` back, install the cwd hook.
fn zshrc_contents() -> String {
    format!(
        r#"{chain}
# Restore the user's ZDOTDIR for good — from here on zsh (and anything the user
# runs) sees their own value, not ours.
if [[ -n "$SPEC_ADE_USER_ZDOTDIR" ]]; then
  ZDOTDIR="$SPEC_ADE_USER_ZDOTDIR"
else
  unset ZDOTDIR
fi
unset SPEC_ADE_USER_ZDOTDIR

# Report the working directory to Spec ADE via OSC 7 (deep-dive 02 §5.3).
spec_ade_report_cwd() {{ {emit} }}
typeset -ga chpwd_functions
chpwd_functions+=(spec_ade_report_cwd)
spec_ade_report_cwd
"#,
        chain = chain_snippet("zshrc"),
        emit = EMIT_OSC7,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_shell_from_path_and_login_argv0() {
        assert_eq!(detect("/bin/zsh"), ShellKind::Zsh);
        assert_eq!(detect("/opt/homebrew/bin/zsh"), ShellKind::Zsh);
        assert_eq!(detect("-zsh"), ShellKind::Zsh); // login shell argv0
        assert_eq!(detect("zsh"), ShellKind::Zsh);
        assert_eq!(detect("/bin/bash"), ShellKind::Bash);
        assert_eq!(detect("/bin/sh"), ShellKind::Bash);
        assert_eq!(detect("/usr/bin/fish"), ShellKind::Other);
        assert_eq!(detect("/usr/bin/env"), ShellKind::Other);
    }

    #[test]
    fn bash_gets_prompt_command_only() {
        let root = std::env::temp_dir();
        let i = prepare("/bin/bash", &root, "t1");
        assert!(i.scratch_dir.is_none(), "bash needs no scratch files");
        let (key, value) = &i.env[0];
        assert_eq!(key, "PROMPT_COMMAND");
        assert!(value.contains("]7;file://"), "must emit OSC 7: {value}");
    }

    #[test]
    fn other_shells_get_no_injection() {
        let root = std::env::temp_dir();
        let i = prepare("/usr/bin/fish", &root, "t2");
        assert!(i.env.is_empty());
        assert!(i.scratch_dir.is_none());
    }

    #[test]
    fn zsh_zdotdir_chains_user_files_and_installs_hook() {
        let root = std::env::temp_dir().join(format!("spec-ade-test-{}", uuid::Uuid::new_v4()));
        let i = prepare("/bin/zsh", &root, "t3");

        let dir = i.scratch_dir.clone().expect("zsh needs a ZDOTDIR");
        assert!(
            i.env
                .iter()
                .any(|(k, v)| k == "ZDOTDIR" && v == &dir.display().to_string()),
            "ZDOTDIR must point at the generated dir"
        );
        assert!(i.env.iter().any(|(k, _)| k == "SPEC_ADE_USER_ZDOTDIR"));

        // All three startup files exist and chain the user's equivalents, so a
        // user's own config is never skipped.
        for (name, user_file) in [
            (".zshenv", ".zshenv"),
            (".zprofile", ".zprofile"),
            (".zshrc", ".zshrc"),
        ] {
            let body = std::fs::read_to_string(dir.join(name)).unwrap();
            assert!(
                body.contains(&format!("$SPEC_ADE_USER_ZDOTDIR/{user_file}")),
                "{name} must source the user's {user_file}"
            );
            assert!(body.contains("builtin source"), "{name} must source it");
        }

        // .zshrc additionally hands ZDOTDIR back and installs the chpwd hook.
        let zshrc = std::fs::read_to_string(dir.join(".zshrc")).unwrap();
        assert!(zshrc.contains("chpwd_functions+=(spec_ade_report_cwd)"));
        assert!(zshrc.contains("]7;file://"), "hook must emit OSC 7");
        assert!(
            zshrc.contains("unset SPEC_ADE_USER_ZDOTDIR"),
            "must not leak our marker var into the user's shell"
        );

        i.cleanup();
        assert!(!dir.exists(), "cleanup must remove the scratch dir");
        let _ = std::fs::remove_dir_all(&root);
    }
}
