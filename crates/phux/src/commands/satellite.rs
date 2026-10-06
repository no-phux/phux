//! The `[[satellites]]` machine registry behind `phux host --role satellite`
//! (ADR-0038, ADR-0066).

use std::path::{Path, PathBuf};

use phux_config::SatelliteConfigEntry;
use phux_config::loader as config_loader;
use toml_edit::{Table, value};

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
    toml_registry::reject_duplicate_names(
        cfg.satellites.iter().map(|s| s.name.as_str()),
        "satellite",
    )?;
    Ok(cfg
        .satellites
        .into_iter()
        .enumerate()
        .map(|(index, satellite)| entry_from_config(index, satellite))
        .collect())
}

pub(crate) fn add_or_update(new: &NewSatellite) -> Result<SatelliteEntry, String> {
    toml_registry::upsert_machine(
        &config_loader::config_path(),
        KEY,
        || {
            Ok(load_registry()?
                .into_iter()
                .find(|entry| entry.name == new.name)
                .map(|entry| entry.index))
        },
        |table| fill_table(table, new),
    )?;
    Ok(SatelliteEntry {
        index: 0,
        name: new.name.clone(),
        endpoint: new.endpoint.clone(),
        enabled: new.enabled,
        token_file: new.token_file.clone(),
        cert_fingerprint: new.cert_fingerprint.clone(),
    })
}

pub(crate) fn remove_entry(entry: &SatelliteEntry) -> Result<(), String> {
    toml_registry::remove_machine_entry(
        &config_loader::config_path(),
        KEY,
        &entry.name,
        &entry.endpoint,
    )
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

pub(crate) fn registry_name(name: &str) -> Result<String, String> {
    toml_registry::plain_machine_name(name)
        .map(str::to_owned)
        .ok_or_else(|| {
            "satellite name must be non-empty and must not contain '/' or ':'".to_owned()
        })
}

fn registry_endpoint(endpoint: &str) -> Result<String, String> {
    let trimmed = endpoint.trim();
    if trimmed.is_empty() || !trimmed.contains("://") {
        Err("satellite endpoint must be a URI such as ssh://devbox".to_owned())
    } else {
        Ok(trimmed.to_owned())
    }
}

/// The token file is later read by the hub daemon, not where
/// `phux host add --role satellite` ran, so it must be absolute.
fn registry_token_file(path: &Path) -> Result<PathBuf, String> {
    toml_registry::validate_token_file(path, "satellite")
}

/// A certificate pin is SHA-256 output: exactly 64 hex digits once the
/// `AB:CD:...` separators `phux pair` prints are dropped. Validating here
/// catches a truncated copy-paste at registration time instead of as a
/// baffling handshake failure when the dialer (phux-v45.3) uses the pin.
fn registry_fingerprint(fingerprint: &str) -> Result<String, String> {
    toml_registry::validate_fingerprint(fingerprint, "satellite")
}

/// Write the whole entry. `add` replaces it, so an absent ADR-0038 auth flag
/// removes the key rather than keeping stale auth material for a new endpoint.
fn fill_table(table: &mut Table, new: &NewSatellite) {
    table.insert("name", value(&new.name));
    table.insert("endpoint", value(&new.endpoint));
    table.insert("enabled", value(new.enabled));
    let token_file = new
        .token_file
        .as_ref()
        .map(|path| path.display().to_string());
    toml_registry::set_or_remove(table, "token-file", token_file);
    toml_registry::set_or_remove(table, "cert-fingerprint", new.cert_fingerprint.as_ref());
}
