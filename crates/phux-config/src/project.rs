//! Project catalog schema (ADR-0152): `[[projects]]` entries that
//! `phux project open NAME` resolves to a checkout.
//!
//! The catalog is pure naming: it never scans the filesystem and holds no
//! trust. A repository's own `.phux/project.toml` recipe runs only after its
//! exact bytes are approved; an explicit `recipe` path written here is the
//! user's own file and is trusted by being written here.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One named project in `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfigEntry {
    /// The label `phux project open NAME` resolves: ASCII letters, digits,
    /// `-`, or `_`, unique across the catalog.
    pub name: String,

    /// The project's checkout: absolute, or `~/`-relative.
    pub path: PathBuf,

    /// A recipe outside the repository (absolute or `~/`-relative), used
    /// instead of the checkout's `.phux/project.toml`. Trusted because the
    /// user wrote it into their own config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<PathBuf>,
}
