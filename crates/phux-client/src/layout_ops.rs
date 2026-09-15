//! Shared headless operations over the persisted TUI layout.
//!
//! CLI commands and MCP tools use this module to read a session's
//! `phux.tui.layout/v1/<session>` value, decode the current v3 [`Workspace`]
//! envelope, apply a mutation using the
//! existing `phux-client-core` layout types, and write a v3 envelope back with
//! `SET_METADATA`. No layout vocabulary is added to the wire protocol.
//!
//! This module deliberately exposes no headless focus mutation. Per ADR-0049,
//! focus is client-local and attention is the navigation signal; layout
//! metadata writers have no authority to yank an attached client's viewport.
//! The compatibility focus fields remain solely because v2 encoding requires
//! them, and attached clients ignore them during reconciliation.
//!
//! The coordination model is explicitly **last-write-wins**. A mutation is a
//! `GET_METADATA` followed by a whole-value `SET_METADATA`; concurrent writers
//! can overwrite one another. The trailing `GET_METADATA` is both a flush
//! barrier for the fire-and-forget SET and the value returned to the caller.
//! Callers should use a dedicated connection: the reads route through
//! [`Connection::request_metadata`], which hands back anything the server
//! interleaved, and this module has no consumer for a `RESOURCE_OUTPUT` or an
//! `EVENT` so it discards them (loudly — see
//! `Reply::into_result_ignoring_interleaved`).

use phux_protocol::ids::{GroupId, ResourceId, SessionId};
use phux_protocol::wire::frame::{FrameKind, Scope};
use thiserror::Error;

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::layout::{
    LayoutDecodeError, LayoutEncodeError, LayoutError, LayoutNode, SplitDir, Workspace, kill_pane,
    leaves, split_at,
};

/// Prefix of the conventional per-session TUI layout metadata key.
pub const LAYOUT_KEY: &str = "phux.tui.layout/v1";

/// The static Group used by v0.x servers for layout metadata.
pub const DEFAULT_LAYOUT_GROUP_ID: GroupId = GroupId::new(1);

/// Return the metadata key for `session`.
#[must_use]
pub fn layout_key(session: SessionId) -> String {
    format!("{LAYOUT_KEY}/{}", session.get())
}

/// Whose layout a [`LAYOUT_KEY`] names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutKeyOwner {
    /// The bare legacy key, written before per-session keying existed. It
    /// names no session, so it can only be the reader's own.
    Legacy,
    /// The per-session key for this session.
    Session(SessionId),
}

/// Whose layout `key` names, or `None` when it is not a layout key we can
/// attribute.
///
/// "Is this the layout family?" was the right question only while a client
/// subscribed to exactly one layout key. Once it watches peers too
/// (phux-k0cw), adopting a peer's topology as your own would silently replace
/// your pane tree, so the question becomes "whose layout is this?".
///
/// This recognizes only the TUI's own `phux.tui.layout/v1` family (SPEC
/// L3.md §3.2). A named projection under a different prefix
/// (`--projection`, §3.5) is deliberately invisible here: the TUI MUST NOT
/// adopt a private arrangement another consumer wrote for itself, so a
/// foreign prefix falls through to `None` exactly like any other unrelated
/// key. See [`validate_projection_key`] for the general `<prefix>.layout/v1/<session>`
/// grammar a `--projection` value is checked against.
#[must_use]
pub fn layout_key_session(key: &str) -> Option<LayoutKeyOwner> {
    if key == LAYOUT_KEY {
        return Some(LayoutKeyOwner::Legacy);
    }
    let suffix = key.strip_prefix(&format!("{LAYOUT_KEY}/"))?;
    // An unparsable suffix names no session we can attribute. It must NOT
    // fall back to `Legacy` — the caller reads that as "ours" and adopts it.
    // Answering `None` drops the frame instead, the only safe direction for a
    // layout we cannot prove is our own.
    Some(LayoutKeyOwner::Session(SessionId::new(
        suffix.parse::<u32>().ok()?,
    )))
}

/// Validate a `--projection KEY` value against the SPEC L3.md §3.5 grammar.
///
/// The grammar is `<prefix>.layout/v1/<session-id>`, where `<session-id>` is
/// the decimal wire id of `session` (the session the operation actually
/// addresses). `<prefix>` may be anything non-empty that does not itself
/// contain the `.layout/v1/` separator — this is deliberately permissive so
/// a consumer's own namespaced key (`myapp.layout/v1/<session>`) validates
/// the same way the reference TUI's own default key does.
///
/// # Errors
///
/// Returns [`LayoutOpsError::InvalidProjectionKey`] when `key` does not
/// parse as `<prefix>.layout/v1/<id>`, or when the embedded id does not name
/// `session`.
pub fn validate_projection_key(key: &str, session: SessionId) -> Result<(), LayoutOpsError> {
    let invalid = || LayoutOpsError::InvalidProjectionKey(key.to_owned());
    let id = projection_key_session(key).ok_or_else(invalid)?;
    if id != session {
        return Err(invalid());
    }
    Ok(())
}

