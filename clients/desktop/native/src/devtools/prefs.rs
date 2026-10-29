//! The saved Developer mode setting: `developer.json` beside `updates.json`
//! in the desktop's config directory, `{"developerMode": true}`.
//!
//! It is app-wide, not part of a window's display preferences: Developer
//! mode is one process-wide switch, read before any window or layout opens.
//! Absent, unreadable or malformed means off.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde_json::{Value, json};

const KEY: &str = "developerMode";

/// Where the setting lives.
#[must_use]
pub fn path() -> PathBuf {
    crate::updates::config_dir().join("developer.json")
}

/// Is Developer mode saved on?
#[must_use]
pub fn load() -> bool {
    load_from(&path())
}

/// Save Developer mode.
///
/// # Errors
///
/// The config directory or file could not be written.
pub fn save(on: bool) -> anyhow::Result<()> {
    save_to(&path(), on)
}

/// [`load`] from `path`.
#[must_use]
pub fn load_from(path: &Path) -> bool {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| value[KEY].as_bool())
        .unwrap_or(false)
}

/// [`save`] to `path`, atomically: a reader never sees half a file.
///
/// # Errors
///
/// The directory or file could not be written.
pub fn save_to(path: &Path, on: bool) -> anyhow::Result<()> {
    let dir = path
        .parent()
        .context("no Developer mode config directory")?;
    std::fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    let staged = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&staged, serde_json::to_vec_pretty(&json!({ KEY: on }))?)
        .with_context(|| format!("could not write {}", staged.display()))?;
    std::fs::rename(&staged, path)
        .with_context(|| format!("could not replace {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("phux-devprefs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn developer_mode_defaults_off_and_round_trips() {
        let dir = scratch("round-trip");
        let file = dir.join("nested/developer.json");
        assert!(!load_from(&file), "absent is off");
        save_to(&file, true).unwrap_or_else(|e| panic!("{e:#}"));
        assert!(load_from(&file));
        let saved: Value = serde_json::from_slice(&std::fs::read(&file).unwrap_or_default())
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(saved, json!({ "developerMode": true }));
        save_to(&file, false).unwrap_or_else(|e| panic!("{e:#}"));
        assert!(!load_from(&file));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_damaged_or_foreign_file_reads_as_off() {
        let dir = scratch("damaged");
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
        let file = dir.join("developer.json");
        for text in [
            "",
            "{",
            "[]",
            r#"{"developerMode":"yes"}"#,
            r#"{"other":true}"#,
        ] {
            std::fs::write(&file, text).unwrap_or_else(|e| panic!("{e}"));
            assert!(!load_from(&file), "{text:?}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_launch_forces_developer_mode_on_whatever_is_saved() {
        assert!(!super::super::effective(false, false));
        assert!(super::super::effective(false, true));
        assert!(super::super::effective(true, false));
        assert!(super::super::effective(true, true));
    }

    #[test]
    fn the_setting_lives_beside_the_update_settings() {
        assert_eq!(path().file_name(), Some("developer.json".as_ref()));
        assert_eq!(
            path().parent(),
            Some(crate::updates::config_dir().as_path())
        );
    }
}
