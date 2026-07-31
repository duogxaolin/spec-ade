//! `git` CLI wrapper — the mutation path (SPEC-005 §5.3).
//!
//! Every mutation shells out to `git` rather than going through libgit2, so the
//! user's credential helper, hooks, GPG signing and protocol.v2 all apply
//! (deep-dive 03 §1.3). The defaults below are lifted from GitButler's
//! `GitExecutor` (`crates/gitbutler-git/src/executor/{mod.rs:71-90,tokio/mod.rs:27-83}`)
//! because each one prevents a specific failure mode — see `run`.
//!
//! `tokio::process` is async-native, so this needs **no** `spawn_blocking`; that
//! is one more advantage of mutating through the CLI (deep-dive 03 §4 #1).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use super::GitError;

/// Wall-clock limit for one `git` invocation.
///
/// A hung `pre-commit` hook would otherwise hold the HTTP connection forever
/// (§9 #8). 30s is generous for a local commit and still bounded.
const TIMEOUT_SECS: u64 = 30;

/// Result of one `git` invocation. Kept even for failures because callers
/// classify on `stderr` (§5.3).
#[derive(Debug, Clone)]
pub struct CliOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CliOutput {
    pub fn ok(&self) -> bool {
        self.code == 0
    }

    /// stdout with the trailing newline removed — what nearly every caller wants
    /// from a single-value `git` query.
    pub fn trimmed(&self) -> &str {
        self.stdout.trim_end_matches(['\n', '\r'])
    }

    /// stderr and stdout together, for classification.
    ///
    /// `git` is inconsistent about which stream carries a diagnostic: `merge`
    /// writes "CONFLICT (content): ..." and "Already up to date." to **stdout**,
    /// while `checkout` writes its refusal to **stderr**. Classifying on only one
    /// stream misses half the cases.
    pub fn combined(&self) -> String {
        let mut all = String::with_capacity(self.stdout.len() + self.stderr.len() + 1);
        all.push_str(&self.stderr);
        if !self.stderr.is_empty() && !self.stdout.is_empty() {
            all.push('\n');
        }
        all.push_str(&self.stdout);
        all
    }
}

/// A `git` CLI bound to one repository working directory.
#[derive(Debug, Clone)]
pub struct GitCli {
    root: PathBuf,
}

impl GitCli {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Run `git` with the safe defaults, returning the output even on failure.
    ///
    /// Each default earns its place:
    /// - `kill_on_drop(true)` — if the client disconnects and the future is
    ///   dropped, the child dies instead of orphaning (deep-dive 03 §1.3).
    /// - `--no-pager` — a pager on a pipe would wait for input that never comes.
    /// - `-c protocol.version=2` — matches what modern `git` negotiates anyway,
    ///   and pinning it keeps behaviour stable across git versions.
    /// - `LC_ALL=C` — forces English messages. Without it, the stderr
    ///   classification in `classify` silently stops matching on a machine with
    ///   another locale (§9 #6).
    /// - `GIT_TERMINAL_PROMPT=0` + `stdin(null)` — a command that wants a
    ///   credential fails fast instead of blocking forever on a stdin that does
    ///   not exist ([SPEC-005 INVENTED-1], §9 #7).
    ///
    /// `args` is always a fixed slice built by the caller; nothing is interpolated
    /// into a shell string, because there is no shell — `Command` execs `git`
    /// directly.
    pub async fn run<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<CliOutput, GitError> {
        let mut cmd = self.command(args);
        cmd.stdin(Stdio::null());
        self.wait(cmd, None).await
    }

    /// Run `git` with `input` on stdin.
    ///
    /// Used for commit messages: `--file -` accepts anything, where `-m` has to
    /// survive being an argument (a message starting with `-`, or longer than the
    /// platform's argv limit).
    ///
    /// This is the one place `stdin(null)` is relaxed. It stays safe because the
    /// pipe is closed as soon as `input` is written, so a command that then asks
    /// for a credential still sees EOF and fails fast rather than hanging.
    pub async fn run_with_stdin<S: AsRef<OsStr>>(
        &self,
        args: &[S],
        input: &str,
    ) -> Result<CliOutput, GitError> {
        let mut cmd = self.command(args);
        cmd.stdin(Stdio::piped());
        self.wait(cmd, Some(input)).await
    }

