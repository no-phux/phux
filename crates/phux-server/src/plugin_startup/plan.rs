//! Which `[[startup]]` entries this server should run.
//!
//! An entry runs only when the manifest's top-level `platforms` and the
//! entry's own `platforms` both allow this OS (an omitted list allows every
//! OS). At most [`MAX_STARTUP_ENTRIES`](phux_config::plugin::MAX_STARTUP_ENTRIES)
//! entries run, taken in `[[plugins]]` order and then manifest order. Later
//! ones are reported as skipped for the cap, not failed validation.

use std::path::Path;

use phux_config::plugin::{
    self, MAX_STARTUP_ENTRIES, PluginConfigEntry, PluginManifest, PluginPlatform,
};

/// One sidecar the supervisor knows how to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupSpec {
    /// Manifest `id`.
    pub plugin_id: String,
    /// Manifest `name`.
    pub plugin_name: String,
    /// Manifest `version`.
    pub plugin_version: String,
    /// `[[startup]]` id.
    pub startup_id: String,
    /// Argv. `argv[0]` is the program.
    pub command: Vec<String>,
    /// Directory containing the manifest. The child's cwd and `PHUX_PLUGIN_ROOT`.
    pub plugin_root: std::path::PathBuf,
    /// Hash of the manifest file bytes. A changed file is a different key,
    /// so that entry restarts and an unchanged one is left alone.
    pub manifest_hash: u64,
}

impl StartupSpec {
    /// Identity used to reconcile: plugin, startup id, and manifest bytes.
    #[must_use]
    pub fn key(&self) -> StartupKey {
        StartupKey {
            plugin_id: self.plugin_id.clone(),
            startup_id: self.startup_id.clone(),
            manifest_hash: self.manifest_hash,
        }
    }
}

/// Reconcile identity. Equal keys are the same sidecar and are not restarted.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StartupKey {
    /// Manifest `id`.
    pub plugin_id: String,
    /// `[[startup]]` id.
    pub startup_id: String,
    /// [`StartupSpec::manifest_hash`].
    pub manifest_hash: u64,
}

/// The sidecars to start, and the ones past the global cap.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartupCatalog {
    /// Entries that get a process, in start order.
    pub admitted: Vec<StartupSpec>,
    /// Entries past the global cap, in the same order. Each is logged
    /// `skipped: cap` and not started.
    pub skipped_cap: Vec<StartupSpec>,
}

/// Build a catalog from `[[plugins]]`, in the order they appear.
///
/// Disabled plugins are skipped. A manifest that fails to load is logged
/// and skipped, so one broken plugin does not stop the others. Platform
/// filtering happens here; validation has already accepted every platform
/// name so one OS can load a multi-platform manifest.
#[must_use]
pub fn catalog_from_plugins(config_path: &Path, plugins: &[PluginConfigEntry]) -> StartupCatalog {
    let mut catalog = StartupCatalog::default();
    for entry in plugins {
        if !entry.enabled {
            continue;
        }
        let manifest_path = plugin::resolve_manifest_path(&entry.manifest, config_path);
        let manifest = match plugin::load_plugin_manifest(&manifest_path) {
            Ok(manifest) => manifest,
            Err(err) => {
                tracing::warn!(
                    manifest = %manifest_path.display(),
                    error = %err,
                    "startup: could not load plugin manifest; skipping its sidecars"
                );
                continue;
            }
        };
        let hash = manifest_hash(&manifest.manifest_path);
        push_manifest(&mut catalog, &manifest, hash);
    }
    catalog
}

fn push_manifest(catalog: &mut StartupCatalog, manifest: &PluginManifest, hash: u64) {
    for startup in &manifest.startup {
        if !platforms_allow(manifest.platforms.as_deref(), startup.platforms.as_deref()) {
            tracing::debug!(
                plugin = %manifest.id,
                startup = %startup.id,
                "startup: platform does not match; skipping"
            );
            continue;
        }
        let spec = StartupSpec {
            plugin_id: manifest.id.clone(),
            plugin_name: manifest.name.clone(),
            plugin_version: manifest.version.clone(),
            startup_id: startup.id.clone(),
            command: startup.command.clone(),
            plugin_root: manifest.plugin_root.clone(),
            manifest_hash: hash,
        };
        if catalog.admitted.len() < MAX_STARTUP_ENTRIES {
            catalog.admitted.push(spec);
        } else {
            catalog.skipped_cap.push(spec);
        }
    }
}

/// Both lists must allow this OS. `None` allows every OS.
fn platforms_allow(manifest: Option<&[PluginPlatform]>, entry: Option<&[PluginPlatform]>) -> bool {
    list_allows(manifest) && list_allows(entry)
}

fn list_allows(platforms: Option<&[PluginPlatform]>) -> bool {
    let Some(platforms) = platforms else {
        return true;
    };
    current_platform().is_some_and(|current| platforms.contains(&current))
}

