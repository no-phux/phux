use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use phux_config::document::{ConfigDocument, Guard, SymlinkPolicy};
use phux_config::loader as config_loader;
use phux_config::plugin::{self, PluginManifest};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

#[derive(Debug)]
pub(super) struct RegistryEntry {
    /// The array of tables in the user's file holding this entry: `plugins`
    /// or `plugins-append`.
    pub(super) key: &'static str,
    pub(super) index: usize,
    pub(super) manifest_text: String,
    pub(super) manifest_path: PathBuf,
    pub(super) enabled: bool,
    pub(super) manifest: PluginManifest,
}

pub(super) fn load_registry() -> Result<Vec<RegistryEntry>, String> {
    load_registry_from_path(&config_loader::config_path())
}

pub(super) fn load_registry_from_path(config_path: &Path) -> Result<Vec<RegistryEntry>, String> {
    let cfg = config_loader::load_from(config_path).map_err(|err| err.to_string())?;
    let mut entries = Vec::with_capacity(cfg.plugins.len());
    let mut seen_ids = BTreeSet::new();
    for (index, entry) in cfg.plugins.into_iter().enumerate() {
        let manifest_text = entry.manifest.to_string_lossy().into_owned();
        let manifest_path = plugin::resolve_manifest_path(&entry.manifest, config_path);
        let manifest = plugin::load_plugin_manifest(&manifest_path)
            .map_err(|err| format!("could not load {}: {err}", manifest_path.display()))?;
        if !seen_ids.insert(manifest.id.clone()) {
            return Err(format!("duplicate plugin id {:?}", manifest.id));
        }
        entries.push(RegistryEntry {
            key: PLUGINS,
            index,
            manifest_text,
            manifest_path,
            enabled: entry.enabled,
            manifest,
        });
    }
    Ok(entries)
}

/// The registered plugin `id`, with `index` pointing at its own
/// `[[plugins]]` table in `config_path`. A plugin an `extends` layer
/// contributes has no table there to edit, so it is an error naming the
/// way to drop it rather than an edit of whichever entry shares its index.
///
/// `doc` is the locked [`ConfigDocument`] the caller will edit, so the
/// index is resolved against the very text the commit checks.
pub(super) fn find_entry(
    doc: &DocumentMut,
    config_path: &Path,
    id: &str,
) -> Result<RegistryEntry, String> {
    let mut entry = load_registry_from_path(config_path)?
        .into_iter()
        .find(|entry| entry.manifest.id == id)
        .ok_or_else(|| format!("plugin {id:?} is not registered"))?;
    (entry.key, entry.index) =
        own_index(doc, config_path, &entry.manifest_path).ok_or_else(|| {
            format!(
                "plugin {id:?} comes from a config layer (`extends`), not {}; \
             to drop it, assign `plugins = [...]` there listing the plugins to keep",
                config_path.display()
            )
        })?;
    Ok(entry)
}

/// Where `doc` (the text of `config_path` itself) declares the plugin whose
/// manifest is `manifest_path`: the array (`plugins` or `plugins-append`)
/// and index.
pub(super) fn own_index(
    doc: &DocumentMut,
    config_path: &Path,
    manifest_path: &Path,
) -> Option<(&'static str, usize)> {
    let canonical =
        |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let wanted = canonical(manifest_path);
    for key in [PLUGINS, PLUGINS_APPEND] {
        let Some(tables) = doc.get(key).and_then(Item::as_array_of_tables) else {
            continue;
        };
        let found = tables.iter().position(|table| {
            table
                .get("manifest")
                .and_then(Item::as_str)
                .is_some_and(|text| {
                    canonical(&plugin::resolve_manifest_path(Path::new(text), config_path))
                        == wanted
                })
        });
        if let Some(index) = found {
            return Some((key, index));
        }
    }
    None
}

const PLUGINS: &str = "plugins";
const PLUGINS_APPEND: &str = "plugins-append";

/// Lock and read `config_path` for a plugin registry edit. The registry
/// decides what code the production server runs: a dev build aimed at
/// production state is refused, and so is a symlinked config.
pub(super) fn open_config(config_path: &Path) -> Result<ConfigDocument, String> {
    Ok(ConfigDocument::open(
        config_path,
        Guard::ProductionState,
        SymlinkPolicy::Refuse,
    )?)
}

pub(super) fn push_entry(
    doc: &mut DocumentMut,
    manifest: &str,
    enabled: bool,
) -> Result<(), String> {
    let plugins = plugin_tables_mut(doc, PLUGINS)?;
    let mut table = Table::new();
    table.insert("manifest", value(manifest));
    table.insert("enabled", value(enabled));
    plugins.push(table);
    Ok(())
}

