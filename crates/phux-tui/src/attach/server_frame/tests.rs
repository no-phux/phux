//! Frame-handler tests: engine routing and barriers, layout metadata
//! reconciliation, output/snapshot paint policy, lifecycle events, and
//! close/detach endings.
#![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

use super::{
    AgentMetaIndex, FrameOutcome, attach_agent_sessions, attach_participants,
    handle_server_frame as handle_server_frame_with_kernel, route_engine_frame,
};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use phux_protocol::ResourceKind;
use phux_protocol::ids::{ClientId, ResourceId, SessionId, WindowId};
use phux_protocol::wire::frame::{CloseReason, DetachReason, FrameKind};
use phux_protocol::wire::info::{
    AgentFacet, LayoutNode, ResourceInfo, SessionInfo, SessionSnapshot, SplitDir, WindowInfo,
};

use crate::attach::outcome::{AttachEnd, AttachError};
use crate::attach::pane_state::PaneSlot;
use crate::attach::render::ReplicaWalk;
use crate::layout::{LayoutState, Workspace};
use crate::predict::{Overlay, PredictionState, PredictiveConfig};

static TRACE_TEST_LOCK: Mutex<()> = Mutex::new(());

/// phux-atch: the attach participant set is the FOCUSED session's panes,
/// not every pane in the snapshot.
///
/// The snapshot is a whole-workspace view by contract, but the server
/// bootstraps only the focused session's panes. Counting the rest as
/// participants left them permanently unresolved, so `ATTACH_READY` was
/// rejected with `remaining` equal to the other sessions' pane count —
/// attaching worked with one session on the server and failed with two or
/// more. This asserts the count directly, because that arithmetic *is*
/// the bug.
#[test]
fn attach_participants_cover_only_the_focused_session() {
    let focused = SessionId::new(1);
    let other = SessionId::new(2);
    let focused_window = WindowId::new(10);
    let other_window = WindowId::new(20);

    let snapshot = SessionSnapshot::new(focused, focused_window, ResourceId::new(100))
        .with_sessions(vec![
            SessionInfo::new(focused, "focused".to_owned()),
            SessionInfo::new(other, "other".to_owned()),
        ])
        .with_windows(vec![
            WindowInfo::new(focused_window, focused, "w0".to_owned()),
            WindowInfo::new(other_window, other, "w0".to_owned()),
        ])
        .with_resources(vec![
            ResourceInfo::new(ResourceId::new(100), focused_window, 80, 24),
            ResourceInfo::new(ResourceId::new(101), focused_window, 80, 24),
            // Belongs to a session this attach does not touch; the server
            // will never bootstrap it.
            ResourceInfo::new(ResourceId::new(200), other_window, 80, 24),
        ]);

    let participants = attach_participants(&snapshot);

    assert_eq!(
        participants,
        vec![ResourceId::new(100), ResourceId::new(101)],
        "only the focused session's panes are bootstrapped, so only they \
             may be attach participants",
    );
    assert!(
        !participants.contains(&ResourceId::new(200)),
        "a pane from another session would never resolve, and ATTACH_READY \
             would be rejected for as many panes as the other sessions hold",
    );
}

/// The single-session case must be unchanged — it is the one shape that
/// worked before, and the fix must not narrow it.
#[test]
fn a_single_session_snapshot_keeps_every_pane() {
    let session = SessionId::new(1);
    let window = WindowId::new(10);
    let snapshot = SessionSnapshot::new(session, window, ResourceId::new(100))
        .with_sessions(vec![SessionInfo::new(session, "only".to_owned())])
        .with_windows(vec![WindowInfo::new(window, session, "w0".to_owned())])
        .with_resources(vec![
            ResourceInfo::new(ResourceId::new(100), window, 80, 24),
            ResourceInfo::new(ResourceId::new(101), window, 80, 24),
        ]);

    assert_eq!(
        attach_participants(&snapshot),
        vec![ResourceId::new(100), ResourceId::new(101)]
    );
}

/// Strip CSI escape sequences (`ESC [ ... final`) from a captured
/// render stream, leaving only the printable glyphs, so a content
/// assertion can't be satisfied by control bytes that happen to share
/// a letter (e.g. the `h`/`l` in `\x1b[?25h` / `\x1b[?25l`).
fn strip_csi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next(); // consume '['
            // Consume params/intermediates up to the final byte (@..~).
            for n in chars.by_ref() {
                if ('@'..='~').contains(&n) {
                    break;
                }
            }
        } else if c != '\x1b' {
            out.push(c);
        }
    }
    out
}

fn tid(id: u32) -> ResourceId {
    ResourceId::local(id)
}
fn stream() -> phux_protocol::StreamId {
    phux_protocol::StreamId::new(1).expect("stream")
}

fn bootstrap() -> phux_protocol::BootstrapId {
    phux_protocol::BootstrapId::new(1).expect("bootstrap")
}

fn begin_frame(terminal_id: &ResourceId) -> FrameKind {
    FrameKind::BootstrapBegin {
        terminal_id: terminal_id.clone(),
        stream_id: stream(),
        bootstrap_id: bootstrap(),
        profile: phux_protocol::BootstrapStreamProfile::SynthesizedVtRaw,
        cols: 80,
        rows: 24,
        base_seq: 0,
    }
}

fn ready_frame(terminal_id: &ResourceId) -> FrameKind {
    FrameKind::BootstrapReady {
        terminal_id: terminal_id.clone(),
        stream_id: stream(),
        bootstrap_id: bootstrap(),
        history_cursor: None,
    }
}

#[test]
fn engine_damage_obeys_attach_barrier_and_ready_publication() {
    let terminal_id = tid(90);
    let mut kernel = phux_client_core::session::SessionKernel::new(
        phux_client_core::engine::ghostty::GhosttyAdapter::new(
            phux_protocol::BootstrapLimits::default(),
        ),
        phux_protocol::BootstrapProfile::SynthesizedVtRaw,
    );
    let mut effects = phux_client_core::session::EffectBuffer::new();
    kernel
        .update(
            phux_client_core::session::KernelInput::AttachStarted {
                attach_id: 7,
                terminals: std::slice::from_ref(&terminal_id),
            },
            &mut effects,
        )
        .expect("attach");
    assert!(
        route_engine_frame(&begin_frame(&terminal_id), &mut kernel, &mut effects)
            .damaged
            .is_empty()
    );
    assert!(
        route_engine_frame(
            &FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id: stream(),
                bootstrap_id: bootstrap(),
                chunk_seq: 0,
                payload: bytes::Bytes::from_static(b"seed"),
            },
            &mut kernel,
            &mut effects,
        )
        .damaged
        .is_empty()
    );
    assert!(
        route_engine_frame(&ready_frame(&terminal_id), &mut kernel, &mut effects)
            .damaged
            .is_empty(),
        "publication damage stays behind ATTACH_READY"
    );
    assert!(
        route_engine_frame(
            &FrameKind::ResourceOutput {
                terminal_id: terminal_id.clone(),
                stream_id: stream(),
                bootstrap_id: bootstrap(),
                seq: 1,
                bytes: bytes::Bytes::from_static(b"before-barrier"),
            },
            &mut kernel,
            &mut effects,
        )
        .damaged
        .is_empty(),
        "pre-barrier live output must not paint directly"
    );
    let released = route_engine_frame(
        &FrameKind::AttachReady { attach_id: 7 },
        &mut kernel,
        &mut effects,
    );
    assert!(released.damaged(&terminal_id));
    let live = route_engine_frame(
        &FrameKind::ResourceOutput {
            terminal_id: terminal_id.clone(),
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            seq: 2,
            bytes: bytes::Bytes::from_static(b"after-barrier"),
        },
        &mut kernel,
        &mut effects,
    );
    assert!(live.damaged(&terminal_id));
    let reply = route_engine_frame(
        &FrameKind::ResourceOutput {
            terminal_id: terminal_id.clone(),
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            seq: 3,
            bytes: bytes::Bytes::from_static(b"\x1b[5n"),
        },
        &mut kernel,
        &mut effects,
    );
    assert_eq!(reply.pty_writes, vec![(terminal_id, b"\x1b[0n".to_vec())]);
}

#[test]
fn ready_history_cursor_is_preserved_into_kernel_request() {
    let terminal_id = tid(91);
    let mut kernel = phux_client_core::session::SessionKernel::new(
        phux_client_core::engine::ghostty::GhosttyAdapter::new(
            phux_protocol::BootstrapLimits::default(),
        ),
        phux_protocol::BootstrapProfile::SynthesizedVtRaw,
    );
    let mut effects = phux_client_core::session::EffectBuffer::new();
    kernel
        .update(
            phux_client_core::session::KernelInput::AttachStarted {
                attach_id: 8,
                terminals: std::slice::from_ref(&terminal_id),
            },
            &mut effects,
        )
        .expect("attach");
    route_engine_frame(&begin_frame(&terminal_id), &mut kernel, &mut effects);
    route_engine_frame(
        &FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            chunk_seq: 0,
            payload: bytes::Bytes::from_static(b"seed"),
        },
        &mut kernel,
        &mut effects,
    );
    let routed = route_engine_frame(
        &FrameKind::BootstrapReady {
            terminal_id: terminal_id.clone(),
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            history_cursor: Some(bytes::Bytes::from_static(b"opaque-cursor")),
        },
        &mut kernel,
        &mut effects,
    );
    assert_eq!(
        routed.history_request,
        Some((
            terminal_id.clone(),
            stream(),
            bootstrap(),
            bytes::Bytes::from_static(b"opaque-cursor"),
            1024 * 1024,
            1024,
        ))
    );
    let rejected = route_engine_frame(
        &FrameKind::HistoryRejected {
            terminal_id: terminal_id.clone(),
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            cursor: bytes::Bytes::from_static(b"opaque-cursor"),
            reason: phux_protocol::wire::frame::HistoryRejectionReason::TooSmall,
            required_bytes: 1024 * 1024,
            required_rows: 2048,
        },
        &mut kernel,
        &mut effects,
    );
    assert!(!rejected.resync_required);
    assert_eq!(
        rejected.history_request,
        Some((
            terminal_id.clone(),
            stream(),
            bootstrap(),
            bytes::Bytes::from_static(b"opaque-cursor"),
            1024 * 1024,
            2048,
        )),
        "a valid larger row requirement retries within the client hard cap"
    );
    let tombstoned = route_engine_frame(
        &FrameKind::HistoryTombstone {
            terminal_id: terminal_id.clone(),
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            cursor: bytes::Bytes::from_static(b"opaque-cursor"),
            reason: phux_protocol::wire::frame::HistoryTombstoneReason::Pruned,
        },
        &mut kernel,
        &mut effects,
    );
    assert!(!tombstoned.resync_required);
    assert!(
        kernel.published_engine(&terminal_id).is_some(),
        "history-only invalidation preserves the live replica"
    );
}

#[test]
fn off_window_ready_waits_for_every_snapshot_pane_and_attach_ready() {
    let focused = tid(94);
    let off_window = tid(95);
    let focused_window = WindowId::new(70);
    let other_window = WindowId::new(71);
    let session = SessionId::new(72);
    // Both windows belong to the ATTACHED session: the aggregate barrier
    // spans a session's windows, and the server bootstraps every pane in
    // it. The window entries are what make that resolvable — a real
    // `build_session_snapshot` always emits one per window, and the
    // attach participant set maps pane -> window -> session through them
    // (phux-atch).
    let snapshot = SessionSnapshot::new(session, focused_window, focused.clone())
        .with_windows(vec![
            WindowInfo::new(focused_window, session, "w0".to_owned()),
            WindowInfo::new(other_window, session, "w1".to_owned()),
        ])
        .with_resources(vec![
            ResourceInfo::new(focused.clone(), focused_window, 80, 24),
            ResourceInfo::new(off_window.clone(), other_window, 80, 24),
        ]);
    let mut kernel = phux_client_core::session::SessionKernel::new(
        phux_client_core::engine::ghostty::GhosttyAdapter::new(
            phux_protocol::BootstrapLimits::default(),
        ),
        phux_protocol::BootstrapProfile::SynthesizedVtRaw,
    );
    let mut effects = phux_client_core::session::EffectBuffer::new();
    let attached = route_engine_frame(
        &FrameKind::Attached {
            attach_id: 9,
            snapshot,
            initial_client_id: ClientId::new(1),
        },
        &mut kernel,
        &mut effects,
    );
    assert!(attached.damaged.is_empty());

    for terminal_id in [&off_window, &focused] {
        assert!(
            route_engine_frame(&begin_frame(terminal_id), &mut kernel, &mut effects)
                .damaged
                .is_empty()
        );
        assert!(
            route_engine_frame(
                &FrameKind::BootstrapChunk {
                    terminal_id: terminal_id.clone(),
                    stream_id: stream(),
                    bootstrap_id: bootstrap(),
                    chunk_seq: 0,
                    payload: bytes::Bytes::from_static(b"seed"),
                },
                &mut kernel,
                &mut effects,
            )
            .damaged
            .is_empty()
        );
        assert!(
            route_engine_frame(&ready_frame(terminal_id), &mut kernel, &mut effects)
                .damaged
                .is_empty(),
            "neither off-window nor focused READY may escape the aggregate barrier"
        );
    }
    let released = route_engine_frame(
        &FrameKind::AttachReady { attach_id: 9 },
        &mut kernel,
        &mut effects,
    );
    assert!(released.damaged(&focused));
    assert!(released.damaged(&off_window));
}

#[test]
fn bootstrap_ready_surfaces_publication_damage_without_attach_barrier() {
    let terminal_id = tid(91);
    let mut kernel = phux_client_core::session::SessionKernel::new(
        phux_client_core::engine::ghostty::GhosttyAdapter::new(
            phux_protocol::BootstrapLimits::default(),
        ),
        phux_protocol::BootstrapProfile::SynthesizedVtRaw,
    );
    let mut effects = phux_client_core::session::EffectBuffer::new();
    route_engine_frame(&begin_frame(&terminal_id), &mut kernel, &mut effects);
    route_engine_frame(
        &FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            chunk_seq: 0,
            payload: bytes::Bytes::from_static(b"seed"),
        },
        &mut kernel,
        &mut effects,
    );
    let ready = route_engine_frame(&ready_frame(&terminal_id), &mut kernel, &mut effects);
    assert!(ready.damaged(&terminal_id));
}
fn dispatch_engine_frame(
    kernel: &mut phux_client_core::session::SessionKernel<
        phux_client_core::engine::ghostty::GhosttyAdapter,
    >,
    effects: &mut phux_client_core::session::EffectBuffer,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    frame: FrameKind,
) -> FrameOutcome {
    let mut out = Vec::new();
    let mut workspace = Workspace::default();
    let mut focused_resource = None;
    let mut zoomed = None;
    let mut session_name = String::new();
    let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
    let overlay = Overlay;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    let mut expected_closes = HashSet::new();
    let mut agent_meta = AgentMetaIndex::default();
    handle_server_frame_with_kernel(
        kernel,
        effects,
        &mut out,
        frame,
        panes,
        &mut workspace,
        &mut focused_resource,
        &mut zoomed,
        &mut session_name,
        &mut false,
        None,
        None,
        None,
        (80, 24),
        &mut predict,
        &overlay,
        None,
        &mut pending_splits,
        &mut pending_windows,
        &mut expected_closes,
        &mut agent_meta,
        false,
        true,
    )
    .expect("engine frame")
}

#[test]
fn pre_barrier_output_refreshes_title_cache_before_attach_ready() {
    let ready_terminal = tid(92);
    let pending_terminal = tid(93);
    let mut kernel = phux_client_core::session::SessionKernel::new(
        phux_client_core::engine::ghostty::GhosttyAdapter::new(
            phux_protocol::BootstrapLimits::default(),
        ),
        phux_protocol::BootstrapProfile::SynthesizedVtRaw,
    );
    let mut effects = phux_client_core::session::EffectBuffer::new();
    let mut panes = HashMap::new();
    kernel
        .update(
            phux_client_core::session::KernelInput::AttachStarted {
                attach_id: 8,
                terminals: &[ready_terminal.clone(), pending_terminal.clone()],
            },
            &mut effects,
        )
        .expect("attach");

    dispatch_engine_frame(
        &mut kernel,
        &mut effects,
        &mut panes,
        begin_frame(&ready_terminal),
    );
    dispatch_engine_frame(
        &mut kernel,
        &mut effects,
        &mut panes,
        FrameKind::BootstrapChunk {
            terminal_id: ready_terminal.clone(),
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            chunk_seq: 0,
            payload: bytes::Bytes::from_static(b"\x1b]2;shell\x07"),
        },
    );
    dispatch_engine_frame(
        &mut kernel,
        &mut effects,
        &mut panes,
        ready_frame(&ready_terminal),
    );
    assert_eq!(panes[&ready_terminal].last_title, "shell");
    dispatch_engine_frame(
        &mut kernel,
        &mut effects,
        &mut panes,
        begin_frame(&pending_terminal),
    );

    let pre_barrier = dispatch_engine_frame(
        &mut kernel,
        &mut effects,
        &mut panes,
        FrameKind::ResourceOutput {
            terminal_id: ready_terminal.clone(),
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            seq: 1,
            bytes: bytes::Bytes::from_static(b"\x1b]2;vim\x07"),
        },
    );
    assert!(!pre_barrier.chrome_dirty);
    assert_eq!(
        panes[&ready_terminal].last_title, "vim",
        "damage suppression must not suppress engine-derived metadata refresh"
    );

    dispatch_engine_frame(
        &mut kernel,
        &mut effects,
        &mut panes,
        FrameKind::BootstrapChunk {
            terminal_id: pending_terminal.clone(),
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            chunk_seq: 0,
            payload: bytes::Bytes::from_static(b"pending"),
        },
    );
    dispatch_engine_frame(
        &mut kernel,
        &mut effects,
        &mut panes,
        ready_frame(&pending_terminal),
    );
    let released = dispatch_engine_frame(
        &mut kernel,
        &mut effects,
        &mut panes,
        FrameKind::AttachReady { attach_id: 8 },
    );
    assert!(released.layout_replaced);
    assert_eq!(panes[&ready_terminal].last_title, "vim");
}
#[allow(clippy::too_many_lines)]
#[test]
fn malformed_history_tombstones_only_history_and_replacement_publishes_atomically() {
    let terminal_id = tid(96);
    let replacement = phux_protocol::BootstrapId::new(2).expect("replacement");
    let mut kernel = phux_client_core::session::SessionKernel::new(
        phux_client_core::engine::ghostty::GhosttyAdapter::new(
            phux_protocol::BootstrapLimits::default(),
        ),
        phux_protocol::BootstrapProfile::SynthesizedVtRaw,
    );
    let mut effects = phux_client_core::session::EffectBuffer::new();
    let mut panes = HashMap::new();
    dispatch_engine_frame(
        &mut kernel,
        &mut effects,
        &mut panes,
        begin_frame(&terminal_id),
    );
    dispatch_engine_frame(
        &mut kernel,
        &mut effects,
        &mut panes,
        FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            chunk_seq: 0,
            payload: bytes::Bytes::from_static(b"\x1b]2;old\x07"),
        },
    );
    dispatch_engine_frame(
        &mut kernel,
        &mut effects,
        &mut panes,
        ready_frame(&terminal_id),
    );

    let rejected = dispatch_engine_frame(
        &mut kernel,
        &mut effects,
        &mut panes,
        FrameKind::HistoryPage {
            terminal_id: terminal_id.clone(),
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            rows: 1,
            page_seq: 1,
            cursor: bytes::Bytes::from_static(b"cursor"),
            next_cursor: None,
            payload: bytes::Bytes::from_static(b"malformed-history"),
        },
    );
    assert!(!rejected.resync_required);
    assert!(effects.as_slice().iter().any(|effect| matches!(
        effect,
        phux_client_core::session::KernelEffect::Status(
            phux_client_core::session::KernelStatus::HistoryUnavailable { .. }
        )
    )));
    assert_eq!(
        kernel
            .history_cache(&terminal_id)
            .expect("published history")
            .status()
            .state,
        phux_client_core::history::HistoryLoadState::Tombstoned
    );
    dispatch_engine_frame(
        &mut kernel,
        &mut effects,
        &mut panes,
        FrameKind::ResourceOutput {
            terminal_id: terminal_id.clone(),
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            seq: 1,
            bytes: bytes::Bytes::from_static(b"\x1b]2;old-live\x07"),
        },
    );
    assert_eq!(
        kernel
            .published_engine(&terminal_id)
            .unwrap()
            .terminal()
            .unwrap()
            .title()
            .unwrap(),
        "old-live",
        "history failure must not stop live terminal output"
    );
    let stale = dispatch_engine_frame(
        &mut kernel,
        &mut effects,
        &mut panes,
        FrameKind::HistoryPage {
            terminal_id: terminal_id.clone(),
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            rows: 1,
            page_seq: 1,
            cursor: bytes::Bytes::from_static(b"stale"),
            next_cursor: None,
            payload: bytes::Bytes::from_static(b"queued-stale-page"),
        },
    );
    assert!(!stale.resync_required);

    kernel
        .update(
            phux_client_core::session::KernelInput::AttachStarted {
                attach_id: 10,
                terminals: std::slice::from_ref(&terminal_id),
            },
            &mut effects,
        )
        .expect("replacement attach");
    dispatch_engine_frame(
        &mut kernel,
        &mut effects,
        &mut panes,
        FrameKind::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id: stream(),
            bootstrap_id: replacement,
            profile: phux_protocol::BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq: 0,
        },
    );
    dispatch_engine_frame(
        &mut kernel,
        &mut effects,
        &mut panes,
        FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id: stream(),
            bootstrap_id: replacement,
            chunk_seq: 0,
            payload: bytes::Bytes::from_static(b"\x1b]2;new\x07"),
        },
    );
    assert_eq!(
        kernel
            .published_engine(&terminal_id)
            .unwrap()
            .terminal()
            .unwrap()
            .title()
            .unwrap(),
        "old-live",
        "replacement remains staged until READY"
    );
    let replacement_ready = dispatch_engine_frame(
        &mut kernel,
        &mut effects,
        &mut panes,
        FrameKind::BootstrapReady {
            terminal_id: terminal_id.clone(),
            stream_id: stream(),
            bootstrap_id: replacement,
            history_cursor: None,
        },
    );
    assert!(!replacement_ready.layout_replaced);
    assert!(!replacement_ready.chrome_dirty);
    assert_eq!(panes[&terminal_id].last_title, "new");
    let released = dispatch_engine_frame(
        &mut kernel,
        &mut effects,
        &mut panes,
        FrameKind::AttachReady { attach_id: 10 },
    );
    assert!(released.layout_replaced);
    assert_eq!(panes[&terminal_id].last_title, "new");
}