/// Parse the session a `<prefix>.layout/v1/<session-id>` key names, without
/// checking it against any particular session. `None` when `key` does not
/// match the grammar at all.
///
/// The `<session-id>` segment must be the session's **canonical** decimal
/// form — no leading zero (other than a bare `"0"`), no leading `+`, no
/// non-ASCII-digit content — enforced as `suffix == id.to_string()` rather
/// than merely "parses as u32". Without this, `myapp.layout/v1/07` and
/// `myapp.layout/v1/7` would both name session 7 but compare unequal as
/// strings: the server's reap cleanup matches the literal key
/// `*.layout/v1/7` (`state/reap.rs`), so a non-canonical key that slipped
/// past a looser check here would validate for `--projection` yet never be
/// found and deleted when its session reaps — orphaned forever. `<prefix>`
/// must also not itself contain the `.layout/v1/` separator, so a key with
/// two occurrences (`a.layout/v1/b.layout/v1/7`) is rejected rather than
/// silently matched on its last one.
///
/// Used to match an unordered pair of `--projection` keys against a pair of
/// sessions (a cross-session `move-pane`) — see `phux_client::pane_move`.
#[must_use]
pub fn projection_key_session(key: &str) -> Option<SessionId> {
    let (prefix, suffix) = key.rsplit_once(".layout/v1/")?;
    if prefix.is_empty() || prefix.contains(".layout/v1/") {
        return None;
    }
    let id = suffix.parse::<u32>().ok()?;
    if suffix != id.to_string() {
        return None;
    }
    Some(SessionId::new(id))
}

/// One pure mutation of a decoded [`Workspace`].
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutMutation {
    /// Insert `new_pane` beside `target`.
    Split {
        /// Existing pane whose leaf is replaced by a split.
        target: ResourceId,
        /// Already-created pane to insert.
        new_pane: ResourceId,
        /// Split axis.
        dir: SplitDir,
        /// Fraction assigned to the existing target, in `(0, 1)`.
        ratio: f32,
    },
    /// Insert a pane without changing serialized active-window or focus fields.
    /// Headless spawn placement uses this so it cannot publish shared focus.
    SplitPreservingFocus {
        /// Existing pane whose leaf is replaced by a split.
        target: ResourceId,
        /// Already-created pane to insert.
        new_pane: ResourceId,
        /// Split axis.
        dir: SplitDir,
        /// Fraction assigned to the existing target, in `(0, 1)`.
        ratio: f32,
    },
    /// Remove `source` from its old parent (collapsing it), then insert it
    /// beside `target`.
    Move {
        /// Existing pane to relocate.
        source: ResourceId,
        /// Existing destination pane.
        target: ResourceId,
        /// Destination split axis.
        dir: SplitDir,
        /// Fraction assigned to `target`, in `(0, 1)`.
        ratio: f32,
    },
    /// Exchange two leaf positions without changing split geometry.
    Swap {
        /// First existing pane.
        first: ResourceId,
        /// Second existing pane.
        second: ResourceId,
    },
    /// Remove `target`, collapsing its parent split. A one-pane window is
    /// removed; the final pane in the workspace cannot be removed because
    /// this mutation API requires a nonempty workspace.
    Close {
        /// Existing pane to remove.
        target: ResourceId,
    },
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
    /// A future `LayoutNode` variant reached a client that cannot rewrite it.
    #[error("unsupported layout node variant")]
    UnsupportedLayoutNode,
    /// A `--projection` value did not parse as `<prefix>.layout/v1/<session>`
    /// for the session being addressed (ADR-0129).
    #[error(
        "projection key {0:?} must be `<prefix>.layout/v1/<session-id>` naming the addressed session"
    )]
    InvalidProjectionKey(String),
    /// [`LayoutOps::write_and_confirm`]'s trailing read found a value, but
    /// not the exact bytes just written.
    #[error(
        "layout write was not confirmed: the read-back value did not match what was written \
         (a concurrent writer, or the write exceeded limits.metadata-value-bytes and was \
         silently dropped — SET_METADATA has no reply to report that directly)"
    )]
    NotConfirmed,
}

/// Stateful request-id allocator and layout metadata client.
///
/// Construct one over a dedicated [`Connection`], then call [`Self::read`] or
/// [`Self::mutate`].
#[derive(Debug)]
pub struct LayoutOps<'a> {
    conn: &'a mut Connection,
    session: SessionId,
    group: GroupId,
    key: String,
    next_request_id: u32,
}

impl<'a> LayoutOps<'a> {
    /// Use the default layout Group and begin allocating at `first_request_id`.
    #[must_use]
    pub fn new(conn: &'a mut Connection, session: SessionId, first_request_id: u32) -> Self {
        Self::in_group(conn, session, DEFAULT_LAYOUT_GROUP_ID, first_request_id)
    }

    /// Use an explicit Group (primarily useful to non-default server setups).
    #[must_use]
    pub fn in_group(
        conn: &'a mut Connection,
        session: SessionId,
        group: GroupId,
        first_request_id: u32,
    ) -> Self {
        Self {
            conn,
            session,
            group,
            key: layout_key(session),
            next_request_id: first_request_id,
        }
    }

    /// Use an explicit named projection key (`--projection`, ADR-0129)
    /// instead of the default `phux.tui.layout/v1/<session>`.
    ///
    /// # Errors
    ///
    /// Returns [`LayoutOpsError::InvalidProjectionKey`] when `key` is not
    /// `<prefix>.layout/v1/<session-id>` for `session`.
    pub fn with_key(
        conn: &'a mut Connection,
        session: SessionId,
        key: String,
        first_request_id: u32,
    ) -> Result<Self, LayoutOpsError> {
        validate_projection_key(&key, session)?;
        Ok(Self {
            conn,
            session,
            group: DEFAULT_LAYOUT_GROUP_ID,
            key,
            next_request_id: first_request_id,
        })
    }

