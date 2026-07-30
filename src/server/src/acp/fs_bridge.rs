//! `fs/read_text_file` + `fs/write_text_file` reverse calls (SPEC-003 §5.5).
//!
//! The agent is the caller here: it asks Spec ADE to touch the filesystem instead
//! of doing it itself, so the client stays the single place where policy lives.
//!
//! SECURITY ([INVENTED-7]): ACP sends **absolute** paths, but they are only
//! honoured inside the session's project root. The path is made relative to the
//! root and then run through the SPEC-002 guard, which rejects `..`, and
//! re-canonicalizes to catch symlinks pointing out of the tree. Without this a
//! wandering agent could read `~/.ssh/id_ed25519` through a capability the user
//! granted for their project.
//!
//! No `rev` check on write (unlike the editor's PUT): the agent has no rev to
//! send, and its write is something the user explicitly asked for by prompting.

use std::path::Path;

use agent_client_protocol::Error as AcpRpcError;
use agent_client_protocol::schema::v1::{
    ReadTextFileRequest, ReadTextFileResponse, WriteTextFileRequest, WriteTextFileResponse,
};

use crate::files;

/// JSON-RPC error for a path the agent may not touch.
///
/// The message names the path the agent asked for — it already knows it — but
/// nothing about what does exist outside the root.
fn rejected(path: &Path, why: &str) -> AcpRpcError {
    AcpRpcError::invalid_params().data(serde_json::json!({
        "path": path.display().to_string(),
        "reason": why,
    }))
}

/// Make an agent-supplied absolute path relative to `root`, for the guard.
///
/// A relative path is taken as already root-relative (tolerated: some agents send
/// one even though the spec says absolute). An absolute path outside the root is
/// refused here rather than being silently reinterpreted as relative — turning
/// `/etc/passwd` into `<root>/etc/passwd` would answer a question the agent did
/// not ask.
fn to_relative(root: &Path, path: &Path) -> Result<String, AcpRpcError> {
    if path.is_relative() {
        return Ok(path.to_string_lossy().into_owned());
    }
    match path.strip_prefix(root) {
        Ok(rel) => Ok(rel.to_string_lossy().into_owned()),
        Err(_) => Err(rejected(path, "path is outside the session's project root")),
    }
}

/// Slice `content` to `limit` lines starting at 1-based `line`.
///
/// 1-based per the ACP schema (`line`: "Line number to start reading from
/// (1-based)"). Treating it as 0-based would shift every read by one line and
/// silently corrupt what the agent believes it read.
fn slice_lines(content: &str, line: Option<u32>, limit: Option<u32>) -> String {
    if line.is_none() && limit.is_none() {
        return content.to_string();
    }
    // `line: 0` is out of range for a 1-based index; treat it as line 1 rather
    // than erroring, since it is an off-by-one in the agent and not a security
    // question.
    let skip = line.unwrap_or(1).saturating_sub(1) as usize;
    let mut lines = content.lines().skip(skip);
    let selected: Vec<&str> = match limit {
        Some(n) => lines.by_ref().take(n as usize).collect(),
        None => lines.by_ref().collect(),
    };
    let mut out = selected.join("\n");
    // Keep a trailing newline when the slice does not run off the end, so a
    // partial read still looks like whole lines to the agent.
    if !out.is_empty() && lines.next().is_some() {
        out.push('\n');
    }
    out
}

/// Handle `fs/read_text_file` against `root`.
///
/// Blocking I/O: callers run this inside `spawn_blocking`.
pub fn read_text_file(
    root: &Path,
    req: &ReadTextFileRequest,
) -> Result<ReadTextFileResponse, AcpRpcError> {
    let rel = to_relative(root, &req.path)?;
    let abs = files::resolve(root, &rel).map_err(|e| rejected(&req.path, &e.to_string()))?;
    let content = std::fs::read_to_string(&abs).map_err(|e| {
        // A binary or missing file is the agent's problem to report, not ours to
        // classify — pass the OS reason through.
        AcpRpcError::resource_not_found(Some(req.path.display().to_string())).data(e.to_string())
    })?;
    Ok(ReadTextFileResponse::new(slice_lines(
        &content, req.line, req.limit,
    )))
}