#[allow(clippy::too_many_arguments)]
fn handle_server_frame<W: crate::attach::RenderSink>(
    out: &mut W,
    frame: FrameKind,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    workspace: &mut Workspace,
    focused_resource: &mut Option<ResourceId>,
    zoomed: &mut Option<ResourceId>,
    session_name: &mut String,
    // phux-k0cw: this client's own session, so a test can drive the
    // foreign-layout guard.
    focused_session: Option<SessionId>,
    status_bar: Option<&mut crate::render::chrome::status_bar::StatusBarPainter>,
    sidebar: Option<crate::attach::paint::SidebarReservation>,
    viewport_dims: (u16, u16),
    predict: &mut PredictionState,
    overlay: &Overlay,
    pending_layout_request: Option<u32>,
    pending_splits: &mut HashMap<u32, crate::attach::actions::PendingSplit>,
    pending_windows: &mut HashMap<u32, crate::attach::actions::PendingWindow>,
    expected_closes: &mut HashSet<ResourceId>,
    agent_meta: &mut AgentMetaIndex,
    overlay_active: bool,
    defer_paint: bool,
) -> Result<FrameOutcome, AttachError> {
    let mut kernel = phux_client_core::session::SessionKernel::new(
        phux_client_core::engine::ghostty::GhosttyAdapter::new(
            phux_protocol::BootstrapLimits::default(),
        ),
        phux_protocol::BootstrapProfile::SynthesizedVtRaw,
    );
    let mut effects = phux_client_core::session::EffectBuffer::new();
    handle_server_frame_with_kernel(
        &mut kernel,
        &mut effects,
        out,
        frame,
        panes,
        workspace,
        focused_resource,
        zoomed,
        session_name,
        &mut false,
        focused_session,
        status_bar,
        sidebar,
        viewport_dims,
        predict,
        overlay,
        pending_layout_request,
        pending_splits,
        pending_windows,
        expected_closes,
        agent_meta,
        overlay_active,
        defer_paint,
    )
}

fn split2(a: u32, b: u32, focus: u32) -> LayoutState {
    LayoutState {
        tree: Some(LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.5,
            left: Box::new(LayoutNode::Leaf(tid(a))),
            right: Box::new(LayoutNode::Leaf(tid(b))),
        }),
        focus: Some(tid(focus)),
    }
}

/// A single-window workspace wrapping `state`, for the reconcile tests.
fn ws1(state: LayoutState) -> Workspace {
    Workspace {
        windows: vec![crate::layout::WindowState::new("1".to_owned(), state)],
        active: 0,
    }
}

/// Leaves of a workspace's window at `idx`.
fn window_leaves(ws: &Workspace, idx: usize) -> Vec<ResourceId> {
    ws.windows[idx]
        .state
        .tree
        .as_ref()
        .map(crate::layout::leaves)
        .unwrap_or_default()
}

#[test]
fn reconcile_accepts_authorized_replacement_without_terminal_overlap() {
    let mut replacement = ws1(split2(1, 2, 1));
    let local = Workspace::single(tid(9));
    replacement.windows[0].id = local.windows[0].id;
    let out = super::reconcile_loaded_workspace(replacement, &local, Some(&tid(9)));
    assert_eq!(out.windows.len(), 1);
    assert_eq!(
        window_leaves(&out, 0),
        vec![tid(1), tid(2)],
        "session authority does not depend on currently admitted replicas"
    );
    assert_eq!(out.windows[0].state.focus, Some(tid(1)));
    assert_eq!(out.windows[0].id, local.windows[0].id);
}

#[test]
fn reconcile_keeps_a_layout_that_contains_the_session_pane() {
    // Legitimate re-attach: the session's focused pane IS a leaf, so the
    // multi-pane tree is preserved (not discarded).
    let own = ws1(split2(1, 2, 1));
    let local = Workspace::single(tid(1));
    let out = super::reconcile_loaded_workspace(own, &local, Some(&tid(1)));
    let leaves = window_leaves(&out, 0);
    assert!(
        leaves.contains(&tid(1)) && leaves.contains(&tid(2)),
        "the session's own layout must be kept: {leaves:?}"
    );
}

#[test]
fn reconcile_without_bootstrap_focus_keeps_the_tree() {
    // No ATTACHED focus to validate against ⇒ don't discard.
    let tree = ws1(split2(1, 2, 1));
    let out = super::reconcile_loaded_workspace(tree, &Workspace::default(), None);
    assert_eq!(
        window_leaves(&out, 0).len(),
        2,
        "no focus to validate ⇒ tree preserved"
    );
}

/// Regression: a multi-window workspace must NOT alias its non-active
/// windows onto the focused pane. The focused pane is a leaf of window 0
/// only; window 1 references a different terminal and must keep it (the
/// "open vim in one window, it shows in the other" bug, where the
/// per-window foreign-discard rewrote every non-active window to
/// `single(focus)`).
#[test]
fn reconcile_multi_window_does_not_alias_non_active_windows() {
    let ws = Workspace {
        windows: vec![
            crate::layout::WindowState::new("1".to_owned(), LayoutState::single(tid(1))),
            crate::layout::WindowState::new("2".to_owned(), LayoutState::single(tid(2))),
        ],
        active: 0,
    };
    // Focus is on window 0's pane (tid 1); window 1 (tid 2) is non-active.
    let local = ws.clone();
    let out = super::reconcile_loaded_workspace(ws, &local, Some(&tid(1)));
    assert_eq!(out.windows.len(), 2, "both windows survive");
    assert_eq!(window_leaves(&out, 0), vec![tid(1)]);
    assert_eq!(
        window_leaves(&out, 1),
        vec![tid(2)],
        "non-active window keeps its own terminal, not aliased onto the focus"
    );
}

/// Build a `panes` map with a warm [`PaneSlot`] per supplied id.
fn panes_for(ids: &[&ResourceId]) -> HashMap<ResourceId, PaneSlot> {
    let mut panes = HashMap::new();
    for id in ids {
        panes.insert((*id).clone(), PaneSlot::new().expect("pane slot"));
    }
    panes
}

struct EngineFixture {
    kernel: super::super::pane_state::AttachKernel,
    effects: phux_client_core::session::EffectBuffer,
}

fn published_fixture(
    entries: &[(&ResourceId, u16, u16, &[u8])],
) -> (EngineFixture, HashMap<ResourceId, PaneSlot>) {
    let (kernel, effects, panes) = super::super::pane_state::published_test_state(entries);
    (EngineFixture { kernel, effects }, panes)
}

/// Drive any frame through the full attached-state dispatcher.
fn try_drive_layout_frame(
    frame: FrameKind,
    pending_layout_request: Option<u32>,
    workspace: &mut Workspace,
    focused: &mut Option<ResourceId>,
    panes: &mut HashMap<ResourceId, PaneSlot>,
) -> Result<FrameOutcome, AttachError> {
    let mut out: Vec<u8> = Vec::new();
    let mut session_name = String::new();
    let mut zoomed: Option<ResourceId> = None;
    let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
    let overlay = Overlay;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    handle_server_frame(
        &mut out,
        frame,
        panes,
        workspace,
        focused,
        &mut zoomed,
        &mut session_name,
        // phux-k0cw: these fixtures are session 1, so a
        // `phux.tui.layout/v1/1` broadcast is OUR layout and is adopted.
        // A key naming any other session is a peer's and must not be.
        Some(SessionId::new(1)),
        None,
        None,
        (80, 24),
        &mut predict,
        &overlay,
        pending_layout_request,
        &mut pending_splits,
        &mut pending_windows,
        &mut HashSet::new(),
        &mut AgentMetaIndex::default(),
        false,
        false,
    )
}

fn drive_layout_frame(
    frame: FrameKind,
    pending_layout_request: Option<u32>,
    workspace: &mut Workspace,
    focused: &mut Option<ResourceId>,
    panes: &mut HashMap<ResourceId, PaneSlot>,
) -> FrameOutcome {
    try_drive_layout_frame(frame, pending_layout_request, workspace, focused, panes)
        .expect("handle layout frame")
}

#[test]
fn duplicate_hello_ok_is_fatal_in_attached_phase() {
    let pane = tid(1);
    let mut workspace = Workspace::single(pane.clone());
    let mut focused = Some(pane.clone());
    let mut panes = panes_for(&[&pane]);
    let error = try_drive_layout_frame(
        FrameKind::HelloOk {
            protocol_major: phux_protocol::PROTOCOL_VERSION.major,
            protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
            protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
            server_caps: phux_protocol::caps::ServerCapabilities::new(),
            server_id: Vec::new(),
            selected_profile: phux_protocol::caps::BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: phux_protocol::caps::BootstrapLimits::default(),
        },
        None,
        &mut workspace,
        &mut focused,
        &mut panes,
    )
    .expect_err("post-negotiation HELLO_OK must terminate the client");
    assert!(matches!(
        error,
        AttachError::Protocol(message) if message.contains("not valid from a server")
    ));
}

/// A `DIRECTORY_LISTING` reaching the attached dispatcher is handed up to
/// the driver (which matches it against its pending `go-to-directory`
/// request), never rejected by the catch-all: that would tear down a healthy
/// attach over a picker reply.
#[test]
fn directory_listing_reply_is_handed_to_the_driver() {
    let pane = tid(1);
    let mut workspace = Workspace::single(pane.clone());
    let mut focused = Some(pane.clone());
    let mut panes = panes_for(&[&pane]);
    let result = Ok(phux_protocol::wire::frame::DirectoryListing {
        path: "/".to_owned(),
        parent: None,
        entries: Vec::new(),
        truncated: false,
    });
    let outcome = drive_layout_frame(
        FrameKind::DirectoryListing {
            request_id: 9,
            result: result.clone(),
        },
        None,
        &mut workspace,
        &mut focused,
        &mut panes,
    );
    assert_eq!(outcome.directory_listing, Some((9, result)));
    assert!(!outcome.exit);
}

/// A request-correlated reply that reaches the attached dispatcher is inert,
/// never fatal.
///
/// # The regression this fences
///
/// `Connection::await_answer` loops until the reply carrying ITS `request_id`
/// arrives and pushes every other frame onto `interleaved`, which is later
/// replayed through this dispatcher. A reply whose awaiter has already been
/// answered — or has gone away — therefore arrives here as ordinary
/// interleaved traffic.
///
/// `COMMAND_RESULT` and `RESOURCE_MOVED` had no arm, so they fell to the
/// catch-all and killed a healthy attach with
/// `protocol error: frame is not valid from a server in the attached phase:
/// CommandResult { request_id: 4, result: Ok }`. `spatial_e2e` caught it:
/// pressing `C-a o` while the layout was being driven concurrently tore the
/// client down over a SUCCESS reply it simply had nowhere to put.
///
/// This is the same rule the `ERROR` arm already stated for the failure twin
/// of these frames (phux-ijuj) and the same "no matching pending request"
/// drop `MetadataValue` already made — the two success replies were the gap.
///
/// `docs/spec/L1.md` §5 already required this: "A `COMMAND` is asynchronous:
/// the server MAY emit other messages ... before `COMMAND_RESULT`. Clients
/// MUST tolerate that ordering." So this fences conformance, not a new
/// policy, and no wire change is involved.
///
/// The assertions are deliberately paired with a still-fatal frame, so a
/// future change that makes the dispatcher total over everything (and thus
/// blind to real protocol violations) fails here rather than silently
/// widening the contract.
#[test]
fn raced_request_correlated_replies_are_inert_not_fatal() {
    let pane = tid(1);

    for frame in [
        FrameKind::CommandResult {
            request_id: 4,
            result: phux_protocol::wire::frame::CommandResult::Ok,
        },
        FrameKind::ResourceMoved {
            request_id: 7,
            result: phux_protocol::wire::frame::MoveResult::Ok(pane.clone()),
        },
    ] {
        let mut workspace = Workspace::single(pane.clone());
        let mut focused = Some(pane.clone());
        let mut panes = panes_for(&[&pane]);
        let before = workspace.clone();

        let outcome = try_drive_layout_frame(
            frame.clone(),
            None,
            &mut workspace,
            &mut focused,
            &mut panes,
        )
        .unwrap_or_else(|err| panic!("{frame:?} must not terminate the attach: {err:?}"));

        // `FrameOutcome` is not `PartialEq` (it is a production type and
        // nothing else needs the impl), so inertness is asserted on the
        // fields that would carry an effect: no teardown, no layout or pane
        // churn, no follow-up emission.
        assert!(!outcome.exit, "{frame:?} must not end the attach");
        assert!(outcome.exit_reason.is_none(), "{frame:?}");
        assert!(!outcome.layout_replaced, "{frame:?}");
        assert!(!outcome.reflow_panes, "{frame:?}");
        assert!(!outcome.emit_set_metadata, "{frame:?}");
        assert!(!outcome.foreign_pane_set_dirty, "{frame:?}");
        assert!(outcome.attach_panes.is_empty(), "{frame:?}");
        assert!(outcome.foreign_layout.is_none(), "{frame:?}");
        assert_eq!(
            workspace, before,
            "{frame:?} must not mutate the client's topology"
        );
        assert_eq!(focused, Some(pane.clone()), "{frame:?} must not move focus");
    }
}

/// ADR-0049: a sibling's layout broadcast contributes topology only. Its
/// serialized active window and per-window focuses cannot yank this client.
#[test]
fn metadata_changed_preserves_valid_local_window_and_pane_focus() {
    use phux_protocol::wire::frame::Scope;

    let mut local = Workspace {
        windows: vec![
            crate::layout::WindowState::new("local-one".to_owned(), split2(1, 2, 2)),
            crate::layout::WindowState::new("local-two".to_owned(), split2(3, 4, 4)),
        ],
        active: 1,
    };
    let mut sibling = local.clone();
    sibling.active = 0;
    sibling.windows[0].name = "shared-one".to_owned();
    sibling.windows[1].name = "shared-two".to_owned();
    sibling.windows[0].state.focus = Some(tid(1));
    sibling.windows[1].state.focus = Some(tid(3));
    if let Some(LayoutNode::Split { ratio, .. }) = sibling.windows[1].state.tree.as_mut() {
        *ratio = 0.7;
    }
    let bytes = sibling.encode_cbor().expect("encode sibling workspace");
    let mut focused = Some(tid(4));
    let mut panes = panes_for(&[&tid(1), &tid(2), &tid(3), &tid(4)]);

    let outcome = drive_layout_frame(
        FrameKind::MetadataChanged {
            scope: Scope::Group(super::DEFAULT_GROUP_ID),
            key: phux_client::layout_ops::layout_key(SessionId::new(1)),
            value: Some(bytes),
        },
        None,
        &mut local,
        &mut focused,
        &mut panes,
    );

    assert!(outcome.layout_replaced);
    assert_eq!(local.active, 1, "sender cannot change the local window");
    assert_eq!(local.windows[0].state.focus, Some(tid(2)));
    assert_eq!(local.windows[1].state.focus, Some(tid(4)));
    assert_eq!(focused, Some(tid(4)), "driver mirror stays client-local");
    assert_eq!(local.windows[0].name, "shared-one", "names are topology");
    assert!(matches!(
        local.windows[1].state.tree,
        Some(LayoutNode::Split { ratio, .. }) if (ratio - 0.7).abs() < f32::EPSILON
    ));
}

#[test]
fn old_layout_schema_refuses_attach_and_broadcast_without_resetting_metadata() {
    let bytes = b"\xa1\x67version\x02".to_vec();
    let mut workspace = Workspace::single(tid(1));
    let before = workspace.clone();
    let mut focused = Some(tid(1));
    let mut panes = panes_for(&[&tid(1)]);
    for frame in [
        FrameKind::MetadataValue {
            request_id: 42,
            value: Some(bytes.clone()),
        },
        FrameKind::MetadataChanged {
            scope: phux_protocol::wire::frame::Scope::Group(super::DEFAULT_GROUP_ID),
            key: phux_client::layout_ops::layout_key(SessionId::new(1)),
            value: Some(bytes),
        },
    ] {
        let error =
            try_drive_layout_frame(frame, Some(42), &mut workspace, &mut focused, &mut panes)
                .expect_err("unsupported metadata must not seed a replacement write");
        assert!(
            matches!(error, AttachError::Protocol(message) if message.contains("stored metadata was preserved"))
        );
        assert_eq!(workspace, before);
        assert_eq!(focused, Some(tid(1)));
    }
}

#[test]
fn shared_window_identity_preserves_focus_on_reorder_and_empty_is_authoritative() {
    let mut local = Workspace::single(tid(1));
    local.add_window("two".into(), tid(2));
    let active_id = local.windows[1].id;
    let mut incoming = local.clone();
    incoming.windows.swap(0, 1);
    incoming.active = 1;
    let mut focused = Some(tid(2));
    let mut panes = panes_for(&[&tid(1), &tid(2)]);
    for topology in [incoming, Workspace::new()] {
        let empty = topology.windows.is_empty();
        let outcome = drive_layout_frame(
            FrameKind::MetadataChanged {
                scope: phux_protocol::wire::frame::Scope::Group(super::DEFAULT_GROUP_ID),
                key: phux_client::layout_ops::layout_key(SessionId::new(1)),
                value: Some(topology.encode_topology_cbor().expect("topology")),
            },
            None,
            &mut local,
            &mut focused,
            &mut panes,
        );
        assert!(outcome.layout_replaced);
        if empty {
            assert!(local.windows.is_empty());
            assert_eq!(focused, None);
            assert_eq!(
                panes.len(),
                2,
                "presentation removal retains durable replicas"
            );
        } else {
            assert_eq!(local.active, 0);
            assert_eq!(local.windows[local.active].id, active_id);
            assert_eq!(focused, Some(tid(2)));
        }
    }
}

/// phux-k0cw, THE guard this stage exists for: once a client subscribes
/// to peers' layout keys, a peer's broadcast must not touch the local
/// pane tree. Before the guard, the layout arm matched the key FAMILY and
/// adopted whatever it decoded — safe only while a client watched exactly
/// one key.
#[test]
fn a_peer_layout_broadcast_leaves_the_local_workspace_untouched() {
    use phux_protocol::wire::frame::Scope;

    let mut local = ws1(split2(1, 2, 1));
    let before = local.clone();
    let peer = ws1(split2(5, 6, 1));
    let bytes = peer.encode_cbor().expect("encode peer workspace");
    let mut focused = Some(tid(1));
    let focused_before = focused.clone();
    let mut panes = panes_for(&[&tid(1), &tid(2)]);

    let outcome = drive_layout_frame(
        FrameKind::MetadataChanged {
            scope: Scope::Group(super::DEFAULT_GROUP_ID),
            // Session 2; the fixture client is session 1.
            key: phux_client::layout_ops::layout_key(SessionId::new(2)),
            value: Some(bytes.clone()),
        },
        None,
        &mut local,
        &mut focused,
        &mut panes,
    );

    assert!(
        !outcome.layout_replaced,
        "a peer's layout must not replace ours"
    );
    assert_eq!(local, before, "the local workspace is byte-identical");
    assert_eq!(focused, focused_before, "local focus is untouched");
    assert!(
        outcome.attach_panes.is_empty(),
        "we must not attach a peer's panes"
    );
    assert_eq!(
        outcome.foreign_layout,
        Some((SessionId::new(2), Some(bytes))),
        "the payload is routed out for the roster instead"
    );

    // A peer tombstone routes out too, rather than resetting our layout
    // to the single-pane bootstrap.
    let outcome = drive_layout_frame(
        FrameKind::MetadataChanged {
            scope: Scope::Group(super::DEFAULT_GROUP_ID),
            key: phux_client::layout_ops::layout_key(SessionId::new(2)),
            value: None,
        },
        None,
        &mut local,
        &mut focused,
        &mut panes,
    );
    assert!(!outcome.layout_replaced);
    assert_eq!(local, before, "a peer tombstone is not our reset");
    assert_eq!(outcome.foreign_layout, Some((SessionId::new(2), None)));
}

#[test]
fn an_unscoped_layout_key_has_no_session_authority() {
    use phux_protocol::wire::frame::Scope;

    let mut local = Workspace::single(tid(1));
    let incoming = ws1(split2(1, 2, 1));
    let bytes = incoming.encode_cbor().expect("encode workspace");
    let mut focused = Some(tid(1));
    let mut panes = panes_for(&[&tid(1), &tid(2)]);

    let outcome = drive_layout_frame(
        FrameKind::MetadataChanged {
            scope: Scope::Group(super::DEFAULT_GROUP_ID),
            key: phux_client::layout_ops::LAYOUT_KEY.to_owned(),
            value: Some(bytes),
        },
        None,
        &mut local,
        &mut focused,
        &mut panes,
    );

    assert!(!outcome.layout_replaced);
    assert!(outcome.foreign_layout.is_none());
    assert_eq!(window_leaves(&local, 0), vec![tid(1)]);
}

/// A `phux.agent/v1` push for a pane outside our workspace is a
/// peer's, even when ATTACHED allocated a mirror slot. It must not enter the local index, which
/// `sync_agent_meta_subscriptions` retains against the LOCAL pane set —
/// a foreign record folded in there would be evicted on the next sweep.
#[test]
fn a_foreign_agent_record_push_stays_out_of_the_local_index() {
    use phux_protocol::wire::frame::{RESOURCE_AGENT_KEY, Scope};

    let mut local = Workspace::single(tid(1));
    let mut focused = Some(tid(1));
    let mut panes = panes_for(&[&tid(1), &tid(77)]);
    let record = br#"{"name":"claude","kind":"claude","state":"blocked"}"#.to_vec();

    let outcome = drive_layout_frame(
        FrameKind::MetadataChanged {
            scope: Scope::Resource(tid(77)),
            key: RESOURCE_AGENT_KEY.to_owned(),
            value: Some(record),
        },
        None,
        &mut local,
        &mut focused,
        &mut panes,
    );

    assert!(
        !outcome.agent_meta_changed,
        "a peer's record is not a local index change"
    );
    assert_eq!(
        outcome.foreign_agent.as_ref().map(|(id, _)| id.clone()),
        Some(tid(77)),
        "the record routes out to the peer cache"
    );
}

#[test]
fn rejected_cross_session_layout_emits_no_attach_panes() {
    use phux_protocol::wire::frame::Scope;

    let mut local = Workspace::single(tid(9));
    let foreign = ws1(split2(1, 2, 1));
    let bytes = foreign.encode_cbor().expect("encode foreign workspace");
    let mut focused = Some(tid(9));
    let mut panes = panes_for(&[&tid(9)]);

    let outcome = drive_layout_frame(
        FrameKind::MetadataChanged {
            scope: Scope::Group(super::DEFAULT_GROUP_ID),
            key: phux_client::layout_ops::layout_key(SessionId::new(2)),
            value: Some(bytes),
        },
        None,
        &mut local,
        &mut focused,
        &mut panes,
    );

    assert!(outcome.attach_panes.is_empty());
    assert_eq!(window_leaves(&local, 0), vec![tid(9)]);
    assert_eq!(focused, Some(tid(9)));
}