pub(super) fn update_entry(
    doc: &mut DocumentMut,
    (key, index): (&'static str, usize),
    manifest: &str,
    enabled: bool,
) -> Result<(), String> {
    let table = plugin_table_mut(doc, key, index)?;
    table.insert("manifest", value(manifest));
    table.insert("enabled", value(enabled));
    Ok(())
}

pub(super) fn set_enabled(
    doc: &mut DocumentMut,
    key: &'static str,
    index: usize,
    enabled: bool,
) -> Result<(), String> {
    let table = plugin_table_mut(doc, key, index)?;
    table.insert("enabled", value(enabled));
    Ok(())
}

pub(super) fn remove_entry(
    doc: &mut DocumentMut,
    key: &'static str,
    index: usize,
) -> Result<(), String> {
    let plugins = plugin_tables_mut(doc, key)?;
    if index >= plugins.len() {
        return Err(format!("plugin registry index {index} disappeared"));
    }
    plugins.remove(index);
    Ok(())
}

pub(super) fn manifest_path_for_config(manifest_path: &Path, config_path: &Path) -> String {
    let Some(config_dir) = config_path.parent() else {
        return manifest_path.display().to_string();
    };
    manifest_path.strip_prefix(config_dir).map_or_else(
        |_| manifest_path.display().to_string(),
        |relative| format!("./{}", relative.display()),
    )
}

fn plugin_table_mut<'a>(
    doc: &'a mut DocumentMut,
    key: &'static str,
    index: usize,
) -> Result<&'a mut Table, String> {
    plugin_tables_mut(doc, key)?
        .get_mut(index)
        .ok_or_else(|| format!("plugin registry index {index} disappeared"))
}

fn plugin_tables_mut<'a>(
    doc: &'a mut DocumentMut,
    key: &'static str,
) -> Result<&'a mut ArrayOfTables, String> {
    let item = doc
        .as_table_mut()
        .entry(key)
        .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()));
    item.as_array_of_tables_mut()
        .ok_or_else(|| format!("`{key}` must be an array of tables"))
}

pub(super) fn temp_nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn write_plugin(dir: &Path, id: &str) -> PathBuf {
        let root = dir.join(id);
        std::fs::create_dir_all(&root).expect("plugin dir");
        let manifest = root.join("phux-plugin.toml");
        std::fs::write(
            &manifest,
            format!("id = \"{id}\"\nname = \"{id}\"\nversion = \"0.1.0\"\nmin_phux_version = \"0.0.1\"\n"),
        )
        .expect("manifest");
        manifest
    }

    /// A layer-contributed plugin is refused by name; a plugin of the
    /// file's own is found at its own table, not at its merged position.
    #[test]
    fn find_entry_edits_only_the_files_own_tables() {
        let dir = tempfile::tempdir().expect("temp dir");
        let layered = write_plugin(dir.path(), "layered");
        let mine = write_plugin(dir.path(), "mine");
        let layer = dir.path().join("layer.toml");
        std::fs::write(
            &layer,
            format!("[[plugins-append]]\nmanifest = \"{}\"\n", layered.display()),
        )
        .expect("layer");
        let config = dir.path().join("config.toml");
        std::fs::write(
            &config,
            format!(
                "extends = [\"{}\"]\n\n[[plugins-append]]\nmanifest = \"{}\"\n",
                layer.display(),
                mine.display()
            ),
        )
        .expect("config");
        let mut doc = open_config(&config).expect("open");
        let refused = find_entry(&doc, &config, "layered").expect_err("inherited");
        assert!(refused.contains("config layer"), "{refused}");
        let entry = find_entry(&doc, &config, "mine").expect("own entry");
        assert_eq!(
            (entry.key, entry.index),
            ("plugins-append", 0),
            "its own table, not merged position 1"
        );
        set_enabled(&mut doc, entry.key, entry.index, false).expect("disable");
        doc.commit().expect("write");
        let reloaded = load_registry_from_path(&config).expect("reload");
        let enabled: Vec<(&str, bool)> = reloaded
            .iter()
            .map(|e| (e.manifest.id.as_str(), e.enabled))
            .collect();
        assert_eq!(enabled, [("layered", true), ("mine", false)]);
    }

    #[test]
    fn a_registry_write_keeps_the_config_file_mode() {
        let dir = tempfile::tempdir().expect("temp dir");
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "# mine\n").expect("seed config");
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600))
            .expect("restrict config");
        let mut doc = open_config(&config).expect("open config");
        push_entry(&mut doc, "./plugins/demo/plugin.toml", true).expect("add entry");
        doc.commit().expect("write config");
        let mode = std::fs::metadata(&config)
            .expect("stat config")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let text = std::fs::read_to_string(&config).expect("reread config");
        assert!(
            text.contains("# mine") && text.contains("[[plugins]]"),
            "{text}"
        );
    }
}