/// Handle `fs/write_text_file` against `root`.
///
/// Creates the file if it does not exist (an agent writing a new file is the
/// common case), but never creates missing parent directories — an implicit
/// `mkdir -p` would turn a typo into a stray tree, matching SPEC-002 §3.5.
pub fn write_text_file(
    root: &Path,
    req: &WriteTextFileRequest,
) -> Result<WriteTextFileResponse, AcpRpcError> {
    let rel = to_relative(root, &req.path)?;
    let abs =
        files::resolve_non_root(root, &rel).map_err(|e| rejected(&req.path, &e.to_string()))?;

    let parent = abs
        .parent()
        .ok_or_else(|| rejected(&req.path, "path has no parent directory"))?;
    if !parent.is_dir() {
        return Err(rejected(&req.path, "parent directory does not exist"));
    }

    if abs.exists() {
        // Existing file: reuse SPEC-002's atomic write (temp in the same dir →
        // rename), which also preserves the executable bit. `None` rev = the
        // deliberate force-overwrite path.
        files::write(root, &rel, &req.content, None)
            .map_err(|e| AcpRpcError::internal_error().data(e.to_string()))?;
    } else {
        files::create(root, &rel, files::CreateKind::File)
            .map_err(|e| AcpRpcError::internal_error().data(e.to_string()))?;
        files::write(root, &rel, &req.content, None)
            .map_err(|e| AcpRpcError::internal_error().data(e.to_string()))?;
    }
    Ok(WriteTextFileResponse::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Root(PathBuf);
    impl Root {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("spec-ade-fsb-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p.canonicalize().unwrap())
        }
    }
    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn read_req(root: &Path, rel: &str) -> ReadTextFileRequest {
        ReadTextFileRequest::new("s1", root.join(rel))
    }

    #[test]
    fn reads_a_file_inside_the_root() {
        let root = Root::new();
        std::fs::write(root.0.join("a.txt"), "hello\n").unwrap();
        let res = read_text_file(&root.0, &read_req(&root.0, "a.txt")).unwrap();
        assert_eq!(res.content, "hello\n");
    }

    #[test]
    fn line_and_limit_are_one_based() {
        let root = Root::new();
        std::fs::write(root.0.join("n.txt"), "l1\nl2\nl3\nl4\nl5\n").unwrap();

        // line=2 must start at "l2" — a 0-based reading would return "l3".
        let req = read_req(&root.0, "n.txt").line(2u32).limit(2u32);
        assert_eq!(read_text_file(&root.0, &req).unwrap().content, "l2\nl3\n");

        // Reaching the end drops the trailing newline we'd otherwise invent.
        let req = read_req(&root.0, "n.txt").line(4u32);
        assert_eq!(read_text_file(&root.0, &req).unwrap().content, "l4\nl5");

        // No line/limit at all is a byte-for-byte read.
        assert_eq!(
            read_text_file(&root.0, &read_req(&root.0, "n.txt"))
                .unwrap()
                .content,
            "l1\nl2\nl3\nl4\nl5\n"
        );
    }

    #[test]
    fn absolute_path_outside_the_root_is_refused_with_no_content() {
        let root = Root::new();
        let req = ReadTextFileRequest::new("s1", "/etc/passwd");
        let err = read_text_file(&root.0, &req).unwrap_err();
        assert_eq!(err.code, agent_client_protocol::ErrorCode::InvalidParams);
        // The error must not leak what was read; only the rejected path echoes back.
        let data = err.data.unwrap().to_string();
        assert!(data.contains("/etc/passwd"), "{data}");
        assert!(
            !data.contains("root:"),
            "file content must never leak: {data}"
        );
    }

    #[test]
    fn traversal_out_of_the_root_is_refused() {
        let root = Root::new();
        // An absolute path that starts inside the root but climbs out: the strip
        // succeeds, so only the guard catches this. That is the case a naive
        // `starts_with` check would let through.
        let req = ReadTextFileRequest::new("s1", root.0.join("../../etc/passwd"));
        let err = read_text_file(&root.0, &req).unwrap_err();
        assert_eq!(err.code, agent_client_protocol::ErrorCode::InvalidParams);
    }

    #[test]
    fn symlink_escaping_the_root_is_refused() {
        let root = Root::new();
        let outside =
            std::env::temp_dir().join(format!("spec-ade-fsb-out-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "TOPSECRET").unwrap();
        std::os::unix::fs::symlink(&outside, root.0.join("link")).unwrap();

        let err = read_text_file(&root.0, &read_req(&root.0, "link/secret.txt")).unwrap_err();
        assert_eq!(err.code, agent_client_protocol::ErrorCode::InvalidParams);

        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn write_creates_and_overwrites_inside_the_root() {
        let root = Root::new();
        let req = WriteTextFileRequest::new("s1", root.0.join("new.txt"), "first");
        write_text_file(&root.0, &req).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.0.join("new.txt")).unwrap(),
            "first"
        );

        let req = WriteTextFileRequest::new("s1", root.0.join("new.txt"), "second");
        write_text_file(&root.0, &req).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.0.join("new.txt")).unwrap(),
            "second"
        );
    }

    #[test]
    fn write_outside_the_root_is_refused_and_writes_nothing() {
        let root = Root::new();
        let outside =
            std::env::temp_dir().join(format!("spec-ade-fsb-out-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&outside).unwrap();
        let target = outside.join("pwned.txt");

        let req = WriteTextFileRequest::new("s1", &target, "pwned");
        assert!(write_text_file(&root.0, &req).is_err());
        assert!(!target.exists(), "refused write must not create the file");

        let _ = std::fs::remove_dir_all(&outside);
    }

    #[test]
    fn write_to_a_missing_parent_is_refused_rather_than_mkdir_p() {
        let root = Root::new();
        let req = WriteTextFileRequest::new("s1", root.0.join("no/such/dir/f.txt"), "x");
        assert!(write_text_file(&root.0, &req).is_err());
        assert!(!root.0.join("no").exists(), "must not create a stray tree");
    }

    #[test]
    fn write_to_the_root_itself_is_refused() {
        let root = Root::new();
        let req = WriteTextFileRequest::new("s1", &root.0, "x");
        assert!(write_text_file(&root.0, &req).is_err());
        assert!(root.0.is_dir(), "the root must still be a directory");
    }
}