#[test]
fn session_keyed_replacement_keeps_stable_window_with_all_new_leaves() {
    let mut local = ws1(split2(1, 2, 2));
    let stable_id = local.windows[0].id;
    let mut replacement = ws1(split2(5, 6, 6));
    replacement.windows[0].id = stable_id;
    let mut focused = Some(tid(2));
    let mut panes = panes_for(&[&tid(1), &tid(2)]);
    let outcome = drive_layout_frame(
        FrameKind::MetadataChanged {
            scope: phux_protocol::wire::frame::Scope::Group(super::DEFAULT_GROUP_ID),
            key: phux_client::layout_ops::layout_key(SessionId::new(1)),
            value: Some(replacement.encode_cbor().unwrap()),
        },
        None,
        &mut local,
        &mut focused,
        &mut panes,
    );
    assert_eq!(local.windows[0].id, stable_id);
    assert_eq!(window_leaves(&local, 0), vec![tid(5), tid(6)]);
    assert_eq!(focused, Some(tid(5)));
    assert_eq!(outcome.attach_panes, vec![tid(5), tid(6)]);
    assert!(!outcome.emit_set_metadata);
    assert_eq!(panes.len(), 2);
}

#[test]
fn metadata_changed_discovers_peer_added_leaf_without_moving_focus() {
    use phux_protocol::wire::frame::Scope;

    let mut local = ws1(split2(1, 2, 1));
    let mut sibling = local.clone();
    let tree = sibling.windows[0].state.tree.as_ref().unwrap();
    sibling.windows[0].state.tree = Some(
        crate::layout::split_at(tree, &tid(2), &tid(3), SplitDir::Vertical, 0.3)
            .expect("split peer tree"),
    );
    sibling.windows[0].state.focus = Some(tid(3));
    let bytes = sibling.encode_cbor().expect("encode sibling workspace");
    let mut focused = Some(tid(1));
    let mut panes = panes_for(&[&tid(1), &tid(2)]);

    let outcome = drive_layout_frame(
        FrameKind::MetadataChanged {
            scope: Scope::Group(super::DEFAULT_GROUP_ID),
            key: phux_client::layout_ops::layout_key(SessionId::new(1)),
            value: Some(bytes),
        },
        None,
        &mut local,
        &mut focused,
        &mut panes,
    );

    assert_eq!(outcome.attach_panes, vec![tid(3)]);
    assert_eq!(focused, Some(tid(1)));
    assert_eq!(local.windows[0].state.focus, Some(tid(1)));
    assert_eq!(window_leaves(&local, 0), vec![tid(1), tid(2), tid(3)]);
}

/// The initial persisted-layout reply uses the same topology-only merge as
/// broadcasts: the attach bootstrap focus wins when it remains a leaf.
#[test]
fn metadata_value_preserves_valid_bootstrap_focus() {
    let mut local = Workspace::single(tid(2));
    let persisted = ws1(split2(1, 2, 1));
    let bytes = persisted.encode_cbor().expect("encode persisted workspace");
    let mut focused = Some(tid(2));
    let mut panes = panes_for(&[&tid(1), &tid(2)]);

    let outcome = drive_layout_frame(
        FrameKind::MetadataValue {
            request_id: 41,
            value: Some(bytes),
        },
        Some(41),
        &mut local,
        &mut focused,
        &mut panes,
    );

    assert!(outcome.layout_replaced);
    assert_eq!(window_leaves(&local, 0), vec![tid(1), tid(2)]);
    assert_eq!(local.windows[0].state.focus, Some(tid(2)));
    assert_eq!(focused, Some(tid(2)));
}

/// When a topology update removes local focus/window state, reconciliation
/// repairs it deterministically rather than adopting the sender's focus.
#[test]
fn reconcile_repairs_missing_local_focus_and_invalid_active_index() {
    let mut local = Workspace::single(tid(1));
    local.add_window("2".to_owned(), tid(2));
    local.add_window("3".to_owned(), tid(9));
    let incoming = Workspace {
        windows: vec![
            crate::layout::WindowState::new("1".to_owned(), split2(1, 4, 4)),
            crate::layout::WindowState::new("2".to_owned(), split2(2, 3, 3)),
        ],
        active: 0,
    };
    let out = super::reconcile_loaded_workspace(incoming, &local, Some(&tid(9)));

    assert_eq!(out.active, 1, "removed local index clamps to last window");
    assert_eq!(out.windows[0].state.focus, Some(tid(1)));
    assert_eq!(out.windows[1].state.focus, Some(tid(2)));
}

/// Layout tombstones retain the existing reset behavior and anchor the
/// replacement single-pane workspace on this client's focused pane.
#[test]
fn layout_tombstone_resets_to_local_focused_pane() {
    use phux_protocol::wire::frame::Scope;

    let mut local = Workspace {
        windows: vec![
            crate::layout::WindowState::new("1".to_owned(), LayoutState::single(tid(1))),
            crate::layout::WindowState::new("2".to_owned(), LayoutState::single(tid(2))),
        ],
        active: 1,
    };
    let mut focused = Some(tid(2));
    let mut panes = panes_for(&[&tid(1), &tid(2)]);

    let outcome = drive_layout_frame(
        FrameKind::MetadataChanged {
            scope: Scope::Group(super::DEFAULT_GROUP_ID),
            key: phux_client::layout_ops::layout_key(SessionId::new(1)),
            value: None,
        },
        None,
        &mut local,
        &mut focused,
        &mut panes,
    );

    assert!(outcome.layout_replaced);
    assert_eq!(local, Workspace::single(tid(2)));
    assert_eq!(focused, Some(tid(2)));
}

/// A single-window workspace whose window is two leaves split
/// side-by-side (vertical divider), with `focus` on the supplied
/// leaf. Exercises the multi-pane render paths without a real tty.
fn two_pane_workspace(left: &ResourceId, right: &ResourceId, focus: &ResourceId) -> Workspace {
    let state = LayoutState {
        tree: Some(LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.5,
            left: Box::new(LayoutNode::Leaf(left.clone())),
            right: Box::new(LayoutNode::Leaf(right.clone())),
        }),
        focus: Some(focus.clone()),
    };
    Workspace {
        windows: vec![crate::layout::WindowState::new("1".to_owned(), state)],
        active: 0,
    }
}

fn drive_output(
    engine: &mut EngineFixture,
    out: &mut Vec<u8>,
    layout: &mut Workspace,
    focused: &mut Option<ResourceId>,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    terminal_id: &ResourceId,
    bytes: &[u8],
) {
    let seq = engine
        .kernel
        .published(terminal_id)
        .expect("published terminal")
        .last_seq()
        .checked_add(1)
        .expect("live sequence");
    let _ = drive_output_seq(engine, out, layout, focused, panes, terminal_id, bytes, seq);
}

/// Like [`drive_output_seq`] but with `defer_paint` set: the frame applies to
/// the mirror and reports its outcome without painting.
#[allow(clippy::too_many_arguments)]
fn drive_output_deferred(
    engine: &mut EngineFixture,
    out: &mut Vec<u8>,
    layout: &mut Workspace,
    focused: &mut Option<ResourceId>,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    terminal_id: &ResourceId,
    bytes: &[u8],
    seq: u64,
) -> FrameOutcome {
    let mut session_name = String::new();
    let mut zoomed: Option<ResourceId> = None;
    let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
    let overlay = Overlay;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    handle_server_frame_with_kernel(
        &mut engine.kernel,
        &mut engine.effects,
        out,
        FrameKind::ResourceOutput {
            terminal_id: terminal_id.clone(),
            stream_id: phux_protocol::StreamId::new(1).expect("stream"),
            bootstrap_id: phux_protocol::BootstrapId::new(1).expect("bootstrap"),
            seq,
            bytes: bytes::Bytes::copy_from_slice(bytes),
        },
        panes,
        layout,
        focused,
        &mut zoomed,
        &mut session_name,
        &mut false,
        None,
        None,
        None,
        (80, 24),
        &mut predict,
        &overlay,
        None,
        &mut pending_splits,
        &mut pending_windows,
        &mut HashSet::new(),
        &mut AgentMetaIndex::default(),
        false,
        // defer_paint
        true,
    )
    .expect("handle_server_frame")
}

/// Like [`drive_output`] but stamps an explicit `seq` and returns the
/// [`FrameOutcome`] so ack-emission tests can inspect `outcome.ack`.
#[allow(clippy::too_many_arguments)]
fn drive_output_seq(
    engine: &mut EngineFixture,
    out: &mut Vec<u8>,
    layout: &mut Workspace,
    focused: &mut Option<ResourceId>,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    terminal_id: &ResourceId,
    bytes: &[u8],
    seq: u64,
) -> FrameOutcome {
    drive_output_seq_with_viewport(
        engine,
        out,
        layout,
        focused,
        panes,
        terminal_id,
        bytes,
        seq,
        (80, 24),
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "test driver mirrors frame inputs"
)]
fn drive_output_seq_with_viewport(
    engine: &mut EngineFixture,
    out: &mut Vec<u8>,
    layout: &mut Workspace,
    focused: &mut Option<ResourceId>,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    terminal_id: &ResourceId,
    bytes: &[u8],
    seq: u64,
    viewport_dims: (u16, u16),
) -> FrameOutcome {
    let mut session_name = String::new();
    let mut zoomed: Option<ResourceId> = None;
    let mut predict = PredictionState::new(
        PredictiveConfig::disabled(),
        viewport_dims.0,
        viewport_dims.1,
    );
    let overlay = Overlay;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    handle_server_frame_with_kernel(
        &mut engine.kernel,
        &mut engine.effects,
        out,
        FrameKind::ResourceOutput {
            terminal_id: terminal_id.clone(),
            stream_id: phux_protocol::StreamId::new(1).expect("stream"),
            bootstrap_id: phux_protocol::BootstrapId::new(1).expect("bootstrap"),
            seq,
            bytes: bytes::Bytes::copy_from_slice(bytes),
        },
        panes,
        layout,
        focused,
        &mut zoomed,
        &mut session_name,
        &mut false,
        None,
        None,
        None,
        viewport_dims,
        &mut predict,
        &overlay,
        None,
        &mut pending_splits,
        &mut pending_windows,
        &mut HashSet::new(),
        &mut AgentMetaIndex::default(),
        false,
        false,
    )
    .expect("handle_server_frame")
}

/// phux-ih39: live output that races ahead of bootstrap publication must
/// not be interpreted against placeholder geometry. Absolute cursor
/// movement past column 80 is the compact regression oracle.
#[test]
fn output_before_snapshot_uses_current_viewport_width() {
    let pane = tid(1);
    let mut layout = Workspace::single(pane.clone());
    let mut focused = Some(pane.clone());
    let (mut engine, mut panes) = published_fixture(&[(&pane, 120, 30, b"")]);
    let mut out: Vec<u8> = Vec::new();

    drive_output_seq_with_viewport(
        &mut engine,
        &mut out,
        &mut layout,
        &mut focused,
        &mut panes,
        &pane,
        b"\x1b[1;100HX",
        1,
        (120, 30),
    );

    let terminal = super::super::pane_state::published_terminal(&engine.kernel, &pane)
        .expect("published terminal");
    assert_eq!(terminal.cols().expect("cols"), 120);
    assert_eq!(terminal.rows().expect("rows"), 30);
    let slot = panes.get_mut(&pane).expect("slot allocated");
    let cell = slot
        .renderer
        .read_grapheme_at(ReplicaWalk::for_test(terminal), 0, 99)
        .expect("read cell");
    assert_eq!(cell, Some('X'));
}

#[test]
fn synchronized_output_paints_only_after_end_across_frames() {
    let pane = tid(1);
    let mut layout = Workspace::single(pane.clone());
    let mut focused = Some(pane.clone());
    let (mut engine, mut panes) = published_fixture(&[(&pane, 80, 24, b"")]);
    let mut out = Vec::new();

    drive_output(
        &mut engine,
        &mut out,
        &mut layout,
        &mut focused,
        &mut panes,
        &pane,
        b"\x1b[?2026hhalf-drawn",
    );
    assert!(out.is_empty(), "begin/body must update only the mirror");
    assert!(panes[&pane].sync_output_since.is_some());

    drive_output(
        &mut engine,
        &mut out,
        &mut layout,
        &mut focused,
        &mut panes,
        &pane,
        b" frame\x1b[?2026l",
    );
    assert!(!out.is_empty(), "end must publish the completed frame");
    assert!(panes[&pane].sync_output_since.is_none());
    let printable = strip_csi(&String::from_utf8_lossy(&out));
    assert!(printable.contains("half-drawn frame"));
}

/// phux-l96p.3: an incremental output paint is ONE synchronized-output block.
///
/// Before this, only the destructive full-frame path wrapped itself in DEC
/// 2026; the incremental path — the one that runs on every `RESOURCE_OUTPUT` —
/// emitted pane cells, then chrome, then the cursor, each visible to the outer
/// terminal as it landed. The invariants asserted here are the frame contract:
/// exactly one open and one close, the open first, the close last, and the
/// cursor hidden for the duration of the paint.
///
/// It is also the seam against phux-l96p.2: the pane renderer opens its OWN
/// `render::SyncOutput` around a dirty repaint, and the frame block holds the
/// nesting depth for its whole life so that guard nests instead of closing
/// the frame's block half way through. A regression there shows up here as
/// two opens and two closes — with the status bar and the cursor stranded
/// outside the transaction, which is precisely the tearing being removed.
#[test]
fn an_incremental_output_paint_is_one_synchronized_block() {
    let pane = tid(1);
    let mut layout = Workspace::single(pane.clone());
    let mut focused = Some(pane.clone());
    let (mut engine, mut panes) = published_fixture(&[(&pane, 80, 24, b"")]);
    let mut out: Vec<u8> = Vec::new();

    drive_output(
        &mut engine,
        &mut out,
        &mut layout,
        &mut focused,
        &mut panes,
        &pane,
        b"hello frame",
    );

    let s = String::from_utf8_lossy(&out);
    assert_eq!(
        s.matches("\x1b[?2026h").count(),
        1,
        "exactly one block opens per frame: {s:?}"
    );
    assert_eq!(
        s.matches("\x1b[?2026l").count(),
        1,
        "exactly one block closes per frame: {s:?}"
    );
    assert!(
        s.starts_with("\x1b[?2026h\x1b[?25l"),
        "the block opens before any cell, cursor hidden: {s:?}"
    );
    assert!(
        s.ends_with("\x1b[?2026l"),
        "the block closes after the cursor placement: {s:?}"
    );
    // The cursor placement is the LAST thing in the frame, so proving it sits
    // before the single close proves the whole composite is inside the block.
    let close = s.rfind("\x1b[?2026l").expect("block closes");
    let cursor = s
        .rfind("\x1b[?25h")
        .expect("frame ends by showing the cursor");
    assert!(
        cursor < close,
        "the cursor is placed inside the block, not after it: {s:?}"
    );
}

/// phux-l96p.3: a suppressed frame paints nothing at all.
///
/// The coalescing mask, a modal overlay, an open synchronized-output
/// transaction and the driver's frame pacer all route through the same
/// `defer_paint` gate. A deferred frame must still apply its bytes to the
/// mirror (that is the whole point of deferring the PAINT rather than the
/// frame), but must not open a block, tile a layout, or touch the chrome.
#[test]
fn a_deferred_output_frame_emits_nothing_but_still_applies() {
    let pane = tid(1);
    let mut layout = Workspace::single(pane.clone());
    let mut focused = Some(pane.clone());
    let (mut engine, mut panes) = published_fixture(&[(&pane, 80, 24, b"")]);
    let mut out: Vec<u8> = Vec::new();
    let seq = engine
        .kernel
        .published(&pane)
        .expect("published terminal")
        .last_seq()
        .checked_add(1)
        .expect("live sequence");

    let outcome = drive_output_deferred(
        &mut engine,
        &mut out,
        &mut layout,
        &mut focused,
        &mut panes,
        &pane,
        b"deferred glyphs",
        seq,
    );

    assert!(out.is_empty(), "a deferred frame writes no bytes: {out:?}");
    assert!(!outcome.chrome_dirty, "no title moved");
    let terminal = super::super::pane_state::published_terminal(&engine.kernel, &pane)
        .expect("published terminal");
    let slot = panes.get_mut(&pane).expect("slot");
    let cell = slot
        .renderer
        .read_grapheme_at(ReplicaWalk::for_test(terminal), 0, 0)
        .expect("read cell");
    assert_eq!(cell, Some('d'), "the mirror still ingested the bytes");
}

/// phux-l96p.3: a frame that changes nothing on screen costs nothing.
///
/// Re-applying identical bytes leaves the mirror's dirty tracking clean and
/// the bar's cache intact, so the frame block never opens — no `?2026h`, no
/// cursor placement, and (against the real sink) no writer-thread wake.
#[test]
fn a_frame_that_changes_nothing_writes_nothing() {
    let pane = tid(1);
    let mut layout = Workspace::single(pane.clone());
    let mut focused = Some(pane.clone());
    let (mut engine, mut panes) = published_fixture(&[(&pane, 80, 24, b"")]);
    let mut warm: Vec<u8> = Vec::new();
    drive_output(
        &mut engine,
        &mut warm,
        &mut layout,
        &mut focused,
        &mut panes,
        &pane,
        b"steady",
    );
    assert!(!warm.is_empty(), "the first paint emits the row");

    // A zero-byte payload changes no cell and moves no cursor.
    let mut quiet: Vec<u8> = Vec::new();
    drive_output(
        &mut engine,
        &mut quiet,
        &mut layout,
        &mut focused,
        &mut panes,
        &pane,
        b"",
    );
    assert!(
        quiet.is_empty(),
        "an output frame that changes nothing must emit nothing: {quiet:?}"
    );
}

/// phux-foz.9: an OSC 0/2 title riding in ordinary `RESOURCE_OUTPUT`
/// bytes is the only identity signal a plain `claude`/`codex` pane
/// emits — the frame must raise `chrome_dirty` when the title moves so
/// the driver refreshes the window labels and the sidebar's agents
/// section (the live repro: run `claude` in a pane, the agent row must
/// appear without waiting for an unrelated chrome event; after exit,
/// the shell's title reset must remove it the same way).
#[test]
fn output_title_change_marks_chrome_dirty() {
    let pane = tid(1);
    let mut layout = Workspace::single(pane.clone());
    let mut focused = Some(pane.clone());
    let (mut engine, mut panes) = published_fixture(&[(&pane, 80, 24, b"")]);
    let mut out: Vec<u8> = Vec::new();

    let plain = drive_output_seq(
        &mut engine,
        &mut out,
        &mut layout,
        &mut focused,
        &mut panes,
        &pane,
        b"just glyphs, no title",
        1,
    );
    assert!(
        !plain.chrome_dirty,
        "output that never touches the title must not repaint the chrome"
    );

    let set = drive_output_seq(
        &mut engine,
        &mut out,
        &mut layout,
        &mut focused,
        &mut panes,
        &pane,
        b"\x1b]2;\xe2\x9c\xb3 claude\x07",
        2,
    );
    assert!(
        set.chrome_dirty,
        "a new OSC 2 title must mark the chrome dirty"
    );

    let unchanged = drive_output_seq(
        &mut engine,
        &mut out,
        &mut layout,
        &mut focused,
        &mut panes,
        &pane,
        b"\x1b]2;\xe2\x9c\xb3 claude\x07more glyphs",
        3,
    );
    assert!(
        !unchanged.chrome_dirty,
        "re-asserting the same title must not repaint the chrome"
    );

    let cleared = drive_output_seq(
        &mut engine,
        &mut out,
        &mut layout,
        &mut focused,
        &mut panes,
        &pane,
        b"\x1b]2;\x07",
        4,
    );
    assert!(
        cleared.chrome_dirty,
        "clearing the title (the agent exited; the shell reset it) must repaint the chrome"
    );
}

/// phux-foz.9: the symmetric bootstrap path — a resync
/// replays the pane's title too, so a previously unseen title raises
/// `chrome_dirty` exactly like the output hot path.
#[test]
fn snapshot_title_change_marks_chrome_dirty() {
    let pane = tid(1);
    let mut layout = Workspace::single(pane.clone());
    let mut focused = Some(pane.clone());
    let (mut engine, mut panes) = published_fixture(&[(&pane, 80, 24, b"")]);
    let mut out: Vec<u8> = Vec::new();

    let first = drive_snapshot(
        &mut engine,
        &mut out,
        &mut layout,
        &mut focused,
        &mut panes,
        &pane,
        80,
        24,
        b"\x1b]2;codex\x07resynced",
        (80, 24),
    );
    assert!(
        first.chrome_dirty,
        "a snapshot carrying a new title must mark the chrome dirty"
    );

    let repeat = drive_snapshot(
        &mut engine,
        &mut out,
        &mut layout,
        &mut focused,
        &mut panes,
        &pane,
        80,
        24,
        b"\x1b]2;codex\x07resynced again",
        (80, 24),
    );
    assert!(
        !repeat.chrome_dirty,
        "an unchanged title replay must not repaint the chrome"
    );
}

#[test]
fn snapshot_during_synchronized_output_waits_for_live_end() {
    let pane = tid(1);
    let mut layout = Workspace::single(pane.clone());
    let mut focused = Some(pane.clone());
    let (mut engine, mut panes) = published_fixture(&[(&pane, 80, 24, b"")]);
    let mut out = Vec::new();

    drive_output(
        &mut engine,
        &mut out,
        &mut layout,
        &mut focused,
        &mut panes,
        &pane,
        b"\x1b[?2026hpartial",
    );
    drive_snapshot(
        &mut engine,
        &mut out,
        &mut layout,
        &mut focused,
        &mut panes,
        &pane,
        80,
        24,
        b"\x1b[!p\x1b[2J\x1b[Hstable snapshot",
        (80, 24),
    );
    assert!(
        !out.is_empty(),
        "replacement publication must paint the new atomic replica"
    );
    assert!(
        panes[&pane].sync_output_since.is_none(),
        "synchronized-output state belongs to the retired replica"
    );
}

