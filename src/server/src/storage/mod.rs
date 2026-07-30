//! Storage — filesystem layout for config dir + ACP chat history.
//!
//! Responsibility (docs/analysis/02-architecture.md §Storage): resolve and
//! manage the on-disk data directory:
//!   ~/.config/spec-ade/settings.json               — all config/state
//!   ~/.config/spec-ade/acp-history/{session_id}.json — per-session chat log
//!
//! Roadmap: Pha 0 onward (07-build-roadmap.md).
//!
//! Notes:
//! - Centralize path resolution here so `settings` and `acp` share one root.
//! - Honor a runtime-data-dir override (env `SPEC_ADE_DATA_DIR`) for tests and
//!   packaging; the default follows the platform config dir.

use std::io;
use std::path::PathBuf;

/// Env var overriding the data directory (used by tests + packaging).
pub const DATA_DIR_ENV: &str = "SPEC_ADE_DATA_DIR";

/// Resolve the Spec ADE data directory.
///
/// Order of precedence:
/// 1. `SPEC_ADE_DATA_DIR` (explicit override — used by tests/packaging).
/// 2. `$XDG_CONFIG_HOME/spec-ade` if `XDG_CONFIG_HOME` is set.
/// 3. `$HOME/.config/spec-ade` (documented default, 02 §Storage).
///
/// This deliberately avoids an extra `dirs`-style dependency: the documented
/// layout is `~/.config/spec-ade`, which the two rules above cover on the
/// Unix targets Spec ADE runs on.
pub fn config_dir() -> io::Result<PathBuf> {
    if let Some(dir) = std::env::var_os(DATA_DIR_ENV) {
        return Ok(PathBuf::from(dir));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let mut p = PathBuf::from(xdg);
        p.push("spec-ade");
        return Ok(p);
    }
    let home = std::env::var_os("HOME").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "cannot resolve data dir: neither SPEC_ADE_DATA_DIR, XDG_CONFIG_HOME, nor HOME is set",
        )
    })?;
    let mut p = PathBuf::from(home);
    p.push(".config");
    p.push("spec-ade");
    Ok(p)
}

/// Resolve `config_dir()` and ensure it exists (creating it if needed).
pub fn ensure_config_dir() -> io::Result<PathBuf> {
    let dir = config_dir()?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Path to the settings document (`<config_dir>/settings.json`).
pub fn settings_path() -> io::Result<PathBuf> {
    let mut p = config_dir()?;
    p.push("settings.json");
    Ok(p)
}

// TODO(persistence): read/write per-session ACP history files under acp-history/.
// SPEC-003 shipped the ACP event log in RAM only (`acp::log::EventLog`), so a
// server restart loses the transcript. Persisting it needs a decision on format
// and pruning that no shipped spec covers yet — deliberately deferred, not missed.
