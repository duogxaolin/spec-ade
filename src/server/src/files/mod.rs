//! Files — high-level project filesystem operations.
//!
//! SPEC-002 §5.1. Everything here is synchronous/blocking by design; the routes
//! layer wraps calls in `tokio::task::spawn_blocking` (02:49-55, 07:148-149).
//!
//! Concurrency model for writes ([INVENTED-9]): `rev = "{mtimeMs}-{size}"` is
//! an optimistic-concurrency tag. Auto-save sends the `rev` it loaded; if the
//! file changed on disk since (external editor, git checkout, an ACP agent),
//! the write is refused with `Conflict` instead of silently clobbering.

pub mod path_guard;
pub mod probe;
pub mod tree;

use std::path::Path;

use serde::Serialize;

pub use path_guard::{PathError, resolve, resolve_non_root};
pub use tree::{DirListing, TREE_ENTRY_CAP, list_dir};

/// Files larger than this are reported as `tooLarge` instead of loaded into
/// the editor ([INVENTED-7]).
pub const FILE_TEXT_MAX: u64 = 4 * 1024 * 1024;

/// Errors from file operations. The routes layer maps these to HTTP.
#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error(transparent)]
    Path(#[from] PathError),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("not a directory: {0}")]
    NotADirectory(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Optimistic-concurrency tag for a file: mtime in ms + size.
///
/// Size participates so an edit that lands within the same millisecond (same
/// mtime) but changes length still misses — the cheapest tag that catches the
/// realistic external-edit cases without server-side state.
pub fn rev_of(meta: &std::fs::Metadata) -> String {
    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    format!("{mtime_ms}-{}", meta.len())
}

/// Result of reading a file (SPEC-002 §3.4). `kind` tells the frontend whether
/// there is editable `content` or only metadata for a "can't open" notice.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum ReadResult {
    #[serde(rename = "text")]
    Text {
        path: String,
        size: u64,
        mtime_ms: u64,
        rev: String,
        eol: &'static str,
        content: String,
    },
    #[serde(rename = "binary")]
    Binary {
        path: String,
        size: u64,
        mtime_ms: u64,
        rev: String,
        mime: &'static str,
    },
    #[serde(rename = "tooLarge")]
    TooLarge {
        path: String,
        size: u64,
        mtime_ms: u64,
        rev: String,
        mime: &'static str,
    },
}

/// Metadata returned after a successful write.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteResult {
    pub rev: String,
    pub size: u64,
    pub mtime_ms: u64,
}

fn mtime_ms_of(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Read `rel` under `root`. Never loads oversized files into memory as text.
pub fn read(root: &Path, rel: &str) -> Result<ReadResult, FileError> {
    let abs = resolve(root, rel)?;
    let meta = std::fs::metadata(&abs).map_err(|_| FileError::NotFound(rel.to_string()))?;
    if meta.is_dir() {
        return Err(FileError::NotADirectory(format!("{rel} is a directory")));
    }

    let size = meta.len();
    let rev = rev_of(&meta);
    let mtime_ms = mtime_ms_of(&meta);

    if size > FILE_TEXT_MAX {
        return Ok(ReadResult::TooLarge {
            path: rel.to_string(),
            size,
            mtime_ms,
            rev,
            mime: probe::mime_for(rel),
        });
    }

    let bytes = std::fs::read(&abs)?;
    match probe::classify(&bytes) {
        probe::Classified::Binary => Ok(ReadResult::Binary {
            path: rel.to_string(),
            size,
            mtime_ms,
            rev,
            mime: probe::mime_for(rel),
        }),
        probe::Classified::Text { eol } => Ok(ReadResult::Text {
            path: rel.to_string(),
            size,
            mtime_ms,
            rev,
            eol: eol.as_str(),
            // classify() proved valid UTF-8; this cannot allocate-and-fail.
            content: String::from_utf8(bytes).expect("classify guaranteed UTF-8"),
        }),
    }
}

/// Write `content` to an *existing* file (creation goes through [`create`] —
/// SPEC-002 §3.4: PUT to a missing path is a 404).
///
/// `expected_rev: Some(rev)` enforces optimistic concurrency; `None` is the
/// explicit force-overwrite path the frontend's "Ghi đè" button uses.
///
/// The write is atomic: temp file in the same directory (rename never crosses
/// devices), permission bits copied from the target, then `rename` over it.
pub fn write(
    root: &Path,
    rel: &str,
    content: &str,
    expected_rev: Option<&str>,
) -> Result<WriteResult, FileError> {
    let abs = resolve(root, rel)?;
    let meta = std::fs::metadata(&abs).map_err(|_| FileError::NotFound(rel.to_string()))?;
    if meta.is_dir() {
        return Err(FileError::NotADirectory(format!("{rel} is a directory")));
    }

    if let Some(expected) = expected_rev {
        let current = rev_of(&meta);
        if current != expected {
            return Err(FileError::Conflict(format!(
                "file changed on disk (rev {current}, client had {expected})"
            )));
        }
    }

    let dir = abs
        .parent()
        .ok_or_else(|| FileError::NotFound(rel.to_string()))?;
    let tmp = dir.join(format!(".spec-ade-{}.tmp", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, content)?;

    // Preserve the target's permission bits (Unix) so an executable script
    // stays executable after a save.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode();
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode));
    }

    if let Err(e) = std::fs::rename(&tmp, &abs) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }

    let new_meta = std::fs::metadata(&abs)?;
    Ok(WriteResult {
        rev: rev_of(&new_meta),
        size: new_meta.len(),
        mtime_ms: mtime_ms_of(&new_meta),
    })
}

