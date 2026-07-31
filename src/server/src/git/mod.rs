//! Git integration — hybrid libgit2 (read) + git CLI (mutation).
//!
//! Spec: `docs/specs/SPEC-005-git-integration.md`.
//!
//! The central rule of this phase (deep-dive 03 §1, from GitButler):
//! **read with a library, write with the CLI.**
//!
//! - Reads (`status`/`diff`/`log`/`blame`/`branches`) go through `git2` inside
//!   `spawn_blocking` — libgit2 is blocking and its handles are `!Send`
//!   (02 §traps, deep-dive 03 §4).
//! - Mutations (`stage`/`commit`/`branch`/`merge`/`discard`) shell out to the
//!   `git` CLI so the user's credential helper, hooks, GPG signing and
//!   protocol.v2 all apply (deep-dive 03 §1.3, §1.5).
//!
//! Module boundary, enforced by what each file imports:
//! - `repo` knows `git2`, never HTTP.
//! - `mutate` knows `cli`, never `git2` — the compiler is what keeps mutations
//!   off libgit2.
//! - `routes::git` knows HTTP, never `git2`.

pub mod cli;
pub mod mutate;
pub mod repo;
pub mod watch;

use std::path::{Component, Path, PathBuf};

pub use cli::GitCli;
pub use watch::GitWatchers;

/// Everything that can go wrong in the git layer.
///
/// Variants exist to be *distinguished*, not just displayed: the CLI's stderr is
/// classified into these so the frontend can react (offer a force, show a
/// conflict list) instead of asking the user to read an error string
/// (deep-dive 03 §1.3 #3, `gitbutler-git/src/repository.rs:511-594`).
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// The project directory is not inside a git repository. Not an error for
    /// `GET status` — the handler turns it into `{isRepo: false}` (C5).
    #[error("not a git repository")]
    NotARepo,

    /// A path was malformed: absolute, or escaping the root with `..` (§5.4).
    ///
    /// 400, matching what the file API already answers for the same input
    /// (SPEC-002 `PathError::Traversal`/`Absolute`) — the client fixes its payload.
    #[error("{0}")]
    Path(String),

    /// A well-formed path that this API refuses to touch: anything under `.git/`
    /// (C17, §5.4).
    ///
    /// 403 rather than 400, on SPEC-002's line: the request was understood and is
    /// refused, so there is nothing for the client to correct. Writing to
    /// `.git/index` would corrupt the repository, and reading `.git/config` hands
    /// out remote URLs that can carry credentials.
    #[error("{0}")]
    Forbidden(String),

    /// The requested object/path does not exist in the repository.
    #[error("{0}")]
    NotFound(String),

    /// `git commit` with nothing staged. 409, and no commit was created (C21).
    #[error("nothing to commit")]
    NothingToCommit,

    /// A merge/checkout stopped because of conflicts. Carries the paths so the
    /// UI can open them in the 3-way editor.
    #[error("{message}")]
    Conflict { message: String, paths: Vec<String> },

    /// `git merge` reported "Already up to date". Exit code 0, so this is *not*
    /// a failure — but it must be told apart from a real merge so the UI does not
    /// claim to have merged something (§9 #9).
    #[error("already up to date")]
    UpToDate,

    /// The operation refused because the working tree would lose changes.
    #[error("{0}")]
    Blocked(String),

    /// `git` exited non-zero for a reason we did not classify. `stderr` is passed
    /// through verbatim — a failing `pre-commit` hook lands here and its message
    /// is the only useful thing we have (C22).
    #[error("git exited {code}: {stderr}")]
    CommandFailed { code: i32, stderr: String },

    /// No `git` on `PATH` ([SPEC-005 INVENTED-13]). Reads still work because
    /// libgit2 is linked in, so this degrades to a read-only panel.
    #[error("git executable not found in PATH")]
    GitMissing,

    /// A `git` invocation exceeded the timeout — usually a hung hook (§9 #8).
    #[error("git timed out after {0}s")]
    Timeout(u64),

    #[error("libgit2: {0}")]
    Libgit2(#[from] git2::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl GitError {
    /// Short group name used as the `error` field of the JSON body
    /// (SPEC-002 §3.6 format).
    pub fn group(&self) -> &'static str {
        match self {
            GitError::NotARepo => "repo",
            GitError::Path(_) | GitError::Forbidden(_) => "path",
            GitError::NotFound(_) => "notFound",
            GitError::NothingToCommit => "nothingToCommit",
            GitError::Conflict { .. } => "conflict",
            GitError::UpToDate => "upToDate",
            GitError::Blocked(_) => "blocked",
            GitError::CommandFailed { .. } | GitError::Libgit2(_) => "git",
            GitError::GitMissing => "gitMissing",
            GitError::Timeout(_) => "timeout",
            GitError::Io(_) => "io",
        }
    }
}

