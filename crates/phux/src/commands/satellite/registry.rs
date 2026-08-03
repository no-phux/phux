use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use phux_config::SatelliteConfigEntry;
use phux_config::loader as config_loader;
use toml_edit::{ArrayOfTables, DocumentMut, Table, value};

use crate::commands::toml_registry;

/// The `config.toml` key this registry owns.
const KEY: &str = "satellites";

#[derive(Debug, Clone)]
pub(crate) struct SatelliteEntry {
    pub(crate) index: usize,
    pub(crate) name: String,
    pub(crate) endpoint: String,
    pub(crate) enabled: bool,
    /// Path to the pairing-token file (ADR-0038). The path is displayable;
    /// the token bytes behind it are the secret and are never read here.
    pub(crate) token_file: Option<PathBuf>,
    /// TLS certificate SHA-256 pin, as printed by `phux pair`. Not a secret.
    pub(crate) cert_fingerprint: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct NewSatellite {
    name: String,
    endpoint: String,
    enabled: bool,
    token_file: Option<PathBuf>,
    cert_fingerprint: Option<String>,
}

impl NewSatellite {
    pub(crate) fn new(
        name: &str,
        endpoint: &str,
        enabled: bool,
        token_file: Option<&Path>,
        cert_fingerprint: Option<&str>,
    ) -> Result<Self, String> {
        Ok(Self {
            name: registry_name(name)?,
            endpoint: registry_endpoint(endpoint)?,
            enabled,
            token_file: token_file.map(registry_token_file).transpose()?,
            cert_fingerprint: cert_fingerprint.map(registry_fingerprint).transpose()?,
        })
    }
}

pub(crate) fn load_registry() -> Result<Vec<SatelliteEntry>, String> {
    let cfg = config_loader::load().map_err(|err| err.to_string())?;
    let mut seen = BTreeSet::new();
    let mut entries = Vec::with_capacity(cfg.satellites.len());
    for (index, satellite) in cfg.satellites.into_iter().enumerate() {
        if !seen.insert(satellite.name.clone()) {
            return Err(format!("duplicate satellite name {:?}", satellite.name));
        }
        entries.push(entry_from_config(index, satellite));
    }
    Ok(entries)
}

pub(crate) fn find_entry(name: &str) -> Result<SatelliteEntry, String> {
    load_registry()?
        .into_iter()
        .find(|entry| entry.name == name)
        .ok_or_else(|| format!("satellite {name:?} is not registered"))
}

pub(crate) fn add_or_update(new: &NewSatellite) -> Result<SatelliteEntry, String> {
    let config_path = config_loader::config_path();
    toml_registry::reject_symlink(&config_path)?;
    let mut doc = toml_registry::read_document(&config_path)?;
    let mut updated = false;
    for entry in load_registry()? {
        if entry.name == new.name {
            update_entry(&mut doc, entry.index, new)?;
            updated = true;
            break;
        }
    }
    if !updated {
        push_entry(&mut doc, new)?;
    }
    toml_registry::write_document(&config_path, &doc)?;
    Ok(SatelliteEntry {
        index: 0,
        name: new.name.clone(),
        endpoint: new.endpoint.clone(),
        enabled: new.enabled,
        token_file: new.token_file.clone(),
        cert_fingerprint: new.cert_fingerprint.clone(),
    })
}

pub(crate) fn remove_entry(index: usize) -> Result<(), String> {
    let config_path = config_loader::config_path();
    toml_registry::reject_symlink(&config_path)?;
    let mut doc = toml_registry::read_document(&config_path)?;
    let satellites = satellite_tables_mut(&mut doc)?;
    if index >= satellites.len() {
        return Err(format!("satellite registry index {index} disappeared"));
    }
    satellites.remove(index);
    toml_registry::write_document(&config_path, &doc)
}

fn entry_from_config(index: usize, satellite: SatelliteConfigEntry) -> SatelliteEntry {
    SatelliteEntry {
        index,
        name: satellite.name,
        endpoint: satellite.endpoint,
        enabled: satellite.enabled,
        token_file: satellite.token_file,
        cert_fingerprint: satellite.cert_fingerprint,
    }
}

fn registry_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.contains('/') || trimmed.contains(':') {
        Err("satellite name must be non-empty and must not contain '/' or ':'".to_owned())
    } else {
        Ok(trimmed.to_owned())
    }
}

fn registry_endpoint(endpoint: &str) -> Result<String, String> {
    let trimmed = endpoint.trim();
    if trimmed.is_empty() || !trimmed.contains("://") {
        Err("satellite endpoint must be a URI such as ssh://devbox".to_owned())
    } else {
        Ok(trimmed.to_owned())
    }
}

/// The token file is referenced from `config.toml` and later read by the hub
/// daemon, whose working directory is unrelated to where `phux satellite add`
/// ran — so a relative path would silently point somewhere else. Require an
/// absolute path. Existence is NOT required: pairing material may land after
/// registration, mirroring how the server token store tolerates a
/// not-yet-created path.
fn registry_token_file(path: &Path) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() || !path.is_absolute() {
        Err(format!(
            "satellite token-file must be an absolute path (got {})",
            path.display()
        ))
    } else {
        Ok(path.to_path_buf())
    }
}

/// A certificate pin is SHA-256 output: exactly 64 hex digits once the
/// `AB:CD:...` separators `phux pair` prints are dropped. Validating here
/// catches a truncated copy-paste at registration time instead of as a
/// baffling handshake failure when the dialer (phux-v45.3) uses the pin.
fn registry_fingerprint(fingerprint: &str) -> Result<String, String> {
    toml_registry::validate_fingerprint(fingerprint, "satellite")
}

fn push_entry(doc: &mut DocumentMut, new: &NewSatellite) -> Result<(), String> {
    satellite_tables_mut(doc)?.push(entry_table(new));
    Ok(())
}

fn update_entry(doc: &mut DocumentMut, index: usize, new: &NewSatellite) -> Result<(), String> {
    let table = satellite_table_mut(doc, index)?;
    table.insert("name", value(&new.name));
    table.insert("endpoint", value(&new.endpoint));
    table.insert("enabled", value(new.enabled));
    set_auth_material(table, new);
    Ok(())
}

fn entry_table(new: &NewSatellite) -> Table {
    let mut table = Table::new();
    table.insert("name", value(&new.name));
    table.insert("endpoint", value(&new.endpoint));
    table.insert("enabled", value(new.enabled));
    set_auth_material(&mut table, new);
    table
}

/// Write (or, on an update that omitted the flags, clear) the ADR-0038 auth
/// keys. `add` replaces the whole entry, so an absent flag removes the key
/// rather than silently keeping stale auth material for a new endpoint.
fn set_auth_material(table: &mut Table, new: &NewSatellite) {
    match &new.token_file {
        Some(path) => {
            table.insert("token-file", value(path.display().to_string()));
        }
        None => {
            table.remove("token-file");
        }
    }
    match &new.cert_fingerprint {
        Some(fingerprint) => {
            table.insert("cert-fingerprint", value(fingerprint));
        }
        None => {
            table.remove("cert-fingerprint");
        }
    }
}

fn satellite_table_mut(doc: &mut DocumentMut, index: usize) -> Result<&mut Table, String> {
    toml_registry::table_mut(doc, KEY, index)
}

fn satellite_tables_mut(doc: &mut DocumentMut) -> Result<&mut ArrayOfTables, String> {
    toml_registry::tables_mut(doc, KEY)
}