    /// Read and decode this session's current v3 layout. Earlier schemas refuse.
    ///
    /// # Errors
    ///
    /// Returns [`LayoutOpsError::MissingLayout`] when the key is absent, plus
    /// transport, refusal, or envelope decode errors.
    pub async fn read(&mut self) -> Result<Workspace, LayoutOpsError> {
        let value = self.get_value().await?;
        let bytes = value.ok_or(LayoutOpsError::MissingLayout)?;
        Workspace::decode_cbor(&bytes).map_err(Into::into)
    }

    /// Read, mutate, encode as v3, SET, then read back and confirm the
    /// value.
    ///
    /// Coordination is last-write-wins at the `SET_METADATA` layer (whoever
    /// writes last simply overwrites), but this call itself is not
    /// tolerant of losing that race: [`Self::write_and_confirm`] requires
    /// the confirming read to match exactly what this call just computed,
    /// so a concurrent writer that lands between this call's own SET and
    /// its confirming GET surfaces as [`LayoutOpsError::NotConfirmed`]
    /// rather than silently handing back someone else's value as if it
    /// were this mutation's result.
    ///
    /// # Errors
    ///
    /// Returns transport/envelope errors or a mutation-specific rejection.
    pub async fn mutate(&mut self, mutation: LayoutMutation) -> Result<Workspace, LayoutOpsError> {
        let mut workspace = self.read().await?;
        apply_mutation(&mut workspace, &mutation)?;
        self.write_and_confirm(&workspace).await
    }

    /// Mutate the stored workspace, or seed it from `fallback` when this
    /// session has no layout metadata yet.
    ///
    /// # Errors
    ///
    /// Returns transport/envelope errors or a mutation-specific rejection.
    pub async fn mutate_or_seed(
        &mut self,
        fallback: Workspace,
        mutation: LayoutMutation,
    ) -> Result<Workspace, LayoutOpsError> {
        let mut workspace = match self.read().await {
            Ok(workspace) => workspace,
            Err(LayoutOpsError::MissingLayout) => fallback,
            Err(err) => return Err(err),
        };
        apply_mutation(&mut workspace, &mutation)?;
        self.write_and_confirm(&workspace).await
    }

    async fn get_value(&mut self) -> Result<Option<Vec<u8>>, LayoutOpsError> {
        let request_id = self.allocate_request_id();
        let reply = self
            .conn
            .request_metadata(request_id, Scope::Group(self.group), self.key.clone())
            .await?;
        // `handle_get_metadata` (`crates/phux-server/src/runtime/client.rs`)
        // answers with METADATA_VALUE and pushes nothing of its own, and this
        // type documents a dedicated connection — one that never sent
        // ATTACH_RESOURCE or SUBSCRIBE_EVENTS, so no pane actor can fan out
        // onto it. Nothing can be interleaved here; if something is, the
        // discard is logged rather than silent.
        reply
            .into_result_ignoring_interleaved()
            .map_err(|refusal| LayoutOpsError::Refused(refusal.message))
    }

    /// Overwrite the stored envelope with exactly `workspace` and confirm it
    /// with a trailing read — the raw write half of [`Self::mutate`], for a
    /// caller that has already computed the whole target value (e.g.
    /// `phux workspace restore` replaying an archived split tree) rather
    /// than applying one [`LayoutMutation`] to what is already there.
    ///
    /// # Errors
    ///
    /// Returns transport/envelope errors, [`LayoutOpsError::MissingLayout`]
    /// when the confirming read finds nothing at all, or
    /// [`LayoutOpsError::NotConfirmed`] when it finds a value that is not
    /// byte-for-byte what was just written — either can mean a concurrent
    /// writer or a value over `limits.metadata-value-bytes` silently
    /// dropped server-side (`SET_METADATA` has no reply frame).
    pub async fn write_and_confirm(
        &mut self,
        workspace: &Workspace,
    ) -> Result<Workspace, LayoutOpsError> {
        let bytes = workspace.encode_cbor()?;
        let set_request_id = self.allocate_request_id();
        self.conn
            .send(&FrameKind::SetMetadata {
                request_id: set_request_id,
                scope: Scope::Group(self.group),
                key: self.key.clone(),
                value: bytes.clone(),
            })
            .await?;
        // SET_METADATA has no reply. The ordered trailing GET proves the
        // server consumed it and also reports a concurrent last writer —
        // but a value over `limits.metadata-value-bytes` is *also* a
        // silent no-op server-side (docs/spec/L3.md §2), so a confirming
        // read that doesn't match what was just written is ambiguous
        // between "someone else won the race" and "this write was too
        // big and got dropped". A read-back that *decodes* but doesn't
        // match what was sent (e.g. a concurrent writer's own, otherwise
        // valid, v3 envelope) must not be accepted as if this write had
        // won — hence the exact byte comparison below, not just
        // "did something decode". Callers that turn a placement mismatch
        // into a "concurrent writer" message should name the cap as a
        // possible cause too (see `phux_client::pane_move`).
        let value = self.get_value().await?;
        let read_back = value.ok_or(LayoutOpsError::MissingLayout)?;
        if read_back != bytes {
            return Err(LayoutOpsError::NotConfirmed);
        }
        Workspace::decode_cbor(&read_back).map_err(Into::into)
    }

    /// The session this handle addresses.
    #[must_use]
    pub const fn session(&self) -> SessionId {
        self.session
    }

    /// The metadata key this handle reads and writes.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    const fn allocate_request_id(&mut self) -> u32 {
        let id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1);
        id
    }
}

