//! `git2` read layer (SPEC-005 §5.2).
//!
//! Every function here is **blocking** and must be called from
//! `spawn_blocking` — libgit2 does synchronous I/O, and blame/diff on a large
//! repository is slow enough to stall a tokio worker (02 §traps, §9 #2).
//!
//! Each function opens the repository itself and drops it before returning. That
//! is not laziness: `git2::Repository` is `!Send`, so it cannot be held across an
//! `.await`, and caching one in `AppState` would only move the problem behind a
//! mutex held for the whole request (deep-dive 03 §4 #1, §9 #1).
//!
//! Nothing in this file knows about HTTP.

use std::path::Path;

use git2::{
    BranchType, Delta, Diff, DiffFormat, DiffOptions, Oid, Repository, RepositoryState, Status,
    StatusOptions,
};
use serde::Serialize;

use super::GitError;
use crate::files::probe::{self, Classified};

/// Binary check, sharing the editor's content probe.
///
/// [SPEC-005 INVENTED-5]: a file must be binary in the git panel exactly where it
/// is binary in the editor, so this defers to `files::probe` rather than
/// re-deriving a heuristic.
fn looks_binary(bytes: &[u8]) -> bool {
    matches!(probe::classify(bytes), Classified::Binary)
}

/// Decode git metadata that is *usually* UTF-8 but not guaranteed to be.
///
/// git2 0.21 returns `Result<&str, Error>` from `shorthand`, `summary`, signature
/// `name`, and friends precisely because git stores bytes. Every such accessor has
/// a `_bytes` twin that cannot fail, and this module uses those: a commit whose
/// author name is Latin-1 should still appear in the log with one character
/// mangled, not vanish from it (§5.2, §9 #4).
fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Largest diff we will materialize ([SPEC-005 INVENTED-3]).
///
/// Above this the client's line diff (O(n·m)) is what hangs the tab, so the limit
/// belongs here rather than in the browser (§9 #13).
const MAX_DIFF_BYTES: usize = 5 * 1024 * 1024;

/// Default page size for the log.
pub const DEFAULT_LOG_LIMIT: usize = 50;
/// Upper bound on a client-requested page size.
const MAX_LOG_LIMIT: usize = 500;

// ---- wire types ------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatus {
    pub is_repo: bool,
    pub head: Option<HeadInfo>,
    pub upstream: Option<UpstreamInfo>,
    pub state: &'static str,
    pub entries: Vec<StatusEntryDto>,
    pub counts: StatusCounts,
}