/// Normalize a client-supplied path into a repo-relative POSIX path.
///
/// Two jobs, both load-bearing (§5.4):
/// 1. Reject traversal, absolute paths and anything inside `.git/`. A `discard`
///    aimed at `.git/index` would corrupt the repository (C16, C17).
/// 2. Return a *relative* path — pathspecs work either way, but relative is what
///    `git` expects and what tests can assert on.
///
/// The filesystem guard (`files::resolve`) still runs for paths that must exist;
/// this is the string-level rule, and it applies even to paths that do not exist
/// any more — a deleted file still has a valid status entry.
pub fn relative_path(rel: &str) -> Result<String, GitError> {
    let raw = rel.trim();
    if raw.is_empty() {
        return Err(GitError::Path("path is required".into()));
    }

    let path = Path::new(raw);
    if path.is_absolute() {
        return Err(GitError::Path(format!("path must be relative: {raw}")));
    }

    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let part = part.to_string_lossy();
                // `.git` at any depth: submodules and nested repos have their own,
                // and none of them are ours to write to.
                if part.eq_ignore_ascii_case(".git") {
                    return Err(GitError::Forbidden(
                        "path inside .git is not allowed".into(),
                    ));
                }
                parts.push(part.into_owned());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(GitError::Path(format!("path escapes the project: {raw}")));
            }
            // A prefix or root inside a path we already know is relative means the
            // input was not what it claimed to be (e.g. `C:foo` on Windows).
            Component::Prefix(_) | Component::RootDir => {
                return Err(GitError::Path(format!("path must be relative: {raw}")));
            }
        }
    }

    if parts.is_empty() {
        return Err(GitError::Path("path is required".into()));
    }
    Ok(parts.join("/"))
}

/// Validate a whole list of client paths at once, preserving order.
pub fn relative_paths(paths: &[String]) -> Result<Vec<String>, GitError> {
    paths.iter().map(|p| relative_path(p)).collect()
}

/// Absolute path for a repo-relative path, for the few callers that need one.
pub fn join_root(root: &Path, rel: &str) -> PathBuf {
    let mut out = root.to_path_buf();
    for part in rel.split('/') {
        out.push(part);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_plain_relative_path() {
        assert_eq!(relative_path("src/a.rs").unwrap(), "src/a.rs");
    }

    #[test]
    fn normalizes_redundant_current_dir() {
        assert_eq!(relative_path("./src/./a.rs").unwrap(), "src/a.rs");
    }

    #[test]
    fn rejects_traversal() {
        assert!(matches!(
            relative_path("../etc/passwd"),
            Err(GitError::Path(_))
        ));
        // Traversal in the middle counts too: `a/../../b` leaves the root.
        assert!(matches!(relative_path("a/../../b"), Err(GitError::Path(_))));
    }

    #[test]
    fn rejects_absolute() {
        assert!(matches!(
            relative_path("/etc/passwd"),
            Err(GitError::Path(_))
        ));
    }

    #[test]
    fn rejects_dot_git_at_any_depth() {
        // `Forbidden`, not `Path`: the string parses fine and points inside the
        // root, so this is a refusal (403) rather than a malformed request (400).
        assert!(matches!(
            relative_path(".git/config"),
            Err(GitError::Forbidden(_))
        ));
        assert!(matches!(
            relative_path("sub/.git/config"),
            Err(GitError::Forbidden(_))
        ));
        // Case-insensitively: macOS and Windows filesystems resolve `.GIT` to the
        // same directory, so a case-sensitive check would be bypassable there.
        assert!(matches!(
            relative_path(".GIT/config"),
            Err(GitError::Forbidden(_))
        ));
    }

    #[test]
    fn allows_names_that_merely_start_with_dot_git() {
        // `.gitignore` is a normal tracked file, not the repo directory.
        assert_eq!(relative_path(".gitignore").unwrap(), ".gitignore");
        assert_eq!(relative_path(".gitattributes").unwrap(), ".gitattributes");
    }

    #[test]
    fn rejects_empty_and_dot() {
        assert!(matches!(relative_path(""), Err(GitError::Path(_))));
        assert!(matches!(relative_path("   "), Err(GitError::Path(_))));
        assert!(matches!(relative_path("."), Err(GitError::Path(_))));
    }

    #[test]
    fn join_root_builds_the_absolute_path() {
        let joined = join_root(Path::new("/tmp/repo"), "src/a.rs");
        assert_eq!(joined, PathBuf::from("/tmp/repo/src/a.rs"));
    }
}
