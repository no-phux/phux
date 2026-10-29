//! Static phux plugin index.
//!
//! Discovers public repositories tagged `phux-plugin`, reads each
//! `phux-plugin.toml` (repository root, or directories listed in
//! `phux-plugin-dirs.toml`), and checks it with the same rules as
//! `phux plugin validate`: [`phux_config::plugin::load_plugin_manifest`]
//! plus [`phux_config::plugin::dead_events`]. Example plugins under
//! `examples/plugins` are included from the local checkout.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod github;
mod index;
mod render;
mod validate;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

pub use render::{render_json, render_markdown};

/// Document version written into every index.
///
/// Adding, removing, or renaming a field bumps this. A reader that only
/// knows an older `schema_version` must not assume the new fields are absent
/// in a compatible way: an additive field is still a new document.
pub const SCHEMA_VERSION: u32 = 1;

/// GitHub requests one index build will make before later repositories are
/// skipped. Search pages count, and so does each manifest fetch.
pub const MAX_HTTP_REQUESTS: u32 = 500;

/// The line the index carries about how plugins run.
pub const NOTICE: &str = "Plugins run as the user and aren't sandboxed (see docs/plugins.md).";

/// Failure while building or writing an index.
#[derive(Debug)]
pub struct Error {
    message: String,
}

impl Error {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// Where repository metadata and manifests come from.
#[derive(Debug, Clone)]
pub enum Source {
    /// Fixture tree (`search.json`, `repos/<owner>/<repo>/...`). No network.
    Offline {
        /// Directory of captured API responses and manifests.
        fixtures: PathBuf,
    },
    /// Public GitHub REST API. `token` is sent only as a bearer token.
    Github {
        /// `GITHUB_TOKEN` when one was provided. Empty means unauthenticated.
        token: Option<String>,
        /// Remaining HTTP calls for this build. Shared across every fetch.
        budget: Arc<AtomicU32>,
    },
}

impl Source {
    /// GitHub source with a fresh [`MAX_HTTP_REQUESTS`] budget.
    #[must_use]
    pub fn github(token: Option<String>) -> Self {
        Self::Github {
            token,
            budget: Arc::new(AtomicU32::new(MAX_HTTP_REQUESTS)),
        }
    }
}

/// Inputs for one index build.
#[derive(Debug, Clone)]
pub struct Options {
    /// GitHub responses, or a fixture directory.
    pub source: Source,
    /// Directory whose children are example plugins (`examples/plugins`).
    pub examples_dir: PathBuf,
    /// `owner/repo` recorded for those examples.
    pub examples_repo: String,
    /// Checkout root. Example `subdir` values are relative to this.
    pub repo_root: PathBuf,
    /// `YYYY-MM-DDTHH:MM:SSZ` stamped into the document.
    pub generated_at: String,
}

/// One published plugin.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Plugin {
    /// `owner/repo`.
    pub repo: String,
    /// Path of the plugin inside the repo. Empty when the manifest is at the root.
    pub subdir: String,
    /// Manifest `id`.
    pub id: String,
    /// Manifest `name`.
    pub name: String,
    /// Manifest `description`, or empty when the manifest omits it.
    pub description: String,
    /// Non-empty manifest sections, in a fixed order.
    pub kinds: Vec<String>,
    /// Manifest `min_phux_version`.
    pub min_phux_version: String,
    /// GitHub stargazer count, when the repository metadata was available.
    pub stars: Option<u64>,
    /// GitHub `pushed_at`, when the repository metadata was available.
    pub pushed_at: Option<String>,
    /// SPDX license id, when GitHub reported one.
    pub license: Option<String>,
    /// `phux plugin install https://github.com/owner/repo`.
    pub install: String,
}

/// A repository or directory the index left out, and why.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Skipped {
    /// `owner/repo`.
    pub repo: String,
    /// Plugin directory, or empty when the whole repository was skipped.
    pub subdir: String,
    /// Stable reason: `archived`, `fork`, `private`, `no valid manifest`, or `invalid ...`.
    pub reason: String,
}

/// The generated document.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Index {
    /// [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Build timestamp, `YYYY-MM-DDTHH:MM:SSZ`.
    pub generated_at: String,
    /// [`NOTICE`].
    pub notice: String,
    /// Plugins that passed validation, sorted by repo, subdirectory, name, id.
    pub plugins: Vec<Plugin>,
    /// Repositories and directories that were not listed, sorted the same way.
    pub skipped: Vec<Skipped>,
}

/// Build the index. Offline sources never open a network connection.
///
/// # Errors
///
/// Returns an error when GitHub cannot be queried, a fixture is unreadable,
/// or example plugins cannot be read. An invalid manifest is recorded in
/// [`Index::skipped`] and is not an error.
pub fn build_index(options: &Options) -> Result<Index, Error> {
    index::build(options)
}

/// Bearer token for the public API.
///
/// Offline builds ignore `raw`. An empty value is unauthenticated. A token
/// that cannot be placed in an Authorization header is rejected.
pub fn credential_from(offline: bool, raw: Option<&str>) -> Result<Option<String>, Error> {
    if offline {
        return Ok(None);
    }
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }
    if raw.len() > 512
        || raw
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '"' | '\'' | '\\'))
    {
        return Err(Error::new(
            "GITHUB_TOKEN has characters that cannot go in an Authorization header",
        ));
    }
    Ok(Some(raw.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_drops_a_token_and_online_keeps_a_plain_one() {
        assert_eq!(credential_from(true, Some("ghp_example")).unwrap(), None);
        assert_eq!(credential_from(false, None).unwrap(), None);
        assert_eq!(credential_from(false, Some("")).unwrap(), None);
        assert_eq!(
            credential_from(false, Some("ghp_example"))
                .unwrap()
                .as_deref(),
            Some("ghp_example")
        );
        assert!(credential_from(false, Some("has space")).is_err());
        assert!(credential_from(false, Some("has\"quote")).is_err());
    }
}