/// What to create (SPEC-002 §3.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateKind {
    File,
    Dir,
}

/// Create an empty file or directory at `rel`.
///
/// The parent must exist (`NotFound`) — implicit `mkdir -p` would paper over
/// mistyped paths ([SPEC-002 §3.5]). An existing target is a `Conflict`.
pub fn create(root: &Path, rel: &str, kind: CreateKind) -> Result<(), FileError> {
    let abs = resolve_non_root(root, rel)?;
    if abs.symlink_metadata().is_ok() {
        return Err(FileError::Conflict(format!("{rel} already exists")));
    }
    let parent = abs
        .parent()
        .ok_or_else(|| FileError::NotFound(rel.to_string()))?;
    if !parent.is_dir() {
        return Err(FileError::NotFound(format!(
            "parent of {rel} does not exist"
        )));
    }
    match kind {
        CreateKind::File => {
            std::fs::write(&abs, b"")?;
        }
        CreateKind::Dir => {
            std::fs::create_dir(&abs)?;
        }
    }
    Ok(())
}

/// Rename/move `rel` to `new_rel` (both inside the root).
pub fn rename(root: &Path, rel: &str, new_rel: &str) -> Result<(), FileError> {
    let from = resolve_non_root(root, rel)?;
    if from.symlink_metadata().is_err() {
        return Err(FileError::NotFound(rel.to_string()));
    }
    let to = resolve_non_root(root, new_rel)?;
    if to.symlink_metadata().is_ok() {
        return Err(FileError::Conflict(format!("{new_rel} already exists")));
    }
    let parent = to
        .parent()
        .ok_or_else(|| FileError::NotFound(new_rel.to_string()))?;
    if !parent.is_dir() {
        return Err(FileError::NotFound(format!(
            "parent of {new_rel} does not exist"
        )));
    }
    std::fs::rename(&from, &to)?;
    Ok(())
}

/// Delete `rel`. A non-empty directory requires `recursive` (SPEC-002 §3.5) —
/// the confirmation lives in the API, not just in the UI.
pub fn delete(root: &Path, rel: &str, recursive: bool) -> Result<(), FileError> {
    let abs = resolve_non_root(root, rel)?;
    let meta = abs
        .symlink_metadata()
        .map_err(|_| FileError::NotFound(rel.to_string()))?;

    if meta.is_dir() {
        let is_empty = std::fs::read_dir(&abs)?.next().is_none();
        if !is_empty && !recursive {
            return Err(FileError::Conflict(format!(
                "{rel} is not empty (pass recursive=true)"
            )));
        }
        if recursive {
            std::fs::remove_dir_all(&abs)?;
        } else {
            std::fs::remove_dir(&abs)?;
        }
    } else {
        std::fs::remove_file(&abs)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Root(PathBuf);
    impl Root {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("spec-ade-files-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p.canonicalize().unwrap())
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn rev_changes_when_size_changes_even_with_equal_mtime() {
        let root = Root::new();
        let a = root.0.join("a.txt");
        std::fs::write(&a, "12345").unwrap();
        let meta1 = std::fs::metadata(&a).unwrap();

        std::fs::write(&a, "1234567890").unwrap();
        // Force identical mtime to isolate the size component of the tag.
        let t = filetime_of(&meta1);
        set_mtime(&a, t);
        let meta2 = std::fs::metadata(&a).unwrap();

        assert_ne!(rev_of(&meta1), rev_of(&meta2), "size must break the tie");
    }

    fn filetime_of(meta: &std::fs::Metadata) -> std::time::SystemTime {
        meta.modified().unwrap()
    }

    fn set_mtime(path: &Path, t: std::time::SystemTime) {
        // Portable-enough mtime set via the `touch -t`-style utime syscall
        // through std: recreate by opening and setting times with the
        // `set_modified` API (stable since 1.75).
        let f = std::fs::File::options().write(true).open(path).unwrap();
        f.set_modified(t).unwrap();
    }

    #[test]
    fn write_with_stale_rev_is_refused_and_disk_untouched() {
        let root = Root::new();
        std::fs::write(root.0.join("f.txt"), "original").unwrap();

        let err = write(&root.0, "f.txt", "clobber", Some("0-0")).unwrap_err();
        assert!(matches!(err, FileError::Conflict(_)));
        assert_eq!(
            std::fs::read_to_string(root.0.join("f.txt")).unwrap(),
            "original"
        );
    }

    #[test]
    fn crlf_and_bom_round_trip_byte_for_byte() {
        let root = Root::new();
        let original: &[u8] = b"\xEF\xBB\xBFline one\r\nline two\r\n";
        std::fs::write(root.0.join("crlf.txt"), original).unwrap();

        let ReadResult::Text { content, rev, .. } = read(&root.0, "crlf.txt").unwrap() else {
            panic!("BOM+CRLF file must classify as text");
        };
        write(&root.0, "crlf.txt", &content, Some(&rev)).unwrap();
        assert_eq!(std::fs::read(root.0.join("crlf.txt")).unwrap(), original);
    }
}