/// [`LayoutOps::new`] plus [`LayoutOps::write_and_confirm`] over a fresh
/// connection.
///
/// The whole-envelope write `phux workspace restore` uses to publish a
/// freshly restored session's layout, which (unlike placement's
/// read-modify-write) is always a first write, never a mutation of an
/// existing value.
///
/// # Errors
///
/// A connect failure, or [`LayoutOps::write_and_confirm`]'s own errors.
pub async fn write_layout_on(
    socket_path: &std::path::Path,
    session: SessionId,
    workspace: &Workspace,
    first_request_id: u32,
) -> Result<(), LayoutOpsError> {
    let mut conn = Connection::connect(socket_path).await?;
    LayoutOps::new(&mut conn, session, first_request_id)
        .write_and_confirm(workspace)
        .await
        .map(|_workspace| ())
}

/// Apply one mutation without doing I/O.
///
/// This is public so CLI/MCP code can test or compose layout changes without
/// duplicating tree algorithms. It only uses `phux-client-core`'s existing
/// [`Workspace`] and [`LayoutNode`] types.
///
/// # Errors
///
/// Rejects missing/duplicate targets, invalid ratios, same-pane operations,
/// and closing the final pane.
pub fn apply_mutation(
    workspace: &mut Workspace,
    mutation: &LayoutMutation,
) -> Result<(), LayoutOpsError> {
    match mutation {
        LayoutMutation::Split {
            target,
            new_pane,
            dir,
            ratio,
        } => apply_split(workspace, target, new_pane, *dir, *ratio, true),
        LayoutMutation::SplitPreservingFocus {
            target,
            new_pane,
            dir,
            ratio,
        } => apply_split(workspace, target, new_pane, *dir, *ratio, false),
        LayoutMutation::Move {
            source,
            target,
            dir,
            ratio,
        } => apply_move(workspace, source, target, *dir, *ratio),
        LayoutMutation::Swap { first, second } => apply_swap(workspace, first, second),
        LayoutMutation::Close { target } => apply_close(workspace, target),
    }
}

/// The tree of `workspace.windows[index]`, blaming `blame` when the window
/// carries no tree at all.
fn window_tree<'a>(
    workspace: &'a Workspace,
    index: usize,
    blame: &ResourceId,
) -> Result<&'a LayoutNode, LayoutOpsError> {
    workspace.windows[index]
        .state
        .tree
        .as_ref()
        .ok_or_else(|| LayoutOpsError::ForeignTarget(blame.clone()))
}

/// The index of the window holding `target`, or [`LayoutOpsError::ForeignTarget`].
fn require_window(workspace: &Workspace, target: &ResourceId) -> Result<usize, LayoutOpsError> {
    find_window(workspace, target).ok_or_else(|| LayoutOpsError::ForeignTarget(target.clone()))
}

/// Split `target` to make room for `new_pane`, moving focus to the new pane
/// only when `focus_new_pane` is set.
fn apply_split(
    workspace: &mut Workspace,
    target: &ResourceId,
    new_pane: &ResourceId,
    dir: SplitDir,
    ratio: f32,
    focus_new_pane: bool,
) -> Result<(), LayoutOpsError> {
    if find_window(workspace, new_pane).is_some() {
        return Err(LayoutOpsError::DuplicatePane(new_pane.clone()));
    }
    let index = require_window(workspace, target)?;
    let tree = window_tree(workspace, index, target)?;
    workspace.windows[index].state.tree = Some(split_at(tree, target, new_pane, dir, ratio)?);
    if focus_new_pane {
        workspace.windows[index].state.focus = Some(new_pane.clone());
        workspace.active = index;
    }
    Ok(())
}

/// Exchange the leaf positions of `first` and `second` across every window.
fn apply_swap(
    workspace: &mut Workspace,
    first: &ResourceId,
    second: &ResourceId,
) -> Result<(), LayoutOpsError> {
    if first == second {
        return Err(LayoutOpsError::SamePane);
    }
    require_window(workspace, first)?;
    require_window(workspace, second)?;
    for window in &mut workspace.windows {
        if let Some(tree) = window.state.tree.as_ref() {
            window.state.tree = Some(swap_leaves(tree, first, second)?);
        }
    }
    // Focus follows Terminal identity, not physical leaf position.
    Ok(())
}

/// Remove `target`, refusing to close the workspace's final pane.
fn apply_close(workspace: &mut Workspace, target: &ResourceId) -> Result<(), LayoutOpsError> {
    workspace.close_pane(target).map_err(|err| match err {
        LayoutError::LastPane => LayoutOpsError::LastPane,
        LayoutError::PaneNotInLayout(id) => LayoutOpsError::ForeignTarget(id),
        err @ LayoutError::InvalidRatio(_) => LayoutOpsError::Layout(err),
    })
}

fn apply_move(
    workspace: &mut Workspace,
    source: &ResourceId,
    target: &ResourceId,
    dir: SplitDir,
    ratio: f32,
) -> Result<(), LayoutOpsError> {
    if source == target {
        return Err(LayoutOpsError::SamePane);
    }
    let source_index = require_window(workspace, source)?;
    let target_index = require_window(workspace, target)?;
    // Validate the destination and ratio before collapsing the source so the
    // public pure helper is transactional on ordinary validation errors.
    let target_tree = window_tree(workspace, target_index, target)?;
    let _ = split_at(target_tree, target, source, dir, ratio)?;

    if source_index == target_index {
        move_within_window(workspace, source_index, source, target, dir, ratio)
    } else {
        move_across_windows(
            workspace,
            MoveWindows {
                source_index,
                target_index,
            },
            source,
            target,
            dir,
            ratio,
        )
    }
}

/// The pair of window indices a cross-window move connects.
#[derive(Debug, Clone, Copy)]
struct MoveWindows {
    source_index: usize,
    target_index: usize,
}

