//! Path guard — resolve a client-supplied relative path inside a project root.
//!
//! SPEC-002 §5.2. Trust model (stated plainly): this is NOT a privilege
//! boundary — a token-holding caller already has arbitrary command execution
//! via the terminal. The guard exists so a frontend bug or a wandering ACP
//! agent cannot read/write outside the project root through *these* endpoints,
//! and so SPEC-003 can reuse the `files::` layer for `fs/*` safely.
//!
//! Strategy: reject `..`/absolute/`.`/empty components *syntactically* first
//! (they never reach a syscall), then canonicalize the deepest existing
//! ancestor and require it to stay under the (already canonical) root — that
//! second step is what catches symlinks pointing out of the tree. The TOCTOU
//! window between canonicalize and open is accepted knowingly: an attacker who
//! can win that race already has a shell.

use std::path::{Component, Path, PathBuf};

/// Why a path was refused. Maps to HTTP in the routes layer:
/// `Absolute`/`Traversal`/`IsRoot` → 400, `Escapes` → 403.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("path must be relative to the project root")]
    Absolute,
    #[error("path may not contain '.', '..' or empty components")]
    Traversal,
    #[error("path resolves outside the project root")]
    Escapes,
    #[error("operation not allowed on the project root itself")]
    IsRoot,
}

/// Resolve `rel` (client-supplied, `/`-separated) against `root`.
///
/// `root` must already be canonical (the project registry stores canonical
/// paths). An empty `rel` resolves to the root itself — callers that must not
/// operate on the root check [`PathError::IsRoot`] via [`resolve_non_root`].
pub fn resolve(root: &Path, rel: &str) -> Result<PathBuf, PathError> {
    if rel.is_empty() {
        return Ok(root.to_path_buf());
    }

    let rel_path = Path::new(rel);
    if rel_path.is_absolute() || rel.starts_with('/') {
        return Err(PathError::Absolute);
    }

    // Syntactic screen: every component must be a normal name. `Component`
    // classification catches `..` (ParentDir), `.` (CurDir) and windows-style
    // prefixes; doubled separators produce no component so they're harmless.
    let mut candidate = root.to_path_buf();
    for component in rel_path.components() {
        match component {
            Component::Normal(name) => candidate.push(name),
            _ => return Err(PathError::Traversal),
        }
    }

    // Symlink screen: canonicalize the deepest existing ancestor (for a file
    // being created, that's typically the parent dir) and require it to stay
    // under root. The not-yet-existing tail can't contain symlinks.
    let mut existing = candidate.clone();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => return Err(PathError::Escapes),
        }
    }
    let canonical = existing.canonicalize().map_err(|_| PathError::Escapes)?;
    if !canonical.starts_with(root) {
        return Err(PathError::Escapes);
    }

    let mut resolved = canonical;
    for name in tail.into_iter().rev() {
        resolved.push(name);
    }
    Ok(resolved)
}

/// As [`resolve`], additionally refusing the project root itself — used by the
/// mutating entry endpoints (SPEC-002 §3.5: create/rename/delete on the root
/// are a 400).
pub fn resolve_non_root(root: &Path, rel: &str) -> Result<PathBuf, PathError> {
    let resolved = resolve(root, rel)?;
    if resolved == root {
        return Err(PathError::IsRoot);
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempdir::Root, PathBuf) {
        let root = tempdir::Root::new("path-guard");
        let canonical = root.path().canonicalize().unwrap();
        (root, canonical)
    }

    /// Minimal tempdir helper: unique dir, removed on drop.
    mod tempdir {
        use std::path::{Path, PathBuf};

        pub struct Root(PathBuf);
        impl Root {
            pub fn new(tag: &str) -> Self {
                let p =
                    std::env::temp_dir().join(format!("spec-ade-{tag}-{}", uuid::Uuid::new_v4()));
                std::fs::create_dir_all(&p).unwrap();
                Self(p)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }
        impl Drop for Root {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    #[test]
    fn empty_path_is_the_root() {
        let (_g, root) = setup();
        assert_eq!(resolve(&root, "").unwrap(), root);
        assert_eq!(resolve_non_root(&root, ""), Err(PathError::IsRoot));
    }

    #[test]
    fn ordinary_relative_paths_resolve_under_root() {
        let (_g, root) = setup();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        assert_eq!(resolve(&root, "a/b").unwrap(), root.join("a/b"));
        // A leaf that doesn't exist yet is fine — creation needs this.
        assert_eq!(
            resolve(&root, "a/b/new.txt").unwrap(),
            root.join("a/b/new.txt")
        );
        // Even a whole missing subtree resolves, as long as it stays inside.
        assert_eq!(resolve(&root, "x/y/z").unwrap(), root.join("x/y/z"));
    }

    #[test]
    fn traversal_and_absolute_are_rejected_syntactically() {
        let (_g, root) = setup();
        assert_eq!(resolve(&root, ".."), Err(PathError::Traversal));
        assert_eq!(resolve(&root, "a/../b"), Err(PathError::Traversal));
        assert_eq!(resolve(&root, "a/.."), Err(PathError::Traversal));
        assert_eq!(resolve(&root, "./a"), Err(PathError::Traversal));
        assert_eq!(resolve(&root, "/etc/passwd"), Err(PathError::Absolute));
    }

    #[test]
    fn symlink_inside_root_is_allowed() {
        let (_g, root) = setup();
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        let resolved = resolve(&root, "link/file.txt").unwrap();
        assert!(resolved.starts_with(&root), "resolved: {resolved:?}");
    }

    #[test]
    fn symlink_escaping_root_is_forbidden() {
        let (_g, root) = setup();
        let outside =
            std::env::temp_dir().join(format!("spec-ade-outside-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("evil")).unwrap();

        assert_eq!(resolve(&root, "evil"), Err(PathError::Escapes));
        assert_eq!(resolve(&root, "evil/secret.txt"), Err(PathError::Escapes));

        let _ = std::fs::remove_dir_all(&outside);
    }
}
