//! Config loader (`docs/consumers/tui.md` §4.1): a missing config file means
//! the shipped defaults; a missing `extends` layer is an error.

use std::path::Path;
use std::{fs, io};

pub use phux_config_core::loader::config_path;

use crate::{Config, ConfigError, parse_with_defaults};

/// Load the config from [`config_path`].
///
/// # Errors
///
/// See [`load_from`].
pub fn load() -> Result<Config, ConfigError> {
    load_from(&config_path())
}

/// Load the config from `path` over the shipped defaults; a missing file is
/// no overrides.
///
/// # Errors
///
/// [`ConfigError::Io`] for a read failure other than not-found; any
/// [`parse_with_defaults`] error.
pub fn load_from(path: &Path) -> Result<Config, ConfigError> {
    match fs::read_to_string(path) {
        Ok(contents) => parse_with_defaults(&contents, path),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            tracing::debug!(
                path = %path.display(),
                "phux config not present; using embedded defaults"
            );
            parse_with_defaults("", path)
        }
        Err(err) => Err(ConfigError::Io(err)),
    }
}
