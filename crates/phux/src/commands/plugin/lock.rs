//! Managed-install lockfile (`plugins.lock`): records where every
//! `phux plugin install`ed package came from — the source kind, the
//! original ref (git URL / local path), the requested branch or tag, the
//! subdirectory inside a git clone, and the resolved commit for git sources —
//! so `phux plugin update` can re-fetch deterministically without re-asking
//! the user.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use phux_config::plugin::PluginManifestBuild;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
    /// SHA-256 of the `[[build]]` steps last approved with `--yes` or a
    /// terminal answer. Absent when the plugin has no build steps, or the
    /// install predates this field (an update then asks again).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) build_steps: Option<String>,
}

/// SHA-256 of approved `[[build]]` steps. `None` when there are none.
///
/// Encoding is a count, then each field as `len:bytes`, so an argument that
/// contains the old separator (`\u{1f}` or a newline) cannot collide with
/// two neighbouring arguments. A lockfile written with the previous
/// encoding does not match and `update` asks again.
pub(super) fn hash_build_steps(steps: &[PluginManifestBuild]) -> Option<String> {
    if steps.is_empty() {
        return None;
    }
    let mut hasher = Sha256::new();
    feed_count(&mut hasher, steps.len());
    for step in steps {
        let platforms = step.platforms.as_deref().unwrap_or(&[]);
        feed_count(&mut hasher, platforms.len());
        for platform in platforms {
            feed_bytes(&mut hasher, format!("{platform:?}").as_bytes());
        }
        feed_count(&mut hasher, step.command.len());
        for arg in &step.command {
            feed_bytes(&mut hasher, arg.as_bytes());
        }
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    Some(out)
}

fn feed_count(hasher: &mut Sha256, count: usize) {
    hasher.update(format!("{count}:").as_bytes());
}

fn feed_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    feed_count(hasher, bytes.len());
    hasher.update(bytes);
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
/// workspace, so two concurrent installs can lose one lockfile write.
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
                    build_steps: Some("abc".to_owned()),
                },
                PluginLockEntry {
                    id: "example.dir-plugin".to_owned(),
                    source: PluginSourceKind::Dir,
                    source_ref: "/srv/plugins/dir-plugin".to_owned(),
                    branch: None,
                    subdir: None,
                    rev: None,
                    build_steps: None,
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
                build_steps: Some("abc".to_owned()),
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

    fn step(argv: &[&str]) -> phux_config::plugin::PluginManifestBuild {
        phux_config::plugin::PluginManifestBuild {
            platforms: None,
            command: argv.iter().map(|arg| (*arg).to_owned()).collect(),
        }
    }

    /// The previous encoding: `{platforms}|{args joined by \u{1f}}` per step.
    fn legacy_build_hash(steps: &[phux_config::plugin::PluginManifestBuild]) -> String {
        use sha2::{Digest, Sha256};
        use std::fmt::Write as _;

        let mut hasher = Sha256::new();
        for step in steps {
            let line = format!("|{}", step.command.join("\u{1f}"));
            hasher.update(line.as_bytes());
            hasher.update(b"\n");
        }
        let digest = hasher.finalize();
        let mut out = String::with_capacity(digest.len() * 2);
        for byte in digest {
            let _ = write!(out, "{byte:02x}");
        }
        out
    }

    /// Args used to be joined with `\u{1f}`, so `["a","b"]` and `["a\u{1f}b"]`
    /// were one hash. A lockfile still holding that digest does not match.
    #[test]
    fn build_step_hash_does_not_collide_on_separator_bytes() {
        let split = step(&["sh", "build.sh", "a", "b"]);
        let glued = step(&["sh", "build.sh", "a\u{1f}b"]);
        let split_hash = super::hash_build_steps(std::slice::from_ref(&split)).unwrap();
        let glued_hash = super::hash_build_steps(std::slice::from_ref(&glued)).unwrap();
        assert_ne!(split_hash, glued_hash);

        let legacy = legacy_build_hash(std::slice::from_ref(&split));
        assert_eq!(legacy, legacy_build_hash(std::slice::from_ref(&glued)));
        assert_ne!(
            split_hash, legacy,
            "an old lockfile hash must not match, so update asks again"
        );
    }
}