/// phux-ih39: the ATTACHED graph already carries per-pane dimensions.
/// Seed slots from that graph so pre-bootstrap output doesn't get
/// interpreted at 80x24.
#[test]
fn attached_seeds_pane_slots_from_snapshot_dimensions() {
    let pane = tid(1);
    let window = WindowId::new(1);
    let session = SessionId::new(1);
    let snapshot = SessionSnapshot::new(session, window, pane.clone())
        .with_resources(vec![ResourceInfo::new(pane.clone(), window, 132, 43)]);
    let mut panes = HashMap::new();
    let mut workspace = Workspace::default();
    let mut focused = None;
    let mut zoomed: Option<ResourceId> = None;
    let mut session_name = String::new();
    let mut predict = PredictionState::new(PredictiveConfig::disabled(), 132, 43);
    let overlay = Overlay;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    let mut out: Vec<u8> = Vec::new();

    handle_server_frame(
        &mut out,
        FrameKind::Attached {
            attach_id: 1,
            snapshot,
            initial_client_id: ClientId::new(1),
        },
        &mut panes,
        &mut workspace,
        &mut focused,
        &mut zoomed,
        &mut session_name,
        None,
        None,
        None,
        (132, 43),
        &mut predict,
        &overlay,
        None,
        &mut pending_splits,
        &mut pending_windows,
        &mut HashSet::new(),
        &mut AgentMetaIndex::default(),
        false,
        false,
    )
    .expect("attached");

    let slot = panes.get_mut(&pane).expect("slot seeded");
    assert_eq!(slot.terminal.cols().expect("cols"), 132);
    assert_eq!(slot.terminal.rows().expect("rows"), 43);
}

/// `SynthesizedVtRaw` live output is applied but does not request an
/// acknowledgement; cumulative frame ACKs belong to state-sync streams.
#[test]
fn synthesized_raw_output_does_not_yield_frame_ack() {
    let left = tid(1);
    let right = tid(2);
    let mut layout = two_pane_workspace(&left, &right, &left);
    let mut focused = Some(left.clone());
    let (mut engine, mut panes) = published_fixture(&[(&left, 80, 24, b""), (&right, 80, 24, b"")]);

    let mut out: Vec<u8> = Vec::new();
    let outcome = drive_output_seq(
        &mut engine,
        &mut out,
        &mut layout,
        &mut focused,
        &mut panes,
        &right,
        b"hi",
        1,
    );
    assert_eq!(
        outcome.ack, None,
        "raw synthesized output must not emit a state-sync acknowledgement"
    );
}

/// A zero sequence is not a live output sentinel in the session kernel:
/// the published generation starts at `base_seq == 0` and therefore
/// requires its first live payload to carry sequence 1.
#[test]
fn terminal_output_seq_zero_is_rejected() {
    let pane = tid(1);
    let (mut engine, _) = published_fixture(&[(&pane, 80, 24, b"")]);
    let route = route_engine_frame(
        &FrameKind::ResourceOutput {
            terminal_id: pane,
            stream_id: phux_protocol::StreamId::new(1).expect("stream"),
            bootstrap_id: phux_protocol::BootstrapId::new(1).expect("bootstrap"),
            seq: 0,
            bytes: bytes::Bytes::from_static(b"hi"),
        },
        &mut engine.kernel,
        &mut engine.effects,
    );
    assert!(
        route.failed.is_some(),
        "sequence zero must be rejected before rendering or acknowledgement"
    );
    assert_eq!(route.ack, None);
}

/// phux-2x9 via the injectable sink: a NON-focused pane must repaint
/// on its own `RESOURCE_OUTPUT` so it isn't visually frozen. We feed
/// output for the right (non-focused) pane and assert the captured VT
/// carries a CUP into the right pane's rect origin plus the emitted
/// graphemes — proving the regression without a live terminal.
/// phux-l96p.3: the settle paints EVERY withheld pane, in one frame.
///
/// The mechanism behind the debt fold in `handle_frame_burst`. The bug it
/// closes: `admit` pushes `next_allowed` to `now + interval` on an admitted
/// burst, so the post-burst deadline guard can never fire on that pass, and
/// the `paint_deadline` select arm sits third in a `biased` select behind
/// `conn.recv()` — which a saturating producer keeps permanently ready. A
/// pane that emitted one line during a refused window and then went quiet
/// (`yes` in pane A, one line in pane B) stayed unpainted for as long as A
/// kept talking. The fix folds the debt into the admitted burst's frame, so
/// what matters is that a multi-pane settle really does paint them all, and
/// as a SINGLE synchronized block rather than one per pane.
#[test]
fn a_settle_paints_every_withheld_pane_in_one_frame() {
    let left = tid(1);
    let right = tid(2);
    let layout = two_pane_workspace(&left, &right, &left);
    let mut focused = Some(left.clone());
    let (mut engine, mut panes) = published_fixture(&[(&left, 80, 24, b""), (&right, 80, 24, b"")]);

    // Both panes emit while their paint is withheld: the mirrors ingest, the
    // screen sees nothing.
    let mut withheld: Vec<u8> = Vec::new();
    for (pane, bytes) in [(&left, &b"LEFTSIDE"[..]), (&right, &b"RIGHTSIDE"[..])] {
        let seq = engine
            .kernel
            .published(pane)
            .expect("published terminal")
            .last_seq()
            .checked_add(1)
            .expect("live sequence");
        let _ = drive_output_deferred(
            &mut engine,
            &mut withheld,
            &mut layout.clone(),
            &mut focused,
            &mut panes,
            pane,
            bytes,
            seq,
        );
    }
    assert!(
        withheld.is_empty(),
        "withheld frames paint nothing: {withheld:?}"
    );

    // The settle: one composited frame covering both owed panes.
    let mut out: Vec<u8> = Vec::new();
    let _ = super::paint_output_frame(
        super::OutputFrame {
            out: &mut out,
            kernel: &engine.kernel,
            panes: &mut panes,
            workspace: &layout,
            zoomed: None,
            focused_resource: focused.as_ref(),
            status_bar: None,
            sidebar: None,
            viewport_dims: (80, 24),
            session_name: "",
            predict: &mut PredictionState::new(PredictiveConfig::disabled(), 80, 24),
            overlay: &Overlay,
        },
        &[left.clone(), right.clone()],
    );

    let s = String::from_utf8_lossy(&out);
    let visible = strip_csi(&s);
    assert!(
        visible.contains("LEFTSIDE"),
        "the focused pane's withheld output must land; visible = {visible:?}"
    );
    assert!(
        visible.contains("RIGHTSIDE"),
        "the QUIET pane's withheld output must land too — this is the pane \
         the bug stranded; visible = {visible:?}"
    );
    assert_eq!(
        s.matches("\x1b[?2026h").count(),
        1,
        "both panes settle inside ONE synchronized block: {s:?}"
    );
    assert_eq!(s.matches("\x1b[?2026l").count(), 1, "and one close: {s:?}");
}

#[test]
fn non_focused_pane_repaints_on_output() {
    let left = tid(1);
    let right = tid(2);
    let mut layout = two_pane_workspace(&left, &right, &left);
    let mut focused = Some(left.clone());
    let (mut engine, mut panes) = published_fixture(&[(&left, 80, 24, b""), (&right, 80, 24, b"")]);

    let mut out: Vec<u8> = Vec::new();
    drive_output(
        &mut engine,
        &mut out,
        &mut layout,
        &mut focused,
        &mut panes,
        &right,
        b"hello",
    );

    let s = String::from_utf8_lossy(&out);
    // The right pane occupies the columns after the divider in an
    // 80-col / 0.5 split: left pane cols 0..39, divider at col 40,
    // right pane from col 41 (0-based) ⇒ 1-based CUP `;42H`.
    assert!(
        s.contains(";42H"),
        "expected CUP into right pane origin (col 42); out = {s:?}"
    );
    // The renderer emits one cell at a time with an SGR delta between
    // cells, so the graphemes are interleaved with escape sequences.
    // Strip CSI sequences before the glyph check, otherwise `h`/`l`
    // would be satisfied by the cursor mode-set bytes (`\x1b[?25h` /
    // `\x1b[?25l`) rather than the pane content itself.
    let visible = strip_csi(&s);
    assert!(
        visible.contains("hello"),
        "non-focused pane should render its glyphs; visible = {visible:?}, raw = {s:?}"
    );
}

#[allow(
    clippy::too_many_arguments,
    reason = "test driver mirrors frame inputs"
)]
fn drive_snapshot(
    engine: &mut EngineFixture,
    out: &mut Vec<u8>,
    layout: &mut Workspace,
    focused: &mut Option<ResourceId>,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    terminal_id: &ResourceId,
    cols: u16,
    rows: u16,
    vt_replay_bytes: &[u8],
    viewport_dims: (u16, u16),
) -> FrameOutcome {
    let published = engine
        .kernel
        .published(terminal_id)
        .expect("published test generation");
    let stream_id = published.key().stream_id;
    let bootstrap_id = phux_protocol::BootstrapId::new(
        published
            .key()
            .bootstrap_id
            .get()
            .checked_add(1)
            .expect("bootstrap id"),
    )
    .expect("next bootstrap");
    let base_seq = published.last_seq();
    let mut session_name = String::new();
    let mut zoomed = None;
    let mut predict = PredictionState::new(
        PredictiveConfig::disabled(),
        viewport_dims.0,
        viewport_dims.1,
    );
    let overlay = Overlay;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    let mut expected_closes = HashSet::new();
    let mut agent_meta = AgentMetaIndex::default();
    let outcome = {
        let mut dispatch = |frame| {
            handle_server_frame_with_kernel(
                &mut engine.kernel,
                &mut engine.effects,
                out,
                frame,
                panes,
                layout,
                focused,
                &mut zoomed,
                &mut session_name,
                &mut false,
                None,
                None,
                None,
                viewport_dims,
                &mut predict,
                &overlay,
                None,
                &mut pending_splits,
                &mut pending_windows,
                &mut expected_closes,
                &mut agent_meta,
                false,
                false,
            )
            .expect("handle bootstrap frame")
        };
        dispatch(FrameKind::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            profile: phux_protocol::BootstrapStreamProfile::SynthesizedVtRaw,
            cols,
            rows,
            base_seq,
        });
        dispatch(FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: bytes::Bytes::copy_from_slice(vt_replay_bytes),
        });
        dispatch(FrameKind::BootstrapReady {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        })
    };
    if outcome.layout_replaced
        && let Some(active) = layout.render_window(zoomed.as_ref())
    {
        super::super::paint::paint_full_frame(
            out,
            active.as_ref(),
            panes,
            &engine.kernel,
            focused.as_ref(),
            viewport_dims,
            None,
            None,
            None,
            &session_name,
            &crate::render::theme::Theme::default(),
        );
    }
    outcome
}

/// phux-paer: on re-attach the server sends a bootstrap per pane; a
/// NON-focused pane's publication must paint into its rect, or the pane
/// renders blank while input still routes — the "screens wiped but still
/// typable" report. The symmetric counterpart to
/// [`non_focused_pane_repaints_on_output`].
#[test]
fn non_focused_pane_repaints_on_snapshot() {
    let left = tid(1);
    let right = tid(2);
    let mut layout = two_pane_workspace(&left, &right, &left);
    let mut focused = Some(left.clone());
    let (mut engine, mut panes) = published_fixture(&[(&left, 39, 24, b""), (&right, 39, 24, b"")]);

    let mut out: Vec<u8> = Vec::new();
    drive_snapshot(
        &mut engine,
        &mut out,
        &mut layout,
        &mut focused,
        &mut panes,
        &right,
        39,
        24,
        b"hello",
        (80, 24),
    );

    let s = String::from_utf8_lossy(&out);
    // Same geometry as the output test: 80-col / 0.5 split ⇒ right pane
    // origin at 0-based col 41 ⇒ 1-based CUP `;42H`.
    assert!(
        s.contains(";42H"),
        "expected CUP into right pane origin (col 42); out = {s:?}"
    );
    let visible = strip_csi(&s);
    assert!(
        visible.contains("hello"),
        "non-focused pane snapshot should render its glyphs; visible = {visible:?}, raw = {s:?}"
    );
}

/// The focused pane's snapshot still renders into its own rect — guards
/// against the phux-paer non-focused branch regressing the focused path.
#[test]
fn focused_pane_repaints_on_snapshot() {
    let left = tid(1);
    let right = tid(2);
    let mut layout = two_pane_workspace(&left, &right, &left);
    let mut focused = Some(left.clone());
    let (mut engine, mut panes) = published_fixture(&[(&left, 39, 24, b""), (&right, 39, 24, b"")]);

    let mut out: Vec<u8> = Vec::new();
    drive_snapshot(
        &mut engine,
        &mut out,
        &mut layout,
        &mut focused,
        &mut panes,
        &left,
        39,
        24,
        b"world",
        (80, 24),
    );

    let s = String::from_utf8_lossy(&out);
    assert!(
        s.contains("\x1b[1;1H"),
        "expected CUP into left pane origin (col 1); out = {s:?}"
    );
    let visible = strip_csi(&s);
    assert!(
        visible.contains("world"),
        "focused pane snapshot should render its glyphs; visible = {visible:?}, raw = {s:?}"
    );
}

/// The focused pane's output renders into its own rect (column 1 for
/// the left pane) and the captured stream is non-empty.
#[test]
fn focused_pane_repaints_on_output() {
    let left = tid(1);
    let right = tid(2);
    let mut layout = two_pane_workspace(&left, &right, &left);
    let mut focused = Some(left.clone());
    let (mut engine, mut panes) = published_fixture(&[(&left, 80, 24, b""), (&right, 80, 24, b"")]);

    let mut out: Vec<u8> = Vec::new();
    drive_output(
        &mut engine,
        &mut out,
        &mut layout,
        &mut focused,
        &mut panes,
        &left,
        b"world",
    );

    let s = String::from_utf8_lossy(&out);
    // Focused pane renders at its origin — column 1, row 2 (row 1 is the
    // pane-grid rail, phux-l96p.8). Glyphs are interleaved with SGR
    // resets, so assert on ordered chars.
    assert!(
        s.contains("\x1b[2;1H"),
        "expected CUP into left pane origin (row 2, col 1); out = {s:?}"
    );
    for ch in ['w', 'o', 'r', 'l', 'd'] {
        assert!(
            s.contains(ch),
            "focused pane glyph {ch:?} missing; out = {s:?}"
        );
    }
}

/// Off-screen invariant: a `RESOURCE_OUTPUT` for a pane that lives in
/// a NON-active window must warm that pane's libghostty mirror but
/// paint nothing (it isn't on screen). The pane has no rect in the
/// active window's composition, so the renderer emits no CUP.
#[test]
fn output_for_inactive_window_pane_warms_mirror_but_does_not_paint() {
    let active_resource = tid(1);
    let other_pane = tid(2);
    // Two windows: active window holds pane 1; window 2 holds pane 2.
    let mut workspace = Workspace::single(active_resource.clone());
    workspace.add_window("2".to_owned(), other_pane.clone());
    // Re-select window 0 as active (add_window activated the new one).
    workspace.select(0);
    let mut focused = Some(active_resource.clone());
    let (mut engine, mut panes) =
        published_fixture(&[(&active_resource, 80, 24, b""), (&other_pane, 80, 24, b"")]);

    let mut out: Vec<u8> = Vec::new();
    drive_output(
        &mut engine,
        &mut out,
        &mut workspace,
        &mut focused,
        &mut panes,
        &other_pane,
        b"offscreen",
    );

    // Nothing painted: the off-screen pane has no rect in the active
    // window, so the renderer wrote no bytes at all.
    assert!(
        out.is_empty(),
        "off-screen pane must not paint; out = {:?}",
        String::from_utf8_lossy(&out),
    );
    // The mirror is warm: reading the grapheme grid back shows the
    // bytes landed in pane 2's libghostty Terminal.
    let terminal = super::super::pane_state::published_terminal(&engine.kernel, &other_pane)
        .expect("pane 2 terminal");
    let slot = panes.get_mut(&other_pane).expect("pane 2 slot");
    let cell = slot
        .renderer
        .read_grapheme_at(ReplicaWalk::for_test(terminal), 0, 0)
        .expect("read cell");
    assert_eq!(cell, Some('o'), "pane 2 mirror should hold the output");
}

/// phux-4li.15: a `RESOURCE_SPAWNED` reply for a parked new-window
/// opens a new window seeded on the spawned pane, makes it active,
/// re-anchors focus, and asks for a broadcast + reflow.
#[test]
fn window_spawned_opens_active_window_focused_on_new_pane() {
    use super::handle_window_spawned;
    use crate::attach::actions::PendingWindow;
    use phux_protocol::wire::frame::SpawnResult;

    let mut workspace = Workspace::single(tid(1)); // window "1", pane 1
    let mut focused = Some(tid(1));
    let mut panes = panes_for(&[&tid(1)]);
    let mut out: Vec<u8> = Vec::new();

    let mut history = crate::attach::focus::FocusHistory::default();
    let before = focused.clone();
    let outcome = handle_window_spawned(
        &mut out,
        &mut workspace,
        &mut focused,
        &mut panes,
        &PendingWindow {
            name: "2".to_owned(),
            adopt: None,
        },
        SpawnResult::Ok(tid(2)),
    )
    .expect("handle_window_spawned");

    assert_eq!(workspace.windows.len(), 2);
    assert_eq!(workspace.active, 1, "new window is active");
    assert_eq!(workspace.windows[1].name, "2");
    history.observe(before, focused.as_ref());
    history.repair(focused.as_ref(), &workspace);
    assert_eq!(focused, Some(tid(2)), "focus follows the new pane");
    assert_eq!(
        history.target(focused.as_ref(), &workspace),
        Some(tid(1)),
        "async new-window completion records the pane being left",
    );
    assert!(panes.contains_key(&tid(2)), "new pane got a slot");
    assert!(outcome.layout_replaced && outcome.emit_set_metadata && outcome.reflow_panes);
    assert!(
        outcome.adopt_spawned.is_empty(),
        "a local spawn already streams to its spawner"
    );
}

/// A window spawned on a satellite through the hub (the directory picker's
/// "open new window here" on a satellite listing) does not open yet: the
/// relayed pane streams to no one until it is attached, and the attach can be
/// refused. The reply hands the driver the window to park on that attach.
#[test]
fn a_window_spawned_on_a_satellite_waits_for_its_attach() {
    use super::handle_window_spawned;
    use crate::attach::actions::PendingWindow;
    use phux_protocol::wire::frame::SpawnResult;

    let sat = ResourceId::satellite(phux_protocol::ids::SatelliteHost::new("edge"), 9);
    let mut workspace = Workspace::single(tid(1));
    let mut focused = Some(tid(1));
    let mut panes = panes_for(&[&tid(1)]);
    let mut out: Vec<u8> = Vec::new();

    let outcome = handle_window_spawned(
        &mut out,
        &mut workspace,
        &mut focused,
        &mut panes,
        &PendingWindow {
            name: "2".to_owned(),
            adopt: None,
        },
        SpawnResult::Ok(sat.clone()),
    )
    .expect("handle_window_spawned");

    assert_eq!(
        workspace.windows.len(),
        1,
        "nothing opens before the attach"
    );
    assert_eq!(focused, Some(tid(1)));
    assert!(!outcome.layout_replaced && !outcome.emit_set_metadata);
    let [crate::attach::actions::ParkedAdopt::Window(window)] = outcome.adopt_spawned.as_slice()
    else {
        panic!("expected one parked window: {:?}", outcome.adopt_spawned);
    };
    assert_eq!(window.name, "2");
    assert_eq!(
        window.adopt,
        Some(crate::attach::actions::Adopt::Spawned(
            crate::attach::actions::SpawnedPane::unbound(sat)
        )),
        "a window spawned here is this client's to clean up"
    );
}

/// The parked satellite window opens, focused and saved, once its attach
/// succeeds, and its pane is not killed.
#[test]
fn a_spawned_satellite_window_opens_when_its_attach_succeeds() {
    let sat = ResourceId::satellite(phux_protocol::ids::SatelliteHost::new("edge"), 9);
    let (outcome, workspace, focused, out, parked) = drive_adopt_reply(
        "2",
        crate::attach::actions::Adopt::Spawned(crate::attach::actions::SpawnedPane::unbound(
            sat.clone(),
        )),
        FrameKind::CommandResult {
            request_id: 9,
            result: phux_protocol::wire::frame::CommandResult::Ok,
        },
    );
    assert_eq!(workspace.windows.len(), 2);
    assert_eq!(workspace.windows[1].name, "2");
    assert_eq!(focused, Some(sat));
    assert!(outcome.layout_replaced && outcome.emit_set_metadata);
    assert!(!out.contains(&0x07), "success does not bell");
    assert!(
        outcome.kill_orphans.is_empty(),
        "a pane with a window is kept"
    );
    assert_eq!(parked, 0);
}

/// A refused attach of a spawned satellite window opens nothing, saves
/// nothing, bells, and names the host. The pane it spawned there is
/// referenced by nothing, so exactly that pane is killed (phux-c2td.20),
/// unless the refusal says the satellite is unreachable: then no kill could
/// reach it, and the hub would hold this client's input behind the attempt.
#[test]
fn a_spawned_satellite_window_refusal_bells_and_names_the_host() {
    use phux_protocol::wire::frame::{CommandResult, ErrorCode};

    let sat = ResourceId::satellite(phux_protocol::ids::SatelliteHost::new("edge"), 9);
    for (frame, killed) in [
        (
            FrameKind::CommandResult {
                request_id: 9,
                result: CommandResult::Error {
                    code: ErrorCode::SatelliteUnreachable,
                    message: "satellite edge is unreachable: link is down".to_owned(),
                },
            },
            Vec::new(),
        ),
        (
            FrameKind::CommandResult {
                request_id: 9,
                result: CommandResult::Error {
                    code: ErrorCode::TerminalNotFound,
                    message: "no such terminal".to_owned(),
                },
            },
            vec![sat.clone()],
        ),
        (
            FrameKind::Error {
                request_id: Some(9),
                code: ErrorCode::ResourceExhausted,
                message: "satellite edge link is saturated; retry".to_owned(),
            },
            vec![sat.clone()],
        ),
    ] {
        let (outcome, workspace, focused, out, parked) = drive_adopt_reply(
            "2",
            crate::attach::actions::Adopt::Spawned(crate::attach::actions::SpawnedPane::unbound(
                sat.clone(),
            )),
            frame,
        );
        assert_eq!(workspace.windows.len(), 1, "no blank window is left behind");
        assert_eq!(focused, Some(tid(1)));
        assert!(!outcome.emit_set_metadata && !outcome.layout_replaced);
        assert!(out.contains(&0x07), "a refusal bells");
        assert_eq!(outcome.notices.len(), 1);
        assert!(
            outcome.notices[0].text.contains("on satellite edge"),
            "the notice names the host: {}",
            outcome.notices[0].text
        );
        assert_eq!(outcome.kill_orphans, killed);
        assert_eq!(parked, 0);
    }
}