impl GitStatus {
    /// The `{isRepo: false}` shape a non-repository project reports (C5).
    pub fn not_a_repo() -> Self {
        Self {
            is_repo: false,
            head: None,
            upstream: None,
            state: "clean",
            entries: Vec::new(),
            counts: StatusCounts::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadInfo {
    /// `None` on a detached HEAD, and also in a fresh repo with no commits yet.
    pub branch: Option<String>,
    pub detached: bool,
    /// `None` before the first commit — an unborn branch has no target.
    pub oid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpstreamInfo {
    pub name: String,
    pub ahead: usize,
    pub behind: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusEntryDto {
    pub path: String,
    /// Source path of a rename, when git detected one.
    pub orig_path: Option<String>,
    /// HEAD→index change: what a commit right now would record.
    pub index: &'static str,
    /// index→worktree change: what is not staged yet.
    pub worktree: &'static str,
    pub conflicted: bool,
    /// Convenience derived from `index != "none"`, so the UI does not repeat it.
    pub staged: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusCounts {
    pub staged: usize,
    pub changed: usize,
    pub untracked: usize,
    pub conflicted: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitDiff {
    pub path: String,
    pub staged: bool,
    pub binary: bool,
    pub patch: String,
    /// Full content of both sides, not just the patch: `unifiedMergeView` needs a
    /// whole document as `original` ([SPEC-005 INVENTED-4]).
    pub old_text: String,
    pub new_text: String,
    /// Empty content and a missing file are different Git states. Hunk actions need
    /// this distinction to unstage a newly-added file instead of staging an empty one.
    pub old_exists: bool,
    pub new_exists: bool,
    /// Blob id of the worktree document this diff was rendered from. A discard-hunk
    /// request sends it back so an external edit cannot be overwritten by stale UI.
    pub worktree_oid: Option<String>,
    pub added: usize,
    pub removed: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitDto {
    pub oid: String,
    pub short: String,
    pub summary: String,
    pub body: String,
    pub author: SignatureDto,
    pub parents: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignatureDto {
    pub name: String,
    pub email: String,
    /// Unix seconds. The client formats it; the server does not guess a timezone.
    pub time: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitLog {
    pub commits: Vec<CommitDto>,
    /// Cursor for the next page — oid of the first commit *not* returned
    /// ([SPEC-005 INVENTED-7]). `None` when the walk is exhausted.
    pub next_before: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitDetail {
    pub commit: CommitDto,
    pub files: Vec<CommitFileDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitFileDto {
    pub path: String,
    pub orig_path: Option<String>,
    pub change: &'static str,
    pub added: usize,
    pub removed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBranches {
    pub current: Option<String>,
    pub local: Vec<LocalBranchDto>,
    pub remote: Vec<RemoteBranchDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalBranchDto {
    pub name: String,
    pub oid: Option<String>,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteBranchDto {
    pub name: String,
    pub oid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBlame {
    pub path: String,
    pub lines: Vec<BlameLineDto>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlameLineDto {
    pub line: usize,
    pub oid: String,
    pub short: String,
    pub author: String,
    pub time: i64,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBlob {
    pub path: String,
    pub rev: String,
    pub binary: bool,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitConflict {
    pub path: String,
    /// `None` when that side has no version of the file — add/add has no base,
    /// delete/modify is missing one of ours/theirs.
    pub base: Option<String>,
    pub ours: Option<String>,
    pub theirs: Option<String>,
    pub binary: bool,
}

// ---- opening ---------------------------------------------------------------

/// Open the repository containing `root`.
///
/// `discover` rather than `open` so a project registered at a subdirectory of a
/// repo still gets its git panel (§2.1).
pub fn open(root: &Path) -> Result<Repository, GitError> {
    Repository::discover(root).map_err(|e| match e.code() {
        git2::ErrorCode::NotFound => GitError::NotARepo,
        _ => GitError::Libgit2(e),
    })
}

/// Whether `root` is inside a git repository, without paying for a full status.
pub fn is_repo(root: &Path) -> bool {
    Repository::discover(root).is_ok()
}

// ---- status ----------------------------------------------------------------

/// Map libgit2's status bitflags onto the two independent axes we report.
///
/// A file can be staged **and** modified again (`git status` calls it `MM`), so
/// index and worktree are separate fields rather than one merged state (§3.1, C3).
fn index_state(status: Status) -> &'static str {
    if status.contains(Status::INDEX_NEW) {
        "new"
    } else if status.contains(Status::INDEX_MODIFIED) {
        "modified"
    } else if status.contains(Status::INDEX_DELETED) {
        "deleted"
    } else if status.contains(Status::INDEX_RENAMED) {
        "renamed"
    } else if status.contains(Status::INDEX_TYPECHANGE) {
        "typechange"
    } else {
        "none"
    }
}

fn worktree_state(status: Status) -> &'static str {
    if status.contains(Status::WT_NEW) {
        "new"
    } else if status.contains(Status::WT_MODIFIED) {
        "modified"
    } else if status.contains(Status::WT_DELETED) {
        "deleted"
    } else if status.contains(Status::WT_RENAMED) {
        "renamed"
    } else if status.contains(Status::WT_TYPECHANGE) {
        "typechange"
    } else {
        "none"
    }
}

fn state_name(state: RepositoryState) -> &'static str {
    match state {
        RepositoryState::Clean => "clean",
        RepositoryState::Merge => "merge",
        RepositoryState::Revert | RepositoryState::RevertSequence => "revert",
        RepositoryState::CherryPick | RepositoryState::CherryPickSequence => "cherryPick",
        RepositoryState::Bisect => "bisect",
        RepositoryState::Rebase
        | RepositoryState::RebaseInteractive
        | RepositoryState::RebaseMerge => "rebase",
        RepositoryState::ApplyMailbox | RepositoryState::ApplyMailboxOrRebase => "apply",
    }
}

/// Full working-tree status (C1–C7).
pub fn status(root: &Path) -> Result<GitStatus, GitError> {
    let repo = open(root)?;
    let project_prefix = project_prefix(&repo, root)?;
    status_of_project(&repo, project_prefix.as_deref())
}

/// Status of an already-open repository rooted at its whole worktree.
pub fn status_of(repo: &Repository) -> Result<GitStatus, GitError> {
    status_of_project(repo, None)
}

fn status_of_project(
    repo: &Repository,
    project_prefix: Option<&Path>,
) -> Result<GitStatus, GitError> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        // Without this, a new directory shows up as one entry ("sub/") instead of
        // the files inside it, and nothing can be staged individually.
        .recurse_untracked_dirs(true)
        // `.gitignore`-d files stay off the UI (§1, C4).
        .include_ignored(false)
        .include_unmodified(false)
        .exclude_submodules(false)
        .renames_head_to_index(true)
        .renames_index_to_workdir(true)
        // Refresh the stat cache in memory, but do NOT write it back: a `GET`
        // must not touch `.git/index`, or it races with the user's own `git`
        // running in a terminal (§5.2, §9 #3, C40).
        .no_refresh(false)
        .update_index(false);

    let statuses = repo.statuses(Some(&mut opts))?;

    let mut entries = Vec::with_capacity(statuses.len());
    let mut counts = StatusCounts::default();

    for entry in statuses.iter() {
        let status = entry.status();

        // Defensive: `include_ignored(false)` should already exclude these, but a
        // file can be both ignored and otherwise-changed in edge cases.
        if status.contains(Status::IGNORED) && status.bits() == Status::IGNORED.bits() {
            continue;
        }

        // `path()` returns Err for non-UTF-8 paths and would drop the entry
        // entirely; lossy renders one byte wrong, which beats hiding a file
        // (§5.2, §9 #4). `git2` always reports paths relative to the repository
        // workdir; strip the registered project's prefix so paths stay inside the
        // project's API namespace.
        let repo_path = String::from_utf8_lossy(entry.path_bytes());
        let Some(project_path) = path_in_project(Path::new(repo_path.as_ref()), project_prefix)
        else {
            continue;
        };
        let path = project_path.to_string_lossy().into_owned();

        // A rename's source lives on the delta's old side. Prefer the
        // index delta (a staged rename) and fall back to the worktree one.
        let orig_path = entry
            .head_to_index()
            .or_else(|| entry.index_to_workdir())
            .and_then(|delta| {
                let old = path_in_project(delta.old_file().path()?, project_prefix)?
                    .to_string_lossy()
                    .into_owned();
                (old != path).then_some(old)
            });

        let conflicted = status.contains(Status::CONFLICTED);
        let index = index_state(status);
        let worktree = worktree_state(status);

        if conflicted {
            counts.conflicted += 1;
        } else {
            // A `MM` file counts in both buckets, because it genuinely is in both
            // (C42) — the UI shows it twice on purpose.
            if index != "none" {
                counts.staged += 1;
            }
            if worktree == "new" {
                counts.untracked += 1;
            } else if worktree != "none" {
                counts.changed += 1;
            }
        }

        entries.push(StatusEntryDto {
            path,
            orig_path,
            index,
            worktree,
            conflicted,
            staged: index != "none",
        });
    }

    entries.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(GitStatus {
        is_repo: true,
        head: Some(head_info(repo)?),
        upstream: upstream_info(repo)?,
        state: state_name(repo.state()),
        entries,
        counts,
    })
}

fn head_info(repo: &Repository) -> Result<HeadInfo, GitError> {
    match repo.head() {
        Ok(head) => {
            let detached = repo.head_detached().unwrap_or(false);
            let branch = if detached {
                None
            } else {
                Some(lossy(head.shorthand_bytes()))
            };
            Ok(HeadInfo {
                branch,
                detached,
                oid: head.target().map(|o| o.to_string()),
            })
        }
        // A fresh `git init` has a HEAD that points at an unborn branch; libgit2
        // reports UnbornBranch. That is a valid repository with no commits, so
        // report the branch name and a null oid rather than failing (C1 runs
        // against a repo with commits, but the panel opens before the first one).
        Err(e)
            if e.code() == git2::ErrorCode::UnbornBranch
                || e.code() == git2::ErrorCode::NotFound =>
        {
            let branch = repo
                .find_reference("HEAD")
                .ok()
                .and_then(|r| r.symbolic_target_bytes().map(lossy))
                .and_then(|full| full.strip_prefix("refs/heads/").map(str::to_string));
            Ok(HeadInfo {
                branch,
                detached: false,
                oid: None,
            })
        }
        Err(e) => Err(GitError::Libgit2(e)),
    }
}

/// Registered-project prefix relative to the repository workdir.
///
/// Empty means the project is the workdir itself. A project can be a subdirectory
/// because repository discovery intentionally walks upward (§2.1).
fn project_prefix(repo: &Repository, root: &Path) -> Result<Option<std::path::PathBuf>, GitError> {
    let workdir = repo
        .workdir()
        .ok_or_else(|| GitError::Path("bare repositories have no project worktree".into()))?;
    let root = root.canonicalize()?;
    let workdir = workdir.canonicalize()?;
    let prefix = root
        .strip_prefix(&workdir)
        .map_err(|_| GitError::Path("project root is outside repository worktree".into()))?;
    Ok((!prefix.as_os_str().is_empty()).then(|| prefix.to_path_buf()))
}

fn repo_relative(prefix: Option<&Path>, rel: &str) -> std::path::PathBuf {
    match prefix {
        Some(prefix) => prefix.join(rel),
        None => std::path::PathBuf::from(rel),
    }
}

fn path_in_project<'a>(repo_path: &'a Path, prefix: Option<&Path>) -> Option<&'a Path> {
    match prefix {
        Some(prefix) => repo_path.strip_prefix(prefix).ok(),
        None => Some(repo_path),
    }
}

fn upstream_info(repo: &Repository) -> Result<Option<UpstreamInfo>, GitError> {
    if repo.head_detached().unwrap_or(false) {
        return Ok(None);
    }
    let Ok(head) = repo.head() else {
        return Ok(None);
    };
    // `find_branch` needs a real `&str`, so this one lookup cannot be lossy: a
    // branch name that is not UTF-8 simply has no upstream we can resolve.
    let Ok(shorthand) = head.shorthand() else {
        return Ok(None);
    };
    let Ok(branch) = repo.find_branch(shorthand, BranchType::Local) else {
        return Ok(None);
    };
    let Ok(upstream) = branch.upstream() else {
        // No tracking branch configured — normal, not an error.
        return Ok(None);
    };

    let name = upstream.name_bytes().map(lossy).unwrap_or_default();

    let (Some(local), Some(remote)) = (head.target(), upstream.get().target()) else {
        return Ok(None);
    };
    let (ahead, behind) = repo.graph_ahead_behind(local, remote)?;
    Ok(Some(UpstreamInfo {
        name,
        ahead,
        behind,
    }))
}

// ---- diff ------------------------------------------------------------------

/// Diff one path, either index-vs-HEAD (`staged`) or worktree-vs-index (C8–C10).
pub fn diff(root: &Path, rel: &str, staged: bool) -> Result<GitDiff, GitError> {
    let repo = open(root)?;
    let project_prefix = project_prefix(&repo, root)?;
    let repo_rel = repo_relative(project_prefix.as_deref(), rel);

    let mut opts = DiffOptions::new();
    opts.pathspec(&repo_rel)
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        // Untracked content is what makes a new file's diff show its lines instead
        // of just a header.
        .show_untracked_content(true)
        .context_lines(3);

    let diff = if staged {
        let tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
        repo.diff_tree_to_index(tree.as_ref(), None, Some(&mut opts))?
    } else {
        repo.diff_index_to_workdir(None, Some(&mut opts))?
    };

    let (old_rev, new_rev) = if staged {
        (BlobRev::Head, BlobRev::Index)
    } else {
        (BlobRev::Index, BlobRev::Worktree)
    };
    let old_bytes = read_blob_bytes(&repo, &repo_rel, old_rev);
    let new_bytes = read_blob_bytes(&repo, &repo_rel, new_rev);
    let old_exists = old_bytes.is_some();
    let new_exists = new_bytes.is_some();
    let worktree_oid = if staged {
        None
    } else {
        new_bytes.as_deref().map(blob_oid).transpose()?
    };

    // Binary is decided from the bytes we are about to ship, not from
    // `DiffFile::is_binary`: libgit2 only sets that flag while it loads a blob, so
    // before `print` runs it is false for everything, and a diff we return early
    // would have claimed a PNG was text (C10). Deciding here also keeps the answer
    // identical to the editor's, which classifies the same bytes ([INVENTED-5]).
    let binary = [&old_bytes, &new_bytes]
        .iter()
        .filter_map(|side| side.as_ref())
        .any(|bytes| looks_binary(bytes));

    let (patch, added, removed) = render_patch(&diff)?;

    if binary {
        // No content for a binary file: the client cannot render it and shipping
        // megabytes of bytes as lossy UTF-8 helps nobody (C10).
        return Ok(GitDiff {
            path: rel.to_string(),
            staged,
            binary: true,
            patch,
            old_text: String::new(),
            new_text: String::new(),
            old_exists,
            new_exists,
            worktree_oid,
            added,
            removed,
            truncated: false,
        });
    }

    let decode = |bytes: Option<Vec<u8>>| {
        bytes
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    };
    let old_text = decode(old_bytes);
    let new_text = decode(new_bytes);

    // [SPEC-005 INVENTED-3]: over the cap, keep the patch header but drop the
    // documents, so the client renders a notice instead of hanging on an O(n·m)
    // diff (§9 #13).
    let truncated = old_text.len() > MAX_DIFF_BYTES || new_text.len() > MAX_DIFF_BYTES;
    if truncated {
        return Ok(GitDiff {
            path: rel.to_string(),
            staged,
            binary: false,
            patch: String::new(),
            old_text: String::new(),
            new_text: String::new(),
            old_exists,
            new_exists,
            worktree_oid,
            added,
            removed,
            truncated: true,
        });
    }

    Ok(GitDiff {
        path: rel.to_string(),
        staged,
        binary: false,
        patch,
        old_text,
        new_text,
        old_exists,
        new_exists,
        worktree_oid,
        added,
        removed,
        truncated: false,
    })
}

/// Render a diff as unified patch text plus line counts.
fn render_patch(diff: &Diff<'_>) -> Result<(String, usize, usize), GitError> {
    let mut patch = String::new();
    let mut added = 0usize;
    let mut removed = 0usize;

    diff.print(DiffFormat::Patch, |_delta, _hunk, line| {
        match line.origin() {
            '+' => added += 1,
            '-' => removed += 1,
            _ => {}
        }
        // libgit2 hands us the origin character separately from the content for
        // content lines, but headers already carry their own prefix.
        if matches!(line.origin(), '+' | '-' | ' ') {
            patch.push(line.origin());
        }
        patch.push_str(&String::from_utf8_lossy(line.content()));
        true
    })?;

    Ok((patch, added, removed))
}

// ---- blob ------------------------------------------------------------------

/// Which version of a file to read.
#[derive(Debug, Clone, Copy)]
pub enum BlobRev {
    Head,
    Index,
    Worktree,
    Commit(Oid),
}

impl BlobRev {
    /// Parse the `rev=` query value (C15).
    pub fn parse(rev: &str) -> Result<Self, GitError> {
        match rev {
            "" | "HEAD" | "head" => Ok(BlobRev::Head),
            "index" | "INDEX" => Ok(BlobRev::Index),
            "worktree" | "WORKTREE" => Ok(BlobRev::Worktree),
            other => Oid::from_str(other)
                .map(BlobRev::Commit)
                .map_err(|_| GitError::NotFound(format!("unknown rev: {other}"))),
        }
    }

    pub fn label(&self) -> String {
        match self {
            BlobRev::Head => "HEAD".into(),
            BlobRev::Index => "index".into(),
            BlobRev::Worktree => "worktree".into(),
            BlobRev::Commit(oid) => oid.to_string(),
        }
    }
}

/// Read one version of a file as a String, or `None` if that version has no such
/// path (a file added on only one side, for instance).
fn read_blob(repo: &Repository, rel: &Path, rev: BlobRev) -> Option<String> {
    let bytes = read_blob_bytes(repo, rel, rev)?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Git blob id without writing the object database. Keeping this helper in the
/// libgit2 read module preserves the read-with-library/write-with-CLI boundary.
pub fn blob_oid(bytes: &[u8]) -> Result<String, GitError> {
    git2::Oid::hash_object(git2::ObjectType::Blob, bytes)
        .map(|oid| oid.to_string())
        .map_err(GitError::Libgit2)
}

fn read_blob_bytes(repo: &Repository, rel: &Path, rev: BlobRev) -> Option<Vec<u8>> {
    match rev {
        BlobRev::Worktree => {
            // `rel` is repository-relative, so the workdir is always its base even
            // when the registered project is a nested directory.
            std::fs::read(repo.workdir()?.join(rel)).ok()
        }
        BlobRev::Index => {
            let index = repo.index().ok()?;
            // Stage 0 is the resolved entry; during a conflict there is none, and
            // the caller wants `conflict()` instead.
            let entry = index.get_path(rel, 0)?;
            let blob = repo.find_blob(entry.id).ok()?;
            Some(blob.content().to_vec())
        }
        BlobRev::Head => {
            let tree = repo.head().ok()?.peel_to_tree().ok()?;
            blob_from_tree(repo, &tree, rel)
        }
        BlobRev::Commit(oid) => {
            let tree = repo.find_commit(oid).ok()?.tree().ok()?;
            blob_from_tree(repo, &tree, rel)
        }
    }
}

fn blob_from_tree(repo: &Repository, tree: &git2::Tree<'_>, rel: &Path) -> Option<Vec<u8>> {
    let entry = tree.get_path(rel).ok()?;
    let blob = repo.find_blob(entry.id()).ok()?;
    Some(blob.content().to_vec())
}

/// `GET …/git/blob` — one version of a file (C15).
pub fn blob(root: &Path, rel: &str, rev: BlobRev) -> Result<GitBlob, GitError> {
    let repo = open(root)?;
    let project_prefix = project_prefix(&repo, root)?;
    let repo_rel = repo_relative(project_prefix.as_deref(), rel);
    let bytes = read_blob_bytes(&repo, &repo_rel, rev)
        .ok_or_else(|| GitError::NotFound(format!("{rel} does not exist at {}", rev.label())))?;

    let binary = looks_binary(&bytes);
    Ok(GitBlob {
        path: rel.to_string(),
        rev: rev.label(),
        binary,
        content: if binary {
            String::new()
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        },
    })
}

// ---- log -------------------------------------------------------------------

fn commit_dto(commit: &git2::Commit<'_>) -> CommitDto {
    let author = commit.author();
    // `summary_bytes`/`body_bytes` split the message the way git does, honouring
    // the blank line *and* collapsing a multi-line subject — reimplementing that
    // with `split_once("\n\n")` gets wrapped subjects wrong.
    let summary = commit.summary_bytes().map(lossy).unwrap_or_default();
    let body = commit
        .body_bytes()
        .map(lossy)
        .map(|b| b.trim_end().to_string())
        .unwrap_or_default();

    let oid = commit.id();
    CommitDto {
        oid: oid.to_string(),
        short: short_oid(&oid),
        summary,
        body,
        author: SignatureDto {
            name: lossy(author.name_bytes()),
            email: lossy(author.email_bytes()),
            time: author.when().seconds(),
        },
        parents: commit.parent_ids().map(|id| id.to_string()).collect(),
    }
}

/// Seven hex chars, the width `git log --oneline` uses by default.
fn short_oid(oid: &Oid) -> String {
    oid.to_string().chars().take(7).collect()
}

/// Commit history, newest first, paginated by cursor ([SPEC-005 INVENTED-7], C11).
pub fn log(
    root: &Path,
    limit: usize,
    before: Option<&str>,
    path: Option<&str>,
) -> Result<GitLog, GitError> {
    let repo = open(root)?;
    let project_prefix = project_prefix(&repo, root)?;
    let path = path.map(|rel| repo_relative(project_prefix.as_deref(), rel));
    let limit = limit.clamp(1, MAX_LOG_LIMIT);

    let mut walk = repo.revwalk()?;
    // `TIME` alone is a time-*priority* queue, not an order: when several commits
    // share a timestamp — which is every commit made within one second, so most of
    // a scripted test and plenty of real rebases — it can emit a parent before its
    // own child. `TOPOLOGICAL` guarantees children first and `TIME` breaks the ties,
    // which together are `git log --date-order`. Without this the cursor in
    // [SPEC-005 INVENTED-7] pages over a non-linear order and repeats commits.
    walk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME)?;

    match before {
        Some(cursor) => {
            let oid = Oid::from_str(cursor)
                .map_err(|_| GitError::NotFound(format!("bad cursor: {cursor}")))?;
            walk.push(oid)?;
        }
        None => {
            if repo.head().is_err() {
                // Unborn branch: no commits yet, and `push_head` would error.
                return Ok(GitLog {
                    commits: Vec::new(),
                    next_before: None,
                });
            }
            walk.push_head()?;
        }
    }

    let mut commits = Vec::with_capacity(limit);
    let mut next_before = None;

    for oid in walk {
        let oid = oid?;
        let commit = repo.find_commit(oid)?;

        if let Some(filter) = path.as_deref()
            && !commit_touches(&repo, &commit, filter)?
        {
            continue;
        }

        if commits.len() == limit {
            // One past the page: this is the cursor, not a returned commit.
            next_before = Some(oid.to_string());
            break;
        }
        commits.push(commit_dto(&commit));
    }

    Ok(GitLog {
        commits,
        next_before,
    })
}

/// Does this commit change `path` relative to its first parent?
fn commit_touches(
    repo: &Repository,
    commit: &git2::Commit<'_>,
    path: &Path,
) -> Result<bool, GitError> {
    let new_tree = commit.tree()?;
    let old_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
    let mut opts = DiffOptions::new();
    opts.pathspec(path);
    let diff = repo.diff_tree_to_tree(old_tree.as_ref(), Some(&new_tree), Some(&mut opts))?;
    // `next().is_some()` rather than a length: we only need "did anything change",
    // and the walk stops at the first delta.
    Ok(diff.deltas().next().is_some())
}

fn delta_name(delta: Delta) -> &'static str {
    match delta {
        Delta::Added => "added",
        Delta::Deleted => "deleted",
        Delta::Modified => "modified",
        Delta::Renamed => "renamed",
        Delta::Copied => "copied",
        Delta::Typechange => "typechange",
        Delta::Ignored => "ignored",
        Delta::Untracked => "untracked",
        Delta::Conflicted => "conflicted",
        Delta::Unmodified | Delta::Unreadable => "unmodified",
    }
}

/// One commit with its per-file line counts (C12).
pub fn commit_detail(root: &Path, oid: &str) -> Result<CommitDetail, GitError> {
    let repo = open(root)?;
    let project_prefix = project_prefix(&repo, root)?;
    let oid = Oid::from_str(oid).map_err(|_| GitError::NotFound(format!("bad oid: {oid}")))?;
    let commit = repo
        .find_commit(oid)
        .map_err(|_| GitError::NotFound(format!("no commit {oid}")))?;

    let new_tree = commit.tree()?;
    let old_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
    let mut opts = DiffOptions::new();
    // When the project is a subdirectory, only show changes within that scope.
    if let Some(prefix) = project_prefix.as_deref() {
        opts.pathspec(prefix);
    }
    let diff = repo.diff_tree_to_tree(old_tree.as_ref(), Some(&new_tree), Some(&mut opts))?;

    // Per-file counts: `print` walks lines grouped by delta, so track the current
    // path as we go rather than diffing each file separately.
    //
    // repo_path → project_path: strip the project prefix so that subdirectory
    // projects see "src/foo.rs" rather than "packages/my-crate/src/foo.rs" (C∞).
    let strip = |repo_path: &std::path::Path| -> String {
        path_in_project(repo_path, project_prefix.as_deref())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| repo_path.to_string_lossy().into_owned())
    };
    let mut files: Vec<CommitFileDto> = Vec::new();
    for delta in diff.deltas() {
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(strip)
            .unwrap_or_default();
        let orig_path = delta
            .old_file()
            .path()
            .map(strip)
            .filter(|old| *old != path);
        files.push(CommitFileDto {
            path,
            orig_path,
            change: delta_name(delta.status()),
            added: 0,
            removed: 0,
        });
    }

    let mut index = 0usize;
    let mut last_path = String::new();
    diff.print(DiffFormat::Patch, |delta, _hunk, line| {
        let path = delta
            .new_file()
            .path()
            .or_else(|| delta.old_file().path())
            .map(strip)
            .unwrap_or_default();
        if path != last_path {
            if let Some(found) = files.iter().position(|f| f.path == path) {
                index = found;
            }
            last_path = path;
        }
        if let Some(file) = files.get_mut(index) {
            match line.origin() {
                '+' => file.added += 1,
                '-' => file.removed += 1,
                _ => {}
            }
        }
        true
    })?;

    Ok(CommitDetail {
        commit: commit_dto(&commit),
        files,
    })
}

// ---- branches --------------------------------------------------------------

/// Local + remote branches with tracking info (C13).
pub fn branches(root: &Path) -> Result<GitBranches, GitError> {
    let repo = open(root)?;
    let current = repo
        .head()
        .ok()
        .filter(|_| !repo.head_detached().unwrap_or(false))
        .map(|h| lossy(h.shorthand_bytes()));

    let mut local = Vec::new();
    let mut remote = Vec::new();

    for item in repo.branches(None)? {
        let (branch, kind) = item?;
        let name = lossy(branch.name_bytes()?);
        let oid = branch.get().target();

        match kind {
            BranchType::Local => {
                let upstream = branch.upstream().ok();
                let upstream_name = upstream
                    .as_ref()
                    .and_then(|u| u.name_bytes().ok().map(lossy));
                let (ahead, behind) = match (oid, upstream.as_ref().and_then(|u| u.get().target()))
                {
                    (Some(local_oid), Some(remote_oid)) => repo
                        .graph_ahead_behind(local_oid, remote_oid)
                        .unwrap_or((0, 0)),
                    _ => (0, 0),
                };
                local.push(LocalBranchDto {
                    current: Some(&name) == current.as_ref(),
                    name,
                    oid: oid.map(|o| o.to_string()),
                    upstream: upstream_name,
                    ahead,
                    behind,
                });
            }
            BranchType::Remote => remote.push(RemoteBranchDto {
                name,
                oid: oid.map(|o| o.to_string()),
            }),
        }
    }

    local.sort_by(|a, b| a.name.cmp(&b.name));
    remote.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(GitBranches {
        current,
        local,
        remote,
    })
}

// ---- blame -----------------------------------------------------------------

/// Per-line blame (C14).
///
/// The slowest read in this module — deep-dive 03 and 02 §traps both single it
/// out. It is only ever reached through `spawn_blocking`.
pub fn blame(root: &Path, rel: &str) -> Result<GitBlame, GitError> {
    let repo = open(root)?;
    let project_prefix = project_prefix(&repo, root)?;
    let repo_rel = repo_relative(project_prefix.as_deref(), rel);
    let mut opts = git2::BlameOptions::new();
    opts.track_copies_same_file(true);

    let blame = repo
        .blame_file(&repo_rel, Some(&mut opts))
        .map_err(|e| match e.code() {
            git2::ErrorCode::NotFound => GitError::NotFound(format!("{rel} is not tracked")),
            _ => GitError::Libgit2(e),
        })?;

    // Line count comes from the file itself: blame hunks cover only committed
    // lines, and a locally-added line at the end has no hunk.
    let content = read_blob(&repo, &repo_rel, BlobRev::Worktree)
        .or_else(|| read_blob(&repo, &repo_rel, BlobRev::Head))
        .unwrap_or_default();
    let total = content.lines().count();

    let mut lines = Vec::with_capacity(total);
    // Summaries are looked up once per commit, not once per line: blaming a 5k
    // line file otherwise does 5k object reads for a handful of commits.
    let mut summaries: std::collections::HashMap<Oid, (String, i64, String)> =
        std::collections::HashMap::new();

    for lineno in 1..=total {
        let Some(hunk) = blame.get_line(lineno) else {
            continue;
        };
        let oid = hunk.final_commit_id();
        let (author, time, summary) = summaries
            .entry(oid)
            .or_insert_with(|| match repo.find_commit(oid) {
                Ok(commit) => (
                    lossy(commit.author().name_bytes()),
                    commit.author().when().seconds(),
                    commit.summary_bytes().map(lossy).unwrap_or_default(),
                ),
                // An uncommitted line blames to the zero oid, which has no commit.
                Err(_) => (
                    hunk.final_signature()
                        .map(|s| lossy(s.name_bytes()))
                        .unwrap_or_default(),
                    0,
                    String::new(),
                ),
            })
            .clone();

        lines.push(BlameLineDto {
            line: lineno,
            oid: oid.to_string(),
            short: short_oid(&oid),
            author,
            time,
            summary,
        });
    }

    Ok(GitBlame {
        path: rel.to_string(),
        lines,
    })
}

// ---- conflict --------------------------------------------------------------

/// The three sides of a conflicted file, for the 3-way merge editor (C30).
///
/// Read from the index's conflict stages (`git2::Index::conflicts`,
/// `src/index.rs:426`) rather than by parsing `<<<<<<<` markers out of the
/// worktree file: the markers are a rendering of this data, and parsing them back
/// loses whichever side git could not represent.
pub fn conflict(root: &Path, rel: &str) -> Result<GitConflict, GitError> {
    let repo = open(root)?;
    let project_prefix = project_prefix(&repo, root)?;
    let repo_rel = repo_relative(project_prefix.as_deref(), rel);
    let index = repo.index()?;

    let mut found = None;
    for item in index.conflicts()? {
        let item = item?;
        let path_of = |entry: &Option<git2::IndexEntry>| {
            entry
                .as_ref()
                .map(|e| String::from_utf8_lossy(&e.path).into_owned())
        };
        let candidate = path_of(&item.our)
            .or_else(|| path_of(&item.their))
            .or_else(|| path_of(&item.ancestor));
        if candidate.as_deref().map(Path::new) == Some(repo_rel.as_path()) {
            found = Some(item);
            break;
        }
    }

    let Some(item) = found else {
        return Err(GitError::NotFound(format!("{rel} is not conflicted")));
    };

    let side = |entry: Option<git2::IndexEntry>| -> Option<Vec<u8>> {
        let entry = entry?;
        let blob = repo.find_blob(entry.id).ok()?;
        Some(blob.content().to_vec())
    };
    let base = side(item.ancestor);
    let ours = side(item.our);
    let theirs = side(item.their);

    let binary = [&base, &ours, &theirs]
        .iter()
        .filter_map(|side| side.as_ref())
        .any(|bytes| looks_binary(bytes));

    let text = |bytes: Option<Vec<u8>>| -> Option<String> {
        bytes.map(|b| {
            if binary {
                String::new()
            } else {
                String::from_utf8_lossy(&b).into_owned()
            }
        })
    };

    Ok(GitConflict {
        path: rel.to_string(),
        base: text(base),
        ours: text(ours),
        theirs: text(theirs),
        binary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_and_worktree_states_are_independent() {
        // The `MM` case: staged, then modified again. Both axes must report, or
        // the UI cannot show the file in two groups (C3, C42).
        let both = Status::INDEX_MODIFIED | Status::WT_MODIFIED;
        assert_eq!(index_state(both), "modified");
        assert_eq!(worktree_state(both), "modified");

        let untracked = Status::WT_NEW;
        assert_eq!(index_state(untracked), "none");
        assert_eq!(worktree_state(untracked), "new");
    }

    #[test]
    fn every_status_flag_maps_to_a_name() {
        for (flag, expected) in [
            (Status::INDEX_NEW, "new"),
            (Status::INDEX_MODIFIED, "modified"),
            (Status::INDEX_DELETED, "deleted"),
            (Status::INDEX_RENAMED, "renamed"),
            (Status::INDEX_TYPECHANGE, "typechange"),
        ] {
            assert_eq!(index_state(flag), expected, "index flag {flag:?}");
        }
        for (flag, expected) in [
            (Status::WT_NEW, "new"),
            (Status::WT_MODIFIED, "modified"),
            (Status::WT_DELETED, "deleted"),
            (Status::WT_RENAMED, "renamed"),
            (Status::WT_TYPECHANGE, "typechange"),
        ] {
            assert_eq!(worktree_state(flag), expected, "worktree flag {flag:?}");
        }
        assert_eq!(index_state(Status::CURRENT), "none");
        assert_eq!(worktree_state(Status::CURRENT), "none");
    }

    #[test]
    fn repository_states_collapse_to_stable_names() {
        assert_eq!(state_name(RepositoryState::Clean), "clean");
        assert_eq!(state_name(RepositoryState::Merge), "merge");
        // Sequences collapse onto the same name as their single-commit form: the
        // UI shows "what is in progress", not how many commits are left.
        assert_eq!(state_name(RepositoryState::RevertSequence), "revert");
        assert_eq!(
            state_name(RepositoryState::CherryPickSequence),
            "cherryPick"
        );
        assert_eq!(state_name(RepositoryState::RebaseInteractive), "rebase");
        assert_eq!(state_name(RepositoryState::RebaseMerge), "rebase");
        assert_eq!(state_name(RepositoryState::ApplyMailbox), "apply");
    }

    #[test]
    fn blob_rev_parses_the_three_named_revisions() {
        assert!(matches!(BlobRev::parse("").unwrap(), BlobRev::Head));
        assert!(matches!(BlobRev::parse("HEAD").unwrap(), BlobRev::Head));
        assert!(matches!(BlobRev::parse("index").unwrap(), BlobRev::Index));
        assert!(matches!(
            BlobRev::parse("worktree").unwrap(),
            BlobRev::Worktree
        ));
    }

    #[test]
    fn blob_rev_parses_an_oid_and_rejects_junk() {
        let oid = "0123456789abcdef0123456789abcdef01234567";
        match BlobRev::parse(oid).unwrap() {
            BlobRev::Commit(parsed) => assert_eq!(parsed.to_string(), oid),
            other => panic!("expected Commit, got {other:?}"),
        }
        assert!(matches!(
            BlobRev::parse("not-a-rev"),
            Err(GitError::NotFound(_))
        ));
    }

    #[test]
    fn blob_rev_labels_round_trip_for_the_wire() {
        assert_eq!(BlobRev::Head.label(), "HEAD");
        assert_eq!(BlobRev::Index.label(), "index");
        assert_eq!(BlobRev::Worktree.label(), "worktree");
    }

    #[test]
    fn not_a_repo_status_is_a_valid_answer_not_an_error() {
        let status = GitStatus::not_a_repo();
        assert!(!status.is_repo);
        assert!(status.entries.is_empty());
        assert_eq!(status.counts, StatusCounts::default());
        assert_eq!(status.state, "clean");
    }

    #[test]
    fn short_oid_is_seven_chars() {
        let oid = Oid::from_str("0123456789abcdef0123456789abcdef01234567").unwrap();
        assert_eq!(short_oid(&oid), "0123456");
    }

    #[test]
    fn the_log_walk_keeps_children_before_parents_at_equal_timestamps() {
        // The bug this pins: `Sort::TIME` alone is a priority queue with no
        // tiebreak, so six commits made inside one second came back as
        // `5, init, 1, 2, 3, 4` — a parent ahead of its children — and the cursor
        // then paged over that order and repeated commits.
        let dir = std::env::temp_dir().join(format!("spec-ade-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(&dir)
                .args(args)
                .env("LC_ALL", "C")
                .output()
                .expect("git on PATH");
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        run(&["init", "--initial-branch=main"]);
        run(&["config", "user.email", "t@spec-ade.invalid"]);
        run(&["config", "user.name", "T"]);
        run(&["config", "commit.gpgsign", "false"]);
        for n in 0..6 {
            std::fs::write(dir.join("a.txt"), format!("{n}\n")).unwrap();
            run(&["add", "--all"]);
            // Same second for all six, which is exactly the failing condition.
            run(&["commit", "-m", &format!("c{n}")]);
        }

        let history = log(&dir, 10, None, None).unwrap();
        let summaries: Vec<&str> = history.commits.iter().map(|c| c.summary.as_str()).collect();
        assert_eq!(summaries, ["c5", "c4", "c3", "c2", "c1", "c0"]);

        // And the cursor is the commit *after* the page, so paging cannot overlap.
        let page = log(&dir, 2, None, None).unwrap();
        let cursor = page.next_before.expect("more history");
        let next = log(&dir, 2, Some(&cursor), None).unwrap();
        assert_eq!(next.commits[0].oid, cursor);
        let first: Vec<&String> = page.commits.iter().map(|c| &c.oid).collect();
        for c in &next.commits {
            assert!(!first.contains(&&c.oid), "{} repeated", c.oid);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delta_names_cover_the_variants_we_show() {
        assert_eq!(delta_name(Delta::Added), "added");
        assert_eq!(delta_name(Delta::Renamed), "renamed");
        assert_eq!(delta_name(Delta::Conflicted), "conflicted");
        assert_eq!(delta_name(Delta::Unmodified), "unmodified");
    }
}
