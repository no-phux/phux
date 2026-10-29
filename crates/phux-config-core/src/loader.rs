//! Where `config.toml` lives, and the client-neutral view of it.
//!
//! [`load_client_from`] merges the same `extends` stack as `phux-config`'s
//! full loader but deserializes only [`ClientConfig`]'s tables, so a client
//! that only dials is unaffected by tables it does not read. The shipped
//! `default.toml` sets none of those tables (`phux-config` tests that), so
//! the stack here starts empty.

use std::path::{Path, PathBuf};
use std::{fs, io};

use serde::Deserialize;

use crate::{ConfigError, OrganizeCfg, RemoteConfigEntry, layer};

/// The config path: `$XDG_CONFIG_HOME/phux/config.toml`, else
/// `$HOME/.config/phux/config.toml` (no I/O).
#[must_use]
#[cfg(not(windows))]
pub fn config_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map_or_else(
            || {
                std::env::var_os("HOME")
                    .map_or_else(PathBuf::new, PathBuf::from)
                    .join(".config")
            },
            PathBuf::from,
        )
        .join("phux")
        .join("config.toml")
}

/// Native Windows configuration in the user's local application-data directory.
#[must_use]
#[cfg(windows)]
pub fn config_path() -> PathBuf {
    crate::instance::windows_data_dir()
        .join("phux")
        .join("config.toml")
}

/// The tables of `config.toml` every client reads. Other tables are left to
/// the client that owns them; these keep their own unknown-field checks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ClientConfig {
    /// `[[remote]]`: the client-side remote-server registry (ADR-0055).
    pub remote: Vec<RemoteConfigEntry>,
    /// `[organize]`: the judge behind `phux organize`.
    pub organize: OrganizeCfg,
}

/// Parse `user_input` and its `extends` layers into a [`ClientConfig`].
/// `path` anchors relative `extends` entries and labels errors.
///
/// # Errors
///
/// [`ConfigError::Parse`] for invalid TOML in any layer or a schema failure
/// in a table [`ClientConfig`] reads; the layer-resolution errors of
/// [`layer::merged_with_budget`].
pub fn parse_client(user_input: &str, path: &Path) -> Result<ClientConfig, ConfigError> {
    let (merged, _) = layer::merged_with_budget("", user_input, path, None)?;
    toml::Value::Table(merged)
        .try_into()
        .map_err(|e: toml::de::Error| ConfigError::parse(path, user_input, e.span(), e.message()))
}

/// [`load_client_from`] at [`config_path`].
///
/// # Errors
///
/// See [`load_client_from`].
pub fn load_client() -> Result<ClientConfig, ConfigError> {
    load_client_from(&config_path())
}

/// Load the [`ClientConfig`] from `path`; a missing file is the defaults.
///
/// # Errors
///
/// [`ConfigError::Io`] for a read failure other than not-found; any
/// [`parse_client`] error.
pub fn load_client_from(path: &Path) -> Result<ClientConfig, ConfigError> {
    match fs::read_to_string(path) {
        Ok(contents) => parse_client(&contents, path),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            tracing::debug!(path = %path.display(), "phux config not present; using defaults");
            Ok(ClientConfig::default())
        }
        Err(err) => Err(ConfigError::Io(err)),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{ClientConfig, parse_client};

    #[test]
    fn reads_its_tables_through_extends_and_ignores_the_rest() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("shared.toml"),
            "[[remote]]\nname = 'box'\nendpoint = 'ssh://box'\n",
        )
        .expect("layer");
        let input = "extends = ['shared.toml']\n\
                     [keybindings]\nprefix = 'C-b'\n\
                     [organize]\noffline = true\n";
        let config = parse_client(input, &dir.path().join("config.toml")).expect("parses");
        assert_eq!(config.remote.len(), 1);
        assert_eq!(config.remote[0].name, "box");
        assert!(config.organize.offline);
    }

    #[test]
    fn its_own_tables_still_reject_unknown_fields() {
        let err = parse_client("[organize]\nofline = true\n", Path::new("config.toml"))
            .expect_err("typo in a read table");
        assert!(err.to_string().contains("ofline"), "{err}");
        assert_eq!(
            parse_client("", Path::new("config.toml")).expect("empty"),
            ClientConfig::default()
        );
    }
}