/// A spawned pane that some window already holds is never killed by a
/// refused attach: it is referenced, whatever the attach said.
#[test]
fn a_refused_attach_never_kills_a_pane_a_window_holds() {
    let sat = ResourceId::satellite(phux_protocol::ids::SatelliteHost::new("edge"), 9);
    let mut workspace = Workspace::single(tid(1));
    workspace.add_window("edge".to_owned(), sat.clone());
    let (outcome, _workspace, _focused, out, parked) = drive_adopt_reply_in(
        workspace,
        Vec::new(),
        "2",
        crate::attach::actions::Adopt::Spawned(crate::attach::actions::SpawnedPane::unbound(sat)),
        reachable_refusal(),
    );
    assert!(out.contains(&0x07), "the refusal still bells");
    assert!(outcome.kill_orphans.is_empty(), "a held pane is kept");
    assert_eq!(parked, 0);
}

/// A spawned pane that another open is waiting on (the session picker's
/// `Existing` adopt of that same pane) is never killed from under it.
#[test]
fn a_refused_attach_never_kills_a_pane_another_open_waits_on() {
    use crate::attach::actions::{Adopt, PendingWindow};

    let sat = ResourceId::satellite(phux_protocol::ids::SatelliteHost::new("edge"), 9);
    let picker_open = PendingWindow {
        name: "edge/build".to_owned(),
        adopt: Some(Adopt::Existing(sat.clone())),
    };
    let (outcome, _workspace, _focused, out, parked) = drive_adopt_reply_in(
        Workspace::single(tid(1)),
        vec![(10, picker_open)],
        "2",
        Adopt::Spawned(crate::attach::actions::SpawnedPane::unbound(sat)),
        reachable_refusal(),
    );
    assert!(out.contains(&0x07), "the refusal still bells");
    assert!(
        outcome.kill_orphans.is_empty(),
        "the picker's open is still waiting on it"
    );
    assert_eq!(parked, 1, "only the refused window is consumed");
}

/// A refusal of request 9 whose code says the satellite answered.
fn reachable_refusal() -> FrameKind {
    FrameKind::CommandResult {
        request_id: 9,
        result: phux_protocol::wire::frame::CommandResult::Error {
            code: phux_protocol::wire::frame::ErrorCode::TerminalNotFound,
            message: "no such terminal".to_owned(),
        },
    }
}

/// The instance token the phux-c2td.25 tests' satellite binds spawns to.
fn edge_token() -> phux_protocol::ids::ServerInstance {
    phux_protocol::ids::ServerInstance::new([3; 16])
}

/// phux-c2td.25: a bound spawn reply parks the window with the satellite's
/// instance token, so a pane it strands can later be killed conditionally.
#[test]
fn a_bound_satellite_window_spawn_keeps_its_instance_token() {
    use super::handle_window_spawned;
    use crate::attach::actions::{Adopt, ParkedAdopt, PendingWindow, SpawnedPane};
    use phux_protocol::wire::frame::SpawnResult;

    let sat = ResourceId::satellite(phux_protocol::ids::SatelliteHost::new("edge"), 9);
    let mut workspace = Workspace::single(tid(1));
    let mut focused = Some(tid(1));
    let mut panes = panes_for(&[&tid(1)]);
    let mut out: Vec<u8> = Vec::new();
    let outcome = handle_window_spawned(
        &mut out,
        &mut workspace,
        &mut focused,
        &mut panes,
        &PendingWindow {
            name: "2".to_owned(),
            adopt: None,
        },
        SpawnResult::OkBound {
            id: sat.clone(),
            instance: edge_token(),
        },
    )
    .expect("handle_window_spawned");
    let [ParkedAdopt::Window(window)] = outcome.adopt_spawned.as_slice() else {
        panic!("expected one parked window: {:?}", outcome.adopt_spawned);
    };
    assert_eq!(
        window.adopt,
        Some(Adopt::Spawned(SpawnedPane {
            id: sat,
            instance: Some(edge_token()),
        }))
    );
}

/// The same for a satellite split; a local pane answered bound (never asked
/// for, but tolerated) still splits in place.
#[test]
fn a_bound_satellite_split_spawn_keeps_its_instance_token() {
    use crate::attach::actions::{ParkedAdopt, SpawnedPane, SplitHost};
    use phux_protocol::wire::frame::SpawnResult;

    let edge = phux_protocol::ids::SatelliteHost::new("edge");
    let reply = drive_split_reply(
        parked_split(SplitHost::Satellite(edge), None),
        FrameKind::ResourceSpawned {
            request_id: 9,
            result: SpawnResult::OkBound {
                id: edge_pane(),
                instance: edge_token(),
            },
        },
    );
    let [ParkedAdopt::Split(split)] = reply.outcome.adopt_spawned.as_slice() else {
        panic!(
            "expected one parked split: {:?}",
            reply.outcome.adopt_spawned
        );
    };
    assert_eq!(
        split.adopt,
        Some(SpawnedPane {
            id: edge_pane(),
            instance: Some(edge_token()),
        })
    );

    let reply = drive_split_reply(
        parked_split(SplitHost::Attached, None),
        FrameKind::ResourceSpawned {
            request_id: 9,
            result: SpawnResult::OkBound {
                id: tid(2),
                instance: edge_token(),
            },
        },
    );
    assert_eq!(reply.leaves, vec![tid(1), tid(2)]);
}

/// phux-c2td.25: a refusal saying the satellite is unreachable strands the
/// pane for a later conditional retry only when its spawn was bound and
/// nothing here references it; it sends no immediate kill either way. A
/// refusal from a satellite that answered kills at once, as before, and
/// strands nothing.
#[test]
fn an_unreachable_refusal_strands_only_a_bound_unreferenced_pane() {
    use crate::attach::actions::{Adopt, SpawnedPane};
    use phux_client::conditional_kill::BoundResource;
    use phux_protocol::wire::frame::{CommandResult, ErrorCode};

    let sat = ResourceId::satellite(phux_protocol::ids::SatelliteHost::new("edge"), 9);
    let bound = SpawnedPane {
        id: sat.clone(),
        instance: Some(edge_token()),
    };
    let unreachable = FrameKind::CommandResult {
        request_id: 9,
        result: CommandResult::Error {
            code: ErrorCode::SatelliteUnreachable,
            message: "satellite edge is unreachable: link is down".to_owned(),
        },
    };

    let (outcome, ..) = drive_adopt_reply("2", Adopt::Spawned(bound.clone()), unreachable.clone());
    assert_eq!(
        outcome.unreachable_strays,
        vec![BoundResource {
            id: sat.clone(),
            instance: edge_token(),
        }]
    );
    assert!(
        outcome.kill_orphans.is_empty(),
        "no kill could reach it now"
    );

    let (outcome, ..) = drive_adopt_reply(
        "2",
        Adopt::Spawned(SpawnedPane::unbound(sat.clone())),
        unreachable.clone(),
    );
    assert!(outcome.unreachable_strays.is_empty(), "unbound: nothing");

    let mut workspace = Workspace::single(tid(1));
    workspace.add_window("edge".to_owned(), sat.clone());
    let (outcome, ..) = drive_adopt_reply_in(
        workspace,
        Vec::new(),
        "2",
        Adopt::Spawned(bound.clone()),
        unreachable,
    );
    assert!(outcome.unreachable_strays.is_empty(), "a held pane is kept");

    let (outcome, ..) = drive_adopt_reply("2", Adopt::Spawned(bound), reachable_refusal());
    assert_eq!(outcome.kill_orphans, vec![sat]);
    assert!(outcome.unreachable_strays.is_empty());
}

/// phux-c2td.3: drive one reply through the dispatcher with a parked
/// satellite-session window (request 9, adopting the existing `edge/@9`).
/// Returns the outcome, the workspace, focus, the bytes written, and how
/// many windows are still parked.
fn drive_satellite_adopt_reply(
    frame: FrameKind,
) -> (FrameOutcome, Workspace, Option<ResourceId>, Vec<u8>, usize) {
    let sat = ResourceId::satellite(phux_protocol::ids::SatelliteHost::new("edge"), 9);
    drive_adopt_reply(
        "edge/build",
        crate::attach::actions::Adopt::Existing(sat),
        frame,
    )
}

/// [`drive_satellite_adopt_reply`] with the parked window named `name`,
/// adopting `adopt`.
fn drive_adopt_reply(
    name: &str,
    adopt: crate::attach::actions::Adopt,
    frame: FrameKind,
) -> (FrameOutcome, Workspace, Option<ResourceId>, Vec<u8>, usize) {
    drive_adopt_reply_in(Workspace::single(tid(1)), Vec::new(), name, adopt, frame)
}

/// [`drive_adopt_reply`] against `workspace`, focused on pane 1, with the
/// windows in `in_flight` parked too.
fn drive_adopt_reply_in(
    mut workspace: Workspace,
    in_flight: Vec<(u32, crate::attach::actions::PendingWindow)>,
    name: &str,
    adopt: crate::attach::actions::Adopt,
    frame: FrameKind,
) -> (FrameOutcome, Workspace, Option<ResourceId>, Vec<u8>, usize) {
    use crate::attach::actions::PendingWindow;

    let mut focused = Some(tid(1));
    let mut panes = panes_for(&[&tid(1)]);
    let mut out: Vec<u8> = Vec::new();
    let mut zoomed = None;
    let mut session_name = String::new();
    let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
    let overlay = Overlay;
    let mut pending_splits = HashMap::new();
    let mut pending_windows: HashMap<u32, PendingWindow> = in_flight.into_iter().collect();
    pending_windows.insert(
        9,
        PendingWindow {
            name: name.to_owned(),
            adopt: Some(adopt),
        },
    );
    let mut expected_closes = HashSet::new();
    let mut agent_meta = AgentMetaIndex::default();
    let outcome = handle_server_frame(
        &mut out,
        frame,
        &mut panes,
        &mut workspace,
        &mut focused,
        &mut zoomed,
        &mut session_name,
        None,
        None,
        None,
        (80, 24),
        &mut predict,
        &overlay,
        None,
        &mut pending_splits,
        &mut pending_windows,
        &mut expected_closes,
        &mut agent_meta,
        false,
        false,
    )
    .expect("adopt reply");
    (outcome, workspace, focused, out, pending_windows.len())
}

/// A successful attach opens the satellite session's window, focused, and
/// asks for the layout broadcast.
#[test]
fn satellite_session_attach_success_opens_its_window() {
    let sat = ResourceId::satellite(phux_protocol::ids::SatelliteHost::new("edge"), 9);
    let (outcome, workspace, focused, out, parked) =
        drive_satellite_adopt_reply(FrameKind::CommandResult {
            request_id: 9,
            result: phux_protocol::wire::frame::CommandResult::Ok,
        });
    assert_eq!(workspace.windows.len(), 2);
    assert_eq!(workspace.windows[1].name, "edge/build");
    assert_eq!(workspace.active, 1);
    assert_eq!(focused, Some(sat));
    assert!(outcome.layout_replaced && outcome.emit_set_metadata && outcome.reflow_panes);
    assert!(outcome.notices.is_empty());
    assert!(!out.contains(&0x07), "success does not bell");
    assert!(outcome.kill_orphans.is_empty());
    assert_eq!(parked, 0, "the parked window is consumed");
}

/// A refused attach — as a `COMMAND_RESULT` error or a correlated `ERROR` —
/// opens no window, saves no layout, bells, and names the host and session.
#[test]
fn satellite_session_attach_refusal_leaves_no_window() {
    use phux_protocol::wire::frame::{CommandResult, ErrorCode};

    for frame in [
        FrameKind::CommandResult {
            request_id: 9,
            result: CommandResult::Error {
                code: ErrorCode::SatelliteUnreachable,
                message: "satellite edge is unreachable: link is down".to_owned(),
            },
        },
        FrameKind::Error {
            request_id: Some(9),
            code: ErrorCode::TerminalNotFound,
            message: "no such terminal".to_owned(),
        },
    ] {
        let (outcome, workspace, focused, out, parked) = drive_satellite_adopt_reply(frame);
        assert_eq!(workspace.windows.len(), 1, "no dead window is left behind");
        assert_eq!(focused, Some(tid(1)), "focus stays put");
        assert!(!outcome.emit_set_metadata, "the shared layout is not saved");
        assert!(!outcome.layout_replaced);
        assert!(out.contains(&0x07), "a refusal bells");
        assert_eq!(outcome.notices.len(), 1);
        assert!(
            outcome.notices[0].text.contains("edge/build"),
            "the notice names the host and session: {}",
            outcome.notices[0].text
        );
        assert!(
            outcome.kill_orphans.is_empty(),
            "a satellite session's own pane is never killed"
        );
        assert_eq!(parked, 0, "the parked window is consumed");
    }
}

/// Drive a `RESOURCE_SPAWNED { Ok }` reply through the full dispatcher
/// with one parked [`PendingSplit`], returning the resulting `zoomed`
/// state (phux-r82.7's zoom-on-spawn contract lives there).
fn drive_spawned_with_pending_split(zoom_on_spawn: bool) -> Option<ResourceId> {
    use crate::attach::actions::PendingSplit;
    use phux_protocol::wire::frame::SpawnResult;

    let anchor = tid(1);
    let mut workspace = Workspace::single(anchor.clone());
    let mut focused = Some(anchor.clone());
    let mut panes = panes_for(&[&anchor]);
    let mut out: Vec<u8> = Vec::new();
    let mut session_name = String::new();
    let mut zoomed: Option<ResourceId> = Some(anchor.clone());
    let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
    let overlay = Overlay;
    let mut pending_splits = HashMap::new();
    pending_splits.insert(
        7,
        PendingSplit {
            focused_at_request: anchor,
            dir: SplitDir::Horizontal,
            zoom_on_spawn,
            host: crate::attach::actions::SplitHost::Attached,
            adopt: None,
        },
    );
    let mut pending_windows = HashMap::new();
    let mut history = crate::attach::focus::FocusHistory::default();
    let before = focused.clone();
    let outcome = handle_server_frame(
        &mut out,
        FrameKind::ResourceSpawned {
            request_id: 7,
            result: SpawnResult::Ok(tid(2)),
        },
        &mut panes,
        &mut workspace,
        &mut focused,
        &mut zoomed,
        &mut session_name,
        None,
        None,
        None,
        (80, 24),
        &mut predict,
        &overlay,
        None,
        &mut pending_splits,
        &mut pending_windows,
        &mut HashSet::new(),
        &mut AgentMetaIndex::default(),
        false,
        false,
    )
    .expect("handle_server_frame");
    history.observe(before, focused.as_ref());
    history.repair(focused.as_ref(), &workspace);
    assert!(outcome.layout_replaced, "split reply replaces the layout");
    assert_eq!(focused, Some(tid(2)), "focus follows the spawned pane");
    assert_eq!(
        history.target(focused.as_ref(), &workspace),
        Some(tid(1)),
        "full async split reply records the anchor as MRU",
    );
    zoomed
}

/// phux-r82.7: a parked split with `zoom_on_spawn` zooms the freshly
/// spawned pane (placement = "zoomed" plugin panes).
#[test]
fn terminal_spawned_zoom_on_spawn_zooms_the_new_pane() {
    assert_eq!(drive_spawned_with_pending_split(true), Some(tid(2)));
}

/// phux-x2hm parity guard: a plain split still un-zooms.
#[test]
fn terminal_spawned_without_zoom_on_spawn_clears_zoom() {
    assert_eq!(drive_spawned_with_pending_split(false), None);
}

/// `edge/@9`, the satellite pane the split tests spawn.
fn edge_pane() -> ResourceId {
    ResourceId::satellite(phux_protocol::ids::SatelliteHost::new("edge"), 9)
}

/// A split of pane 1 parked with `host` and `adopt`.
fn parked_split(
    host: crate::attach::actions::SplitHost,
    adopt: Option<ResourceId>,
) -> crate::attach::actions::PendingSplit {
    crate::attach::actions::PendingSplit {
        focused_at_request: tid(1),
        dir: SplitDir::Horizontal,
        zoom_on_spawn: false,
        host,
        adopt: adopt.map(crate::attach::actions::SpawnedPane::unbound),
    }
}

/// What one reply did to a workspace with a split parked under request 9.
struct SplitReply {
    outcome: FrameOutcome,
    /// The active window's leaves after the reply.
    leaves: Vec<ResourceId>,
    focused: Option<ResourceId>,
    belled: bool,
    parked: usize,
    workspace: Workspace,
}

/// phux-c2td.18: drive one reply through the dispatcher with `split` parked
/// under request 9 against a workspace holding pane 1.
fn drive_split_reply(split: crate::attach::actions::PendingSplit, frame: FrameKind) -> SplitReply {
    drive_split_reply_in(Workspace::single(tid(1)), Some(tid(1)), split, frame)
}

/// [`drive_split_reply`] against `workspace`, focused on `focused`.
fn drive_split_reply_in(
    mut workspace: Workspace,
    mut focused: Option<ResourceId>,
    split: crate::attach::actions::PendingSplit,
    frame: FrameKind,
) -> SplitReply {
    let mut panes = panes_for(&[&tid(1)]);
    let mut out: Vec<u8> = Vec::new();
    let mut zoomed = None;
    let mut session_name = String::new();
    let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
    let overlay = Overlay;
    let mut pending_splits = HashMap::from([(9, split)]);
    let mut pending_windows = HashMap::new();
    let outcome = handle_server_frame(
        &mut out,
        frame,
        &mut panes,
        &mut workspace,
        &mut focused,
        &mut zoomed,
        &mut session_name,
        None,
        None,
        None,
        (80, 24),
        &mut predict,
        &overlay,
        None,
        &mut pending_splits,
        &mut pending_windows,
        &mut HashSet::new(),
        &mut AgentMetaIndex::default(),
        false,
        false,
    )
    .expect("split reply");
    let leaves = workspace
        .active_window()
        .and_then(|window| window.tree.as_ref())
        .map(crate::layout::leaves)
        .unwrap_or_default();
    SplitReply {
        outcome,
        leaves,
        focused,
        belled: out.contains(&0x07),
        parked: pending_splits.len(),
        workspace,
    }
}

/// phux-c2td.18: a split spawned on a satellite does not apply yet. The
/// relayed pane streams to no one until it is attached, and the attach can
/// be refused, so the reply hands the split back to be parked on it.
#[test]
fn a_split_spawned_on_a_satellite_waits_for_its_attach() {
    use crate::attach::actions::{ParkedAdopt, SplitHost};
    use phux_protocol::wire::frame::SpawnResult;

    let edge = phux_protocol::ids::SatelliteHost::new("edge");
    let reply = drive_split_reply(
        parked_split(SplitHost::Satellite(edge.clone()), None),
        FrameKind::ResourceSpawned {
            request_id: 9,
            result: SpawnResult::Ok(edge_pane()),
        },
    );
    assert_eq!(
        reply.leaves,
        vec![tid(1)],
        "nothing splits before the attach"
    );
    assert_eq!(reply.focused, Some(tid(1)));
    assert!(!reply.outcome.layout_replaced && !reply.outcome.emit_set_metadata);
    assert!(!reply.belled);
    let [ParkedAdopt::Split(split)] = reply.outcome.adopt_spawned.as_slice() else {
        panic!(
            "expected one parked split: {:?}",
            reply.outcome.adopt_spawned
        );
    };
    assert_eq!(
        split.adopt,
        Some(crate::attach::actions::SpawnedPane::unbound(edge_pane()))
    );
    assert_eq!(split.host, SplitHost::Satellite(edge));
    assert_eq!(split.focused_at_request, tid(1));
}

/// The parked satellite split applies, focused and saved, once its attach
/// succeeds.
#[test]
fn a_spawned_satellite_split_applies_when_its_attach_succeeds() {
    use crate::attach::actions::SplitHost;

    let edge = phux_protocol::ids::SatelliteHost::new("edge");
    let reply = drive_split_reply(
        parked_split(SplitHost::Satellite(edge), Some(edge_pane())),
        FrameKind::CommandResult {
            request_id: 9,
            result: phux_protocol::wire::frame::CommandResult::Ok,
        },
    );
    assert_eq!(reply.leaves, vec![tid(1), edge_pane()]);
    assert_eq!(reply.focused, Some(edge_pane()));
    assert!(reply.outcome.layout_replaced && reply.outcome.emit_set_metadata);
    assert!(reply.outcome.reflow_panes);
    assert!(reply.outcome.notices.is_empty());
    assert!(!reply.belled, "success does not bell");
    assert!(
        reply.outcome.kill_orphans.is_empty(),
        "an applied split keeps its pane"
    );
    assert_eq!(reply.parked, 0, "the parked split is consumed");
}

/// A refused attach of a spawned satellite split, as a `COMMAND_RESULT`
/// error or a correlated `ERROR`, leaves no dead split: nothing splits,
/// nothing is saved, the bell rings, and the notice names the host.
#[test]
fn a_spawned_satellite_split_refusal_bells_and_names_the_host() {
    use crate::attach::actions::SplitHost;
    use phux_protocol::wire::frame::{CommandResult, ErrorCode};

    let edge = phux_protocol::ids::SatelliteHost::new("edge");
    for (frame, killed) in [
        (
            FrameKind::CommandResult {
                request_id: 9,
                result: CommandResult::Error {
                    code: ErrorCode::SatelliteUnreachable,
                    message: "satellite edge is unreachable: link is down".to_owned(),
                },
            },
            Vec::new(),
        ),
        (
            FrameKind::Error {
                request_id: Some(9),
                code: ErrorCode::TerminalNotFound,
                message: "no such terminal".to_owned(),
            },
            vec![edge_pane()],
        ),
    ] {
        let reply = drive_split_reply(
            parked_split(SplitHost::Satellite(edge.clone()), Some(edge_pane())),
            frame,
        );
        assert_eq!(reply.leaves, vec![tid(1)], "no dead split is left behind");
        assert_eq!(reply.focused, Some(tid(1)), "focus stays put");
        assert!(!reply.outcome.emit_set_metadata && !reply.outcome.layout_replaced);
        assert!(reply.belled, "a refusal bells");
        assert_eq!(reply.outcome.notices.len(), 1);
        assert!(
            reply.outcome.notices[0]
                .text
                .contains("split onto satellite edge"),
            "the notice names the host: {}",
            reply.outcome.notices[0].text
        );
        assert_eq!(
            reply.outcome.kill_orphans, killed,
            "the spawned pane, unless no kill could reach it"
        );
        assert_eq!(reply.parked, 0, "the parked split is consumed");
    }
}