    /// Run with stdin and classify a non-zero exit, mirroring [`Self::checked`].
    pub async fn checked_with_stdin<S: AsRef<OsStr>>(
        &self,
        args: &[S],
        input: &str,
    ) -> Result<CliOutput, GitError> {
        let output = self.run_with_stdin(args, input).await?;
        if output.ok() {
            return Ok(output);
        }
        Err(classify(&output))
    }

    /// Build the command with every safe default applied.
    fn command<S: AsRef<OsStr>>(&self, args: &[S]) -> Command {
        let mut cmd = Command::new("git");
        cmd.kill_on_drop(true)
            .current_dir(&self.root)
            .arg("--no-pager")
            .args(["-c", "protocol.version=2"])
            .args(args)
            .env("LC_ALL", "C")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // No console window when a packaged desktop build shells out
        // (deep-dive 03 §1.3, `executor/tokio/mod.rs:27-83`).
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        cmd
    }

    /// Spawn, feed stdin if any, and collect the output under the timeout.
    async fn wait(&self, mut cmd: Command, input: Option<&str>) -> Result<CliOutput, GitError> {
        let child = async {
            let mut child = cmd.spawn()?;
            if let Some(input) = input {
                // `take` so the handle drops at the end of this block, closing the
                // pipe. Without the close, `git commit --file -` waits forever for
                // an EOF that never arrives.
                if let Some(mut stdin) = child.stdin.take() {
                    use tokio::io::AsyncWriteExt;
                    stdin.write_all(input.as_bytes()).await?;
                    stdin.shutdown().await?;
                }
            }
            child.wait_with_output().await
        };
        let output = match tokio::time::timeout(Duration::from_secs(TIMEOUT_SECS), child).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(GitError::GitMissing);
            }
            Ok(Err(e)) => return Err(GitError::Io(e)),
            // The future is dropped here, so `kill_on_drop` reaps the child.
            Err(_) => return Err(GitError::Timeout(TIMEOUT_SECS)),
        };

        Ok(CliOutput {
            // 127 mirrors a shell's "command not found"; `None` means the process
            // was killed by a signal, which is a failure either way.
            code: output.status.code().unwrap_or(127),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    /// Run `git`, turning a non-zero exit into a classified `GitError`.
    pub async fn checked<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<CliOutput, GitError> {
        let output = self.run(args).await?;
        if output.ok() {
            return Ok(output);
        }
        Err(classify(&output))
    }
}

/// Map a failed `git` invocation onto a `GitError` variant.
///
/// Pattern taken from GitButler, which parses stderr to classify push/fetch
/// failures rather than making the user match strings
/// (deep-dive 03 §1.3, `repository.rs:511-594`).
///
/// Order matters: the checks run most-specific first. "nothing to commit" also
/// contains the word "changes", so a looser check placed earlier would swallow it.
pub fn classify(output: &CliOutput) -> GitError {
    let text = output.combined();
    let lower = text.to_lowercase();

    if lower.contains("not a git repository") {
        return GitError::NotARepo;
    }
    if lower.contains("nothing to commit")
        || lower.contains("no changes added to commit")
        || lower.contains("nothing added to commit")
    {
        return GitError::NothingToCommit;
    }
    if text.contains("CONFLICT (") || lower.contains("automatic merge failed") {
        return GitError::Conflict {
            message: first_meaningful_line(&text),
            paths: Vec::new(),
        };
    }
    if lower.contains("would be overwritten")
        || lower.contains("local changes")
        || lower.contains("please commit your changes or stash them")
    {
        return GitError::Blocked(first_meaningful_line(&text));
    }
    if lower.contains("did not match any file")
        || lower.contains("error: pathspec")
        || lower.contains("unknown revision or path not in the working tree")
    {
        return GitError::NotFound(first_meaningful_line(&text));
    }
    if lower.contains("could not read username")
        || lower.contains("terminal prompts disabled")
        || lower.contains("authentication failed")
    {
        // [SPEC-005 INVENTED-1]: no askpass server, so a credential prompt is a
        // hard failure. Say so plainly instead of leaking "terminal prompts
        // disabled", which reads like a bug in our own wrapper.
        return GitError::Blocked(
            "git needs credentials, which this build cannot prompt for (push/fetch is SPEC-009)"
                .into(),
        );
    }

    GitError::CommandFailed {
        code: output.code,
        // Trimmed, not truncated: a hook's message is often several lines and all
        // of them matter (C22).
        stderr: if text.trim().is_empty() {
            format!("git exited {} with no output", output.code)
        } else {
            text.trim().to_string()
        },
    }
}

/// First non-empty line, used as a one-line error summary.
fn first_meaningful_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("git failed")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(code: i32, stderr: &str) -> CliOutput {
        CliOutput {
            code,
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }

    fn out_stdout(code: i32, stdout: &str) -> CliOutput {
        CliOutput {
            code,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    #[test]
    fn classifies_nothing_to_commit() {
        // `git commit` writes this to stdout, not stderr — the reason `combined()`
        // exists.
        let o = out_stdout(1, "On branch main\nnothing to commit, working tree clean\n");
        assert!(matches!(classify(&o), GitError::NothingToCommit));
    }

    #[test]
    fn classifies_no_changes_added() {
        let o = out_stdout(1, "no changes added to commit (use \"git add\")\n");
        assert!(matches!(classify(&o), GitError::NothingToCommit));
    }

    #[test]
    fn classifies_conflict() {
        let o = out_stdout(
            1,
            "Auto-merging a.txt\nCONFLICT (content): Merge conflict in a.txt\nAutomatic merge failed; fix conflicts\n",
        );
        match classify(&o) {
            GitError::Conflict { message, .. } => assert_eq!(message, "Auto-merging a.txt"),
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[test]
    fn classifies_blocked_checkout() {
        let o = out(
            1,
            "error: Your local changes to the following files would be overwritten by checkout:\n\ta.txt\n",
        );
        assert!(matches!(classify(&o), GitError::Blocked(_)));
    }

    #[test]
    fn classifies_bad_pathspec() {
        let o = out(
            1,
            "error: pathspec 'nope.txt' did not match any file(s) known to git\n",
        );
        assert!(matches!(classify(&o), GitError::NotFound(_)));
    }

    #[test]
    fn classifies_missing_repo() {
        let o = out(
            128,
            "fatal: not a git repository (or any of the parent directories): .git\n",
        );
        assert!(matches!(classify(&o), GitError::NotARepo));
    }

    #[test]
    fn classifies_credential_prompt_as_blocked_with_our_own_wording() {
        let o = out(
            128,
            "fatal: could not read Username for 'https://github.com': terminal prompts disabled\n",
        );
        match classify(&o) {
            // The user should not have to interpret "terminal prompts disabled",
            // which sounds like our bug rather than a missing credential helper.
            GitError::Blocked(msg) => assert!(msg.contains("credentials"), "got {msg:?}"),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn falls_back_to_command_failed_with_full_stderr() {
        // A failing hook: not classifiable, and every line matters (C22).
        let o = out(1, ".git/hooks/pre-commit: line 2: boom\nhook exited 1\n");
        match classify(&o) {
            GitError::CommandFailed { code, stderr } => {
                assert_eq!(code, 1);
                assert!(stderr.contains("boom"));
                assert!(stderr.contains("hook exited 1"));
            }
            other => panic!("expected CommandFailed, got {other:?}"),
        }
    }

    #[test]
    fn command_failed_still_says_something_when_git_was_silent() {
        let o = out(3, "   \n");
        match classify(&o) {
            GitError::CommandFailed { stderr, .. } => assert!(stderr.contains("exited 3")),
            other => panic!("expected CommandFailed, got {other:?}"),
        }
    }

    #[test]
    fn combined_joins_both_streams_without_a_stray_newline() {
        let both = CliOutput {
            code: 1,
            stdout: "out".into(),
            stderr: "err".into(),
        };
        assert_eq!(both.combined(), "err\nout");
        assert_eq!(out(1, "err").combined(), "err");
        assert_eq!(out_stdout(1, "out").combined(), "out");
    }

    #[test]
    fn trimmed_strips_only_the_trailing_newline() {
        let o = out_stdout(0, "  main  \n");
        assert_eq!(o.trimmed(), "  main  ");
    }

    #[tokio::test]
    async fn reports_not_a_repo_for_a_plain_directory() {
        let dir = std::env::temp_dir().join(format!("spec-ade-cli-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let cli = GitCli::new(&dir);

        // `rev-parse` in a non-repo is the cheapest real invocation that proves the
        // wrapper runs `git` at all and that classification is wired up.
        let err = cli.checked(&["rev-parse", "--git-dir"]).await;
        match err {
            Err(GitError::NotARepo) => {}
            // A machine without git still passes: that is the documented degrade
            // ([SPEC-005 INVENTED-13]), not a test failure.
            Err(GitError::GitMissing) => {}
            other => panic!("expected NotARepo or GitMissing, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
