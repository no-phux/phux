//! The organization store (ADR-0162): where committed sessions, resources,
//! arrangements, and receipts live.

use std::path::PathBuf;

/// Where a server keeps its organization store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum OrgStoreLocation {
    /// A SQLite file at this path.
    Path(PathBuf),
    /// Process memory only: records die with the server. In-process tests
    /// and embedders that persist nothing use it.
    #[default]
    Memory,
}

impl OrgStoreLocation {
    /// The production location: `<state_dir>/org/v1.sqlite3`.
    #[must_use]
    pub fn default_path() -> Self {
        Self::Path(phux_config::instance::state_dir().join("org").join("v1.sqlite3"))
    }
}
