//! [`Session`]: a named, persistent group of resources on one authority.
//!
//! A session is an organization record, not evidence that anything runs.
//! Membership lives here (`members`); presentation lives in an optional
//! shared [`crate::Arrangement`].

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::activation::{SessionLifetime, Stamp};
use crate::keys::{ArrangementKey, ResourceKey, Revision, SessionKey};

/// Longest session or resource name, in UTF-8 bytes.
pub const MAX_NAME_BYTES: usize = 256;

/// Descriptive session attributes. They never organize or authorize.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attrs {
    /// Project label.
    #[serde(default)]
    pub project: Option<String>,
    /// Working directory new resources default to.
    #[serde(default)]
    pub directory: Option<PathBuf>,
    /// Host label shown beside the name.
    #[serde(default)]
    pub host_label: Option<String>,
    /// The repository the session was created in (ADR-0168).
    #[serde(default)]
    pub repository: Option<Box<crate::project_identity::SessionProject>>,
    /// Opaque mission task id, lowercase hex. Set by `phux mission start`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission_task: Option<String>,
}

/// A session record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    /// Persistent key.
    pub key: SessionKey,
    /// Unique among live sessions on this authority.
    pub name: String,
    /// Whether the session survives empty.
    pub lifetime: SessionLifetime,
    /// An imported group shown in Intake until explicitly organized.
    #[serde(default)]
    pub awaiting_organization: bool,
    /// Member resources in creation (or arrival) order. Each resource is a
    /// member of exactly one session.
    pub members: Vec<ResourceKey>,
    /// The shared arrangement, once one has been saved.
    #[serde(default)]
    pub arrangement: Option<ArrangementKey>,
    /// Descriptive attributes.
    #[serde(default)]
    pub attrs: Attrs,
    /// When the session was created.
    pub created: Stamp,
    /// Bumps on every committed change to this record.
    pub revision: Revision,
    /// Advisory: the member that last received human input (R14). Not
    /// focus; never moves a client. Not versioned by `revision`.
    #[serde(default)]
    pub recent: Option<ResourceKey>,
}

impl Session {
    /// Whether the session has no members.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.members.is_empty()
    }
}
