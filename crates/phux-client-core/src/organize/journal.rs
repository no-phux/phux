//! The undo record of one applied plan.

use phux_protocol::ids::{AuthorityId, ResourceKey, SessionKey};
use serde::{Deserialize, Serialize};

use super::plan::PlannedRename;

/// Schema of the [`Journal`] document.
pub const JOURNAL_SCHEMA: u8 = 1;

/// What applying a plan did, in order: enough to undo it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    /// Document schema.
    pub schema_version: u8,
    /// The server whose records the run changed; undo refuses any other.
    #[serde(default)]
    pub authority: Option<AuthorityId>,
    /// Sessions created.
    pub created: Vec<CreatedSession>,
    /// Terminals moved.
    pub moved: Vec<MovedPane>,
    /// Sessions renamed.
    pub renamed: Vec<PlannedRename>,
}

impl Journal {
    /// An empty journal at the current schema.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            schema_version: JOURNAL_SCHEMA,
            authority: None,
            created: Vec::new(),
            moved: Vec::new(),
            renamed: Vec::new(),
        }
    }

    /// Whether nothing was done.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.created.is_empty() && self.moved.is_empty() && self.renamed.is_empty()
    }
}

/// A session an organize run created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedSession {
    /// Its key.
    pub key: SessionKey,
    /// Its name.
    pub name: String,
}

/// A terminal an organize run moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MovedPane {
    /// The terminal.
    pub pane: ResourceKey,
    /// The session it left.
    pub from: SessionKey,
    /// The session it joined.
    pub to: SessionKey,
}

#[cfg(test)]
mod frozen {
    #[test]
    fn organize_journal() {
        phux_frozen::json::<super::Journal>("organize-journal").check();
    }
}
