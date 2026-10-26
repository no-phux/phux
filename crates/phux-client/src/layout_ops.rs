//! Headless read-modify-write of a session's shared arrangement, for the CLI
//! and MCP.
//!
//! Against a server that advertises `ORGANIZATION`, the arrangement is the
//! session's record: [`LayoutOps`] reads it with `ARRANGEMENT_GET`,
//! translates its keys to live handles ([`crate::layout::wire`]), applies the
//! pure [`apply_mutation`], and writes it back with `ARRANGEMENT_REPLACE`
//! naming the revision it read: a real compare-and-swap, retried on
//! `CONFLICT` against a fresh read. Such a server gives every session an
//! arrangement (SPEC L2 §2.2), so seeding is the same compare-and-swap:
//! an arrangement with no groups takes the fallback.
//!
//! Against an older server, and always for a named projection key
//! (ADR-0129), the layout is the v3 [`ArrangementDoc`] envelope under L3
//! metadata (`phux.tui.layout/v1/<session>`). Writes there are
//! last-write-wins; a trailing `GET_METADATA` flushes the fire-and-forget SET
//! and confirms it.
//!
//! There is deliberately no headless focus mutation (ADR-0049). Use a
//! dedicated, unsubscribed connection.

use phux_protocol::ids::{ArrangementKey, GroupId, ResourceId, Revision, SessionId};
use thiserror::Error;

use crate::attach::AttachError;
use crate::layout::wire::{EntryBinding, HandleMap, WireError};
use crate::layout::{
    ArrangementDoc, LayoutDecodeError, LayoutEncodeError, LayoutError, MutationError,
};
use crate::org::OrgError;

pub use crate::layout::{LAYOUT_KEY, LayoutMutation, layout_key};

mod ops;

pub use ops::{LayoutOps, write_layout_on};

/// Apply one mutation without doing I/O; see [`crate::layout::apply_mutation`].
///
/// # Errors
///
/// Rejects missing/duplicate targets, invalid ratios, same-pane operations,
/// and closing the final pane.
pub fn apply_mutation(
    workspace: &mut ArrangementDoc,
    mutation: &LayoutMutation,
) -> Result<(), LayoutOpsError> {
    crate::layout::apply_mutation(workspace, mutation).map_err(Into::into)
}

impl From<MutationError> for LayoutOpsError {
    fn from(error: MutationError) -> Self {
        match error {
            MutationError::Layout(error) => Self::Layout(error),
            MutationError::ForeignTarget(id) => Self::ForeignTarget(id),
            MutationError::DuplicatePane(id) => Self::DuplicatePane(id),
            MutationError::SamePane => Self::SamePane,
            MutationError::LastPane => Self::LastPane,
        }
    }
}

/// The static Group used by v0.x servers for layout metadata.
pub const DEFAULT_LAYOUT_GROUP_ID: GroupId = crate::layout::LAYOUT_METADATA_GROUP;

/// Whose layout a [`LAYOUT_KEY`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKeyOwner {
    /// The bare legacy key: it names no session, so it is the reader's own.
    Legacy,
    /// The per-session key for this session.
    Session(SessionId),
}

/// Whose layout `key` names, or `None` when it is not attributable.
///
/// Only the TUI's own `phux.tui.layout/v1` family (docs/organization.md) is
/// recognized: a named projection under another prefix (§3.5) is private to
/// its writer and must never be adopted by the TUI.
#[must_use]
pub fn layout_key_session(key: &str) -> Option<LayoutKeyOwner> {
    if key == LAYOUT_KEY {
        return Some(LayoutKeyOwner::Legacy);
    }
    let suffix = key.strip_prefix(&format!("{LAYOUT_KEY}/"))?;
    // An unparsable suffix is `None`, never `Legacy` (which reads as ours).
    Some(LayoutKeyOwner::Session(SessionId::new(
        suffix.parse::<u32>().ok()?,
    )))
}

/// Validate a `--projection KEY` value against the grammar
/// `<prefix>.layout/v1/<session-id>`, naming `session`.
///
/// # Errors
///
/// [`LayoutOpsError::InvalidProjectionKey`] when `key` does not parse, or
/// names another session.
pub fn validate_projection_key(key: &str, session: SessionId) -> Result<(), LayoutOpsError> {
    let invalid = || LayoutOpsError::InvalidProjectionKey(key.to_owned());
    let id = crate::layout::projection_key_session(key).ok_or_else(invalid)?;
    if id != session {
        return Err(invalid());
    }
    Ok(())
}

/// Errors from pure mutations and metadata request/reply operations.
#[derive(Debug, Error)]
pub enum LayoutOpsError {
    /// Transport or framing failed.
    #[error(transparent)]
    Transport(#[from] AttachError),
    /// The stored envelope was malformed or unsupported.
    #[error(transparent)]
    Decode(#[from] LayoutDecodeError),
    /// The rewritten v3 envelope could not be encoded.
    #[error(transparent)]
    Encode(#[from] LayoutEncodeError),
    /// An existing tree operation rejected the request.
    #[error(transparent)]
    Layout(#[from] LayoutError),
    /// No layout value exists for the requested session.
    #[error("session has no persisted layout metadata")]
    MissingLayout,
    /// A mutation named a pane outside this workspace.
    #[error("pane is not in this session layout: {0:?}")]
    ForeignTarget(ResourceId),
    /// A split tried to insert an id already present in the workspace.
    #[error("pane is already in this session layout: {0:?}")]
    DuplicatePane(ResourceId),
    /// A two-target operation named the same pane twice.
    #[error("layout operation requires two distinct panes")]
    SamePane,
    /// Closing the final pane would produce an unencodable empty workspace.
    #[error("cannot close the final pane in a persisted layout")]
    LastPane,
    /// The server rejected a correlated request.
    #[error("server refused layout request: {0}")]
    Refused(String),
    /// A `--projection` value did not parse as `<prefix>.layout/v1/<session>`
    /// for the session being addressed (ADR-0129).
    #[error(
        "projection key {0:?} must be `<prefix>.layout/v1/<session-id>` naming the addressed session"
    )]
    InvalidProjectionKey(String),
    /// An organization request failed.
    #[error(transparent)]
    Org(#[from] OrgError),
    /// The document names a pane that is not a member record of this server.
    #[error(transparent)]
    Wire(#[from] WireError),
    /// Every compare-and-swap attempt lost to a concurrent writer.
    #[error("the arrangement kept changing underneath this edit; retry")]
    Conflict,
    /// The confirming read found other bytes than those just written.
    #[error(
        "layout write was not confirmed: the read-back value did not match what was written \
         (a concurrent writer, or the write exceeded limits.metadata-value-bytes and was \
         silently dropped — SET_METADATA has no reply to report that directly)"
    )]
    NotConfirmed,
}

/// A session arrangement as read from the server, with what writing it back
/// needs.
#[derive(Debug, Clone)]
pub struct ReadArrangement {
    /// The handle-space document.
    pub document: ArrangementDoc,
    /// The arrangement's key.
    pub key: ArrangementKey,
    /// The revision `document` was read at.
    pub revision: Revision,
    /// Key-to-handle translation for the session's members.
    pub handles: HandleMap,
    /// Entry ids and unplaceable entries, for the write-back.
    pub binding: EntryBinding,
}

#[cfg(test)]
mod tests;