const fn current_platform() -> Option<PluginPlatform> {
    if cfg!(target_os = "macos") {
        Some(PluginPlatform::Macos)
    } else if cfg!(target_os = "linux") {
        Some(PluginPlatform::Linux)
    } else if cfg!(target_os = "windows") {
        Some(PluginPlatform::Windows)
    } else {
        None
    }
}

/// FNV-1a of the manifest bytes. Stable for the life of a process and across
/// processes, so a reload compares equal to the start that read the same file.
fn manifest_hash(path: &Path) -> u64 {
    match std::fs::read(path) {
        Ok(bytes) => hash_bytes(&bytes),
        Err(err) => {
            tracing::warn!(
                manifest = %path.display(),
                error = %err,
                "startup: could not read manifest bytes; treating the hash as changed"
            );
            0
        }
    }
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;

    fn write_plugin(dir: &Path, id: &str, body: &str) -> PluginConfigEntry {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("phux-plugin.toml");
        std::fs::write(
            &path,
            format!(
                "id = \"{id}\"\nname = \"{id}\"\nversion = \"0.1.0\"\nmin_phux_version = \"0.0.2\"\n{body}"
            ),
        )
        .unwrap();
        PluginConfigEntry {
            manifest: path,
            enabled: true,
        }
    }

    fn startup(id: &str, platforms: &str) -> String {
        format!("[[startup]]\nid = \"{id}\"\n{platforms}command = [\"sleep\", \"30\"]\n")
    }

    #[test]
    fn global_cap_follows_plugin_order_and_skips_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let mut a = String::new();
        for n in 0..5 {
            a.push_str(&startup(&format!("a{n}"), ""));
        }
        let mut b = String::new();
        for n in 0..4 {
            b.push_str(&startup(&format!("b{n}"), ""));
        }
        let plugins = vec![
            write_plugin(&tmp.path().join("a"), "plug.a", &a),
            write_plugin(&tmp.path().join("b"), "plug.b", &b),
        ];
        let catalog = catalog_from_plugins(tmp.path(), &plugins);
        let ids: Vec<_> = catalog
            .admitted
            .iter()
            .map(|spec| spec.startup_id.as_str())
            .collect();
        assert_eq!(ids, ["a0", "a1", "a2", "a3", "a4", "b0", "b1", "b2"]);
        assert_eq!(catalog.skipped_cap.len(), 1);
        assert_eq!(catalog.skipped_cap[0].startup_id, "b3");
        assert_eq!(catalog.skipped_cap[0].plugin_id, "plug.b");
    }

    #[test]
    fn disabled_plugins_do_not_consume_the_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let mut disabled = write_plugin(&tmp.path().join("off"), "plug.off", &startup("off", ""));
        disabled.enabled = false;
        let on = write_plugin(&tmp.path().join("on"), "plug.on", &startup("on", ""));
        let catalog = catalog_from_plugins(tmp.path(), &[disabled, on]);
        assert_eq!(catalog.admitted.len(), 1);
        assert_eq!(catalog.admitted[0].startup_id, "on");
        assert!(catalog.skipped_cap.is_empty());
    }

    #[test]
    fn entry_platforms_are_anded_with_the_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let this = if cfg!(target_os = "linux") {
            "linux"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "windows"
        };
        let other = if this == "linux" { "windows" } else { "linux" };
        let body = format!(
            "platforms = [\"{this}\"]\n{}\n{}\n{}\n",
            startup("both", &format!("platforms = [\"{this}\"]\n")),
            startup("entry-other", &format!("platforms = [\"{other}\"]\n")),
            startup("manifest-only", ""),
        );
        let wrong_manifest = format!(
            "platforms = [\"{other}\"]\n{}",
            startup("manifest-blocks", &format!("platforms = [\"{this}\"]\n")),
        );
        let plugins = vec![
            write_plugin(&tmp.path().join("ok"), "plug.ok", &body),
            write_plugin(&tmp.path().join("no"), "plug.no", &wrong_manifest),
        ];
        let catalog = catalog_from_plugins(tmp.path(), &plugins);
        let ids: Vec<_> = catalog
            .admitted
            .iter()
            .map(|spec| spec.startup_id.as_str())
            .collect();
        assert_eq!(ids, ["both", "manifest-only"]);
        assert!(catalog.skipped_cap.is_empty());
    }

    #[test]
    fn the_same_file_bytes_hash_equal_and_a_change_does_not() {
        let tmp = tempfile::tempdir().unwrap();
        let plugin = write_plugin(&tmp.path().join("p"), "plug.p", &startup("web", ""));
        let first = catalog_from_plugins(tmp.path(), std::slice::from_ref(&plugin));
        let second = catalog_from_plugins(tmp.path(), std::slice::from_ref(&plugin));
        assert_eq!(
            first.admitted[0].manifest_hash,
            second.admitted[0].manifest_hash
        );
        let path = tmp.path().join("p").join("phux-plugin.toml");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push('\n');
        std::fs::write(&path, text).unwrap();
        let third = catalog_from_plugins(tmp.path(), &[plugin]);
        assert_ne!(
            first.admitted[0].manifest_hash,
            third.admitted[0].manifest_hash
        );
    }
}