/// Re-place `source` next to `target` inside the single window holding both:
/// collapse the source leaf first, then split the collapsed tree.
fn move_within_window(
    workspace: &mut Workspace,
    index: usize,
    source: &ResourceId,
    target: &ResourceId,
    dir: SplitDir,
    ratio: f32,
) -> Result<(), LayoutOpsError> {
    let tree = window_tree(workspace, index, source)?;
    let collapsed = kill_pane(tree, source)?.ok_or(LayoutOpsError::LastPane)?;
    let moved = split_at(&collapsed, target, source, dir, ratio)?;
    workspace.windows[index].state.tree = Some(moved);
    workspace.windows[index].state.focus = Some(source.clone());
    workspace.active = index;
    Ok(())
}

/// Move `source` out of its window and into `target`'s, repairing the vacated
/// window's focus and pruning it if it emptied.
fn move_across_windows(
    workspace: &mut Workspace,
    windows: MoveWindows,
    source: &ResourceId,
    target: &ResourceId,
    dir: SplitDir,
    ratio: f32,
) -> Result<(), LayoutOpsError> {
    let MoveWindows {
        source_index,
        target_index,
    } = windows;
    let source_tree = window_tree(workspace, source_index, source)?;
    workspace.windows[source_index].state.tree = kill_pane(source_tree, source)?;
    repair_focus(&mut workspace.windows[source_index].state);

    let target_tree = window_tree(workspace, target_index, target)?;
    workspace.windows[target_index].state.tree =
        Some(split_at(target_tree, target, source, dir, ratio)?);
    workspace.windows[target_index].state.focus = Some(source.clone());
    workspace.active = target_index;
    workspace.prune_empty_windows();
    Ok(())
}

fn find_window(workspace: &Workspace, target: &ResourceId) -> Option<usize> {
    workspace.windows.iter().position(|window| {
        window
            .state
            .tree
            .as_ref()
            .is_some_and(|tree| leaves(tree).contains(target))
    })
}

fn repair_focus(state: &mut crate::layout::LayoutState) {
    state.focus = state.tree.as_ref().and_then(|tree| {
        let panes = leaves(tree);
        state
            .focus
            .as_ref()
            .filter(|focus| panes.contains(focus))
            .cloned()
            .or_else(|| panes.into_iter().next())
    });
}