/// A satellite split whose relayed spawn is refused (the satellite is
/// unreachable) bells and names the host too.
#[test]
fn a_satellite_split_whose_spawn_is_refused_names_the_host() {
    use crate::attach::actions::SplitHost;
    use phux_protocol::wire::frame::{SpawnError, SpawnResult};

    let reply = drive_split_reply(
        parked_split(
            SplitHost::Satellite(phux_protocol::ids::SatelliteHost::new("edge")),
            None,
        ),
        FrameKind::ResourceSpawned {
            request_id: 9,
            result: SpawnResult::Err(SpawnError::SatelliteUnreachable(
                "satellite edge link is down".to_owned(),
            )),
        },
    );
    assert_eq!(reply.leaves, vec![tid(1)]);
    assert!(reply.belled);
    assert_eq!(reply.outcome.notices.len(), 1);
    assert!(
        reply.outcome.notices[0]
            .text
            .contains("could not split onto satellite edge: satellite edge link is down"),
        "{}",
        reply.outcome.notices[0].text
    );
    assert!(reply.outcome.adopt_spawned.is_empty());
}

/// A hub without host-aware spawns opened a satellite pane's split on
/// itself. It applies as a local split does, and the notice says the pane
/// is on this host, so it is not taken for a pane on the satellite.
#[test]
fn a_satellite_split_on_an_older_hub_says_it_opened_on_this_host() {
    use crate::attach::actions::SplitHost;
    use phux_protocol::wire::frame::SpawnResult;

    let reply = drive_split_reply(
        parked_split(
            SplitHost::AttachedInsteadOf(phux_protocol::ids::SatelliteHost::new("edge")),
            None,
        ),
        FrameKind::ResourceSpawned {
            request_id: 9,
            result: SpawnResult::Ok(tid(2)),
        },
    );
    assert_eq!(reply.leaves, vec![tid(1), tid(2)]);
    assert!(reply.outcome.adopt_spawned.is_empty());
    assert_eq!(reply.outcome.notices.len(), 1);
    let text = &reply.outcome.notices[0].text;
    assert!(
        text.contains("on this host") && text.contains("edge"),
        "{text}"
    );
}

/// A local split's reply raises no notice.
#[test]
fn a_local_split_reply_raises_no_notice() {
    use crate::attach::actions::SplitHost;
    use phux_protocol::wire::frame::SpawnResult;

    let reply = drive_split_reply(
        parked_split(SplitHost::Attached, None),
        FrameKind::ResourceSpawned {
            request_id: 9,
            result: SpawnResult::Ok(tid(2)),
        },
    );
    assert_eq!(reply.leaves, vec![tid(1), tid(2)]);
    assert!(reply.outcome.notices.is_empty());
}

/// phux-c2td.18: the user switched windows while a satellite split waited on
/// its attach. The split lands beside its source pane, in that pane's window,
/// and neither the window on screen nor its focus moves.
#[test]
fn a_split_parked_across_a_window_switch_lands_beside_its_source() {
    use crate::attach::actions::SplitHost;

    let mut workspace = Workspace::single(tid(1));
    workspace.add_window("2".to_owned(), tid(5));
    assert_eq!(workspace.active, 1, "the user is on the other window");
    let reply = drive_split_reply_in(
        workspace,
        Some(tid(5)),
        parked_split(
            SplitHost::Satellite(phux_protocol::ids::SatelliteHost::new("edge")),
            Some(edge_pane()),
        ),
        FrameKind::CommandResult {
            request_id: 9,
            result: phux_protocol::wire::frame::CommandResult::Ok,
        },
    );
    assert_eq!(
        window_leaves(&reply.workspace, 0),
        vec![tid(1), edge_pane()]
    );
    assert_eq!(window_leaves(&reply.workspace, 1), vec![tid(5)]);
    assert_eq!(reply.workspace.active, 1, "the window on screen stays");
    assert_eq!(reply.focused, Some(tid(5)), "focus is not stolen");
    assert!(reply.outcome.emit_set_metadata, "the split is saved");
    assert!(!reply.belled);
}

/// phux-c2td.18: the source pane closed while its split waited. Nothing is
/// split beside some other pane: the split is dropped, with a bell and a
/// notice, for a satellite split's attach and a local split's spawn alike.
/// phux-c2td.20: the pane it spawned is referenced by nothing, so it is
/// killed.
#[test]
fn a_split_whose_source_pane_closed_is_dropped() {
    use crate::attach::actions::SplitHost;
    use phux_protocol::wire::frame::SpawnResult;

    let edge = phux_protocol::ids::SatelliteHost::new("edge");
    for (split, frame, spawned) in [
        (
            parked_split(SplitHost::Satellite(edge), Some(edge_pane())),
            FrameKind::CommandResult {
                request_id: 9,
                result: phux_protocol::wire::frame::CommandResult::Ok,
            },
            edge_pane(),
        ),
        (
            parked_split(SplitHost::Attached, None),
            FrameKind::ResourceSpawned {
                request_id: 9,
                result: SpawnResult::Ok(tid(2)),
            },
            tid(2),
        ),
    ] {
        // Pane 1, the split's source, is gone; pane 3 is what remains.
        let reply = drive_split_reply_in(Workspace::single(tid(3)), Some(tid(3)), split, frame);
        assert_eq!(reply.leaves, vec![tid(3)], "nothing lands beside pane 3");
        assert_eq!(reply.focused, Some(tid(3)));
        assert!(!reply.outcome.emit_set_metadata && !reply.outcome.layout_replaced);
        assert!(reply.belled);
        assert_eq!(reply.outcome.notices.len(), 1);
        assert!(
            reply.outcome.notices[0].text.contains("split dropped"),
            "{}",
            reply.outcome.notices[0].text
        );
        assert_eq!(reply.outcome.kill_orphans, vec![spawned]);
        assert_eq!(reply.parked, 0);
    }
}

/// phux-flywheel: the apply-vs-paint split is observable. Driving a
/// `RESOURCE_OUTPUT` for the focused pane under a debug-level capturing
/// subscriber must close BOTH child spans — `vt_apply` (libghostty
/// parse) and `paint_trigger` (render) — so a trace can attribute
/// client lag to apply-ms vs paint-ms separately. We assert on
/// span-close events (the parse + render each report their own busy
/// time) rather than the fused parent `handle_server_frame` close.
#[test]
fn output_emits_separate_apply_and_paint_spans() {
    use std::sync::Arc;
    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::{Registry, fmt};

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("lock").extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> MakeWriter<'a> for Buf {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    let _guard = TRACE_TEST_LOCK.lock().expect("trace test lock");

    let buf = Buf::default();
    let layer = fmt::layer()
        .with_ansi(false)
        .with_writer(buf.clone())
        .with_span_events(fmt::format::FmtSpan::CLOSE);
    let subscriber = Registry::default().with(layer);

    {
        tracing::subscriber::set_global_default(subscriber)
            .expect("install test tracing subscriber");
        tracing_core::callsite::rebuild_interest_cache();
        let left = tid(1);
        let right = tid(2);
        let mut layout = two_pane_workspace(&left, &right, &left);
        let mut focused = Some(left.clone());
        let (mut engine, mut panes) =
            published_fixture(&[(&left, 80, 24, b""), (&right, 80, 24, b"")]);
        let mut out: Vec<u8> = Vec::new();
        // Drive the focused pane so the paint trigger fires.
        drive_output(
            &mut engine,
            &mut out,
            &mut layout,
            &mut focused,
            &mut panes,
            &left,
            b"hi",
        );
    }

    let log = String::from_utf8(buf.0.lock().expect("lock").clone()).expect("utf8");
    // Both child spans must have closed (FmtSpan::CLOSE prints a
    // `close` line carrying `time.busy` per span name).
    assert!(
        log.contains("vt_apply"),
        "vt_apply span never closed; log:\n{log}"
    );
    assert!(
        log.contains("paint_trigger"),
        "paint_trigger span never closed; log:\n{log}"
    );
    // And the parent fused span is still present (apply+paint).
    assert!(
        log.contains("handle_server_frame"),
        "parent span missing; log:\n{log}"
    );
}

/// A `Bell` frame routes a BEL byte through the injected sink, so a
/// headless capture (and a future agent surface) can observe it.
#[test]
fn bell_frame_writes_bel_to_sink() {
    let mut layout = Workspace::single(tid(1));
    let mut focused = Some(tid(1));
    let mut zoomed: Option<ResourceId> = None;
    let mut panes: HashMap<ResourceId, PaneSlot> = HashMap::new();
    let mut session_name = String::new();
    let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
    let overlay = Overlay;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();

    let mut out: Vec<u8> = Vec::new();
    handle_server_frame(
        &mut out,
        FrameKind::Bell {
            terminal_id: tid(1),
        },
        &mut panes,
        &mut layout,
        &mut focused,
        &mut zoomed,
        &mut session_name,
        None,
        None,
        None,
        (80, 24),
        &mut predict,
        &overlay,
        None,
        &mut pending_splits,
        &mut pending_windows,
        &mut HashSet::new(),
        &mut AgentMetaIndex::default(),
        false,
        false,
    )
    .expect("handle_server_frame");

    assert_eq!(&out, b"\x07", "bell must emit a single BEL byte");
}

/// Drive a `RESOURCE_CLOSED { terminal_id, exit_status }` through
/// [`handle_server_frame`] and return the resulting [`FrameOutcome`]
/// so the consumer-side detach policy (phux-4r1) can be asserted.
fn drive_closed(
    layout: &mut Workspace,
    focused: &mut Option<ResourceId>,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    terminal_id: &ResourceId,
    exit_status: Option<i32>,
) -> FrameOutcome {
    drive_closed_expecting(
        layout,
        focused,
        panes,
        terminal_id,
        exit_status,
        &mut HashSet::new(),
    )
}

/// [`drive_closed`] with a caller-owned `expected_closes` set, so the
/// phux-i0e8.2.2 suppress-and-drain contract can be asserted.
fn drive_closed_expecting(
    layout: &mut Workspace,
    focused: &mut Option<ResourceId>,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    terminal_id: &ResourceId,
    exit_status: Option<i32>,
    expected_closes: &mut HashSet<ResourceId>,
) -> FrameOutcome {
    let mut out: Vec<u8> = Vec::new();
    let mut session_name = String::new();
    let mut zoomed: Option<ResourceId> = None;
    let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
    let overlay = Overlay;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    handle_server_frame(
        &mut out,
        FrameKind::ResourceClosed {
            terminal_id: terminal_id.clone(),
            exit_status,
            reason: phux_protocol::wire::frame::CloseReason::Unknown,
        },
        panes,
        layout,
        focused,
        &mut zoomed,
        &mut session_name,
        None,
        None,
        None,
        (80, 24),
        &mut predict,
        &overlay,
        None,
        &mut pending_splits,
        &mut pending_windows,
        expected_closes,
        &mut AgentMetaIndex::default(),
        false,
        false,
    )
    .expect("handle_server_frame")
}

/// phux-4r1: the detach policy is consumer-owned. When the LAST pane
/// closes there is nothing left to render or route input to, so the
/// TUI detaches itself — the `ResourceClosed` arm returns
/// `FrameOutcome { exit: true }`. This is the consumer-side half of
/// the EOF reshape: the server emits `RESOURCE_CLOSED` (an L1
/// lifecycle fact) and the client decides to leave.
#[test]
fn last_pane_closed_detaches_the_client() {
    let pane = tid(1);
    let mut workspace = Workspace::single(pane.clone());
    let mut focused = Some(pane.clone());
    let mut panes = panes_for(&[&pane]);

    let outcome = drive_closed(&mut workspace, &mut focused, &mut panes, &pane, Some(0));

    assert!(
        outcome.exit,
        "closing the only pane must make the consumer detach (exit: true)",
    );
    assert_eq!(
        outcome.exit_reason,
        Some(AttachEnd::LastPaneClosed {
            exit_status: Some(0)
        }),
        "the exit must carry WHY: the last pane closed, with its status",
    );
    assert!(
        workspace.windows.is_empty(),
        "the workspace must have no windows left after the last pane closes",
    );
    assert!(
        !panes.contains_key(&pane),
        "the closed pane's slot must be dropped",
    );
}

/// Drive one frame through the dispatcher with a caller-owned keep-empty
/// mark and session name (ADR-0105).
fn drive_keep_empty(
    frame: FrameKind,
    workspace: &mut Workspace,
    focused: &mut Option<ResourceId>,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    session_name: &mut String,
    keep_empty: &mut bool,
) -> FrameOutcome {
    drive_keep_empty_with(
        frame,
        workspace,
        focused,
        panes,
        session_name,
        keep_empty,
        None,
    )
}

/// [`drive_keep_empty`] with a pending layout `GET_METADATA` request id, so
/// the attach-time layout reply can be driven.
fn drive_keep_empty_with(
    frame: FrameKind,
    workspace: &mut Workspace,
    focused: &mut Option<ResourceId>,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    session_name: &mut String,
    keep_empty: &mut bool,
    pending_layout_request: Option<u32>,
) -> FrameOutcome {
    let mut kernel = phux_client_core::session::SessionKernel::new(
        phux_client_core::engine::ghostty::GhosttyAdapter::new(
            phux_protocol::BootstrapLimits::default(),
        ),
        phux_protocol::BootstrapProfile::SynthesizedVtRaw,
    );
    let mut effects = phux_client_core::session::EffectBuffer::new();
    let mut out: Vec<u8> = Vec::new();
    let mut zoomed = None;
    let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
    let overlay = Overlay;
    handle_server_frame_with_kernel(
        &mut kernel,
        &mut effects,
        &mut out,
        frame,
        panes,
        workspace,
        focused,
        &mut zoomed,
        session_name,
        keep_empty,
        None,
        None,
        None,
        (80, 24),
        &mut predict,
        &overlay,
        pending_layout_request,
        &mut HashMap::new(),
        &mut HashMap::new(),
        &mut HashSet::new(),
        &mut AgentMetaIndex::default(),
        false,
        false,
    )
    .expect("handle_server_frame")
}

fn closed_cleanly(pane: &ResourceId) -> FrameKind {
    FrameKind::ResourceClosed {
        terminal_id: pane.clone(),
        exit_status: Some(0),
        reason: CloseReason::Unknown,
    }
}

/// ADR-0105: the last pane of a keep-empty session closing keeps the attach
/// and leaves an empty workspace for the empty state, and asks the driver
/// to tombstone the layout that now names only dead panes.
#[test]
fn keep_empty_last_pane_close_stays_attached_in_the_empty_state() {
    let pane = tid(1);
    let mut workspace = Workspace::single(pane.clone());
    let mut focused = Some(pane.clone());
    let mut panes = panes_for(&[&pane]);
    let mut name = "parked".to_owned();
    let mut keep = true;

    let outcome = drive_keep_empty(
        closed_cleanly(&pane),
        &mut workspace,
        &mut focused,
        &mut panes,
        &mut name,
        &mut keep,
    );

    assert!(!outcome.exit, "a keep-empty session must not detach");
    assert!(outcome.exit_reason.is_none());
    assert!(outcome.clear_layout, "the dead layout is tombstoned");
    assert!(outcome.layout_replaced, "the empty state is painted");
    assert!(workspace.windows.is_empty());
    assert_eq!(focused, None);
    assert!(!panes.contains_key(&pane));
}

/// A `phux.session.keep_empty/v1` broadcast moves the mark only for this
/// client's own session, and the moved mark decides the last pane's close.
#[test]
fn keep_empty_mark_follows_broadcasts_for_this_session_only() {
    use phux_protocol::wire::frame::{SESSION_KEEP_EMPTY_KEY, Scope};

    let pane = tid(1);
    let mut workspace = Workspace::single(pane.clone());
    let mut focused = Some(pane.clone());
    let mut panes = panes_for(&[&pane]);
    let mut name = "work".to_owned();
    let mut keep = false;
    let mark = |value: &[u8]| FrameKind::MetadataChanged {
        scope: Scope::Global,
        key: SESSION_KEEP_EMPTY_KEY.to_owned(),
        value: Some(value.to_vec()),
    };

    drive_keep_empty(
        mark(b"other\0true"),
        &mut workspace,
        &mut focused,
        &mut panes,
        &mut name,
        &mut keep,
    );
    assert!(!keep, "another session's mark is not ours");
    drive_keep_empty(
        mark(b"work\0true"),
        &mut workspace,
        &mut focused,
        &mut panes,
        &mut name,
        &mut keep,
    );
    assert!(keep);

    let outcome = drive_keep_empty(
        closed_cleanly(&pane),
        &mut workspace,
        &mut focused,
        &mut panes,
        &mut name,
        &mut keep,
    );
    assert!(!outcome.exit, "the broadcast mark keeps the attach");
}

/// Attaching to a session that is already empty starts in the empty state:
/// no workspace, no focus, and no pane slot for the sentinel focus id.
#[test]
fn attached_to_an_empty_session_starts_in_the_empty_state() {
    let sid = SessionId::new(4);
    let snapshot = SessionSnapshot::new(sid, WindowId::new(0), ResourceId::local(0))
        .with_sessions(vec![SessionInfo::new(sid, "parked").with_keep_empty(true)]);
    let mut workspace = Workspace::single(tid(9));
    let mut focused = Some(tid(9));
    let mut panes = HashMap::new();
    let mut name = String::new();
    let mut keep = false;

    let outcome = drive_keep_empty(
        FrameKind::Attached {
            attach_id: 1,
            snapshot,
            initial_client_id: ClientId::new(3),
        },
        &mut workspace,
        &mut focused,
        &mut panes,
        &mut name,
        &mut keep,
    );

    assert!(keep, "the snapshot's keep-empty mark is adopted");
    assert_eq!(name, "parked");
    assert!(workspace.windows.is_empty());
    assert_eq!(focused, None);
    assert!(
        panes.is_empty(),
        "the sentinel focus must not get a pane slot"
    );
    assert!(outcome.subscribe_layout);
    assert_eq!(outcome.own_client_id, Some(ClientId::new(3)));
}

/// A persisted layout whose every leaf is absent from the snapshot (left by a
/// keep-empty session that lost its last window with no client attached) is
/// discarded on attach, so the empty state shows instead of a dead pane.
#[test]
fn a_layout_of_only_dead_panes_is_discarded_on_attach() {
    let sid = SessionId::new(4);
    let snapshot = SessionSnapshot::new(sid, WindowId::new(0), ResourceId::local(0))
        .with_sessions(vec![SessionInfo::new(sid, "parked").with_keep_empty(true)]);
    let mut workspace = Workspace::default();
    let mut focused = None;
    let mut panes = HashMap::new();
    let mut name = String::new();
    let mut keep = false;
    drive_keep_empty(
        FrameKind::Attached {
            attach_id: 1,
            snapshot,
            initial_client_id: ClientId::new(3),
        },
        &mut workspace,
        &mut focused,
        &mut panes,
        &mut name,
        &mut keep,
    );

    let stale = Workspace::single(tid(9)).encode_cbor().expect("encode");
    let outcome = drive_keep_empty_with(
        FrameKind::MetadataValue {
            request_id: 7,
            value: Some(stale),
        },
        &mut workspace,
        &mut focused,
        &mut panes,
        &mut name,
        &mut keep,
        Some(7),
    );

    assert!(outcome.layout_get_answered, "the GET is still answered");
    assert!(outcome.attach_panes.is_empty(), "no dead pane is attached");
    assert!(workspace.windows.is_empty(), "the empty state stays");
    assert_eq!(focused, None);
}

/// `new-window` from the empty state: the spawn reply opens the first
/// window, focused on the new pane, and broadcasts the layout.
#[test]
fn new_window_from_the_empty_state_opens_the_first_window() {
    use super::handle_window_spawned;
    use crate::attach::actions::PendingWindow;
    use phux_protocol::wire::frame::SpawnResult;

    let mut workspace = Workspace::default();
    let mut focused = None;
    let mut panes = HashMap::new();
    let mut out: Vec<u8> = Vec::new();

    let outcome = handle_window_spawned(
        &mut out,
        &mut workspace,
        &mut focused,
        &mut panes,
        &PendingWindow {
            name: "1".to_owned(),
            adopt: None,
        },
        SpawnResult::Ok(tid(5)),
    )
    .expect("handle_window_spawned");

    assert_eq!(workspace.windows.len(), 1);
    assert_eq!(workspace.active, 0);
    assert_eq!(focused, Some(tid(5)));
    assert!(panes.contains_key(&tid(5)));
    assert!(outcome.emit_set_metadata && outcome.reflow_panes);
}

/// phux-i0e8.2.2: a last-pane death by signal (or unknown cause)
/// carries `exit_status: None` up as the exit reason, so the CLI can
/// say "killed" instead of pretending the exit was clean.
#[test]
fn last_pane_signal_death_carries_none_status_in_exit_reason() {
    let pane = tid(1);
    let mut workspace = Workspace::single(pane.clone());
    let mut focused = Some(pane.clone());
    let mut panes = panes_for(&[&pane]);

    let outcome = drive_closed(&mut workspace, &mut focused, &mut panes, &pane, None);

    assert!(outcome.exit);
    assert_eq!(
        outcome.exit_reason,
        Some(AttachEnd::LastPaneClosed { exit_status: None }),
    );
}

/// phux-l83x: `DETACHED` exits the loop *with* the server's stated
/// reason. Before the frame carried one, every ending — a requested
/// detach, a server shutting down under the user, another client taking
/// the attach — reached the CLI as the same wordless `Detached`.
#[test]
fn detached_carries_the_servers_reason_into_the_exit() {
    for reason in [
        None,
        Some(DetachReason::Requested),
        Some(DetachReason::ServerShutdown),
        Some(DetachReason::Replaced),
    ] {
        let pane = tid(1);
        let mut workspace = Workspace::single(pane.clone());
        let mut focused = Some(pane.clone());
        let mut panes = panes_for(&[&pane]);
        let mut out: Vec<u8> = Vec::new();
        let mut session_name = String::new();
        let mut zoomed: Option<ResourceId> = None;
        let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
        let overlay = Overlay;
        let mut pending_splits = HashMap::new();
        let mut pending_windows = HashMap::new();

        let outcome = handle_server_frame(
            &mut out,
            FrameKind::Detached {
                reason,
                message: "diagnostic only".to_owned(),
            },
            &mut panes,
            &mut workspace,
            &mut focused,
            &mut zoomed,
            &mut session_name,
            None,
            None,
            None,
            (80, 24),
            &mut predict,
            &overlay,
            None,
            &mut pending_splits,
            &mut pending_windows,
            &mut HashSet::new(),
            &mut AgentMetaIndex::default(),
            false,
            false,
        )
        .expect("handle_server_frame");

        assert!(outcome.exit, "DETACHED always ends the loop");
        assert_eq!(
            outcome.exit_reason,
            Some(AttachEnd::Detached { reason }),
            "the ending must carry the reason the server stated, including none",
        );
    }
}

