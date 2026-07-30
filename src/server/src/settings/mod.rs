//! Settings — global config/state persisted as JSON.
//!
//! Responsibility (docs/analysis/02-architecture.md §Storage, 06 §Settings):
//! own the `~/.config/spec-ade/settings.json` document holding all config/state
//! (projects, layouts, claws, auth token, etc.).
//!
//! SPEC-002 adds: the `editor` section with `Option<Option<T>>` partial-update
//! semantics (absent = keep, null = back to default, value = set), the
//! `projects` registry, and `SettingsStore` — the single mutable handle the
//! routes share so concurrent PUTs can't lose updates.
//!
//! SECURITY ([INVENTED-1]): `auth_token` is never exposed by `GET /api/settings`
//! and never writable via `PUT` — an authenticated client that could rotate the
//! token would turn any frontend bug into a lockout.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::storage;

/// Editor preferences (SPEC-002 [INVENTED-2]): exactly the keys the editor
/// consumes. Bounds are validated on PUT — out of range is a 400, not a clamp.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EditorSettings {
    pub font_size: u16,
    pub tab_size: u16,
    pub insert_spaces: bool,
    pub word_wrap: bool,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            font_size: 14,
            tab_size: 2,
            insert_spaces: true,
            word_wrap: false,
        }
    }
}

/// Bounds for editor settings ([INVENTED-2]).
pub const FONT_SIZE_RANGE: std::ops::RangeInclusive<u16> = 8..=40;
pub const TAB_SIZE_RANGE: std::ops::RangeInclusive<u16> = 1..=8;

/// A registered project (SPEC-002 §3.2). `path` is canonical and unique;
/// `id` is the URL key ([INVENTED-4]).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectEntry {
    pub id: String,
    pub path: String,
    pub name: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub sort_order: i64,
}

/// Persisted config/state document (`settings.json`).
///
/// `#[serde(default)]` keeps deserialization forward-compatible: a settings
/// file written by a later phase (with extra fields) still loads, and a file
/// missing a section gets defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Random session token required on every `/api/*` request except health.
    ///
    /// Generated on first run and persisted so it survives restarts (the Tauri
    /// WebView / CLI reads it back to authenticate — deep-dive 02 §4.4 step 1).
    #[serde(default)]
    pub auth_token: String,

    /// Editor preferences, exposed via `GET/PUT /api/settings`.
    #[serde(default)]
    pub editor: EditorSettings,

    /// Registered projects (SPEC-002). Mutated only through the projects API.
    #[serde(default)]
    pub projects: Vec<ProjectEntry>,

    /// Configured ACP agents (SPEC-003 §3.4). Read-only this phase — seeded on
    /// first run, edited by hand until a management UI exists.
    #[serde(default = "crate::acp::agent::default_agents")]
    pub acp_agents: Vec<crate::acp::agent::AcpAgentEntry>,
}

impl Default for Settings {
    // Hand-written rather than `#[derive(Default)]` so this and the `serde(default
    // = ...)` on `acp_agents` can't drift apart — a derived empty `Vec` here would
    // make a brand-new in-memory `Settings` disagree with one just loaded from an
    // empty file.
    fn default() -> Self {
        Self {
            auth_token: String::new(),
            editor: EditorSettings::default(),
            projects: Vec::new(),
            acp_agents: crate::acp::agent::default_agents(),
        }
    }
}

impl Settings {
    /// Generate a fresh token. Uses a UUID v4 (122 bits of randomness) rendered
    /// as a hyphen-free hex string — ample entropy for an unguessable session
    /// token, and available without pulling in a separate RNG crate.
    fn generate_token() -> String {
        Uuid::new_v4().simple().to_string()
    }

    /// Load settings from disk, creating the file (with a freshly generated
    /// token) on first run. Also backfills a token if an existing file lacks
    /// one, persisting the change.
    ///
    /// The data dir and file are created if absent (`storage::ensure_config_dir`).
    pub fn load_or_init() -> io::Result<Self> {
        storage::ensure_config_dir()?;
        let path = storage::settings_path()?;
        Self::load_or_init_at(&path)
    }

    /// As [`Settings::load_or_init`] with an explicit file path — the seam the
    /// store and tests use so the file location is fixed at construction, not
    /// re-resolved from the environment on every save.
    pub fn load_or_init_at(path: &std::path::Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut settings = if path.exists() {
            let raw = std::fs::read_to_string(path)?;
            // A malformed/empty file must not brick startup: fall back to
            // defaults (a fresh token is then generated + persisted below).
            serde_json::from_str::<Settings>(&raw).unwrap_or_default()
        } else {
            Settings::default()
        };

        if settings.auth_token.is_empty() {
            settings.auth_token = Self::generate_token();
            settings.save_to(path)?;
        }

        Ok(settings)
    }

    /// Persist settings to `<config_dir>/settings.json` (pretty-printed).
    pub fn save(&self) -> io::Result<()> {
        storage::ensure_config_dir()?;
        let path = storage::settings_path()?;
        self.save_to(&path)
    }

    /// Persist to an explicit path.
    pub fn save_to(&self, path: &std::path::Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, json)
    }
}

/// Shared, mutable settings handle.
///
/// One `Mutex` guards read-modify-write so two concurrent `PUT`s can't lose an
/// update (SPEC-002 §9). The lock is synchronous and never held across an
/// `.await` — callers run mutations inside `spawn_blocking`.
#[derive(Clone)]
pub struct SettingsStore {
    inner: Arc<Mutex<Settings>>,
    path: PathBuf,
}

