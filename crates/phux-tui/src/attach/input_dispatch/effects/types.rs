//! The effect vocabulary: what a resolved action asks the async pass to
//! do, and the pending state some effects park.

use phux_protocol::ResourceId;
use phux_protocol::ids::SessionId;
use phux_protocol::wire::frame::FrameKind;

use crate::attach::actions::{PendingSplit, PendingWindow};
use crate::attach::directory_picker::PendingDirectory;
use crate::attach::jobs::Job;
use crate::layout::{ArrangementDoc, SplitDir};

/// Side-effects a resolved action wants from the driver.
#[derive(Debug, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "action dispatcher returns independent side-effect flags to keep async I/O outside run_action"
)]
pub(in crate::attach) struct ActionEffects {
    /// `true` ⇒ the active window was mutated in-place; driver repaints.
    pub(in crate::attach) layout_mutated: bool,
    /// `true` ⇒ flip the driver's pane-zoom state (zoom the
    /// focused pane to fill the window, or un-zoom). `apply_action_effects`
    /// owns the actual toggle since the `zoomed` state lives in the driver.
    pub(in crate::attach) toggle_zoom: bool,
    /// Flip the driver's sidebar state (and reflow).
    pub(in crate::attach) toggle_sidebar: bool,
    /// ADR-0168: a project group key whose sidebar collapse to flip.
    pub(in crate::attach) toggle_project: Option<String>,
    /// An `agents-filter` change for the sidebar's Agents panel.
    pub(in crate::attach) agents_filter: Option<crate::render::chrome::sidebar::FilterRequest>,
    /// The new focused pane, already applied to the active window.
    pub(in crate::attach) set_focus: Option<ResourceId>,
    /// `true` ⇒ emit `SET_METADATA` carrying the new layout envelope.
    pub(in crate::attach) set_metadata: bool,
    /// `true` ⇒ emit a terminal bell (BEL `\x07`).
    pub(in crate::attach) bell: bool,
    /// The active window changed: drop the prediction queue.
    pub(in crate::attach) clear_predict: bool,
    /// `true` ⇒ emit `DETACH` and wait for `DETACHED`.
    pub(in crate::attach) detach: bool,
    /// A parked `split-pane` spawn: send, then park in `pending_splits`.
    pub(in crate::attach) spawn_terminal: Option<(u32, PendingSplit, FrameKind)>,
    /// A parked `new-window` spawn: send, then park in `pending_windows`.
    pub(in crate::attach) spawn_window: Option<(u32, PendingWindow, FrameKind)>,
    /// ADR-0147: a floating plugin overlay spawn `(request, title, frame)`:
    /// send, then park the title in `pending_floating`.
    pub(in crate::attach) spawn_floating: Option<(u32, String, FrameKind)>,
    /// A picker-confirmed existing-pane move. The async effect applier opens a
    /// dedicated control connection and runs the canonical headless operation.
    pub(in crate::attach) move_pane: Option<PaneMoveIntent>,
    /// A session management request, prepared or confirmed on a control connection.
    pub(in crate::attach) session_management: Option<phux_config::keybind::ResolvedAction>,
    /// Background work to start off the input loop (ADR-0172): a plugin
    /// action or an organize run. Its result returns as a toast.
    pub(in crate::attach) job: Option<Job>,
    /// A `go-to-directory` action built a `LIST_DIRECTORY` request. The
    /// async caller records it (id and listed host) as the pending listing,
    /// then sends it; the reply opens the directory picker.
    pub(in crate::attach) list_directory: Option<(PendingDirectory, FrameKind)>,
    pub(in crate::attach) query_path: Option<(crate::attach::path_picker::PendingPath, FrameKind)>,
    pub(in crate::attach) insert_path: Option<(ResourceId, String)>,
    /// `KILL_RESOURCE` commands, one per targeted Terminal, sent in order.
    pub(in crate::attach) kill_frames: Vec<FrameKind>,
    /// `(request_id, Terminal)` per kill, parked in `pending_kills` so a
    /// `TerminalNotFound` refusal can fold a dead leaf out.
    pub(in crate::attach) kill_requests: Vec<(u32, ResourceId)>,
    /// The kill targets, parked in `expected_closes` to silence their notices.
    pub(in crate::attach) expected_closes: Vec<ResourceId>,
    /// ADR-0033 supervisory commands for the focused pane, sent in order.
    pub(in crate::attach) command_frames: Vec<FrameKind>,
    /// An in-process re-attach (switch or create) for the driver.
    pub(in crate::attach) reattach: Option<ReattachTarget>,
    /// ADR-0140: `switch-host { host, name }` committed. Unlike
    /// [`Self::reattach`] this leaves the server: the batch detaches, then
    /// the process becomes `phux attach` against that machine
    /// ([`crate::attach::hosts::exec_switch_host`]). `(host, session)`.
    pub(in crate::attach) switch_host: Option<(String, String)>,
    /// rename-session: a committed rename. Carries the new name. The async
    /// caller ([`apply_action_effects`]) sends a `SESSION_RENAME` for the
    /// *current* session against an `ORGANIZATION` server, else a
    /// `SET_METADATA` write of `SESSION_NAME_KEY` plus a correlated
    /// `GET_STATE` barrier, and parks [`PendingSessionRename`] so the driver
    /// can apply or refuse from the reply. The local status name stays
    /// unchanged until that confirmation (or the `SESSION_RENAMED` event or
    /// `METADATA_CHANGED` broadcast) so a rejected rename cannot lie. Peers
    /// learn the new name from the same event or broadcast rather than
    /// waiting for a re-attach.
    pub(in crate::attach) rename_session: Option<String>,
    /// A requested live config reload, run by the driver after the batch.
    pub(in crate::attach) reload_config: bool,
    /// OSC 52 clipboard payload to emit on the host terminal after this
    /// action (the `report-bug` path copies the bundle path).
    pub(in crate::attach) clipboard: Option<String>,
}