/// Drive an `EVENT { terminal, Asked }` through [`handle_server_frame`]
/// and return the outcome (phux-foz.1 / ADR-0035).
fn drive_asked(
    layout: &mut Workspace,
    focused: &mut Option<ResourceId>,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    terminal_id: &ResourceId,
) -> FrameOutcome {
    use phux_protocol::wire::frame::AgentEvent;
    let mut out: Vec<u8> = Vec::new();
    let mut session_name = String::new();
    let mut zoomed: Option<ResourceId> = None;
    let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
    let overlay = Overlay;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    let mut agent_meta = AgentMetaIndex::default();
    handle_server_frame(
        &mut out,
        FrameKind::Event {
            terminal: Some(terminal_id.clone()),
            event: AgentEvent::Asked {
                id: "q1".to_owned(),
                question: "deploy to prod?".to_owned(),
                suggestions: vec!["yes".to_owned(), "no".to_owned()],
                elapsed_seconds: None,
            },
        },
        panes,
        layout,
        focused,
        &mut zoomed,
        &mut session_name,
        None,
        None,
        None,
        (80, 24),
        &mut predict,
        &overlay,
        None,
        &mut pending_splits,
        &mut pending_windows,
        &mut HashSet::new(),
        &mut agent_meta,
        false,
        false,
    )
    .expect("handle_server_frame")
}

/// phux-foz.1: an ADR-0035 `Asked` event raises the pane's attention
/// flag and asks the driver to repaint the chrome — including for a
/// NON-focused pane (the whole point is surfacing a question the user
/// is not looking at).
#[test]
fn asked_event_sets_attention_and_dirties_chrome() {
    let left = tid(1);
    let right = tid(2);
    let mut layout = two_pane_workspace(&left, &right, &left);
    let mut focused = Some(left.clone());
    let mut panes = panes_for(&[&left, &right]);

    let outcome = drive_asked(&mut layout, &mut focused, &mut panes, &right);

    assert!(
        panes.get(&right).expect("slot").attention,
        "the asking pane's attention flag must raise"
    );
    assert!(
        !panes.get(&left).expect("slot").attention,
        "the other pane stays quiet"
    );
    assert!(outcome.chrome_dirty, "the chrome must repaint");
}

/// Server-wide mirror slots do not make a peer's question local.
#[test]
fn peer_asked_event_with_a_cached_slot_routes_to_foreign_attention() {
    let local = tid(1);
    let peer = tid(9);
    let mut layout = Workspace::single(local.clone());
    let mut focused = Some(local.clone());
    let mut panes = panes_for(&[&local, &peer]);
    let outcome = drive_asked(&mut layout, &mut focused, &mut panes, &peer);
    assert_eq!(outcome.foreign_attention, Some(peer.clone()));
    assert!(
        panes.get(&peer).unwrap().attention,
        "retain the observation until ownership is fully known"
    );
    assert!(!outcome.chrome_dirty);
}

/// Provisional workspace ownership must not lose a coalesced local question.
#[test]
fn early_local_ask_survives_persisted_layout_adoption() {
    let first = tid(1);
    let later = tid(2);
    let mut workspace = Workspace::single(first.clone());
    let mut focused = Some(first.clone());
    let mut panes = panes_for(&[&first, &later]);
    let asked = drive_asked(&mut workspace, &mut focused, &mut panes, &later);
    assert_eq!(asked.foreign_attention, Some(later.clone()));
    let complete = two_pane_workspace(&first, &later, &first);
    let adopted = drive_layout_frame(
        FrameKind::MetadataValue {
            request_id: 42,
            value: Some(complete.encode_cbor().unwrap()),
        },
        Some(42),
        &mut workspace,
        &mut focused,
        &mut panes,
    );
    assert!(adopted.layout_replaced);
    assert!(
        panes.get(&later).unwrap().attention,
        "the server need not repeat Asked"
    );
    let repeated = drive_asked(&mut workspace, &mut focused, &mut panes, &later);
    assert!(!repeated.chrome_dirty);
    assert!(
        repeated.foreign_attention.is_none(),
        "ownership now resolves locally"
    );
}

/// phux-foz.1: a repeated `Asked` while the flag is already up changes
/// no visible state, so it must not request another repaint.
#[test]
fn repeated_asked_event_does_not_redirty_chrome() {
    let pane = tid(1);
    let mut layout = Workspace::single(pane.clone());
    let mut focused = Some(pane.clone());
    let mut panes = panes_for(&[&pane]);

    let first = drive_asked(&mut layout, &mut focused, &mut panes, &pane);
    assert!(first.chrome_dirty);
    let second = drive_asked(&mut layout, &mut focused, &mut panes, &pane);
    assert!(
        !second.chrome_dirty,
        "an already-flagged pane must not force a repaint"
    );
    assert!(panes.get(&pane).expect("slot").attention, "flag stays up");
}

/// phux-foz.1: an `Asked` for a pane with no slot yet (it can precede
/// the first snapshot) is dropped without a repaint, mirroring the
/// early-`TerminalControl` policy.
#[test]
fn asked_event_for_unknown_pane_is_dropped() {
    let known = tid(1);
    let unknown = tid(9);
    let mut layout = Workspace::single(known.clone());
    let mut focused = Some(known.clone());
    let mut panes = panes_for(&[&known]);

    let outcome = drive_asked(&mut layout, &mut focused, &mut panes, &unknown);

    assert!(!outcome.chrome_dirty, "no slot, nothing to repaint");
    assert!(
        !panes.contains_key(&unknown),
        "no slot is allocated for an event-only pane"
    );
}

/// phux-foz.4: drive one agent event through [`handle_server_frame`]
/// with minimal single-pane scaffolding; returns the outcome.
fn drive_event(
    panes: &mut HashMap<ResourceId, PaneSlot>,
    terminal_id: &ResourceId,
    event: phux_protocol::wire::frame::AgentEvent,
) -> FrameOutcome {
    let mut layout = Workspace::single(terminal_id.clone());
    let mut focused = Some(terminal_id.clone());
    let mut out: Vec<u8> = Vec::new();
    let mut session_name = String::new();
    let mut zoomed: Option<ResourceId> = None;
    let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
    let overlay = Overlay;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    let mut agent_meta = AgentMetaIndex::default();
    handle_server_frame(
        &mut out,
        FrameKind::Event {
            terminal: Some(terminal_id.clone()),
            event,
        },
        panes,
        &mut layout,
        &mut focused,
        &mut zoomed,
        &mut session_name,
        None,
        None,
        None,
        (80, 24),
        &mut predict,
        &overlay,
        None,
        &mut pending_splits,
        &mut pending_windows,
        &mut HashSet::new(),
        &mut agent_meta,
        false,
        false,
    )
    .expect("handle_server_frame")
}

/// phux-foz.4: a `cwd_changed` event lands in the pane's slot and
/// dirties the chrome; repeating the same directory is a no-op.
#[test]
fn cwd_changed_event_updates_slot_and_coalesces() {
    use phux_protocol::wire::frame::AgentEvent;
    let pane = tid(1);
    let mut panes = panes_for(&[&pane]);

    let first = drive_event(
        &mut panes,
        &pane,
        AgentEvent::CwdChanged {
            cwd: "/tmp/work".to_owned(),
        },
    );
    assert!(first.chrome_dirty, "a new cwd must repaint the chrome");
    assert_eq!(
        panes.get(&pane).expect("slot").cwd.as_deref(),
        Some("/tmp/work")
    );

    let repeat = drive_event(
        &mut panes,
        &pane,
        AgentEvent::CwdChanged {
            cwd: "/tmp/work".to_owned(),
        },
    );
    assert!(!repeat.chrome_dirty, "unchanged cwd must not repaint");
}

/// phux-foz.4: a `command_finished` event records the exit code (and a
/// later code replaces it); an unchanged value is a no-op.
#[test]
fn command_finished_event_records_last_exit() {
    use phux_protocol::wire::frame::AgentEvent;
    let pane = tid(1);
    let mut panes = panes_for(&[&pane]);
    assert_eq!(panes.get(&pane).expect("slot").last_exit, None);

    let first = drive_event(
        &mut panes,
        &pane,
        AgentEvent::CommandFinished { exit_code: Some(0) },
    );
    assert!(first.chrome_dirty);
    assert_eq!(panes.get(&pane).expect("slot").last_exit, Some(0));

    let repeat = drive_event(
        &mut panes,
        &pane,
        AgentEvent::CommandFinished { exit_code: Some(0) },
    );
    assert!(!repeat.chrome_dirty, "same code must not repaint");

    let failed = drive_event(
        &mut panes,
        &pane,
        AgentEvent::CommandFinished {
            exit_code: Some(127),
        },
    );
    assert!(failed.chrome_dirty);
    assert_eq!(panes.get(&pane).expect("slot").last_exit, Some(127));
}

/// phux-foz.4: cwd/exit events for a pane with no slot yet are dropped
/// without a repaint, mirroring the early-`TerminalControl` policy.
#[test]
fn cwd_and_exit_events_for_unknown_pane_are_dropped() {
    use phux_protocol::wire::frame::AgentEvent;
    let known = tid(1);
    let unknown = tid(9);
    let mut panes = panes_for(&[&known]);

    let cwd = drive_event(
        &mut panes,
        &unknown,
        AgentEvent::CwdChanged {
            cwd: "/x".to_owned(),
        },
    );
    let exit = drive_event(
        &mut panes,
        &unknown,
        AgentEvent::CommandFinished { exit_code: Some(1) },
    );
    assert!(!cwd.chrome_dirty && !exit.chrome_dirty);
    assert!(!panes.contains_key(&unknown));
}

/// Activity-only events are valid on an attached subscription but carry
/// no client projection state; they must not abort the attach.
#[test]
fn idle_event_is_ignored() {
    use phux_protocol::wire::frame::AgentEvent;
    let pane = tid(1);
    let mut panes = panes_for(&[&pane]);

    let outcome = drive_event(&mut panes, &pane, AgentEvent::Idle);
    assert!(!outcome.chrome_dirty);
}

/// phux-i0e8.2.1: a `TerminalControl` event carrying `holder` and a
/// running lifecycle.
fn control_event(holder: Option<ClientId>) -> phux_protocol::wire::frame::AgentEvent {
    use phux_protocol::wire::frame::{AgentEvent, ControlAction, ResourceLifecycle};
    AgentEvent::TerminalControl {
        lifecycle: ResourceLifecycle::Running,
        exit_status: None,
        input_holder: holder,
        action: match holder {
            Some(_) => ControlAction::Acquired,
            None => ControlAction::Released,
        },
        actor: holder,
    }
}

/// phux-i0e8.2.1: drive one frame through [`handle_server_frame`]
/// with an explicit focused pane (which `drive_event` pins to the
/// event's own terminal), for the input-authority notice tests.
fn drive_frame_focused(
    panes: &mut HashMap<ResourceId, PaneSlot>,
    focused_id: &ResourceId,
    frame: FrameKind,
) -> FrameOutcome {
    let mut layout = Workspace::single(focused_id.clone());
    let mut focused = Some(focused_id.clone());
    let mut out: Vec<u8> = Vec::new();
    let mut session_name = String::new();
    let mut zoomed: Option<ResourceId> = None;
    let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
    let overlay = Overlay;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    let mut agent_meta = AgentMetaIndex::default();
    handle_server_frame(
        &mut out,
        frame,
        panes,
        &mut layout,
        &mut focused,
        &mut zoomed,
        &mut session_name,
        None,
        None,
        None,
        (80, 24),
        &mut predict,
        &overlay,
        None,
        &mut pending_splits,
        &mut pending_windows,
        &mut HashSet::new(),
        &mut agent_meta,
        false,
        false,
    )
    .expect("handle_server_frame")
}

/// phux-i0e8.2.1 acceptance (a): a focused-pane input-authority holder
/// TRANSITION yields the expected notice; the attach-time initial
/// state (the first `TerminalControl` a slot sees) yields none.
#[test]
fn focused_holder_transition_yields_a_notice_and_initial_state_does_not() {
    use crate::render::chrome::status_bar::NoticeSeverity;
    let pane = tid(1);
    let mut panes = panes_for(&[&pane]);
    let holder = ClientId::new(9);

    // Attach-time initial state: first control event folds silently.
    let initial = drive_event(&mut panes, &pane, control_event(Some(holder)));
    assert!(initial.chrome_dirty, "the badge still refreshes");
    assert!(
        initial.notices.is_empty(),
        "the attach-time initial state must not raise a notice"
    );
    assert_eq!(panes.get(&pane).expect("slot").input_holder, Some(holder));

    // A later holder change is a transition: notice raised.
    let released = drive_event(&mut panes, &pane, control_event(None));
    assert_eq!(released.notices.len(), 1, "one notice per transition");
    assert_eq!(released.notices[0].severity, NoticeSeverity::Info);
    assert_eq!(released.notices[0].text, "input: wheel released");

    let seized = drive_event(&mut panes, &pane, control_event(Some(holder)));
    assert_eq!(seized.notices.len(), 1);
    assert_eq!(seized.notices[0].text, "input: c9 took the wheel");

    // A control event that does NOT move the holder (e.g. a freeze)
    // is not an authority transition: no notice.
    let same = drive_event(&mut panes, &pane, control_event(Some(holder)));
    assert!(
        same.notices.is_empty(),
        "an unchanged holder must not raise a notice"
    );
}

/// phux-i0e8.2.1: a holder transition on an UNFOCUSED pane refreshes
/// the chrome but raises no notice — the transient slot is scoped to
/// the pane the user is typing into.
#[test]
fn unfocused_holder_transition_yields_no_notice() {
    let focused = tid(1);
    let background = tid(2);
    let mut panes = panes_for(&[&focused, &background]);
    let holder = ClientId::new(4);

    // Seed the background pane's initial control state, then transition.
    let _ = drive_frame_focused(
        &mut panes,
        &focused,
        FrameKind::Event {
            terminal: Some(background.clone()),
            event: control_event(None),
        },
    );
    let outcome = drive_frame_focused(
        &mut panes,
        &focused,
        FrameKind::Event {
            terminal: Some(background.clone()),
            event: control_event(Some(holder)),
        },
    );
    assert!(outcome.chrome_dirty, "the badge state still folds");
    assert!(
        outcome.notices.is_empty(),
        "a background pane's handover must not steal the notice slot"
    );
    assert_eq!(
        panes.get(&background).expect("slot").input_holder,
        Some(holder),
    );
}

/// phux-i0e8.2.1 acceptance (b): an uncorrelated
/// `ERROR { SATELLITE_UNREACHABLE }` — the hub announcing a
/// degraded-federation transition — yields a Warn notice; the
/// correlated shape stays on its request/reply path (no notice).
#[test]
fn degraded_federation_transition_yields_a_warn_notice() {
    use crate::render::chrome::status_bar::NoticeSeverity;
    use phux_protocol::wire::frame::ErrorCode;
    let pane = tid(1);
    let mut panes = panes_for(&[&pane]);

    let outcome = drive_frame_focused(
        &mut panes,
        &pane,
        FrameKind::Error {
            request_id: None,
            code: ErrorCode::SatelliteUnreachable,
            message: "satellite gpubox unreachable".to_owned(),
        },
    );
    assert_eq!(outcome.notices.len(), 1);
    assert_eq!(outcome.notices[0].severity, NoticeSeverity::Warn);
    assert_eq!(
        outcome.notices[0].text,
        "federation degraded: satellite gpubox unreachable",
    );

    let correlated = drive_frame_focused(
        &mut panes,
        &pane,
        FrameKind::Error {
            request_id: Some(7),
            code: ErrorCode::SatelliteUnreachable,
            message: "satellite gpubox unreachable".to_owned(),
        },
    );
    assert!(
        correlated.notices.is_empty(),
        "a correlated satellite error belongs to its request, not the notice slot"
    );
}

/// phux-ijuj: no `ErrorCode`, in either correlation shape, ends the
/// attach.
///
/// SPEC §9 puts termination on `DETACHED` plus transport close, and this
/// server emits the same code both fatally and non-fatally, so a
/// client-side fatality table cannot be sound. The dispatcher therefore
/// degrades on every `ERROR`: uncorrelated codes raise a Warn notice
/// naming the code, correlated ones stay on their request/reply path.
///
/// The code list is swept out of the wire tables rather than written by
/// hand, so a code added to the protocol is covered without editing this
/// test.
#[test]
fn no_error_code_is_fatal_in_the_attached_phase() {
    use crate::render::chrome::status_bar::NoticeSeverity;
    use phux_protocol::wire::frame::ErrorCode;

    let codes: Vec<ErrorCode> = (0..=u16::MAX).filter_map(ErrorCode::from_wire).collect();
    assert!(
        !codes.is_empty(),
        "the wire tables must define at least one error code"
    );

    let pane = tid(1);
    for code in codes {
        for request_id in [None, Some(11_u32)] {
            let mut workspace = Workspace::single(pane.clone());
            let mut focused = Some(pane.clone());
            let mut panes = panes_for(&[&pane]);
            let outcome = try_drive_layout_frame(
                FrameKind::Error {
                    request_id,
                    code,
                    message: "the pane fell over".to_owned(),
                },
                None,
                &mut workspace,
                &mut focused,
                &mut panes,
            )
            .unwrap_or_else(|error| {
                panic!(
                    "ERROR {code:?} (request_id={request_id:?}) must not end the attach: {error:?}"
                )
            });
            assert!(
                !outcome.exit,
                "ERROR {code:?} (request_id={request_id:?}) must not ask the driver to exit"
            );

            match (request_id, code) {
                (Some(_), _) => assert!(
                    outcome.notices.is_empty(),
                    "a correlated {code:?} belongs to its request, not the notice slot"
                ),
                (None, ErrorCode::SatelliteUnreachable) => assert_eq!(
                    outcome.notices[0].text, "federation degraded: the pane fell over",
                    "the degraded-federation wording is its own"
                ),
                (None, _) => {
                    assert_eq!(
                        outcome.notices.len(),
                        1,
                        "an uncorrelated {code:?} must surface exactly one notice"
                    );
                    assert_eq!(outcome.notices[0].severity, NoticeSeverity::Warn);
                    assert!(
                        outcome.notices[0].text.contains("the pane fell over"),
                        "the notice must carry the server's message: {}",
                        outcome.notices[0].text
                    );
                    assert!(
                        outcome.notices[0].text.contains(&format!("{code:?}")),
                        "the notice must name the code: {}",
                        outcome.notices[0].text
                    );
                }
            }
        }
    }
}

/// phux-ijuj: per-pane scrollback loss reaches the status bar.
///
/// `KernelStatus::HistoryUnavailable` used to be swallowed by the
/// catch-all `tracing::warn!` over kernel statuses, so a pane whose
/// history boundary was retired went quiet while its live output kept
/// flowing. The kernel names the Terminal, so this notice can too.
#[test]
fn history_unavailable_status_names_the_pane_in_a_warn_notice() {
    use crate::render::chrome::status_bar::NoticeSeverity;

    let terminal_id = tid(3);
    let mut kernel = phux_client_core::session::SessionKernel::new(
        phux_client_core::engine::ghostty::GhosttyAdapter::new(
            phux_protocol::BootstrapLimits::default(),
        ),
        phux_protocol::BootstrapProfile::SynthesizedVtRaw,
    );
    let mut effects = phux_client_core::session::EffectBuffer::new();
    let mut panes = HashMap::new();
    dispatch_engine_frame(
        &mut kernel,
        &mut effects,
        &mut panes,
        begin_frame(&terminal_id),
    );
    dispatch_engine_frame(
        &mut kernel,
        &mut effects,
        &mut panes,
        ready_frame(&terminal_id),
    );

    // A history page the codec cannot decode retires this pane's
    // scrollback boundary; live output keeps flowing.
    let outcome = dispatch_engine_frame(
        &mut kernel,
        &mut effects,
        &mut panes,
        FrameKind::HistoryPage {
            terminal_id,
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            rows: 1,
            page_seq: 1,
            cursor: bytes::Bytes::from_static(b"cursor"),
            next_cursor: None,
            payload: bytes::Bytes::from_static(b"malformed-history"),
        },
    );

    assert_eq!(
        outcome.notices.len(),
        1,
        "a retired history boundary must surface exactly one notice"
    );
    assert_eq!(outcome.notices[0].severity, NoticeSeverity::Warn);
    assert_eq!(
        outcome.notices[0].text,
        "pane 3: scrollback unavailable (CodecFailure)"
    );
}

/// phux-4r1: closing one of several panes is NOT a detach. The
/// survivor stays attached — the `ResourceClosed` arm folds the
/// closed leaf out, re-anchors focus, and asks for a repaint +
/// reflow + broadcast, with `exit: false`.
#[test]
fn closing_one_of_several_panes_keeps_the_client_attached() {
    let left = tid(1);
    let right = tid(2);
    let mut workspace = two_pane_workspace(&left, &right, &left);
    let mut focused = Some(left.clone());
    let mut panes = panes_for(&[&left, &right]);

    let outcome = drive_closed(&mut workspace, &mut focused, &mut panes, &left, Some(0));

    assert!(
        !outcome.exit,
        "a surviving pane means the client stays attached (exit: false)",
    );
    assert_eq!(
        workspace.windows.len(),
        1,
        "the window survives with the remaining pane",
    );
    assert_eq!(
        focused,
        Some(right),
        "focus re-anchors onto the surviving leaf",
    );
    assert!(
        outcome.layout_replaced && outcome.emit_set_metadata && outcome.reflow_panes,
        "the fold triggers repaint + sibling broadcast + survivor reflow",
    );
    assert!(
        outcome.notices.is_empty(),
        "a clean exit 0 is the user typing `exit` — no notice",
    );
}

/// phux-i0e8.2.2: a surviving layout gets a transient Warn notice when
/// a sibling pane dies with a non-zero status — the OOM-killed / crashed
/// process must not vanish silently while the fold animates over it.
#[test]
fn survivor_close_with_nonzero_status_raises_warn_notice() {
    use crate::render::chrome::status_bar::NoticeSeverity;
    let left = tid(1);
    let right = tid(2);
    let mut workspace = two_pane_workspace(&left, &right, &left);
    let mut focused = Some(left.clone());
    let mut panes = panes_for(&[&left, &right]);

    let outcome = drive_closed(&mut workspace, &mut focused, &mut panes, &left, Some(137));

    assert_eq!(outcome.notices.len(), 1, "exactly one notice per close");
    assert_eq!(outcome.notices[0].severity, NoticeSeverity::Warn);
    assert_eq!(outcome.notices[0].text, "pane 1: exited 137");
}