impl SettingsStore {
    /// Wrap already-loaded settings; `path` is where saves go.
    pub fn new(settings: Settings, path: PathBuf) -> Self {
        Self {
            inner: Arc::new(Mutex::new(settings)),
            path,
        }
    }

    /// Snapshot of the current settings.
    pub fn snapshot(&self) -> Settings {
        lock(&self.inner).clone()
    }

    /// Mutate under the lock and persist before releasing it.
    ///
    /// Persisting inside the lock makes "what's on disk" match "what the next
    /// reader sees" — a failed save surfaces as an error and leaves memory
    /// unchanged (the mutation is applied to a scratch copy first).
    pub fn update<T>(
        &self,
        mutate: impl FnOnce(&mut Settings) -> Result<T, SettingsError>,
    ) -> Result<T, SettingsError> {
        let mut guard = lock(&self.inner);
        let mut draft = guard.clone();
        let out = mutate(&mut draft)?;
        draft
            .save_to(&self.path)
            .map_err(|e| SettingsError::Io(e.to_string()))?;
        *guard = draft;
        Ok(out)
    }
}

/// Errors from settings mutations, mapped to HTTP by the routes layer.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("{0}")]
    Invalid(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("io error: {0}")]
    Io(String),
}

/// `Option<Option<T>>` field of a PUT body (06 §Settings [DOCS]):
/// `None` = absent = keep, `Some(None)` = null = back to default,
/// `Some(Some(v))` = set. Serde only calls the deserializer when the key is
/// present, so `#[serde(default)]` yields `None` for absent keys.
pub fn double_option<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Deserialize::deserialize(de).map(Some)
}

use serde::Deserializer;

/// Partial update for the `editor` section (SPEC-002 §5.6).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditorPatch {
    #[serde(default, deserialize_with = "double_option")]
    pub font_size: Option<Option<u16>>,
    #[serde(default, deserialize_with = "double_option")]
    pub tab_size: Option<Option<u16>>,
    #[serde(default, deserialize_with = "double_option")]
    pub insert_spaces: Option<Option<bool>>,
    #[serde(default, deserialize_with = "double_option")]
    pub word_wrap: Option<Option<bool>>,
}

impl EditorPatch {
    /// Apply onto `current`, validating bounds. Out-of-range is an error, not a
    /// clamp — silently storing a value the user didn't ask for is invented data.
    pub fn apply(&self, current: &mut EditorSettings) -> Result<(), SettingsError> {
        let defaults = EditorSettings::default();

        if let Some(v) = &self.font_size {
            let v = v.unwrap_or(defaults.font_size);
            if !FONT_SIZE_RANGE.contains(&v) {
                return Err(SettingsError::Invalid(format!(
                    "fontSize {v} out of range {:?}",
                    FONT_SIZE_RANGE
                )));
            }
            current.font_size = v;
        }
        if let Some(v) = &self.tab_size {
            let v = v.unwrap_or(defaults.tab_size);
            if !TAB_SIZE_RANGE.contains(&v) {
                return Err(SettingsError::Invalid(format!(
                    "tabSize {v} out of range {:?}",
                    TAB_SIZE_RANGE
                )));
            }
            current.tab_size = v;
        }
        if let Some(v) = &self.insert_spaces {
            current.insert_spaces = v.unwrap_or(defaults.insert_spaces);
        }
        if let Some(v) = &self.word_wrap {
            current.word_wrap = v.unwrap_or(defaults.word_wrap);
        }
        Ok(())
    }
}

/// Lock, recovering from poisoning (same rationale as `pty::lock`).
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// TODO(spec-002-followup): one-time SQLite -> JSON migration on first load —
// only relevant when importing from the original Spec ADE; tracked in the spec
// as out of scope until a real legacy file exists to migrate.

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(json: &str) -> EditorPatch {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn absent_fields_keep_current_values() {
        let mut editor = EditorSettings {
            font_size: 18,
            ..Default::default()
        };
        patch("{}").apply(&mut editor).unwrap();
        assert_eq!(editor.font_size, 18);
    }

    #[test]
    fn null_resets_to_default() {
        let mut editor = EditorSettings {
            tab_size: 8,
            ..Default::default()
        };
        patch(r#"{"tabSize": null}"#).apply(&mut editor).unwrap();
        assert_eq!(editor.tab_size, EditorSettings::default().tab_size);
    }

    #[test]
    fn value_sets_and_validates_bounds() {
        let mut editor = EditorSettings::default();
        patch(r#"{"fontSize": 20}"#).apply(&mut editor).unwrap();
        assert_eq!(editor.font_size, 20);

        let err = patch(r#"{"fontSize": 100}"#).apply(&mut editor);
        assert!(matches!(err, Err(SettingsError::Invalid(_))));
        // A failed patch must not have applied partially.
        assert_eq!(editor.font_size, 20);
    }

    #[test]
    fn unknown_keys_are_rejected_at_deserialization() {
        assert!(serde_json::from_str::<EditorPatch>(r#"{"theme": "light"}"#).is_err());
    }

    #[test]
    fn store_update_persists_and_is_atomic_on_error() {
        let dir = std::env::temp_dir().join(format!("spec-ade-settings-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let store = SettingsStore::new(Settings::default(), path.clone());

        store
            .update(|s| {
                s.editor.font_size = 30;
                Ok(())
            })
            .unwrap();
        assert_eq!(store.snapshot().editor.font_size, 30);
        let on_disk: Settings =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk.editor.font_size, 30);

        // A mutation that errors must leave both memory and disk untouched.
        let _ = store.update(|s| -> Result<(), SettingsError> {
            s.editor.font_size = 99;
            Err(SettingsError::Invalid("nope".into()))
        });
        assert_eq!(store.snapshot().editor.font_size, 30);
    }
}