fn swap_leaves(
    node: &LayoutNode,
    first: &ResourceId,
    second: &ResourceId,
) -> Result<LayoutNode, LayoutOpsError> {
    match node {
        LayoutNode::Leaf(id) if id == first => Ok(LayoutNode::Leaf(second.clone())),
        LayoutNode::Leaf(id) if id == second => Ok(LayoutNode::Leaf(first.clone())),
        LayoutNode::Leaf(id) => Ok(LayoutNode::Leaf(id.clone())),
        LayoutNode::Split {
            dir,
            ratio,
            left,
            right,
        } => Ok(LayoutNode::Split {
            dir: *dir,
            ratio: *ratio,
            left: Box::new(swap_leaves(left, first, second)?),
            right: Box::new(swap_leaves(right, first, second)?),
        }),
        _ => Err(LayoutOpsError::UnsupportedLayoutNode),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{LayoutState, WindowState};
    use crate::testkit::{ScriptSpec, ScriptedServer};
    use phux_protocol::wire::frame::ErrorCode;

    fn tid(id: u32) -> ResourceId {
        ResourceId::local(id)
    }

    /// phux-k0cw: the family test became a WHOSE test, because a client that
    /// watches peers must not adopt their layouts.
    #[test]
    fn layout_key_session_names_the_owner_not_just_the_family() {
        // The bare legacy key predates per-session keying, so it names no
        // session and can only be our own.
        assert_eq!(layout_key_session(LAYOUT_KEY), Some(LayoutKeyOwner::Legacy));
        assert_eq!(
            layout_key_session("phux.tui.layout/v1/7"),
            Some(LayoutKeyOwner::Session(SessionId::new(7)))
        );
        assert_eq!(
            layout_key_session(&layout_key(SessionId::new(42))),
            Some(LayoutKeyOwner::Session(SessionId::new(42))),
            "the writer and the reader must agree on the key shape"
        );
        // Unparsable suffix: NOT `Legacy`. That is the answer the caller
        // adopts as its own layout — so an unattributable key must drop out
        // of the family entirely rather than be mistaken for ours.
        assert_eq!(layout_key_session("phux.tui.layout/v1/zzz"), None);
        // A key that merely shares the prefix-without-separator is not in the
        // family, and unrelated keys aren't either.
        assert_eq!(layout_key_session("phux.tui.layout/v12"), None);
        assert_eq!(layout_key_session("phux.tui.other/v1"), None);
    }

    /// ADR-0129: the TUI must never adopt a private (non-default) named
    /// projection as its own layout. `layout_key_session` is the one gate
    /// the TUI reconciliation path uses to decide "is this my layout?", so
    /// pinning it against every custom `--projection` prefix here is what
    /// keeps a private arrangement invisible to the shared TUI/Cockpit view.
    #[test]
    fn tui_never_recognizes_a_private_projection_key_as_its_own() {
        for foreign in [
            "myapp.layout/v1/7",
            "scratch.layout/v1/7",
            "a.b.c.layout/v1/7",
        ] {
            assert_eq!(
                layout_key_session(foreign),
                None,
                "{foreign:?} names a private projection, not the TUI's own key"
            );
        }
        // The TUI's own default key for the same session IS recognized —
        // the contrast that proves the above isn't an accidental blanket
        // rejection.
        assert_eq!(
            layout_key_session("phux.tui.layout/v1/7"),
            Some(LayoutKeyOwner::Session(SessionId::new(7)))
        );
    }

    #[test]
    fn projection_key_must_be_a_layout_v1_key_for_the_addressed_session() {
        let session = SessionId::new(7);
        assert!(validate_projection_key("phux.tui.layout/v1/7", session).is_ok());
        assert!(validate_projection_key("myapp.layout/v1/7", session).is_ok());
        assert!(matches!(
            validate_projection_key("myapp.layout/v1/8", session),
            Err(LayoutOpsError::InvalidProjectionKey(_))
        ));
        assert!(matches!(
            validate_projection_key("not-a-layout-key", session),
            Err(LayoutOpsError::InvalidProjectionKey(_))
        ));
        assert!(matches!(
            validate_projection_key(".layout/v1/7", session),
            Err(LayoutOpsError::InvalidProjectionKey(_))
        ));
    }

    /// The session-id segment must be canonical decimal: no leading zero,
    /// no leading `+`, nothing but ASCII digits. A non-canonical id that
    /// merely *parses* to the right session would validate here but never
    /// match the literal key the server's reap cleanup deletes
    /// (`state/reap.rs`'s exact `.layout/v1/<id>` suffix match), orphaning
    /// it forever.
    #[test]
    fn projection_key_session_id_must_be_canonical_decimal() {
        let session = SessionId::new(7);
        for non_canonical in [
            "myapp.layout/v1/07",
            "myapp.layout/v1/+7",
            "myapp.layout/v1/7 ",
        ] {
            assert!(
                matches!(
                    validate_projection_key(non_canonical, session),
                    Err(LayoutOpsError::InvalidProjectionKey(_))
                ),
                "{non_canonical:?} must be rejected as non-canonical"
            );
            assert_eq!(
                projection_key_session(non_canonical),
                None,
                "{non_canonical:?} must not resolve to any session"
            );
        }
        // The zero session id is its own canonical form.
        assert_eq!(
            projection_key_session("myapp.layout/v1/0"),
            Some(SessionId::new(0))
        );
    }

    /// A prefix that itself contains the `.layout/v1/` separator is
    /// rejected rather than matched on its last occurrence — otherwise a
    /// key like `a.layout/v1/b.layout/v1/7` would silently validate with
    /// `a.layout/v1/b` as "the prefix".
    #[test]
    fn projection_key_prefix_must_not_contain_the_separator_itself() {
        assert_eq!(projection_key_session("a.layout/v1/b.layout/v1/7"), None);
    }

    fn split(left: u32, right: u32, dir: SplitDir, ratio: f32) -> LayoutNode {
        LayoutNode::Split {
            dir,
            ratio,
            left: Box::new(LayoutNode::Leaf(tid(left))),
            right: Box::new(LayoutNode::Leaf(tid(right))),
        }
    }

    fn two_window_workspace() -> Workspace {
        Workspace {
            windows: vec![
                WindowState::new(
                    "editor".to_owned(),
                    LayoutState {
                        tree: Some(split(1, 2, SplitDir::Horizontal, 0.6)),
                        focus: Some(tid(1)),
                    },
                ),
                WindowState::new("tests".to_owned(), LayoutState::single(tid(3))),
            ],
            active: 0,
        }
    }

    // Fixed bytes emitted by the pre-window v1 encoder. Keeping this literal
    // prevents a current encoder change from weakening the old-schema refusal
    // fixture along with the decoder under test.
    const LEGACY_V1_FIXTURE: &[u8] = &[
        163, 103, 118, 101, 114, 115, 105, 111, 110, 1, 100, 114, 111, 111, 116, 165, 100, 107,
        105, 110, 100, 101, 115, 112, 108, 105, 116, 99, 100, 105, 114, 104, 118, 101, 114, 116,
        105, 99, 97, 108, 101, 114, 97, 116, 105, 111, 249, 52, 0, 100, 108, 101, 102, 116, 162,
        100, 107, 105, 110, 100, 100, 108, 101, 97, 102, 100, 112, 97, 110, 101, 162, 100, 107,
        105, 110, 100, 101, 108, 111, 99, 97, 108, 98, 105, 100, 1, 101, 114, 105, 103, 104, 116,
        162, 100, 107, 105, 110, 100, 100, 108, 101, 97, 102, 100, 112, 97, 110, 101, 162, 100,
        107, 105, 110, 100, 101, 108, 111, 99, 97, 108, 98, 105, 100, 2, 101, 102, 111, 99, 117,
        115, 162, 100, 107, 105, 110, 100, 101, 108, 111, 99, 97, 108, 98, 105, 100, 2,
    ];

    #[test]
    fn fixed_v1_fixture_is_refused_without_implicit_rewrite() {
        assert!(matches!(
            Workspace::decode_cbor(LEGACY_V1_FIXTURE),
            Err(LayoutDecodeError::UnsupportedVersion(1))
        ));
    }

    #[test]
    fn current_fixture_supports_split_and_swap() {
        let fixture = two_window_workspace().encode_cbor().unwrap();
        let mut workspace = Workspace::decode_cbor(&fixture).unwrap();

        apply_mutation(
            &mut workspace,
            &LayoutMutation::Split {
                target: tid(3),
                new_pane: tid(4),
                dir: SplitDir::Vertical,
                ratio: 0.3,
            },
        )
        .unwrap();
        assert_eq!(workspace.windows[1].state.focus, Some(tid(4)));
        assert_eq!(
            leaves(workspace.windows[1].state.tree.as_ref().unwrap()),
            vec![tid(3), tid(4)]
        );

        apply_mutation(
            &mut workspace,
            &LayoutMutation::Swap {
                first: tid(1),
                second: tid(4),
            },
        )
        .unwrap();
        assert_eq!(
            leaves(workspace.windows[0].state.tree.as_ref().unwrap()),
            vec![tid(4), tid(2)]
        );
        assert_eq!(
            leaves(workspace.windows[1].state.tree.as_ref().unwrap()),
            vec![tid(3), tid(1)]
        );
    }

    #[test]
    fn headless_split_preserves_serialized_focus_and_active_window() {
        let mut workspace = two_window_workspace();
        apply_mutation(
            &mut workspace,
            &LayoutMutation::SplitPreservingFocus {
                target: tid(3),
                new_pane: tid(4),
                dir: SplitDir::Vertical,
                ratio: 0.3,
            },
        )
        .unwrap();
        assert_eq!(workspace.active, 0);
        assert_eq!(workspace.windows[0].state.focus, Some(tid(1)));
        assert_eq!(workspace.windows[1].state.focus, Some(tid(3)));
        assert_eq!(
            leaves(workspace.windows[1].state.tree.as_ref().unwrap()),
            vec![tid(3), tid(4)]
        );
    }

    #[test]
    fn close_collapses_nested_parent_and_repairs_focus() {
        let nested = LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.5,
            left: Box::new(split(1, 2, SplitDir::Vertical, 0.4)),
            right: Box::new(LayoutNode::Leaf(tid(3))),
        };
        let mut workspace = Workspace {
            windows: vec![WindowState::new(
                "1".to_owned(),
                LayoutState {
                    tree: Some(nested),
                    focus: Some(tid(2)),
                },
            )],
            active: 0,
        };
        apply_mutation(&mut workspace, &LayoutMutation::Close { target: tid(2) }).unwrap();
        assert_eq!(
            leaves(workspace.active_window().unwrap().tree.as_ref().unwrap()),
            vec![tid(1), tid(3)]
        );
        assert_eq!(workspace.active_window().unwrap().focus, Some(tid(1)));
    }

    #[test]
    fn move_collapses_source_and_can_remove_an_empty_window() {
        let mut workspace = two_window_workspace();
        apply_mutation(
            &mut workspace,
            &LayoutMutation::Move {
                source: tid(3),
                target: tid(2),
                dir: SplitDir::Vertical,
                ratio: 0.7,
            },
        )
        .unwrap();
        assert_eq!(workspace.windows.len(), 1);
        assert_eq!(
            leaves(workspace.windows[0].state.tree.as_ref().unwrap()),
            vec![tid(1), tid(2), tid(3)]
        );
        assert_eq!(workspace.windows[0].state.focus, Some(tid(3)));
        let LayoutNode::Split { right, .. } = workspace.windows[0].state.tree.as_ref().unwrap()
        else {
            panic!("expected outer split");
        };
        assert_eq!(leaves(right), vec![tid(2), tid(3)]);
    }

    #[test]
    fn malformed_and_foreign_targets_are_rejected_without_mutation() {
        assert!(matches!(
            Workspace::decode_cbor(b"not cbor"),
            Err(LayoutDecodeError::Cbor(_))
        ));
        let mut workspace = two_window_workspace();
        let original = workspace.clone();
        let err =
            apply_mutation(&mut workspace, &LayoutMutation::Close { target: tid(99) }).unwrap_err();
        assert!(matches!(err, LayoutOpsError::ForeignTarget(id) if id == tid(99)));
        assert_eq!(workspace, original);

        let err = apply_mutation(
            &mut workspace,
            &LayoutMutation::Split {
                target: tid(1),
                new_pane: tid(2),
                dir: SplitDir::Horizontal,
                ratio: 0.5,
            },
        )
        .unwrap_err();
        assert!(matches!(err, LayoutOpsError::DuplicatePane(id) if id == tid(2)));
        assert!(matches!(
            apply_mutation(
                &mut Workspace::single(tid(1)),
                &LayoutMutation::Close { target: tid(1) }
            ),
            Err(LayoutOpsError::LastPane)
        ));

        let original = workspace.clone();
        assert!(matches!(
            apply_mutation(
                &mut workspace,
                &LayoutMutation::Move {
                    source: tid(1),
                    target: tid(3),
                    dir: SplitDir::Horizontal,
                    ratio: f32::NAN,
                }
            ),
            Err(LayoutOpsError::Layout(LayoutError::InvalidRatio(ratio))) if ratio.is_nan()
        ));
        assert_eq!(workspace, original, "a rejected move is transactional");
    }

    #[tokio::test]
    async fn mutate_correlates_replies_and_confirms_set() {
        let (client_stream, server_stream) = tokio::net::UnixStream::pair().unwrap();
        let mut client = Connection::from_stream(client_stream);
        let initial = two_window_workspace();

        // The harness stores what this session SETs and hands it back on the
        // confirming GET, so the read-modify-write round trip runs against
        // the same read-your-own-write behaviour `handle_set_metadata` /
        // `handle_get_metadata` give it — not a canned echo. The
        // METADATA_VALUE for request 999 belongs to a different pipelined
        // request and is pushed AHEAD of this one's reply, which is the only
        // ordering in which mis-correlation is a hazard.
        let spec = ScriptSpec::new()
            .foreign_metadata_value(999)
            .stored_metadata(
                Scope::Group(DEFAULT_LAYOUT_GROUP_ID),
                &layout_key(SessionId::new(7)),
                initial.encode_cbor().unwrap(),
            );
        let server_task = tokio::spawn(ScriptedServer::on_stream(server_stream, spec).run());

        let confirmed = LayoutOps::new(&mut client, SessionId::new(7), 10)
            .mutate(LayoutMutation::Swap {
                first: tid(1),
                second: tid(2),
            })
            .await
            .unwrap();
        assert_eq!(
            leaves(confirmed.windows[0].state.tree.as_ref().unwrap()),
            vec![tid(2), tid(1)]
        );
        // `ops` only borrows the connection; dropping the connection is what
        // ends the harness's serve loop.
        drop(client);

        let seen = server_task.await.unwrap();
        assert!(
            matches!(
                seen.first(),
                Some(FrameKind::GetMetadata { request_id: 10, scope, key })
                    if *scope == Scope::Group(DEFAULT_LAYOUT_GROUP_ID)
                        && *key == layout_key(SessionId::new(7))
            ),
            "the read leg is a group-scoped GET on the session's layout key; got {:?}",
            seen.first()
        );
        let Some(FrameKind::SetMetadata {
            request_id: 11,
            value,
            ..
        }) = seen.get(1)
        else {
            panic!("expected SET on request 11, got {:?}", seen.get(1));
        };
        let written = Workspace::decode_cbor(value).unwrap();
        assert_eq!(
            leaves(written.windows[0].state.tree.as_ref().unwrap()),
            vec![tid(2), tid(1)]
        );
        assert!(
            matches!(
                seen.get(2),
                Some(FrameKind::GetMetadata { request_id: 12, .. })
            ),
            "the confirming GET closes the CAS; got {:?}",
            seen.get(2)
        );
    }

    /// ADR-0129: `--projection` writes and reads the named key it was given,
    /// never the shared `phux.tui.layout/v1/<session>` default.
    #[tokio::test]
    async fn insert_pane_with_projection_writes_the_named_key_and_leaves_the_default_untouched() {
        let (client_stream, server_stream) = tokio::net::UnixStream::pair().unwrap();
        let mut client = Connection::from_stream(client_stream);
        let initial = two_window_workspace();
        let named_key = "myapp.layout/v1/7".to_owned();
        let spec = ScriptSpec::new().stored_metadata(
            Scope::Group(DEFAULT_LAYOUT_GROUP_ID),
            &named_key,
            initial.encode_cbor().unwrap(),
        );
        let server_task = tokio::spawn(ScriptedServer::on_stream(server_stream, spec).run());

        let mut ops =
            LayoutOps::with_key(&mut client, SessionId::new(7), named_key.clone(), 10).unwrap();
        let confirmed = ops
            .mutate(LayoutMutation::Swap {
                first: tid(1),
                second: tid(2),
            })
            .await
            .unwrap();
        assert_eq!(
            leaves(confirmed.windows[0].state.tree.as_ref().unwrap()),
            vec![tid(2), tid(1)]
        );
        drop(ops);
        drop(client);

        let seen = server_task.await.unwrap();
        assert!(
            seen.iter().any(
                |frame| matches!(frame, FrameKind::GetMetadata { key, .. } if key == &named_key)
            ),
            "the read must target the named projection key, not the default"
        );
        assert!(
            !seen.iter().any(|frame| matches!(
                frame,
                FrameKind::GetMetadata { key, .. } | FrameKind::SetMetadata { key, .. }
                    if key == &layout_key(SessionId::new(7))
            )),
            "the shared default key must never be touched by a --projection request"
        );
    }

    /// ADR-0129 review item 5(a): a confirming read that decodes fine but
    /// is not byte-for-byte what was just written — here, because the
    /// server silently dropped the `SET_METADATA` (the same shape a value
    /// over `limits.metadata-value-bytes` has) and the confirming `GET`
    /// still sees the old stored value — must not be accepted as this
    /// write's own result.
    #[tokio::test]
    async fn write_and_confirm_refuses_a_read_back_that_does_not_match_what_was_written() {
        let (client_stream, server_stream) = tokio::net::UnixStream::pair().unwrap();
        let mut client = Connection::from_stream(client_stream);
        let stale = Workspace::single(tid(9)).encode_cbor().unwrap();
        let key = layout_key(SessionId::new(7));
        let spec = ScriptSpec::new()
            .stored_metadata(Scope::Group(DEFAULT_LAYOUT_GROUP_ID), &key, stale)
            .drop_metadata_writes(Scope::Group(DEFAULT_LAYOUT_GROUP_ID), &key);
        let server_task = tokio::spawn(ScriptedServer::on_stream(server_stream, spec).run());

        let result = LayoutOps::new(&mut client, SessionId::new(7), 10)
            .write_and_confirm(&Workspace::single(tid(1)))
            .await;
        assert!(
            matches!(result, Err(LayoutOpsError::NotConfirmed)),
            "a dropped write's stale read-back must not decode as success, got {result:?}"
        );
        drop(client);
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn correlated_error_is_reported() {
        let (client_stream, server_stream) = tokio::net::UnixStream::pair().unwrap();
        let mut client = Connection::from_stream(client_stream);
        let spec = ScriptSpec::new().refuse_metadata(ErrorCode::InvalidCommand, "foreign group");
        let server_task = tokio::spawn(ScriptedServer::on_stream(server_stream, spec).run());
        let result = LayoutOps::in_group(&mut client, SessionId::new(1), GroupId::new(77), 5)
            .read()
            .await;
        assert!(
            matches!(result, Err(LayoutOpsError::Refused(message)) if message == "foreign group")
        );
        drop(client);
        server_task.await.unwrap();
    }
}
