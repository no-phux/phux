use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use phux_config::loader as config_loader;
use phux_config::plugin::{self, PluginManifest};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

#[derive(Debug)]
pub(super) struct RegistryEntry {
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
            index,
            manifest_text,
            manifest_path,
            enabled: entry.enabled,
            manifest,
        });
    }
    Ok(entries)
}

pub(super) fn find_entry(config_path: &Path, id: &str) -> Result<RegistryEntry, String> {
    load_registry_from_path(config_path)?
        .into_iter()
        .find(|entry| entry.manifest.id == id)
        .ok_or_else(|| format!("plugin {id:?} is not registered"))
}

pub(super) fn read_config_document(config_path: &Path) -> Result<DocumentMut, String> {
    match std::fs::read_to_string(config_path) {
        Ok(input) => input
            .parse::<DocumentMut>()
            .map_err(|err| format!("could not parse {}: {err}", config_path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(DocumentMut::new()),
        Err(err) => Err(format!("could not read {}: {err}", config_path.display())),
    }
}

pub(super) fn write_config_document(config_path: &Path, doc: &DocumentMut) -> Result<(), String> {
    reject_symlinked_config(config_path)?;
    let parent = config_path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", config_path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    let file_name = config_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has no file name", config_path.display()))?;
    let tmp_path = parent.join(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        temp_nonce()
    ));
    let write_result = write_temp_file(&tmp_path, doc.to_string().as_bytes())
        .and_then(|()| std::fs::rename(&tmp_path, config_path).map_err(|err| err.to_string()));
    if let Err(err) = write_result {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!("could not write {}: {err}", config_path.display()));
    }
    Ok(())
}

pub(super) fn push_entry(
    doc: &mut DocumentMut,
    manifest: &str,
    enabled: bool,
) -> Result<(), String> {
    let plugins = plugin_tables_mut(doc)?;
    let mut table = Table::new();
    table.insert("manifest", value(manifest));
    table.insert("enabled", value(enabled));
    plugins.push(table);
    Ok(())
}

pub(super) fn update_entry(
    doc: &mut DocumentMut,
    index: usize,
    manifest: &str,
    enabled: bool,
) -> Result<(), String> {
    let table = plugin_table_mut(doc, index)?;
    table.insert("manifest", value(manifest));
    table.insert("enabled", value(enabled));
    Ok(())
}

pub(super) fn set_enabled(
    doc: &mut DocumentMut,
    index: usize,
    enabled: bool,
) -> Result<(), String> {
    let table = plugin_table_mut(doc, index)?;
    table.insert("enabled", value(enabled));
    Ok(())
}

pub(super) fn remove_entry(doc: &mut DocumentMut, index: usize) -> Result<(), String> {
    let plugins = plugin_tables_mut(doc)?;
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

fn plugin_table_mut(doc: &mut DocumentMut, index: usize) -> Result<&mut Table, String> {
    plugin_tables_mut(doc)?
        .get_mut(index)
        .ok_or_else(|| format!("plugin registry index {index} disappeared"))
}

fn plugin_tables_mut(doc: &mut DocumentMut) -> Result<&mut ArrayOfTables, String> {
    let item = doc
        .as_table_mut()
        .entry("plugins")
        .or_insert_with(|| Item::ArrayOfTables(ArrayOfTables::new()));
    item.as_array_of_tables_mut()
        .ok_or_else(|| "`plugins` must be an array of tables".to_owned())
}

pub(super) fn reject_symlinked_config(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(format!("{} must not be a symlink", path.display()))
        }
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("could not inspect {}: {err}", path.display())),
    }
}

pub(super) fn temp_nonce() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

fn write_temp_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|err| err.to_string())?;
    file.write_all(bytes).map_err(|err| err.to_string())?;
    file.sync_all().map_err(|err| err.to_string())
}