/// Exact move selected by the fuzzy destination picker.
#[derive(Debug)]
pub(in crate::attach) struct PaneMoveIntent {
    pub(in crate::attach) source: ResourceId,
    pub(in crate::attach) target: ResourceId,
    pub(in crate::attach) dir: SplitDir,
    pub(in crate::attach) ratio: f32,
}

/// An in-process re-attach raised by `switch-session`/`new-session`; the
/// outer loop re-attaches on the same connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReattachTarget {
    /// Switch to an existing session (`switch-session`).
    Existing {
        /// Target session display name. Used when `id` is absent (a typed
        /// name or a satellite hop) and as the human-facing log label.
        name: String,
        /// Stable session identity from a painted row (a rename cannot
        /// retarget it); `None` for satellite rows.
        id: Option<SessionId>,
        /// Window to select once the target's layout loads; out of range
        /// is a logged no-op.
        window: Option<usize>,
        /// DFS leaf ordinal within `window` to focus, likewise.
        pane: Option<usize>,
        /// Pane identity to focus after re-attach, instead of indices.
        resource: Option<ResourceId>,
    },
    /// Create — or attach to, if it already exists — a session by name
    /// (`new-session`). `directory` and `host` come from the focused pane
    /// or an explicit pick, never from the client process's working directory.
    Create {
        /// Session name.
        name: String,
        /// Seed-pane directory. `None` leaves the server default.
        directory: Option<String>,
        /// Satellite host. `None` creates on the attached server.
        host: Option<String>,
    },
}

/// An in-flight `rename-session` waiting on its `SESSION_RENAME` reply, or
/// on its `GET_STATE` barrier (`SET_METADATA` has no reply).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::attach) struct PendingSessionRename {
    /// Request id of the `SESSION_RENAME`, or of the confirming `GET_STATE`.
    pub barrier: u32,
    /// The attached session's id when the rename was sent, if known.
    pub session_id: Option<SessionId>,
    /// Display name at send time (`current` in `current\0new`).
    pub current: String,
    /// Requested replacement name.
    pub new_name: String,
    /// Sent as `SESSION_RENAME` (ADR-0161): the reply is `COMMITTED` or
    /// `REFUSED`, not a snapshot.
    pub organization: bool,
}

/// Encode the workspace for `SET_METADATA`, logging encode failures.
/// Returns `None` on failure — caller should not emit a frame in that case.
pub(in crate::attach) fn encode_layout_or_log(workspace: &ArrangementDoc) -> Option<Vec<u8>> {
    match workspace.encode_cbor() {
        Ok(bytes) => Some(bytes),
        Err(err) => {
            tracing::warn!(
                error = %err,
                reason = %err,
                "arrangement save skipped"
            );
            None
        }
    }
}
