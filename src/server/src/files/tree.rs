//! Directory listing — one level at a time, gitignore-aware.
//!
//! SPEC-002 §5.3 / [INVENTED-6]: the tree is lazy — each request lists exactly
//! one directory with `ignore::Walk` at `max_depth(1)`. `WalkParallel` is the
//! recommendation for the *search backend* (deep-dive 04:56-58); a depth-1
//! listing is a glorified `read_dir` and a thread pool for it is pure overhead.
//!
//! Filter configuration is deliberate, not `standard_filters(true)` — that
//! bundle turns on `hidden(true)` too (deep-dive 04:76-78) and would hide
//! `.env`/`.github` from the IDE ([INVENTED-10]). `.git` and `node_modules`
//! are excluded unconditionally (07:39), even when no `.gitignore` names them.

use std::path::Path;

use serde::Serialize;

/// Cap on entries returned for a single directory ([INVENTED-11]). Beyond
/// this the listing is cut and flagged `truncated` — honest, unlike a silent cut.
pub const TREE_ENTRY_CAP: usize = 5000;

/// Kind of a directory entry. Symlinks are reported, never followed
/// ([INVENTED-12]) — no infinite cycles, and escapes are caught by the guard
/// at read/write time instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Dir,
    File,
    Symlink,
}

/// One child of the listed directory.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeEntry {
    pub name: String,
    /// Path relative to the project root, `/`-separated — feeds straight back
    /// into `?path=` on subsequent requests.
    pub path: String,
    pub kind: EntryKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    pub mtime_ms: u64,
}

/// A listed directory.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirListing {
    pub path: String,
    pub truncated: bool,
    pub entries: Vec<TreeEntry>,
}

/// List the direct children of `abs` (which lives at `rel` under the project
/// root). Blocking — run inside `spawn_blocking`.
pub fn list_dir(abs: &Path, rel: &str) -> std::io::Result<DirListing> {
    let mut entries: Vec<TreeEntry> = Vec::new();
    let mut truncated = false;

    let walker = ignore::WalkBuilder::new(abs)
        .max_depth(Some(1))
        .hidden(false) // [INVENTED-10]: show .env, .github, .gitignore
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true)
        .parents(true) // a root .gitignore must apply when listing subdirs
        // `require_git` defaults to TRUE: without it, a project directory that
        // has a `.gitignore` but no `.git` gets NO ignore rules applied at all
        // ([INVENTED-6a]). A file tree is not `git status` — the user wrote
        // `.gitignore` to say "this is noise", and that intent holds whether or
        // not the directory has been `git init`ed yet. Cost of turning it off:
        // `.gitignore` files ABOVE the git root are also read, which git itself
        // would skip. Acceptable, and consistent with `git_global(true)` already
        // letting host-level ignores shape the tree.
        .require_git(false)
        .follow_links(false) // [INVENTED-12]
        .filter_entry(|e| !matches!(e.file_name().to_str(), Some(".git" | "node_modules")))
        .build();

    for entry in walker {
        let Ok(entry) = entry else { continue };
        // Depth 0 is the directory being listed itself.
        if entry.depth() == 0 {
            continue;
        }
        if entries.len() >= TREE_ENTRY_CAP {
            truncated = true;
            break;
        }

        let name = entry.file_name().to_string_lossy().into_owned();
        let child_rel = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };

        // file_type() is None only for stdin; treat unknown as file.
        let ft = entry.file_type();
        let kind = match ft {
            Some(t) if t.is_symlink() => EntryKind::Symlink,
            Some(t) if t.is_dir() => EntryKind::Dir,
            _ => EntryKind::File,
        };

        // Metadata straight from the entry; failures degrade to zeros rather
        // than dropping the entry (the name is still worth showing).
        let meta = entry.metadata().ok();
        let size = match kind {
            EntryKind::File => meta.as_ref().map(|m| m.len()),
            _ => None,
        };
        let mtime_ms = meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        entries.push(TreeEntry {
            name,
            path: child_rel,
            kind,
            size,
            mtime_ms,
        });
    }

    sort_entries(&mut entries);
    Ok(DirListing {
        path: rel.to_string(),
        truncated,
        entries,
    })
}

/// Dirs first, then files/symlinks; case-insensitive name order within each
/// group, with the raw name as tiebreaker so equal-ignoring-case names have a
/// stable order.
pub fn sort_entries(entries: &mut [TreeEntry]) {
    entries.sort_by(|a, b| {
        let a_dir = a.kind == EntryKind::Dir;
        let b_dir = b.kind == EntryKind::Dir;
        b_dir
            .cmp(&a_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: EntryKind) -> TreeEntry {
        TreeEntry {
            name: name.to_string(),
            path: name.to_string(),
            kind,
            size: None,
            mtime_ms: 0,
        }
    }

    #[test]
    fn dirs_sort_before_files_case_insensitively() {
        let mut entries = vec![
            entry("zeta.txt", EntryKind::File),
            entry("Alpha", EntryKind::Dir),
            entry("beta", EntryKind::Dir),
            entry("Beta.txt", EntryKind::File),
            entry("alpha.txt", EntryKind::File),
        ];
        sort_entries(&mut entries);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            ["Alpha", "beta", "alpha.txt", "Beta.txt", "zeta.txt"]
        );
    }

    #[test]
    fn same_name_different_case_orders_stably() {
        let mut entries = vec![
            entry("readme.md", EntryKind::File),
            entry("README.md", EntryKind::File),
        ];
        sort_entries(&mut entries);
        assert_eq!(entries[0].name, "README.md");
        assert_eq!(entries[1].name, "readme.md");
    }
}
