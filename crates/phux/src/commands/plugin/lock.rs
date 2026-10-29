//! Managed-install lockfile (`plugins.lock`): records where every
//! `phux plugin install`ed package came from — the source kind, the
//! original ref (git URL / local path), the requested branch or tag, the
//! subdirectory inside a git clone, and the resolved commit for git sources —
//! so `phux plugin update` can re-fetch deterministically without re-asking
//! the user.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Lockfile name inside the managed plugins directory.
pub(super) const LOCKFILE_NAME: &str = "plugins.lock";

const LOCKFILE_VERSION: u16 = 1;

/// The whole `plugins.lock` document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct PluginLockFile {
    /// Lockfile format version.
    pub(super) version: u16,
    /// One entry per installed plugin, sorted by id.
    #[serde(default, rename = "plugin")]
    pub(super) plugins: Vec<PluginLockEntry>,
}

impl Default for PluginLockFile {
    fn default() -> Self {
        Self {
            version: LOCKFILE_VERSION,
            plugins: Vec::new(),
        }
    }
}

/// One installed plugin's provenance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct PluginLockEntry {
    /// Plugin id from its manifest; also names the install directory.
    pub(super) id: String,
    /// How the package was fetched.
    pub(super) source: PluginSourceKind,
    /// Original ref: git URL, or the absolute local directory/tarball path.
    #[serde(rename = "ref")]
    pub(super) source_ref: String,
    /// Requested branch or tag (`--rev`), git sources only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) branch: Option<String>,
    /// Subdirectory of a git clone that holds `phux-plugin.toml`.
    /// Absent for root plugins and for locks written before subdirectory
    /// installs, which still deserialize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) subdir: Option<String>,
    /// Resolved commit hash, git sources only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) rev: Option<String>,
}

/// Source kinds `phux plugin install` accepts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(super) enum PluginSourceKind {
    /// Cloned from a git URL with the system `git`.
    Git,
    /// Copied from a local directory.
    Dir,
    /// Extracted from a local tarball with the system `tar`.
    Tarball,
}

impl std::fmt::Display for PluginSourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Git => "git",
            Self::Dir => "dir",
            Self::Tarball => "tarball",
        })
    }
}

/// Path of the lockfile inside `plugins_dir`.
pub(super) fn lockfile_path(plugins_dir: &Path) -> PathBuf {
    plugins_dir.join(LOCKFILE_NAME)
}

/// Read the lockfile, treating a missing file as an empty lock.
pub(super) fn read_lockfile(plugins_dir: &Path) -> Result<PluginLockFile, String> {
    let path = lockfile_path(plugins_dir);
    let input = match std::fs::read_to_string(&path) {
        Ok(input) => input,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PluginLockFile::default());
        }
        Err(err) => return Err(format!("could not read {}: {err}", path.display())),
    };
    toml::from_str(&input).map_err(|err| format!("could not parse {}: {err}", path.display()))
}

/// Write the lockfile (entries sorted by id for stable diffs).
///
/// No advisory lock: `fs2` and `fd-lock` are not dependencies of this
/// workspace, so concurrent install/update is not serialized here.
pub(super) fn write_lockfile(plugins_dir: &Path, lock: &PluginLockFile) -> Result<(), String> {
    let mut lock = lock.clone();
    lock.plugins.sort_by(|a, b| a.id.cmp(&b.id));
    let rendered = toml::to_string_pretty(&lock)
        .map_err(|err| format!("could not render {LOCKFILE_NAME}: {err}"))?;
    std::fs::create_dir_all(plugins_dir)
        .map_err(|err| format!("could not create {}: {err}", plugins_dir.display()))?;
    let path = lockfile_path(plugins_dir);
    std::fs::write(&path, rendered)
        .map_err(|err| format!("could not write {}: {err}", path.display()))
}

/// Insert or replace the entry with `entry.id`.
pub(super) fn upsert_entry(lock: &mut PluginLockFile, entry: PluginLockEntry) {
    if let Some(existing) = lock.plugins.iter_mut().find(|e| e.id == entry.id) {
        *existing = entry;
    } else {
        lock.plugins.push(entry);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "tests")]

    use super::{
        PluginLockEntry, PluginLockFile, PluginSourceKind, read_lockfile, upsert_entry,
        write_lockfile,
    };

    fn sample_lock() -> PluginLockFile {
        PluginLockFile {
            version: 1,
            plugins: vec![
                PluginLockEntry {
                    id: "example.git-plugin".to_owned(),
                    source: PluginSourceKind::Git,
                    source_ref: "https://example.invalid/plugin.git".to_owned(),
                    branch: Some("main".to_owned()),
                    subdir: Some("plugins/demo".to_owned()),
                    rev: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
                },
                PluginLockEntry {
                    id: "example.dir-plugin".to_owned(),
                    source: PluginSourceKind::Dir,
                    source_ref: "/srv/plugins/dir-plugin".to_owned(),
                    branch: None,
                    subdir: None,
                    rev: None,
                },
            ],
        }
    }

    /// Lockfile round-trip: write, read back, and land on the same value
    /// (modulo the stable id sort applied at write time).
    #[test]
    fn lockfile_round_trips_through_disk() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut lock = sample_lock();

        write_lockfile(dir.path(), &lock).unwrap();
        let read = read_lockfile(dir.path()).unwrap();

        lock.plugins.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(read, lock);
    }

    /// A missing lockfile reads as the empty default rather than an error,
    /// so the first `phux plugin install` needs no bootstrap step.
    #[test]
    fn missing_lockfile_reads_as_default() {
        let dir = tempfile::TempDir::new().unwrap();

        let read = read_lockfile(dir.path()).unwrap();

        assert_eq!(read, PluginLockFile::default());
        assert_eq!(read.version, 1);
        assert!(read.plugins.is_empty());
    }

    /// Upsert replaces by id instead of appending duplicates.
    #[test]
    fn upsert_replaces_existing_entry_by_id() {
        let mut lock = sample_lock();

        upsert_entry(
            &mut lock,
            PluginLockEntry {
                id: "example.git-plugin".to_owned(),
                source: PluginSourceKind::Git,
                source_ref: "https://example.invalid/plugin.git".to_owned(),
                branch: Some("main".to_owned()),
                subdir: Some("plugins/demo".to_owned()),
                rev: Some("fedcba9876543210fedcba9876543210fedcba98".to_owned()),
            },
        );

        assert_eq!(lock.plugins.len(), 2);
        let entry = lock
            .plugins
            .iter()
            .find(|e| e.id == "example.git-plugin")
            .unwrap();
        assert_eq!(
            entry.rev.as_deref(),
            Some("fedcba9876543210fedcba9876543210fedcba98")
        );
    }

    /// A corrupt lockfile is a hard, named error — never silently reset.
    #[test]
    fn corrupt_lockfile_is_a_named_error() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join(super::LOCKFILE_NAME), "version = [").unwrap();

        let err = read_lockfile(dir.path()).unwrap_err();

        assert!(err.contains("plugins.lock"), "{err}");
    }
}
