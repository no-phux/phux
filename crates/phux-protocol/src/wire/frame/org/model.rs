//! Organization value types: outcomes, live targets, launch specs, filters, moves, and arrangement documents.

use super::{
    ActivationId, ResourceId, ResourceKey, Revision, ServerInstance, SessionKey, SplitDirection,
    WorkspaceKey,
};

/// How an activation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Outcome {
    /// The process exited with this status.
    Exited(i32),
    /// The process was ended by this signal.
    Signaled(i32),
    /// The server did not observe the end (it ended while no server ran).
    Unobserved,
}

pub(super) const OUTCOME_EXITED: u8 = 0;

pub(super) const OUTCOME_SIGNALED: u8 = 1;

pub(super) const OUTCOME_UNOBSERVED: u8 = 2;

/// The current execution of a resource, as a kind operation addresses it: a
/// stale activation fails before any effect.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LiveTarget {
    /// The live handle.
    pub resource: ResourceId,
    /// The server instance whose id space `resource` belongs to.
    pub instance: ServerInstance,
    /// The activation the caller means.
    pub activation: ActivationId,
}

/// Presentation committed with resource membership and launch intent.
#[derive(Debug, Clone, PartialEq)]
pub enum CreatePlacement {
    /// Append a new unnamed group; does not change client focus.
    NewGroup,
    /// Split one stable entry only while it still shows the observed execution.
    Beside {
        /// Stable entry id.
        entry: [u8; 16],
        /// Durable anchor resource.
        resource: ResourceKey,
        /// Anchor's execution incarnation.
        instance: ServerInstance,
        /// Anchor's activation.
        activation: ActivationId,
        /// Split axis.
        dir: SplitDirection,
        /// Fraction kept by the anchor, strictly between zero and one.
        ratio: f32,
    },
}

/// How a resource's execution starts and restarts. Kind-specific; a
/// Terminal reads every field.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct LaunchSpec {
    /// Argv; empty = the default shell.
    pub command: Vec<String>,
    /// Working directory.
    pub cwd: Option<String>,
    /// Environment pairs, in order.
    pub env: Vec<(String, String)>,
    /// `TERM` override.
    pub term: Option<String>,
    /// Kind-specific restore record, e.g. the agent conversation a
    /// relaunched Terminal resumes.
    pub restore: Option<Vec<u8>>,
}

impl LaunchSpec {
    /// A launch of `command` (empty = the default shell).
    #[must_use]
    pub const fn new(command: Vec<String>) -> Self {
        Self {
            command,
            cwd: None,
            env: Vec::new(),
            term: None,
            restore: None,
        }
    }

    /// Builder setter for [`Self::cwd`].
    #[must_use]
    pub fn with_cwd(mut self, cwd: Option<String>) -> Self {
        self.cwd = cwd;
        self
    }

    /// Builder setter for [`Self::env`].
    #[must_use]
    pub fn with_env(mut self, env: Vec<(String, String)>) -> Self {
        self.env = env;
        self
    }

    /// Builder setter for [`Self::term`].
    #[must_use]
    pub fn with_term(mut self, term: Option<String>) -> Self {
        self.term = term;
        self
    }

    /// Builder setter for [`Self::restore`].
    #[must_use]
    pub fn with_restore(mut self, restore: Option<Vec<u8>>) -> Self {
        self.restore = restore;
        self
    }
}

/// Which records a `GET_ORGANIZATION` page lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum OrgFilter {
    /// Every session and resource the caller inventories.
    #[default]
    All,
    /// One session and its members.
    Session(SessionKey),
    /// The sessions one workspace links.
    Workspace(WorkspaceKey),
    /// Intake: the designated Intake session and sessions awaiting
    /// organization, with their members.
    Intake,
}

pub(super) const FILTER_ALL: u8 = 0;

pub(super) const FILTER_SESSION: u8 = 1;

pub(super) const FILTER_WORKSPACE: u8 = 2;

pub(super) const FILTER_INTAKE: u8 = 3;

/// One resource of a `RESOURCE_MOVE_MANY` batch, with the source session
/// revision the caller saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MoveItem {
    /// The resource to move.
    pub resource: ResourceKey,
    /// Its current session.
    pub from: SessionKey,
    /// The source session's expected revision.
    pub from_expected: Revision,
}

/// One record's revision: a commit's resulting revision, or a revision an
/// undo expects to still hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RecordRevision {
    /// The record's key bytes (a session, resource, arrangement, or
    /// workspace key; keys are unique across kinds per authority).
    pub key: [u8; 16],
    /// The revision.
    pub revision: Revision,
}

/// The record an arrangement belongs to. On the wire: a `SESSION` field or a
/// `WORKSPACE` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ArrangementOwner {
    /// A session's arrangement; it may name only that session's members.
    Session(SessionKey),
    /// A workspace's arrangement.
    Workspace(WorkspaceKey),
}

/// A leaf of an arrangement: one presented entry of one target.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntryRef {
    /// Random id of this entry, stable while the entry exists.
    pub entry: [u8; 16],
    /// What it shows.
    pub target: EntryTarget,
}

/// What an arrangement entry shows. Only a [`Self::Member`] is checked
/// against the owning session's members.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EntryTarget {
    /// A resource record of this authority, by key.
    Member(ResourceKey),
    /// A resource a federation satellite serves, by its satellite-local id.
    Satellite {
        /// The satellite host token, 1..=255 bytes.
        host: crate::ids::SatelliteHost,
        /// The satellite-local resource id.
        id: u32,
    },
}

pub(super) const TARGET_MEMBER: u8 = 0;

pub(super) const TARGET_SATELLITE: u8 = 1;

/// Most bytes in a satellite entry's host token.
pub(super) const MAX_ENTRY_HOST_BYTES: usize = 255;

/// A node of an arrangement group's split tree.
#[derive(Debug, Clone, PartialEq)]
pub enum ArrangementNode {
    /// One entry.
    Leaf(EntryRef),
    /// Two children sharing the parent's rectangle.
    Split {
        /// The split axis.
        dir: SplitDirection,
        /// Fraction given to `first`, in the open interval `(0, 1)`.
        ratio: f32,
        /// Left or top child.
        first: Box<Self>,
        /// Right or bottom child.
        second: Box<Self>,
    },
}

/// One group of an arrangement (a tab): a name and one split tree.
#[derive(Debug, Clone, PartialEq)]
pub struct ArrangementGroup {
    /// Random id of the group, stable while it exists.
    pub id: [u8; 16],
    /// Display name, at most 256 bytes.
    pub name: Option<String>,
    /// The split tree.
    pub tree: ArrangementNode,
}

/// A presentation of resources: ordered groups, each one split tree whose
/// leaves name resources by key. Encoded at most 64 KiB.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ArrangementDoc {
    /// The groups, in display order.
    pub groups: Vec<ArrangementGroup>,
}

impl ArrangementDoc {
    /// Validate the complete encoded shape against the wire's size and depth bounds.
    /// Server-generated incremental edits must pass this before durable commitment too.
    ///
    /// # Errors
    /// Any arrangement the ordinary wire decoder would refuse.
    pub fn validate_wire(&self) -> Result<(), super::DecodeError> {
        let mut bytes = bytes::BytesMut::new();
        super::arrangement::encode_arrangement_doc(self, &mut super::Encoder::new(&mut bytes));
        super::arrangement::decode_arrangement_doc(&bytes).map(|_| ())
    }
}