/// phux-i0e8.2.2: `exit_status: None` (signal kill / unknown) names the
/// shape rather than inventing a code.
#[test]
fn survivor_close_by_signal_names_the_kill_shape() {
    use crate::render::chrome::status_bar::NoticeSeverity;
    let left = tid(1);
    let right = tid(2);
    let mut workspace = two_pane_workspace(&left, &right, &left);
    let mut focused = Some(left.clone());
    let mut panes = panes_for(&[&left, &right]);

    let outcome = drive_closed(&mut workspace, &mut focused, &mut panes, &right, None);

    assert_eq!(outcome.notices.len(), 1);
    assert_eq!(outcome.notices[0].severity, NoticeSeverity::Warn);
    assert_eq!(
        outcome.notices[0].text,
        "pane 2: killed (signal or unknown)"
    );
}

/// phux-i0e8.2.2: a close THIS client requested (kill-pane /
/// kill-window parked the id in `expected_closes`) is suppressed —
/// and the marker is DRAINED, so a later spontaneous death of the
/// same id would notify again.
#[test]
fn expected_close_suppresses_notice_and_drains_the_marker() {
    let left = tid(1);
    let right = tid(2);
    let mut workspace = two_pane_workspace(&left, &right, &left);
    let mut focused = Some(left.clone());
    let mut panes = panes_for(&[&left, &right]);
    let mut expected: HashSet<ResourceId> = HashSet::new();
    expected.insert(left.clone());

    let outcome = drive_closed_expecting(
        &mut workspace,
        &mut focused,
        &mut panes,
        &left,
        Some(137),
        &mut expected,
    );

    assert!(
        outcome.notices.is_empty(),
        "a client-initiated kill is not news to the client",
    );
    assert!(
        expected.is_empty(),
        "the expectation must be consumed by the close it predicted",
    );
}

#[test]
fn closing_the_mru_pane_clears_stale_history() {
    let left = tid(1);
    let right = tid(2);
    let mut workspace = two_pane_workspace(&left, &right, &left);
    let mut focused = Some(left.clone());
    let mut panes = panes_for(&[&left, &right]);
    let mut history = crate::attach::focus::FocusHistory::with_previous(right.clone());

    let before = focused.clone();
    let _ = drive_closed(&mut workspace, &mut focused, &mut panes, &right, Some(0));
    history.observe(before, focused.as_ref());
    history.repair(focused.as_ref(), &workspace);

    assert_eq!(
        history.previous(),
        None,
        "closed MRU target must be cleared"
    );
}

/// ADR-0040: drive one frame through [`handle_server_frame`] with a
/// caller-owned [`AgentMetaIndex`], for the agent-metadata arms.
fn drive_meta_frame(frame: FrameKind, agent_meta: &mut AgentMetaIndex) -> FrameOutcome {
    let pane = tid(1);
    let mut layout = Workspace::single(pane.clone());
    let mut focused = Some(pane.clone());
    // phux-k0cw: the agent arm now checks pane membership before folding
    // a record into the LOCAL index, so the fixture must hold the slot it
    // claims to be receiving records for — which is what a subscribed
    // pane always has in practice.
    let mut panes: HashMap<ResourceId, PaneSlot> = panes_for(&[&pane]);
    let mut out: Vec<u8> = Vec::new();
    let mut session_name = String::new();
    let mut zoomed: Option<ResourceId> = None;
    let mut predict = PredictionState::new(PredictiveConfig::disabled(), 80, 24);
    let overlay = Overlay;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    handle_server_frame(
        &mut out,
        frame,
        &mut panes,
        &mut layout,
        &mut focused,
        &mut zoomed,
        &mut session_name,
        None,
        None,
        None,
        (80, 24),
        &mut predict,
        &overlay,
        None,
        &mut pending_splits,
        &mut pending_windows,
        &mut HashSet::new(),
        agent_meta,
        false,
        false,
    )
    .expect("handle_server_frame")
}

/// ADR-0040: a subscribed `phux.agent/v1` broadcast decodes into the
/// index and flags the chrome refresh; the tombstone (DELETE) clears
/// the record so labels fall back to the OSC-title path.
#[test]
fn agent_metadata_broadcast_updates_index_and_tombstone_clears_it() {
    use phux_protocol::wire::frame::{RESOURCE_AGENT_KEY, Scope};
    let pane = tid(1);
    let mut agent_meta = AgentMetaIndex::default();

    let outcome = drive_meta_frame(
        FrameKind::MetadataChanged {
            scope: Scope::Resource(pane.clone()),
            key: RESOURCE_AGENT_KEY.to_owned(),
            value: Some(br#"{"name":"reviewer","state":"blocked"}"#.to_vec()),
        },
        &mut agent_meta,
    );
    assert!(outcome.agent_meta_changed, "a new record must flag chrome");
    let record = agent_meta.records.get(&pane).expect("record stored");
    assert_eq!(record.name, "reviewer");
    assert_eq!(
        record.state,
        phux_client::agent_meta::AgentMetaState::Blocked
    );

    // Re-asserting the identical record is a no-op (no repaint churn).
    let outcome = drive_meta_frame(
        FrameKind::MetadataChanged {
            scope: Scope::Resource(pane.clone()),
            key: RESOURCE_AGENT_KEY.to_owned(),
            value: Some(br#"{"name":"reviewer","state":"blocked"}"#.to_vec()),
        },
        &mut agent_meta,
    );
    assert!(
        !outcome.agent_meta_changed,
        "identical record must not flag"
    );

    // Tombstone (DELETE_METADATA) clears the record.
    let outcome = drive_meta_frame(
        FrameKind::MetadataChanged {
            scope: Scope::Resource(pane.clone()),
            key: RESOURCE_AGENT_KEY.to_owned(),
            value: None,
        },
        &mut agent_meta,
    );
    assert!(outcome.agent_meta_changed, "a cleared record must flag");
    assert!(!agent_meta.records.contains_key(&pane));
}

/// ADR-0040: a `GET_METADATA` reply correlated through
/// `AgentMetaIndex::pending` seeds the record for a pane whose agent
/// declared itself before we attached; an absent key (`value: None`)
/// resolves the pending entry without inventing a record.
#[test]
fn agent_metadata_get_reply_is_correlated_by_request_id() {
    let pane = tid(1);
    let mut agent_meta = AgentMetaIndex::default();
    agent_meta.pending.insert(77, pane.clone());

    let outcome = drive_meta_frame(
        FrameKind::MetadataValue {
            request_id: 77,
            value: Some(br#"{"name":"codex","kind":"codex","state":"working"}"#.to_vec()),
        },
        &mut agent_meta,
    );
    assert!(outcome.agent_meta_changed);
    assert!(agent_meta.pending.is_empty(), "pending entry consumed");
    assert_eq!(agent_meta.records.get(&pane).expect("record").name, "codex");

    agent_meta.pending.insert(78, pane);
    let outcome = drive_meta_frame(
        FrameKind::MetadataValue {
            request_id: 78,
            value: None,
        },
        &mut agent_meta,
    );
    assert!(outcome.agent_meta_changed, "absent key clears the record");
    assert!(agent_meta.records.is_empty());
}

/// phux-foz.5: the `phux.config.reload/v1` doorbell flags a config
/// reload on a non-tombstone Global broadcast; tombstones and
/// non-Global scopes do not ring it.
#[test]
fn config_reload_doorbell_flags_reload_and_ignores_tombstones() {
    use phux_protocol::wire::frame::{CONFIG_RELOAD_KEY, Scope};
    let mut agent_meta = AgentMetaIndex::default();

    let outcome = drive_meta_frame(
        FrameKind::MetadataChanged {
            scope: Scope::Global,
            key: CONFIG_RELOAD_KEY.to_owned(),
            value: Some(b"1234-99".to_vec()),
        },
        &mut agent_meta,
    );
    assert!(outcome.config_reload, "the doorbell must flag a reload");
    assert!(
        !outcome.layout_replaced && !outcome.agent_meta_changed,
        "the doorbell must not masquerade as a layout or agent change",
    );

    // Tombstone (DELETE_METADATA): not a reload request.
    let outcome = drive_meta_frame(
        FrameKind::MetadataChanged {
            scope: Scope::Global,
            key: CONFIG_RELOAD_KEY.to_owned(),
            value: None,
        },
        &mut agent_meta,
    );
    assert!(!outcome.config_reload, "a tombstone must not ring it");

    // Wrong scope: some other consumer's key reuse must not ring it.
    let outcome = drive_meta_frame(
        FrameKind::MetadataChanged {
            scope: Scope::Resource(tid(9)),
            key: CONFIG_RELOAD_KEY.to_owned(),
            value: Some(b"5678-99".to_vec()),
        },
        &mut agent_meta,
    );
    assert!(!outcome.config_reload, "non-Global scope must not ring it");
}

/// ADR-0040: malformed record bytes (bad JSON, empty name) must read
/// as "no declared agent" — never a stored record, never a crash.
#[test]
fn agent_metadata_rejects_malformed_records() {
    use phux_protocol::wire::frame::{RESOURCE_AGENT_KEY, Scope};
    let pane = tid(1);
    let mut agent_meta = AgentMetaIndex::default();
    let outcome = drive_meta_frame(
        FrameKind::MetadataChanged {
            scope: Scope::Resource(pane),
            key: RESOURCE_AGENT_KEY.to_owned(),
            value: Some(b"not json at all".to_vec()),
        },
        &mut agent_meta,
    );
    assert!(!outcome.agent_meta_changed);
    assert!(agent_meta.records.is_empty());
}

// ---------------------------------------------------------------------------
// Resource kinds: AgentSession children ride the attach as record streams.
// ---------------------------------------------------------------------------

/// A snapshot with one Terminal pane and one `AgentSession` bound to it, the
/// way a server advertises a `phux agent session open` child: no grid
/// (`0x0`), no window, a parent, and an agent facet.
fn mixed_kind_snapshot(pane: &ResourceId, agent: &ResourceId) -> SessionSnapshot {
    let window = WindowId::new(1);
    let session = SessionId::new(1);
    SessionSnapshot::new(session, window, pane.clone())
        .with_sessions(vec![SessionInfo::new(session, "work".to_owned())])
        .with_windows(vec![WindowInfo::new(window, session, "w0".to_owned())])
        .with_resources(vec![
            ResourceInfo::new(pane.clone(), window, 100, 30),
            ResourceInfo::new(agent.clone(), WindowId::new(0), 0, 0)
                .with_kind(ResourceKind::AgentSession)
                .with_parent(Some(pane.clone()))
                .with_agent(Some(
                    AgentFacet::new("claude", "working").with_native_id(Some("s-1".to_owned())),
                )),
        ])
}

fn kind_fixture() -> EngineFixture {
    EngineFixture {
        kernel: phux_client_core::session::SessionKernel::new(
            phux_client_core::engine::ghostty::GhosttyAdapter::new(
                phux_protocol::BootstrapLimits::default(),
            ),
            phux_protocol::BootstrapProfile::SynthesizedVtRaw,
        ),
        effects: phux_client_core::session::EffectBuffer::new(),
    }
}

/// Drive one frame through the dispatcher with a caller-owned kernel, the
/// shape every kind test below needs.
fn drive_kind_frame(
    fixture: &mut EngineFixture,
    frame: FrameKind,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    workspace: &mut Workspace,
    focused: &mut Option<ResourceId>,
) -> Result<FrameOutcome, AttachError> {
    let mut out: Vec<u8> = Vec::new();
    let mut zoomed: Option<ResourceId> = None;
    let mut session_name = String::new();
    let mut predict = PredictionState::new(PredictiveConfig::disabled(), 100, 30);
    let overlay = Overlay;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    handle_server_frame_with_kernel(
        &mut fixture.kernel,
        &mut fixture.effects,
        &mut out,
        frame,
        panes,
        workspace,
        focused,
        &mut zoomed,
        &mut session_name,
        &mut false,
        Some(SessionId::new(1)),
        None,
        None,
        (100, 30),
        &mut predict,
        &overlay,
        None,
        &mut pending_splits,
        &mut pending_windows,
        &mut HashSet::new(),
        &mut AgentMetaIndex::default(),
        false,
        false,
    )
}

#[test]
fn attach_participants_and_agent_sessions_split_a_mixed_kind_snapshot() {
    let pane = tid(1);
    let agent = tid(2);
    let snapshot = mixed_kind_snapshot(&pane, &agent);
    let participants = attach_participants(&snapshot);
    assert_eq!(
        participants,
        vec![pane],
        "only the Terminal-kind resource is a barrier participant"
    );
    let children: Vec<&ResourceId> = attach_agent_sessions(&snapshot, &participants)
        .into_iter()
        .map(|info| &info.id)
        .collect();
    assert_eq!(children, vec![&agent]);
    assert!(
        attach_agent_sessions(&snapshot, &[]).is_empty(),
        "a child whose parent is not attached is not declared"
    );
}

#[test]
fn attached_seeds_a_slot_only_for_the_terminal_and_declares_the_agent_session() {
    let pane = tid(1);
    let agent = tid(2);
    let mut fixture = kind_fixture();
    let mut panes = HashMap::new();
    let mut workspace = Workspace::default();
    let mut focused = None;

    let outcome = drive_kind_frame(
        &mut fixture,
        FrameKind::Attached {
            attach_id: 1,
            snapshot: mixed_kind_snapshot(&pane, &agent),
            initial_client_id: ClientId::new(1),
        },
        &mut panes,
        &mut workspace,
        &mut focused,
    )
    .expect("attached");

    assert!(panes.contains_key(&pane));
    assert!(
        !panes.contains_key(&agent),
        "an AgentSession never gets a pane slot (it would be sized from 0x0)"
    );
    assert_eq!(panes.len(), 1);
    assert_eq!(
        fixture.kernel.resource_kind(&agent),
        Some(ResourceKind::AgentSession)
    );
    let view = fixture.kernel.agent_session(&agent).expect("declared");
    assert_eq!(view.parent, Some(&pane));
    assert_eq!(view.state.provider.as_deref(), Some("claude"));
    assert_eq!(
        view.state.status,
        phux_client_core::session::agent_stream::AgentSessionStatus::Working
    );
    assert!(
        outcome.pane_cwds.iter().all(|(id, _)| id != &agent),
        "no cwd for a resource without one"
    );
    assert_eq!(
        crate::layout::leaves(workspace.active_window().unwrap().tree.as_ref().unwrap()),
        vec![pane],
        "the layout holds the pane alone"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one stream lifecycle through the dispatcher: BEGIN, CHUNK, READY, live"
)]
fn an_agent_stream_bootstraps_without_a_slot_and_dirties_the_chrome() {
    let pane = tid(1);
    let agent = tid(2);
    let mut fixture = kind_fixture();
    let mut panes = HashMap::new();
    let mut workspace = Workspace::default();
    let mut focused = None;
    drive_kind_frame(
        &mut fixture,
        FrameKind::Attached {
            attach_id: 1,
            snapshot: mixed_kind_snapshot(&pane, &agent),
            initial_client_id: ClientId::new(1),
        },
        &mut panes,
        &mut workspace,
        &mut focused,
    )
    .expect("attached");

    let stream_id = stream();
    let bootstrap_id = bootstrap();
    let begin = drive_kind_frame(
        &mut fixture,
        FrameKind::BootstrapBegin {
            terminal_id: agent.clone(),
            stream_id,
            bootstrap_id,
            profile: phux_protocol::BootstrapStreamProfile::AgentEventsJsonlV1,
            cols: 0,
            rows: 0,
            base_seq: 0,
        },
        &mut panes,
        &mut workspace,
        &mut focused,
    )
    .expect("agent BEGIN at 0x0 is legal");
    assert!(!begin.chrome_dirty);
    assert!(
        !panes.contains_key(&agent),
        "BEGIN seeds no slot for a stream"
    );

    let chunk = "{\"seq\":1,\"ts_ms\":1,\"type\":\"ask\",\"data\":{}}\n";
    drive_kind_frame(
        &mut fixture,
        FrameKind::BootstrapChunk {
            terminal_id: agent.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: bytes::Bytes::from(chunk.as_bytes().to_vec()),
        },
        &mut panes,
        &mut workspace,
        &mut focused,
    )
    .expect("chunk");
    let ready = drive_kind_frame(
        &mut fixture,
        FrameKind::BootstrapReady {
            terminal_id: agent.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
        &mut panes,
        &mut workspace,
        &mut focused,
    )
    .expect("ready");
    assert!(
        ready.chrome_dirty,
        "the published log changes what the sidebar shows"
    );
    assert!(
        !ready.layout_replaced,
        "no pane repaint for a record stream"
    );

    let live = "{\"seq\":2,\"ts_ms\":2,\"type\":\"stop\",\"data\":{}}\n";
    let output = drive_kind_frame(
        &mut fixture,
        FrameKind::ResourceOutput {
            terminal_id: agent.clone(),
            stream_id,
            bootstrap_id,
            seq: 1,
            bytes: bytes::Bytes::from(live.as_bytes().to_vec()),
        },
        &mut panes,
        &mut workspace,
        &mut focused,
    )
    .expect("live records");
    assert!(output.chrome_dirty);
    assert!(
        !panes.contains_key(&agent),
        "live records never seed a slot either"
    );

    let rows = crate::attach::agent_rows::agent_session_rows(&fixture.kernel);
    let under_pane = rows.get(&pane).expect("row under the parent pane");
    assert_eq!(under_pane.len(), 1);
    assert_eq!(under_pane[0].id, agent);
    assert_eq!(under_pane[0].provider.as_deref(), Some("claude"));
    assert_eq!(
        under_pane[0].state,
        phux_client::agent_meta::AgentMetaState::Done
    );
}

#[test]
fn closing_an_agent_session_removes_only_its_row() {
    let pane = tid(1);
    let agent = tid(2);
    let mut fixture = kind_fixture();
    let mut panes = HashMap::new();
    let mut workspace = Workspace::default();
    let mut focused = None;
    drive_kind_frame(
        &mut fixture,
        FrameKind::Attached {
            attach_id: 1,
            snapshot: mixed_kind_snapshot(&pane, &agent),
            initial_client_id: ClientId::new(1),
        },
        &mut panes,
        &mut workspace,
        &mut focused,
    )
    .expect("attached");
    let before = workspace.clone();

    let outcome = drive_kind_frame(
        &mut fixture,
        FrameKind::ResourceClosed {
            terminal_id: agent.clone(),
            exit_status: None,
            reason: CloseReason::ParentClosed,
        },
        &mut panes,
        &mut workspace,
        &mut focused,
    )
    .expect("child close");

    assert!(!outcome.exit, "a child closing never ends the attach");
    assert!(outcome.chrome_dirty, "the row leaves the strip");
    assert!(!outcome.layout_replaced && !outcome.emit_set_metadata && !outcome.reflow_panes);
    assert!(
        outcome.notices.is_empty(),
        "no pane-exit notice for a stream"
    );
    assert_eq!(workspace, before, "the parent's layout is untouched");
    assert!(panes.contains_key(&pane));
    assert!(fixture.kernel.agent_session(&agent).is_none());
    assert!(
        crate::attach::agent_rows::agent_session_rows(&fixture.kernel).is_empty(),
        "no row survives the close"
    );
}

#[test]
fn a_layout_naming_an_agent_session_is_refused() {
    let pane = tid(1);
    let agent = tid(2);
    let mut fixture = kind_fixture();
    let mut panes = HashMap::new();
    let mut workspace = Workspace::default();
    let mut focused = None;
    drive_kind_frame(
        &mut fixture,
        FrameKind::Attached {
            attach_id: 1,
            snapshot: mixed_kind_snapshot(&pane, &agent),
            initial_client_id: ClientId::new(1),
        },
        &mut panes,
        &mut workspace,
        &mut focused,
    )
    .expect("attached");

    // A stale or foreign writer tiles the agent session as if it were a pane.
    let mut bad = Workspace::single(pane.clone());
    bad.windows[0].state = split2(1, 2, 1);
    let bytes = bad.encode_cbor().expect("encode");
    let outcome = drive_kind_frame(
        &mut fixture,
        FrameKind::MetadataChanged {
            scope: phux_protocol::wire::frame::Scope::Group(super::DEFAULT_GROUP_ID),
            key: phux_client::layout_ops::layout_key(SessionId::new(1)),
            value: Some(bytes),
        },
        &mut panes,
        &mut workspace,
        &mut focused,
    )
    .expect_err("non-terminal layout must be explicitly refused");
    assert!(outcome.to_string().contains("is not a terminal resource"));
    assert_eq!(
        crate::layout::leaves(workspace.active_window().unwrap().tree.as_ref().unwrap()),
        vec![pane],
    );
}

#[test]
fn a_live_spawned_agent_session_is_declared_and_attached_as_a_stream() {
    let pane = tid(1);
    let agent = tid(2);
    let late = tid(3);
    let mut fixture = kind_fixture();
    let mut panes = HashMap::new();
    let mut workspace = Workspace::default();
    let mut focused = None;
    drive_kind_frame(
        &mut fixture,
        FrameKind::Attached {
            attach_id: 1,
            snapshot: mixed_kind_snapshot(&pane, &agent),
            initial_client_id: ClientId::new(1),
        },
        &mut panes,
        &mut workspace,
        &mut focused,
    )
    .expect("attached");

    let outcome = drive_kind_frame(
        &mut fixture,
        FrameKind::Event {
            terminal: Some(late.clone()),
            event: phux_protocol::wire::frame::AgentEvent::ResourceSpawned {
                kind: ResourceKind::AgentSession,
                parent: Some(pane.clone()),
            },
        },
        &mut panes,
        &mut workspace,
        &mut focused,
    )
    .expect("spawn event");
    assert_eq!(
        outcome.attach_panes,
        vec![late.clone()],
        "attach the stream"
    );
    assert!(outcome.chrome_dirty);
    assert!(
        !outcome.foreign_pane_set_dirty,
        "our own child is not a peer change"
    );
    assert!(
        !panes.contains_key(&late),
        "still no pane slot for a stream"
    );
    let view = fixture.kernel.agent_session(&late).expect("declared");
    assert_eq!(view.parent, Some(&pane));

    // A child of a pane this client does not hold is a peer's business.
    let outcome = drive_kind_frame(
        &mut fixture,
        FrameKind::Event {
            terminal: Some(tid(4)),
            event: phux_protocol::wire::frame::AgentEvent::ResourceSpawned {
                kind: ResourceKind::AgentSession,
                parent: Some(tid(99)),
            },
        },
        &mut panes,
        &mut workspace,
        &mut focused,
    )
    .expect("peer spawn event");
    assert!(outcome.attach_panes.is_empty());
    assert!(outcome.foreign_pane_set_dirty);
    assert!(fixture.kernel.agent_session(&tid(4)).is_none());
}
