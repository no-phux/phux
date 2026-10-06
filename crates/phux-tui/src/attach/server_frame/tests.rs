//! Frame-handler tests: engine routing and barriers, layout metadata
//! reconciliation, output/snapshot paint policy, lifecycle events, and
//! close/detach endings.
#![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

use super::{
    FrameEnv, FrameOutcome, attach_agent_sessions, attach_participants, handle_server_frame,
    route_engine_frame,
};

use phux_client_core::session::EffectBuffer;
use phux_protocol::ResourceKind;
use phux_protocol::ids::{ClientId, ResourceId, SatelliteHost, SessionId, WindowId};
use phux_protocol::wire::frame::{
    AgentEvent, CloseReason, CommandResult, DetachReason, ErrorCode, FrameKind, Scope, SpawnResult,
};
use phux_protocol::wire::info::{
    AgentFacet, ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo,
};

use crate::layout::{LayoutNode, SplitDir};

use crate::attach::actions::{
    Adopt, ParkedAdopt, PendingSplit, PendingWindow, SpawnedPane, SplitHost,
};
use crate::attach::outcome::{AttachEnd, AttachError};
use crate::attach::pane_state::{AttachKernel, PaneSlot};
use crate::attach::render::ReplicaWalk;
use crate::attach::session_mirror::SessionMirror;
use crate::layout::{LayoutState, WindowState, Workspace};
use crate::predict::{PredictionState, PredictiveConfig};
use crate::render::chrome::status_bar::NoticeSeverity;

// ---- fixtures --------------------------------------------------------------

fn tid(id: u32) -> ResourceId {
    ResourceId::local(id)
}

/// `edge/@9`, the satellite pane the spawn/adopt tests use.
fn edge_pane() -> ResourceId {
    ResourceId::satellite(SatelliteHost::new("edge"), 9)
}

/// The instance token the satellite binds its spawns to.
fn edge_token() -> phux_protocol::ids::ServerInstance {
    phux_protocol::ids::ServerInstance::new([3; 16])
}

fn stream() -> phux_protocol::StreamId {
    phux_protocol::StreamId::new(1).expect("stream")
}

fn bootstrap() -> phux_protocol::BootstrapId {
    phux_protocol::BootstrapId::new(1).expect("bootstrap")
}

fn kernel() -> AttachKernel {
    phux_client_core::session::SessionKernel::new(
        phux_client_core::engine::ghostty::GhosttyAdapter::new(
            phux_protocol::BootstrapLimits::default(),
        ),
        phux_protocol::BootstrapProfile::SynthesizedVtRaw,
    )
}

fn attach_started(
    kernel: &mut AttachKernel,
    effects: &mut EffectBuffer,
    id: u32,
    ids: &[ResourceId],
) {
    kernel
        .update(
            phux_client_core::session::KernelInput::AttachStarted {
                attach_id: id,
                terminals: ids,
            },
            effects,
        )
        .expect("attach");
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

fn chunk_frame(terminal_id: &ResourceId, payload: &'static [u8]) -> FrameKind {
    FrameKind::BootstrapChunk {
        terminal_id: terminal_id.clone(),
        stream_id: stream(),
        bootstrap_id: bootstrap(),
        chunk_seq: 0,
        payload: bytes::Bytes::from_static(payload),
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

fn output_frame(terminal_id: &ResourceId, seq: u64, bytes: &[u8]) -> FrameKind {
    FrameKind::ResourceOutput {
        terminal_id: terminal_id.clone(),
        stream_id: stream(),
        bootstrap_id: bootstrap(),
        seq,
        bytes: bytes::Bytes::copy_from_slice(bytes),
    }
}

fn history_page(terminal_id: &ResourceId, cursor: &'static [u8]) -> FrameKind {
    FrameKind::HistoryPage {
        terminal_id: terminal_id.clone(),
        stream_id: stream(),
        bootstrap_id: bootstrap(),
        rows: 1,
        page_seq: 1,
        cursor: bytes::Bytes::from_static(cursor),
        next_cursor: None,
        payload: bytes::Bytes::from_static(b"malformed-history"),
    }
}

fn layout_changed(session: u32, value: Option<Vec<u8>>) -> FrameKind {
    meta_changed(
        Scope::Group(super::DEFAULT_GROUP_ID),
        &phux_client::layout_ops::layout_key(SessionId::new(session)),
        value,
    )
}

fn meta_changed(scope: Scope, key: &str, value: Option<Vec<u8>>) -> FrameKind {
    FrameKind::MetadataChanged {
        scope,
        key: key.to_owned(),
        value,
        actor: None,
    }
}

fn closed(terminal_id: &ResourceId, exit_status: Option<i32>) -> FrameKind {
    FrameKind::ResourceClosed {
        terminal_id: terminal_id.clone(),
        exit_status,
        reason: CloseReason::Unknown,
        signal: None,
    }
}

fn event(terminal_id: &ResourceId, event: AgentEvent) -> FrameKind {
    FrameKind::Event {
        terminal: Some(terminal_id.clone()),
        event,
        stamp: None,
    }
}

fn asked() -> AgentEvent {
    AgentEvent::Asked {
        id: "q1".to_owned(),
        question: "deploy to prod?".to_owned(),
        suggestions: vec!["yes".to_owned(), "no".to_owned()],
        elapsed_seconds: None,
    }
}

fn command_ok(request_id: u32) -> FrameKind {
    FrameKind::CommandResult {
        request_id,
        result: CommandResult::Ok,
    }
}

fn command_err(request_id: u32, code: ErrorCode, message: &str) -> FrameKind {
    FrameKind::CommandResult {
        request_id,
        result: CommandResult::Error {
            code,
            message: message.to_owned(),
        },
    }
}

fn unreachable_refusal(request_id: u32) -> FrameKind {
    command_err(
        request_id,
        ErrorCode::SatelliteUnreachable,
        "satellite edge is unreachable: link is down",
    )
}

/// A refusal of request 9 whose code says the satellite answered.
fn reachable_refusal() -> FrameKind {
    command_err(9, ErrorCode::TerminalNotFound, "no such terminal")
}

fn attached(snapshot: SessionSnapshot, client: u32) -> FrameKind {
    FrameKind::Attached {
        attach_id: 1,
        snapshot,
        initial_client_id: ClientId::new(client),
    }
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

/// A single-window workspace wrapping `state`.
fn ws1(state: LayoutState) -> Workspace {
    Workspace {
        windows: vec![WindowState::new("1".to_owned(), state)],
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

/// Pane 1 split with `other` side by side, focused on `focus`.
fn beside(workspace: &mut Workspace, other: &ResourceId) {
    let tree = workspace
        .active_window()
        .and_then(|w| w.tree.clone())
        .expect("tree");
    workspace.active_window_mut().expect("window").tree = Some(
        crate::layout::split_at(&tree, &tid(1), other, SplitDir::Horizontal, 0.5).expect("split"),
    );
}

/// Strip CSI sequences so a content assertion cannot match control bytes.
fn strip_csi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
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

/// The frame dispatcher's rig: the [`SessionMirror`] it folds into, the sink
/// it paints to, and the per-frame [`FrameEnv`] knobs a test varies.
struct H {
    mirror: SessionMirror,
    out: Vec<u8>,
    viewport: (u16, u16),
    layout_request: Option<u32>,
    defer_paint: bool,
}

impl H {
    /// An empty client: no panes, no workspace, a fresh kernel.
    fn new() -> Self {
        Self {
            mirror: SessionMirror::new(
                kernel(),
                PredictionState::new(PredictiveConfig::disabled(), 80, 24),
            ),
            out: Vec::new(),
            viewport: (80, 24),
            layout_request: None,
            defer_paint: false,
        }
    }

    /// A client on `ws`, focused on its active window's focus, with a warm
    /// slot for every pane in `slots`.
    fn on(ws: Workspace, slots: &[&ResourceId]) -> Self {
        let mut h = Self::new();
        h.mirror.focused_resource = ws.active_window().and_then(|w| w.focus.clone());
        h.mirror.workspace = ws;
        for id in slots {
            h.mirror
                .panes
                .insert((*id).clone(), PaneSlot::new().expect("pane slot"));
        }
        h
    }

    /// A client on `ws` whose panes are published replicas of `entries`.
    fn published(ws: Workspace, entries: &[(&ResourceId, u16, u16, &[u8])]) -> Self {
        let (kernel, effects, panes) = crate::attach::pane_state::published_test_state(entries);
        let mut h = Self::on(ws, &[]);
        h.mirror.engine_kernel = kernel;
        h.mirror.kernel_effects = effects;
        h.mirror.panes = panes;
        h
    }

    fn with_viewport(mut self, viewport: (u16, u16)) -> Self {
        self.viewport = viewport;
        self.mirror.predict =
            PredictionState::new(PredictiveConfig::disabled(), viewport.0, viewport.1);
        self
    }

    fn try_send(&mut self, frame: FrameKind) -> Result<FrameOutcome, AttachError> {
        let env = FrameEnv {
            // These fixtures are session 1: a `layout/v1/1` broadcast is ours.
            focused_session: Some(SessionId::new(1)),
            viewport_dims: self.viewport,
            pending_layout_request: self.layout_request,
            defer_paint: self.defer_paint,
            ..FrameEnv::default()
        };
        handle_server_frame(&mut self.mirror, env, &mut self.out, frame)
    }

    fn send(&mut self, frame: FrameKind) -> FrameOutcome {
        self.try_send(frame).expect("handle_server_frame")
    }

    fn next_seq(&self, id: &ResourceId) -> u64 {
        self.mirror
            .engine_kernel
            .published(id)
            .expect("published terminal")
            .last_seq()
            .checked_add(1)
            .expect("live sequence")
    }

    /// Live output at the pane's next sequence.
    fn output(&mut self, id: &ResourceId, bytes: &[u8]) -> FrameOutcome {
        let seq = self.next_seq(id);
        self.send(output_frame(id, seq, bytes))
    }

    /// A replacement bootstrap (BEGIN/CHUNK/READY) of `bytes` at `cols x
    /// rows`, painting the full frame when it replaced the layout.
    fn snapshot(&mut self, id: &ResourceId, cols: u16, rows: u16, bytes: &[u8]) -> FrameOutcome {
        let published = self
            .mirror
            .engine_kernel
            .published(id)
            .expect("published generation");
        let stream_id = published.key().stream_id;
        let bootstrap_id = phux_protocol::BootstrapId::new(
            published
                .key()
                .bootstrap_id
                .get()
                .checked_add(1)
                .expect("id"),
        )
        .expect("next bootstrap");
        let base_seq = published.last_seq();
        self.send(FrameKind::BootstrapBegin {
            terminal_id: id.clone(),
            stream_id,
            bootstrap_id,
            profile: phux_protocol::BootstrapStreamProfile::SynthesizedVtRaw,
            cols,
            rows,
            base_seq,
        });
        self.send(FrameKind::BootstrapChunk {
            terminal_id: id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: bytes::Bytes::copy_from_slice(bytes),
        });
        let outcome = self.send(FrameKind::BootstrapReady {
            terminal_id: id.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        });
        if outcome.layout_replaced
            && let Some(active) = self
                .mirror
                .workspace
                .render_window(self.mirror.zoomed.as_ref())
        {
            let theme = crate::render::theme::Theme::default();
            let mut chrome = crate::attach::chrome_ctx::ChromeCtx {
                viewport: self.viewport,
                sidebar: None,
                status_bar: None,
                sidebar_painter: None,
                session_name: &self.mirror.session_name,
                theme: &theme,
            };
            crate::attach::paint::paint_full_frame(
                &mut self.out,
                active.as_ref(),
                &mut self.mirror.panes,
                &self.mirror.engine_kernel,
                self.mirror.focused_resource.as_ref(),
                &mut chrome,
            );
        }
        outcome
    }

    fn leaves(&self) -> Vec<ResourceId> {
        self.mirror
            .workspace
            .active_window()
            .and_then(|w| w.tree.as_ref())
            .map(crate::layout::leaves)
            .unwrap_or_default()
    }

    fn belled(&self) -> bool {
        self.out.contains(&0x07)
    }

    fn out_str(&self) -> String {
        String::from_utf8_lossy(&self.out).into_owned()
    }

    /// The first grapheme of `id`'s published replica at (`row`, `col`).
    fn cell(&mut self, id: &ResourceId, row: u16, col: u16) -> Option<char> {
        let terminal =
            crate::attach::pane_state::published_terminal(&self.mirror.engine_kernel, id)
                .expect("published terminal");
        self.mirror
            .panes
            .get_mut(id)
            .expect("slot")
            .renderer
            .read_grapheme_at(ReplicaWalk::for_test(terminal), row, col)
            .expect("read cell")
    }
}

// ---- engine routing and the attach barrier ---------------------------------

/// The attach participant set is the focused session's panes only: the
/// server bootstraps nothing else, so counting other sessions' panes left
/// `ATTACH_READY` rejected whenever a second session existed.
#[test]
fn attach_participants_cover_only_the_focused_session() {
    let (focused, other) = (SessionId::new(1), SessionId::new(2));
    let (focused_window, other_window) = (WindowId::new(10), WindowId::new(20));
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
            ResourceInfo::new(ResourceId::new(200), other_window, 80, 24),
        ]);
    assert_eq!(
        attach_participants(&snapshot),
        vec![ResourceId::new(100), ResourceId::new(101)]
    );
}

#[test]
fn engine_damage_obeys_attach_barrier_and_ready_publication() {
    let id = tid(90);
    let (mut kernel, mut effects) = (kernel(), EffectBuffer::new());
    attach_started(&mut kernel, &mut effects, 7, std::slice::from_ref(&id));
    let mut route = |frame: FrameKind| route_engine_frame(&frame, &mut kernel, &mut effects);
    assert!(route(begin_frame(&id)).damaged.is_empty());
    assert!(route(chunk_frame(&id, b"seed")).damaged.is_empty());
    assert!(
        route(ready_frame(&id)).damaged.is_empty(),
        "publication damage stays behind ATTACH_READY"
    );
    assert!(
        route(output_frame(&id, 1, b"before-barrier"))
            .damaged
            .is_empty(),
        "pre-barrier live output must not paint directly"
    );
    assert!(route(FrameKind::AttachReady { attach_id: 7 }).damaged(&id));
    assert!(route(output_frame(&id, 2, b"after-barrier")).damaged(&id));
}

#[test]
fn ready_history_cursor_is_preserved_into_kernel_request() {
    use phux_protocol::wire::frame::{HistoryRejectionReason, HistoryTombstoneReason};
    let id = tid(91);
    let (mut kernel, mut effects) = (kernel(), EffectBuffer::new());
    attach_started(&mut kernel, &mut effects, 8, std::slice::from_ref(&id));
    let cursor = bytes::Bytes::from_static(b"opaque-cursor");
    let mut route = |frame: FrameKind| route_engine_frame(&frame, &mut kernel, &mut effects);
    route(begin_frame(&id));
    route(chunk_frame(&id, b"seed"));
    let routed = route(FrameKind::BootstrapReady {
        terminal_id: id.clone(),
        stream_id: stream(),
        bootstrap_id: bootstrap(),
        history_cursor: Some(cursor.clone()),
    });
    let request = |rows| {
        Some((
            id.clone(),
            stream(),
            bootstrap(),
            cursor.clone(),
            1024 * 1024,
            rows,
        ))
    };
    assert_eq!(routed.history_request, request(1024));
    let rejected = route(FrameKind::HistoryRejected {
        terminal_id: id.clone(),
        stream_id: stream(),
        bootstrap_id: bootstrap(),
        cursor: cursor.clone(),
        reason: HistoryRejectionReason::TooSmall,
        required_bytes: 1024 * 1024,
        required_rows: 2048,
    });
    assert!(!rejected.resync_required);
    assert_eq!(
        rejected.history_request,
        request(2048),
        "a valid larger row requirement retries within the client hard cap"
    );
    let tombstoned = route(FrameKind::HistoryTombstone {
        terminal_id: id.clone(),
        stream_id: stream(),
        bootstrap_id: bootstrap(),
        cursor: cursor.clone(),
        reason: HistoryTombstoneReason::Pruned,
    });
    assert!(!tombstoned.resync_required);
    assert!(
        kernel.published_engine(&id).is_some(),
        "history-only invalidation preserves the live replica"
    );
}

#[test]
fn off_window_ready_waits_for_every_snapshot_pane_and_attach_ready() {
    let (focused, off_window) = (tid(94), tid(95));
    let (focused_window, other_window) = (WindowId::new(70), WindowId::new(71));
    let session = SessionId::new(72);
    // Both windows belong to the attached session, so both panes participate.
    let snapshot = SessionSnapshot::new(session, focused_window, focused.clone())
        .with_windows(vec![
            WindowInfo::new(focused_window, session, "w0".to_owned()),
            WindowInfo::new(other_window, session, "w1".to_owned()),
        ])
        .with_resources(vec![
            ResourceInfo::new(focused.clone(), focused_window, 80, 24),
            ResourceInfo::new(off_window.clone(), other_window, 80, 24),
        ]);
    let (mut kernel, mut effects) = (kernel(), EffectBuffer::new());
    let mut route = |frame: FrameKind| route_engine_frame(&frame, &mut kernel, &mut effects);
    let mut attach = attached(snapshot, 1);
    if let FrameKind::Attached { attach_id, .. } = &mut attach {
        *attach_id = 9;
    }
    assert!(route(attach).damaged.is_empty());
    for id in [&off_window, &focused] {
        assert!(route(begin_frame(id)).damaged.is_empty());
        assert!(route(chunk_frame(id, b"seed")).damaged.is_empty());
        assert!(
            route(ready_frame(id)).damaged.is_empty(),
            "no READY may escape the aggregate barrier"
        );
    }
    let released = route(FrameKind::AttachReady { attach_id: 9 });
    assert!(released.damaged(&focused) && released.damaged(&off_window));
}

#[test]
fn bootstrap_ready_surfaces_publication_damage_without_attach_barrier() {
    let id = tid(91);
    let (mut kernel, mut effects) = (kernel(), EffectBuffer::new());
    route_engine_frame(&begin_frame(&id), &mut kernel, &mut effects);
    route_engine_frame(&chunk_frame(&id, b"seed"), &mut kernel, &mut effects);
    assert!(route_engine_frame(&ready_frame(&id), &mut kernel, &mut effects).damaged(&id));
}

/// A zero sequence is not a live-output sentinel: the first live payload of a
/// generation at `base_seq == 0` carries sequence 1.
#[test]
fn terminal_output_seq_zero_is_rejected() {
    let pane = tid(1);
    let mut h = H::published(Workspace::single(pane.clone()), &[(&pane, 80, 24, b"")]);
    let route = route_engine_frame(
        &output_frame(&pane, 0, b"hi"),
        &mut h.mirror.engine_kernel,
        &mut h.mirror.kernel_effects,
    );
    assert!(route.failed.is_some());
    assert_eq!(route.ack, None);
}

#[test]
fn pre_barrier_output_refreshes_title_cache_before_attach_ready() {
    let (ready, pending) = (tid(92), tid(93));
    let mut h = H::new();
    attach_started(
        &mut h.mirror.engine_kernel,
        &mut h.mirror.kernel_effects,
        8,
        &[ready.clone(), pending.clone()],
    );
    h.send(begin_frame(&ready));
    h.send(chunk_frame(&ready, b"\x1b]2;shell\x07"));
    h.send(ready_frame(&ready));
    assert_eq!(h.mirror.panes[&ready].last_title, "shell");
    h.send(begin_frame(&pending));

    let pre_barrier = h.send(output_frame(&ready, 1, b"\x1b]2;vim\x07"));
    assert!(!pre_barrier.chrome_dirty);
    assert_eq!(
        h.mirror.panes[&ready].last_title, "vim",
        "damage suppression must not suppress engine-derived metadata refresh"
    );

    h.send(chunk_frame(&pending, b"pending"));
    h.send(ready_frame(&pending));
    assert!(
        h.send(FrameKind::AttachReady { attach_id: 8 })
            .layout_replaced
    );
    assert_eq!(h.mirror.panes[&ready].last_title, "vim");
}

#[test]
fn malformed_history_tombstones_only_history_and_replacement_publishes_atomically() {
    let id = tid(96);
    let replacement = phux_protocol::BootstrapId::new(2).expect("replacement");
    let mut h = H::new();
    h.send(begin_frame(&id));
    h.send(chunk_frame(&id, b"\x1b]2;old\x07"));
    h.send(ready_frame(&id));

    assert!(!h.mirror.panes[&id].history_degraded);
    let rejected = h.send(history_page(&id, b"cursor"));
    assert!(!rejected.resync_required);
    assert!(rejected.chrome_dirty && h.mirror.panes[&id].history_degraded);
    assert!(
        h.mirror
            .kernel_effects
            .as_slice()
            .iter()
            .any(|effect| matches!(
                effect,
                phux_client_core::session::KernelEffect::Status(
                    phux_client_core::session::KernelStatus::HistoryUnavailable { .. }
                )
            ))
    );
    assert_eq!(
        h.mirror
            .engine_kernel
            .history_cache(&id)
            .expect("history")
            .status()
            .state,
        phux_client_core::history::HistoryLoadState::Tombstoned
    );
    h.send(output_frame(&id, 1, b"\x1b]2;old-live\x07"));
    let title = |h: &H| {
        h.mirror
            .engine_kernel
            .published_engine(&id)
            .unwrap()
            .terminal()
            .unwrap()
            .title()
            .unwrap()
            .to_owned()
    };
    assert_eq!(
        title(&h),
        "old-live",
        "history failure must not stop live output"
    );
    assert!(!h.send(history_page(&id, b"stale")).resync_required);
    assert!(h.mirror.panes[&id].history_degraded, "the mark persists");

    attach_started(
        &mut h.mirror.engine_kernel,
        &mut h.mirror.kernel_effects,
        10,
        std::slice::from_ref(&id),
    );
    h.send(FrameKind::BootstrapBegin {
        terminal_id: id.clone(),
        stream_id: stream(),
        bootstrap_id: replacement,
        profile: phux_protocol::BootstrapStreamProfile::SynthesizedVtRaw,
        cols: 80,
        rows: 24,
        base_seq: 0,
    });
    h.send(FrameKind::BootstrapChunk {
        terminal_id: id.clone(),
        stream_id: stream(),
        bootstrap_id: replacement,
        chunk_seq: 0,
        payload: bytes::Bytes::from_static(b"\x1b]2;new\x07"),
    });
    assert_eq!(
        title(&h),
        "old-live",
        "replacement remains staged until READY"
    );
    let ready = h.send(FrameKind::BootstrapReady {
        terminal_id: id.clone(),
        stream_id: stream(),
        bootstrap_id: replacement,
        history_cursor: None,
    });
    // A fresh replica has fresh history: the mark clears and the badge repaints.
    assert!(!ready.layout_replaced && ready.chrome_dirty);
    assert!(!h.mirror.panes[&id].history_degraded);
    assert_eq!(h.mirror.panes[&id].last_title, "new");
    assert!(
        h.send(FrameKind::AttachReady { attach_id: 10 })
            .layout_replaced
    );
    assert_eq!(h.mirror.panes[&id].last_title, "new");
}

/// Per-pane scrollback loss reaches the status bar, naming the pane.
#[test]
fn history_unavailable_status_names_the_pane_in_a_warn_notice() {
    let id = tid(3);
    let mut h = H::new();
    h.send(begin_frame(&id));
    h.send(ready_frame(&id));
    let outcome = h.send(history_page(&id, b"cursor"));
    assert_eq!(outcome.notices.len(), 1);
    assert_eq!(outcome.notices[0].severity, NoticeSeverity::Warn);
    assert_eq!(
        outcome.notices[0].text,
        "pane 3: scrollback unavailable (CodecFailure)"
    );
}

/// A server-pruned history boundary marks the pane past the notice's TTL; a
/// loading cache before it leaves the pane unmarked.
#[test]
fn history_tombstone_marks_the_pane_degraded() {
    let id = tid(5);
    let mut h = H::new();
    h.send(begin_frame(&id));
    let ready = h.send(FrameKind::BootstrapReady {
        terminal_id: id.clone(),
        stream_id: stream(),
        bootstrap_id: bootstrap(),
        history_cursor: Some(bytes::Bytes::from_static(b"c0")),
    });
    assert!(
        ready.history_request.is_some(),
        "READY starts a history fetch"
    );
    assert!(
        !h.mirror.panes[&id].history_degraded,
        "a loading cache is healthy"
    );
    assert!(!ready.chrome_dirty);

    let pruned = h.send(FrameKind::HistoryTombstone {
        terminal_id: id.clone(),
        stream_id: stream(),
        bootstrap_id: bootstrap(),
        cursor: bytes::Bytes::from_static(b"c0"),
        reason: phux_protocol::wire::frame::HistoryTombstoneReason::Pruned,
    });
    assert_eq!(pruned.notices.len(), 1, "the transient notice still fires");
    assert!(pruned.chrome_dirty);
    assert!(h.mirror.panes[&id].history_degraded);

    // A repeated tombstone for the advanced cursor is ignored, not a flip.
    let repeat = h.send(FrameKind::HistoryTombstone {
        terminal_id: id.clone(),
        stream_id: stream(),
        bootstrap_id: bootstrap(),
        cursor: bytes::Bytes::from_static(b"c0"),
        reason: phux_protocol::wire::frame::HistoryTombstoneReason::Pruned,
    });
    assert!(!repeat.chrome_dirty);
    assert!(h.mirror.panes[&id].history_degraded);
}

// ---- layout reconciliation -------------------------------------------------

#[test]
fn reconcile_accepts_authorized_replacement_and_keeps_trees() {
    let reconcile = super::reconcile_loaded_workspace;
    let mut replacement = ws1(split2(1, 2, 1));
    let local = Workspace::single(tid(9));
    replacement.windows[0].id = local.windows[0].id;
    let out = reconcile(replacement, &local, Some(&tid(9)));
    assert_eq!(
        window_leaves(&out, 0),
        vec![tid(1), tid(2)],
        "session authority does not depend on currently admitted replicas"
    );
    assert_eq!(out.windows[0].state.focus, Some(tid(1)));
    assert_eq!(out.windows[0].id, local.windows[0].id);

    // A tree containing the session pane, or with no focus to validate
    // against, is kept whole.
    let out = reconcile(
        ws1(split2(1, 2, 1)),
        &Workspace::single(tid(1)),
        Some(&tid(1)),
    );
    assert_eq!(window_leaves(&out, 0), vec![tid(1), tid(2)]);
    let out = reconcile(ws1(split2(1, 2, 1)), &Workspace::default(), None);
    assert_eq!(window_leaves(&out, 0).len(), 2);
}

/// Regression: non-active windows must not be aliased onto the focused pane
/// ("open vim in one window, it shows in the other").
#[test]
fn reconcile_multi_window_does_not_alias_non_active_windows() {
    let ws = Workspace {
        windows: vec![
            WindowState::new("1".to_owned(), LayoutState::single(tid(1))),
            WindowState::new("2".to_owned(), LayoutState::single(tid(2))),
        ],
        active: 0,
    };
    let out = super::reconcile_loaded_workspace(ws.clone(), &ws, Some(&tid(1)));
    assert_eq!(out.windows.len(), 2);
    assert_eq!(window_leaves(&out, 0), vec![tid(1)]);
    assert_eq!(window_leaves(&out, 1), vec![tid(2)]);
}

/// A topology update that removes local focus/window state is repaired
/// deterministically rather than adopting the sender's focus.
#[test]
fn reconcile_repairs_missing_local_focus_and_invalid_active_index() {
    let mut local = Workspace::single(tid(1));
    local.add_window("2".to_owned(), tid(2));
    local.add_window("3".to_owned(), tid(9));
    let incoming = Workspace {
        windows: vec![
            WindowState::new("1".to_owned(), split2(1, 4, 4)),
            WindowState::new("2".to_owned(), split2(2, 3, 3)),
        ],
        active: 0,
    };
    let out = super::reconcile_loaded_workspace(incoming, &local, Some(&tid(9)));
    assert_eq!(out.active, 1, "removed local index clamps to last window");
    assert_eq!(out.windows[0].state.focus, Some(tid(1)));
    assert_eq!(out.windows[1].state.focus, Some(tid(2)));
}

#[test]
fn duplicate_hello_ok_is_fatal_in_attached_phase() {
    let mut h = H::on(Workspace::single(tid(1)), &[&tid(1)]);
    let error = h
        .try_send(FrameKind::HelloOk {
            protocol_major: phux_protocol::PROTOCOL_VERSION.major,
            protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
            protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
            server_caps: phux_protocol::caps::ServerCapabilities::new(),
            server_id: Vec::new(),
            selected_profile: phux_protocol::caps::BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: phux_protocol::caps::BootstrapLimits::default(),
        })
        .expect_err("post-negotiation HELLO_OK must terminate the client");
    assert!(matches!(
        error,
        AttachError::Protocol(message) if message.contains("not valid from a server")
    ));
}

/// A `DIRECTORY_LISTING` is handed to the driver, never rejected by the
/// catch-all (that would tear down a healthy attach over a picker reply).
#[test]
fn directory_listing_reply_is_handed_to_the_driver() {
    let mut h = H::on(Workspace::single(tid(1)), &[&tid(1)]);
    let result = Ok(phux_protocol::wire::frame::DirectoryListing {
        path: "/".to_owned(),
        parent: None,
        entries: Vec::new(),
        truncated: false,
    });
    let outcome = h.send(FrameKind::DirectoryListing {
        request_id: 9,
        result: result.clone(),
    });
    assert_eq!(outcome.directory_listing, Some((9, result)));
    assert!(!outcome.exit);
}

/// A `PATH_RESULTS` is handed to the driver, which matches it against the
/// newest in-flight query.
#[test]
fn path_results_reply_reaches_driver_for_generation_matching() {
    let mut h = H::on(Workspace::single(tid(1)), &[&tid(1)]);
    let result = Ok(phux_protocol::wire::frame::PathResults {
        root: "/src".into(),
        parent: Some("/".into()),
        rows: vec![],
        status: phux_protocol::wire::frame::PathStatus::Complete,
    });
    let outcome = h.send(FrameKind::PathResults {
        request_id: 7,
        result: result.clone(),
    });
    assert_eq!(outcome.path_results, Some((7, result)));
    assert!(!outcome.exit);
}

/// A request-correlated reply whose awaiter already moved on arrives here as
/// interleaved traffic and must be inert, not fatal (L1 §5). `C-a o` during
/// a concurrent layout drive once tore the client down over a success reply.
#[test]
fn raced_request_correlated_replies_are_inert_not_fatal() {
    let pane = tid(1);
    for frame in [
        command_ok(4),
        FrameKind::ResourceMoved {
            request_id: 7,
            result: phux_protocol::wire::frame::MoveResult::Ok(pane.clone()),
        },
    ] {
        let mut h = H::on(Workspace::single(pane.clone()), &[&pane]);
        let before = h.mirror.workspace.clone();
        let outcome = h
            .try_send(frame.clone())
            .unwrap_or_else(|err| panic!("{frame:?} must not terminate the attach: {err:?}"));
        assert!(!outcome.exit && outcome.exit_reason.is_none(), "{frame:?}");
        assert!(
            !outcome.layout_replaced && !outcome.reflow_panes,
            "{frame:?}"
        );
        assert!(
            !outcome.emit_set_metadata && !outcome.foreign_pane_set_dirty,
            "{frame:?}"
        );
        assert!(
            outcome.attach_panes.is_empty() && outcome.foreign_layout.is_none(),
            "{frame:?}"
        );
        assert_eq!(
            h.mirror.workspace, before,
            "{frame:?} must not mutate the topology"
        );
        assert_eq!(
            h.mirror.focused_resource,
            Some(pane.clone()),
            "{frame:?} must not move focus"
        );
    }
}

/// ADR-0049: a sibling's layout broadcast contributes topology only; its
/// active window and per-window focuses cannot yank this client.
#[test]
fn metadata_changed_preserves_valid_local_window_and_pane_focus() {
    let local = Workspace {
        windows: vec![
            WindowState::new("local-one".to_owned(), split2(1, 2, 2)),
            WindowState::new("local-two".to_owned(), split2(3, 4, 4)),
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
    let mut h = H::on(local, &[&tid(1), &tid(2), &tid(3), &tid(4)]);
    let outcome = h.send(layout_changed(1, Some(sibling.encode_cbor().unwrap())));

    assert!(outcome.layout_replaced);
    assert_eq!(
        h.mirror.workspace.active, 1,
        "sender cannot change the local window"
    );
    assert_eq!(h.mirror.workspace.windows[0].state.focus, Some(tid(2)));
    assert_eq!(h.mirror.workspace.windows[1].state.focus, Some(tid(4)));
    assert_eq!(
        h.mirror.focused_resource,
        Some(tid(4)),
        "driver mirror stays client-local"
    );
    assert_eq!(
        h.mirror.workspace.windows[0].name, "shared-one",
        "names are topology"
    );
    assert!(matches!(
        h.mirror.workspace.windows[1].state.tree,
        Some(LayoutNode::Split { ratio, .. }) if (ratio - 0.7).abs() < f32::EPSILON
    ));
}

#[test]
fn old_layout_schema_refuses_attach_and_broadcast_without_resetting_metadata() {
    let bytes = b"\xa1\x67version\x02".to_vec();
    let mut h = H::on(Workspace::single(tid(1)), &[&tid(1)]);
    h.layout_request = Some(42);
    let before = h.mirror.workspace.clone();
    for frame in [
        FrameKind::MetadataValue {
            request_id: 42,
            value: Some(bytes.clone()),
        },
        layout_changed(1, Some(bytes)),
    ] {
        let error = h
            .try_send(frame)
            .expect_err("unsupported metadata must not seed a write");
        assert!(
            matches!(error, AttachError::Protocol(message) if message.contains("stored metadata was preserved"))
        );
        assert_eq!(h.mirror.workspace, before);
        assert_eq!(h.mirror.focused_resource, Some(tid(1)));
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
    let mut h = H::on(local, &[&tid(1), &tid(2)]);

    let outcome = h.send(layout_changed(
        1,
        Some(incoming.encode_topology_cbor().unwrap()),
    ));
    assert!(outcome.layout_replaced);
    assert_eq!(h.mirror.workspace.active, 0);
    assert_eq!(
        h.mirror.workspace.windows[h.mirror.workspace.active].id,
        active_id
    );
    assert_eq!(h.mirror.focused_resource, Some(tid(2)));

    let outcome = h.send(layout_changed(
        1,
        Some(Workspace::new().encode_topology_cbor().unwrap()),
    ));
    assert!(outcome.layout_replaced);
    assert!(h.mirror.workspace.windows.is_empty());
    assert_eq!(h.mirror.focused_resource, None);
    assert_eq!(
        h.mirror.panes.len(),
        2,
        "presentation removal retains durable replicas"
    );
}

/// A peer's layout broadcast (or tombstone) must not touch the local tree;
/// it is routed out for the roster instead.
#[test]
fn a_peer_layout_broadcast_leaves_the_local_workspace_untouched() {
    let mut h = H::on(ws1(split2(1, 2, 1)), &[&tid(1), &tid(2)]);
    let before = h.mirror.workspace.clone();
    let bytes = ws1(split2(5, 6, 1)).encode_cbor().unwrap();

    let outcome = h.send(layout_changed(2, Some(bytes.clone())));
    assert!(!outcome.layout_replaced);
    assert_eq!(h.mirror.workspace, before);
    assert_eq!(h.mirror.focused_resource, Some(tid(1)));
    assert!(
        outcome.attach_panes.is_empty(),
        "we must not attach a peer's panes"
    );
    assert_eq!(
        outcome.foreign_layout,
        Some((SessionId::new(2), Some(bytes)))
    );

    let outcome = h.send(layout_changed(2, None));
    assert!(!outcome.layout_replaced);
    assert_eq!(
        h.mirror.workspace, before,
        "a peer tombstone is not our reset"
    );
    assert_eq!(outcome.foreign_layout, Some((SessionId::new(2), None)));
}

/// Neither the unscoped legacy key nor a named `--projection` key (ADR-0129)
/// has authority over the TUI's workspace, even for our own session.
#[test]
fn unscoped_and_projection_layout_keys_are_never_adopted() {
    let bytes = ws1(split2(1, 2, 1)).encode_cbor().unwrap();
    for key in [phux_client::layout_ops::LAYOUT_KEY, "myapp.layout/v1/1"] {
        let mut h = H::on(Workspace::single(tid(1)), &[&tid(1), &tid(2)]);
        let before = h.mirror.workspace.clone();
        let outcome = h.send(meta_changed(
            Scope::Group(super::DEFAULT_GROUP_ID),
            key,
            Some(bytes.clone()),
        ));
        assert!(!outcome.layout_replaced, "{key}");
        assert!(outcome.foreign_layout.is_none(), "{key}");
        assert_eq!(h.mirror.workspace, before, "{key}");
    }
}

/// A `phux.agent/v1` push for a pane outside our workspace is a peer's, even
/// with a mirror slot; folding it locally would be evicted on the next sweep.
#[test]
fn a_foreign_agent_record_push_stays_out_of_the_local_index() {
    use phux_protocol::wire::frame::RESOURCE_AGENT_KEY;
    let mut h = H::on(Workspace::single(tid(1)), &[&tid(1), &tid(77)]);
    let record = br#"{"name":"claude","kind":"claude","state":"blocked"}"#.to_vec();
    let outcome = h.send(meta_changed(
        Scope::Resource(tid(77)),
        RESOURCE_AGENT_KEY,
        Some(record),
    ));
    assert!(!outcome.agent_meta_changed);
    assert_eq!(
        outcome.foreign_agent.as_ref().map(|(id, _)| id.clone()),
        Some(tid(77))
    );
}

#[test]
fn session_keyed_replacement_keeps_stable_window_with_all_new_leaves() {
    let local = ws1(split2(1, 2, 2));
    let stable_id = local.windows[0].id;
    let mut replacement = ws1(split2(5, 6, 6));
    replacement.windows[0].id = stable_id;
    let mut h = H::on(local, &[&tid(1), &tid(2)]);
    let outcome = h.send(layout_changed(1, Some(replacement.encode_cbor().unwrap())));
    assert_eq!(h.mirror.workspace.windows[0].id, stable_id);
    assert_eq!(window_leaves(&h.mirror.workspace, 0), vec![tid(5), tid(6)]);
    assert_eq!(h.mirror.focused_resource, Some(tid(5)));
    assert_eq!(outcome.attach_panes, vec![tid(5), tid(6)]);
    assert!(!outcome.emit_set_metadata);
    assert_eq!(h.mirror.panes.len(), 2);
}

#[test]
fn metadata_changed_discovers_peer_added_leaf_without_moving_focus() {
    let local = ws1(split2(1, 2, 1));
    let mut sibling = local.clone();
    let tree = sibling.windows[0].state.tree.as_ref().unwrap();
    sibling.windows[0].state.tree =
        Some(crate::layout::split_at(tree, &tid(2), &tid(3), SplitDir::Vertical, 0.3).unwrap());
    sibling.windows[0].state.focus = Some(tid(3));
    let mut h = H::on(local, &[&tid(1), &tid(2)]);
    let outcome = h.send(layout_changed(1, Some(sibling.encode_cbor().unwrap())));
    assert_eq!(outcome.attach_panes, vec![tid(3)]);
    assert_eq!(h.mirror.focused_resource, Some(tid(1)));
    assert_eq!(h.mirror.workspace.windows[0].state.focus, Some(tid(1)));
    assert_eq!(
        window_leaves(&h.mirror.workspace, 0),
        vec![tid(1), tid(2), tid(3)]
    );
}

/// The persisted-layout reply uses the topology-only merge too: the
/// bootstrap focus wins when it remains a leaf.
#[test]
fn metadata_value_preserves_valid_bootstrap_focus() {
    let mut h = H::on(Workspace::single(tid(2)), &[&tid(1), &tid(2)]);
    h.layout_request = Some(41);
    let outcome = h.send(FrameKind::MetadataValue {
        request_id: 41,
        value: Some(ws1(split2(1, 2, 1)).encode_cbor().unwrap()),
    });
    assert!(outcome.layout_replaced);
    assert_eq!(window_leaves(&h.mirror.workspace, 0), vec![tid(1), tid(2)]);
    assert_eq!(h.mirror.workspace.windows[0].state.focus, Some(tid(2)));
    assert_eq!(h.mirror.focused_resource, Some(tid(2)));
}

/// A layout tombstone resets to a single-pane workspace on the local focus.
#[test]
fn layout_tombstone_resets_to_local_focused_pane() {
    let local = Workspace {
        windows: vec![
            WindowState::new("1".to_owned(), LayoutState::single(tid(1))),
            WindowState::new("2".to_owned(), LayoutState::single(tid(2))),
        ],
        active: 1,
    };
    let mut h = H::on(local, &[&tid(1), &tid(2)]);
    assert!(h.send(layout_changed(1, None)).layout_replaced);
    assert_eq!(h.mirror.workspace, Workspace::single(tid(2)));
    assert_eq!(h.mirror.focused_resource, Some(tid(2)));
}

/// A coalesced local question arriving before the persisted layout is
/// adopted survives that adoption.
#[test]
fn early_local_ask_survives_persisted_layout_adoption() {
    let (first, later) = (tid(1), tid(2));
    let mut h = H::on(Workspace::single(first.clone()), &[&first, &later]);
    h.layout_request = Some(42);
    assert_eq!(
        h.send(event(&later, asked())).foreign_attention,
        Some(later.clone())
    );
    let complete = ws1(split2(1, 2, 1));
    let adopted = h.send(FrameKind::MetadataValue {
        request_id: 42,
        value: Some(complete.encode_cbor().unwrap()),
    });
    assert!(adopted.layout_replaced);
    assert!(
        h.mirror.panes[&later].attention,
        "the server need not repeat Asked"
    );
    let repeated = h.send(event(&later, asked()));
    assert!(!repeated.chrome_dirty);
    assert!(
        repeated.foreign_attention.is_none(),
        "ownership now resolves locally"
    );
}

// ---- output and snapshot paint policy --------------------------------------

/// Live output racing ahead of bootstrap publication is not interpreted
/// against placeholder geometry (CUP past column 80 is the oracle).
#[test]
fn output_before_snapshot_uses_current_viewport_width() {
    let pane = tid(1);
    let mut h = H::published(Workspace::single(pane.clone()), &[(&pane, 120, 30, b"")])
        .with_viewport((120, 30));
    h.send(output_frame(&pane, 1, b"\x1b[1;100HX"));
    let terminal =
        crate::attach::pane_state::published_terminal(&h.mirror.engine_kernel, &pane).unwrap();
    assert_eq!(
        (terminal.cols().unwrap(), terminal.rows().unwrap()),
        (120, 30)
    );
    assert_eq!(h.cell(&pane, 0, 99), Some('X'));
}

#[test]
fn synchronized_output_paints_only_after_end_across_frames() {
    let pane = tid(1);
    let mut h = H::published(Workspace::single(pane.clone()), &[(&pane, 80, 24, b"")]);
    h.output(&pane, b"\x1b[?2026hhalf-drawn");
    assert!(h.out.is_empty(), "begin/body must update only the mirror");
    assert!(h.mirror.panes[&pane].sync_output_since.is_some());
    h.output(&pane, b" frame\x1b[?2026l");
    assert!(h.mirror.panes[&pane].sync_output_since.is_none());
    assert!(strip_csi(&h.out_str()).contains("half-drawn frame"));
}

/// An incremental output paint is ONE synchronized-output block, cursor
/// hidden throughout and placed inside it; the renderer's own nested block
/// must not close the frame's early.
#[test]
fn an_incremental_output_paint_is_one_synchronized_block() {
    let pane = tid(1);
    let mut h = H::published(Workspace::single(pane.clone()), &[(&pane, 80, 24, b"")]);
    h.output(&pane, b"hello frame");
    let s = h.out_str();
    assert_eq!(s.matches("\x1b[?2026h").count(), 1, "{s:?}");
    assert_eq!(s.matches("\x1b[?2026l").count(), 1, "{s:?}");
    assert!(s.starts_with("\x1b[?2026h\x1b[?25l"), "{s:?}");
    assert!(s.ends_with("\x1b[?2026l"), "{s:?}");
    let close = s.rfind("\x1b[?2026l").unwrap();
    let cursor = s
        .rfind("\x1b[?25h")
        .expect("frame ends by showing the cursor");
    assert!(
        cursor < close,
        "the cursor is placed inside the block: {s:?}"
    );
}

/// A deferred frame (coalescing, overlay, sync output, pacer) still applies
/// its bytes to the mirror but paints nothing.
#[test]
fn a_deferred_output_frame_emits_nothing_but_still_applies() {
    let pane = tid(1);
    let mut h = H::published(Workspace::single(pane.clone()), &[(&pane, 80, 24, b"")]);
    h.defer_paint = true;
    let outcome = h.output(&pane, b"deferred glyphs");
    assert!(h.out.is_empty(), "{:?}", h.out);
    assert!(!outcome.chrome_dirty);
    assert_eq!(
        h.cell(&pane, 0, 0),
        Some('d'),
        "the mirror still ingested the bytes"
    );
}

/// A frame that changes nothing on screen emits nothing (no block, no
/// cursor, no writer wake).
#[test]
fn a_frame_that_changes_nothing_writes_nothing() {
    let pane = tid(1);
    let mut h = H::published(Workspace::single(pane.clone()), &[(&pane, 80, 24, b"")]);
    h.output(&pane, b"steady");
    assert!(!h.out.is_empty());
    h.out.clear();
    h.output(&pane, b"");
    assert!(h.out.is_empty(), "{:?}", h.out);
}

/// An OSC title is the only identity signal a plain agent CLI emits: moving
/// it (output or snapshot) marks the chrome dirty; re-asserting it does not.
#[test]
fn title_changes_mark_chrome_dirty() {
    let pane = tid(1);
    let mut h = H::published(Workspace::single(pane.clone()), &[(&pane, 80, 24, b"")]);
    for (bytes, dirty) in [
        (&b"just glyphs, no title"[..], false),
        (b"\x1b]2;\xe2\x9c\xb3 claude\x07", true),
        (b"\x1b]2;\xe2\x9c\xb3 claude\x07more glyphs", false),
        (b"\x1b]2;\x07", true),
    ] {
        assert_eq!(h.output(&pane, bytes).chrome_dirty, dirty, "{bytes:?}");
    }
    assert!(
        h.snapshot(&pane, 80, 24, b"\x1b]2;codex\x07resynced")
            .chrome_dirty
    );
    assert!(
        !h.snapshot(&pane, 80, 24, b"\x1b]2;codex\x07resynced again")
            .chrome_dirty
    );
}

#[test]
fn snapshot_during_synchronized_output_waits_for_live_end() {
    let pane = tid(1);
    let mut h = H::published(Workspace::single(pane.clone()), &[(&pane, 80, 24, b"")]);
    h.output(&pane, b"\x1b[?2026hpartial");
    h.snapshot(&pane, 80, 24, b"\x1b[!p\x1b[2J\x1b[Hstable snapshot");
    assert!(
        !h.out.is_empty(),
        "replacement publication paints the new replica"
    );
    assert!(
        h.mirror.panes[&pane].sync_output_since.is_none(),
        "synchronized-output state belongs to the retired replica"
    );
}

/// ATTACHED carries per-pane dimensions; slots are seeded from them so
/// pre-bootstrap output is not interpreted at 80x24.
#[test]
fn attached_seeds_pane_slots_from_snapshot_dimensions() {
    let (pane, window) = (tid(1), WindowId::new(1));
    let snapshot = SessionSnapshot::new(SessionId::new(1), window, pane.clone())
        .with_resources(vec![ResourceInfo::new(pane.clone(), window, 132, 43)]);
    let mut h = H::new().with_viewport((132, 43));
    h.send(attached(snapshot, 1));
    let slot = &h.mirror.panes[&pane];
    assert_eq!(
        (slot.terminal.cols().unwrap(), slot.terminal.rows().unwrap()),
        (132, 43)
    );
}

/// Cumulative frame ACKs belong to state-sync streams, not raw output.
#[test]
fn synthesized_raw_output_does_not_yield_frame_ack() {
    let (left, right) = (tid(1), tid(2));
    let mut h = H::published(
        ws1(split2(1, 2, 1)),
        &[(&left, 80, 24, b""), (&right, 80, 24, b"")],
    );
    assert_eq!(h.send(output_frame(&right, 1, b"hi")).ack, None);
}

/// Every withheld pane settles in ONE synchronized frame, including a pane
/// that went quiet while another kept talking (it once stayed unpainted).
#[test]
fn a_settle_paints_every_withheld_pane_in_one_frame() {
    let (left, right) = (tid(1), tid(2));
    let mut h = H::published(
        ws1(split2(1, 2, 1)),
        &[(&left, 80, 24, b""), (&right, 80, 24, b"")],
    );
    h.defer_paint = true;
    h.output(&left, b"LEFTSIDE");
    h.output(&right, b"RIGHTSIDE");
    assert!(h.out.is_empty(), "withheld frames paint nothing");

    let mut out: Vec<u8> = Vec::new();
    let _ = super::paint_output_frame(
        super::OutputFrame {
            out: &mut out,
            kernel: &h.mirror.engine_kernel,
            panes: &mut h.mirror.panes,
            workspace: &h.mirror.workspace,
            zoomed: None,
            focused_resource: h.mirror.focused_resource.as_ref(),
            status_bar: None,
            sidebar: None,
            viewport_dims: (80, 24),
            session_name: "",
            predict: &mut h.mirror.predict,
        },
        &[left, right],
    );
    let s = String::from_utf8_lossy(&out);
    let visible = strip_csi(&s);
    assert!(
        visible.contains("LEFTSIDE") && visible.contains("RIGHTSIDE"),
        "{visible:?}"
    );
    assert_eq!(s.matches("\x1b[?2026h").count(), 1, "{s:?}");
    assert_eq!(s.matches("\x1b[?2026l").count(), 1, "{s:?}");
}

/// Every pane, focused or not, repaints into its own rect on its own output
/// or snapshot; a non-focused pane that did not stayed visually frozen, or
/// blank after re-attach while input still routed.
#[test]
fn every_pane_repaints_into_its_rect_on_output_and_snapshot() {
    let (left, right) = (tid(1), tid(2));
    // 80 cols split 0.5: the right pane starts at 1-based col 42; row 1 is
    // the pane rail, so the left pane's output lands at row 2.
    for (pane, via_snapshot, cup) in [
        (&right, false, ";42H"),
        (&left, false, "\x1b[2;1H"),
        (&right, true, ";42H"),
        (&left, true, "\x1b[1;1H"),
    ] {
        let dims = if via_snapshot { 39 } else { 80 };
        let mut h = H::published(
            ws1(split2(1, 2, 1)),
            &[(&left, dims, 24, b""), (&right, dims, 24, b"")],
        );
        if via_snapshot {
            h.snapshot(pane, 39, 24, b"hello");
        } else {
            h.output(pane, b"hello");
        }
        let s = h.out_str();
        assert!(s.contains(cup), "{pane:?} snapshot={via_snapshot}: {s:?}");
        assert!(
            strip_csi(&s).contains("hello"),
            "{pane:?} snapshot={via_snapshot}: {s:?}"
        );
    }
}

/// Output for a pane in a non-active window warms its mirror but paints
/// nothing.
#[test]
fn output_for_inactive_window_pane_warms_mirror_but_does_not_paint() {
    let (active, other) = (tid(1), tid(2));
    let mut ws = Workspace::single(active.clone());
    ws.add_window("2".to_owned(), other.clone());
    ws.select(0);
    let mut h = H::published(ws, &[(&active, 80, 24, b""), (&other, 80, 24, b"")]);
    h.output(&other, b"offscreen");
    assert!(h.out.is_empty(), "{:?}", h.out_str());
    assert_eq!(h.cell(&other, 0, 0), Some('o'));
}

/// The apply-vs-paint split is observable: output closes both the
/// `vt_apply` and `paint_trigger` child spans under `handle_server_frame`.
#[test]
fn output_emits_separate_apply_and_paint_spans() {
    use std::sync::{Arc, Mutex};
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

    let buf = Buf::default();
    let layer = fmt::layer()
        .with_ansi(false)
        .with_writer(buf.clone())
        .with_span_events(fmt::format::FmtSpan::CLOSE);
    tracing::subscriber::set_global_default(Registry::default().with(layer))
        .expect("install test tracing subscriber");
    tracing_core::callsite::rebuild_interest_cache();
    let (left, right) = (tid(1), tid(2));
    let mut h = H::published(
        ws1(split2(1, 2, 1)),
        &[(&left, 80, 24, b""), (&right, 80, 24, b"")],
    );
    h.output(&left, b"hi");

    let log = String::from_utf8(buf.0.lock().expect("lock").clone()).expect("utf8");
    for span in ["vt_apply", "paint_trigger", "handle_server_frame"] {
        assert!(log.contains(span), "{span} span missing; log:\n{log}");
    }
}

/// A pane retitling itself is routine: the kernel's title/bell/history
/// statuses must not log at WARN, which filled client logs to 10 MB with a
/// line per agent spinner frame and printed on `phux snapshot --rendered`.
#[test]
fn routine_kernel_statuses_do_not_log_at_warn() {
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::Layer as _;
    use tracing_subscriber::fmt::MakeWriter;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::{Registry, filter::LevelFilter, fmt};

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

    let buf = Buf::default();
    let layer = fmt::layer()
        .with_ansi(false)
        .with_writer(buf.clone())
        .with_filter(LevelFilter::WARN);
    let subscriber = Registry::default().with(layer);
    tracing::subscriber::with_default(subscriber, || {
        tracing_core::callsite::rebuild_interest_cache();
        let pane = tid(1);
        let mut h = H::published(Workspace::single(pane.clone()), &[(&pane, 80, 24, b"")]);
        h.output(&pane, b"\x1b]2;working\x07\x07");
    });

    let log = String::from_utf8(buf.0.lock().expect("lock").clone()).expect("utf8");
    assert!(
        !log.contains("session kernel status"),
        "routine statuses logged at WARN:\n{log}"
    );
}

#[test]
fn bell_frame_writes_bel_to_sink() {
    let mut h = H::on(Workspace::single(tid(1)), &[]);
    h.send(FrameKind::Bell {
        terminal_id: tid(1),
    });
    assert_eq!(&h.out, b"\x07");
}

// ---- spawned windows and splits --------------------------------------------

fn spawn_window(h: &mut H, result: SpawnResult) -> FrameOutcome {
    super::handle_window_spawned(
        &mut h.out,
        &mut h.mirror.workspace,
        &mut h.mirror.focused_resource,
        &mut h.mirror.panes,
        &PendingWindow {
            name: "2".to_owned(),
            adopt: None,
        },
        result,
    )
    .expect("handle_window_spawned")
}

/// A new-window reply opens an active window focused on the new pane, and the
/// async completion records the pane being left as the MRU target.
#[test]
fn window_spawned_opens_active_window_focused_on_new_pane() {
    let mut h = H::on(Workspace::single(tid(1)), &[&tid(1)]);
    let mut history = crate::attach::focus::FocusHistory::default();
    let before = h.mirror.focused_resource.clone();
    let outcome = spawn_window(&mut h, SpawnResult::Ok(tid(2)));
    assert_eq!(
        (h.mirror.workspace.windows.len(), h.mirror.workspace.active),
        (2, 1)
    );
    assert_eq!(h.mirror.workspace.windows[1].name, "2");
    history.observe(before, h.mirror.focused_resource.as_ref());
    history.repair(h.mirror.focused_resource.as_ref(), &h.mirror.workspace);
    assert_eq!(h.mirror.focused_resource, Some(tid(2)));
    assert_eq!(
        history.target(h.mirror.focused_resource.as_ref(), &h.mirror.workspace),
        Some(tid(1))
    );
    assert!(h.mirror.panes.contains_key(&tid(2)));
    assert!(outcome.layout_replaced && outcome.emit_set_metadata && outcome.reflow_panes);
    assert!(
        outcome.adopt_spawned.is_empty(),
        "a local spawn already streams to us"
    );
}

/// `new-window` from the empty state opens the first window.
#[test]
fn new_window_from_the_empty_state_opens_the_first_window() {
    let mut h = H::new();
    let outcome = spawn_window(&mut h, SpawnResult::Ok(tid(5)));
    assert_eq!(
        (h.mirror.workspace.windows.len(), h.mirror.workspace.active),
        (1, 0)
    );
    assert_eq!(h.mirror.focused_resource, Some(tid(5)));
    assert!(h.mirror.panes.contains_key(&tid(5)));
    assert!(outcome.emit_set_metadata && outcome.reflow_panes);
}

/// A window spawned on a satellite does not open yet: the relayed pane
/// streams to no one until attached, and the attach can be refused. The
/// reply parks it, keeping a bound spawn's instance token.
#[test]
fn a_window_spawned_on_a_satellite_waits_for_its_attach() {
    for (result, instance) in [
        (SpawnResult::Ok(edge_pane()), None),
        (
            SpawnResult::OkBound {
                id: edge_pane(),
                instance: edge_token(),
            },
            Some(edge_token()),
        ),
    ] {
        let mut h = H::on(Workspace::single(tid(1)), &[&tid(1)]);
        let outcome = spawn_window(&mut h, result);
        assert_eq!(
            h.mirror.workspace.windows.len(),
            1,
            "nothing opens before the attach"
        );
        assert_eq!(h.mirror.focused_resource, Some(tid(1)));
        assert!(!outcome.layout_replaced && !outcome.emit_set_metadata);
        let [ParkedAdopt::Window(window)] = outcome.adopt_spawned.as_slice() else {
            panic!("expected one parked window: {:?}", outcome.adopt_spawned);
        };
        assert_eq!(window.name, "2");
        assert_eq!(
            window.adopt,
            Some(Adopt::Spawned(SpawnedPane {
                id: edge_pane(),
                instance
            }))
        );
    }
}

/// Drive one reply with a window named `name` parked on request 9 adopting
/// `adopt`, against `ws` focused on pane 1, with `in_flight` parked too.
fn adopt_reply_in(
    ws: Workspace,
    in_flight: Vec<(u32, PendingWindow)>,
    name: &str,
    adopt: Adopt,
    frame: FrameKind,
) -> (FrameOutcome, H) {
    let mut h = H::on(ws, &[&tid(1)]);
    h.mirror.focused_resource = Some(tid(1));
    h.mirror.pending_windows = in_flight.into_iter().collect();
    h.mirror.pending_windows.insert(
        9,
        PendingWindow {
            name: name.to_owned(),
            adopt: Some(adopt),
        },
    );
    let outcome = h.send(frame);
    (outcome, h)
}

fn adopt_reply(adopt: Adopt, frame: FrameKind) -> (FrameOutcome, H) {
    adopt_reply_in(Workspace::single(tid(1)), Vec::new(), "2", adopt, frame)
}

fn spawned_edge() -> Adopt {
    Adopt::Spawned(SpawnedPane::unbound(edge_pane()))
}

/// The parked satellite window opens, focused and saved, once its attach
/// succeeds; its pane is kept.
#[test]
fn a_spawned_satellite_window_opens_when_its_attach_succeeds() {
    let (outcome, h) = adopt_reply(spawned_edge(), command_ok(9));
    assert_eq!(h.mirror.workspace.windows.len(), 2);
    assert_eq!(h.mirror.workspace.windows[1].name, "2");
    assert_eq!(h.mirror.focused_resource, Some(edge_pane()));
    assert!(outcome.layout_replaced && outcome.emit_set_metadata);
    assert!(!h.belled());
    assert!(outcome.kill_orphans.is_empty());
    assert!(h.mirror.pending_windows.is_empty());
}

/// A refused attach opens and saves nothing, bells, names the host, and kills
/// the unreferenced pane it spawned, unless the satellite is unreachable (no
/// kill could reach it, and the hub would hold our input behind it).
#[test]
fn a_spawned_satellite_window_refusal_bells_and_names_the_host() {
    for (frame, killed) in [
        (unreachable_refusal(9), Vec::new()),
        (reachable_refusal(), vec![edge_pane()]),
        (
            FrameKind::Error {
                request_id: Some(9),
                code: ErrorCode::ResourceExhausted,
                message: "satellite edge link is saturated; retry".to_owned(),
            },
            vec![edge_pane()],
        ),
    ] {
        let (outcome, h) = adopt_reply(spawned_edge(), frame);
        assert_eq!(
            h.mirror.workspace.windows.len(),
            1,
            "no blank window is left behind"
        );
        assert_eq!(h.mirror.focused_resource, Some(tid(1)));
        assert!(!outcome.emit_set_metadata && !outcome.layout_replaced);
        assert!(h.belled());
        assert_eq!(outcome.notices.len(), 1);
        assert!(
            outcome.notices[0].text.contains("on satellite edge"),
            "{}",
            outcome.notices[0].text
        );
        assert_eq!(outcome.kill_orphans, killed);
        assert!(h.mirror.pending_windows.is_empty());
    }
}

/// A refused attach never kills a pane something still references: a window
/// holding it, or another open waiting on it.
#[test]
fn a_refused_attach_never_kills_a_referenced_pane() {
    let mut held = Workspace::single(tid(1));
    held.add_window("edge".to_owned(), edge_pane());
    let picker_open = PendingWindow {
        name: "edge/build".to_owned(),
        adopt: Some(Adopt::Existing(edge_pane())),
    };
    for (ws, in_flight, parked) in [
        (held, Vec::new(), 0),
        (Workspace::single(tid(1)), vec![(10, picker_open)], 1),
    ] {
        let (outcome, h) = adopt_reply_in(ws, in_flight, "2", spawned_edge(), reachable_refusal());
        assert!(h.belled(), "the refusal still bells");
        assert!(outcome.kill_orphans.is_empty());
        assert_eq!(
            h.mirror.pending_windows.len(),
            parked,
            "only the refused window is consumed"
        );
    }
}

/// An unreachable refusal strands a pane for a later conditional retry only
/// when its spawn was bound and nothing references it, and kills nothing
/// now; a reachable refusal kills at once and strands nothing.
#[test]
fn an_unreachable_refusal_strands_only_a_bound_unreferenced_pane() {
    use phux_client::conditional_kill::BoundResource;
    let bound = Adopt::Spawned(SpawnedPane {
        id: edge_pane(),
        instance: Some(edge_token()),
    });

    let (outcome, _) = adopt_reply(bound.clone(), unreachable_refusal(9));
    assert_eq!(
        outcome.unreachable_strays,
        vec![BoundResource {
            id: edge_pane(),
            instance: edge_token(),
        }]
    );
    assert!(outcome.kill_orphans.is_empty());

    let (outcome, _) = adopt_reply(spawned_edge(), unreachable_refusal(9));
    assert!(outcome.unreachable_strays.is_empty(), "unbound: nothing");

    let mut held = Workspace::single(tid(1));
    held.add_window("edge".to_owned(), edge_pane());
    let (outcome, _) = adopt_reply_in(held, Vec::new(), "2", bound.clone(), unreachable_refusal(9));
    assert!(outcome.unreachable_strays.is_empty(), "a held pane is kept");

    let (outcome, _) = adopt_reply(bound, reachable_refusal());
    assert_eq!(outcome.kill_orphans, vec![edge_pane()]);
    assert!(outcome.unreachable_strays.is_empty());
}

/// Opening an existing satellite session: success opens its window focused
/// and saved; a refusal (either shape) opens nothing, bells, names the host
/// and session, and never kills the session's own pane.
#[test]
fn satellite_session_attach_opens_its_window_or_refuses_cleanly() {
    let existing = || Adopt::Existing(edge_pane());
    let ws = || Workspace::single(tid(1));
    let (outcome, h) = adopt_reply_in(ws(), Vec::new(), "edge/build", existing(), command_ok(9));
    assert_eq!(h.mirror.workspace.windows.len(), 2);
    assert_eq!(
        (
            h.mirror.workspace.windows[1].name.as_str(),
            h.mirror.workspace.active
        ),
        ("edge/build", 1)
    );
    assert_eq!(h.mirror.focused_resource, Some(edge_pane()));
    assert!(outcome.layout_replaced && outcome.emit_set_metadata && outcome.reflow_panes);
    assert!(outcome.notices.is_empty() && !h.belled() && outcome.kill_orphans.is_empty());
    assert!(h.mirror.pending_windows.is_empty());

    for frame in [
        unreachable_refusal(9),
        FrameKind::Error {
            request_id: Some(9),
            code: ErrorCode::TerminalNotFound,
            message: "no such terminal".to_owned(),
        },
    ] {
        let (outcome, h) = adopt_reply_in(ws(), Vec::new(), "edge/build", existing(), frame);
        assert_eq!(h.mirror.workspace.windows.len(), 1);
        assert_eq!(h.mirror.focused_resource, Some(tid(1)));
        assert!(!outcome.emit_set_metadata && !outcome.layout_replaced);
        assert!(h.belled());
        assert_eq!(outcome.notices.len(), 1);
        assert!(
            outcome.notices[0].text.contains("edge/build"),
            "{}",
            outcome.notices[0].text
        );
        assert!(outcome.kill_orphans.is_empty());
        assert!(h.mirror.pending_windows.is_empty());
    }
}

/// A split of pane 1 parked with `host` and `adopt`.
fn parked_split(host: SplitHost, adopt: Option<ResourceId>) -> PendingSplit {
    PendingSplit {
        focused_at_request: tid(1),
        dir: SplitDir::Horizontal,
        zoom_on_spawn: false,
        host,
        adopt: adopt.map(SpawnedPane::unbound),
        open_existing: None,
    }
}

fn satellite_split(adopt: Option<ResourceId>) -> PendingSplit {
    parked_split(SplitHost::Satellite(SatelliteHost::new("edge")), adopt)
}

/// Drive one reply with `split` parked under request 9, against `ws`
/// focused on `focused`.
fn split_reply_in(
    ws: Workspace,
    focused: ResourceId,
    split: PendingSplit,
    frame: FrameKind,
) -> (FrameOutcome, H) {
    let mut h = H::on(ws, &[&tid(1)]);
    h.mirror.focused_resource = Some(focused);
    h.mirror.pending_splits.insert(9, split);
    let outcome = h.send(frame);
    (outcome, h)
}

fn split_reply(split: PendingSplit, frame: FrameKind) -> (FrameOutcome, H) {
    split_reply_in(Workspace::single(tid(1)), tid(1), split, frame)
}

fn spawned(id: ResourceId) -> FrameKind {
    FrameKind::ResourceSpawned {
        request_id: 9,
        result: SpawnResult::Ok(id),
    }
}

/// A parked split's spawn reply zooms the new pane under `zoom_on_spawn`,
/// else clears zoom; focus follows the spawned pane and the anchor becomes
/// the MRU target.
#[test]
fn a_split_reply_zooms_only_under_zoom_on_spawn() {
    for zoom_on_spawn in [true, false] {
        let mut split = parked_split(SplitHost::Attached, None);
        split.zoom_on_spawn = zoom_on_spawn;
        let mut h = H::on(Workspace::single(tid(1)), &[&tid(1)]);
        h.mirror.zoomed = Some(tid(1));
        h.mirror.pending_splits.insert(9, split);
        let mut history = crate::attach::focus::FocusHistory::default();
        let before = h.mirror.focused_resource.clone();
        assert!(h.send(spawned(tid(2))).layout_replaced);
        history.observe(before, h.mirror.focused_resource.as_ref());
        history.repair(h.mirror.focused_resource.as_ref(), &h.mirror.workspace);
        assert_eq!(h.mirror.focused_resource, Some(tid(2)));
        assert_eq!(
            history.target(h.mirror.focused_resource.as_ref(), &h.mirror.workspace),
            Some(tid(1))
        );
        assert_eq!(h.mirror.zoomed, zoom_on_spawn.then(|| tid(2)));
    }
}

/// A split spawned on a satellite does not apply until its attach succeeds;
/// the reply parks it, keeping a bound spawn's instance token. A local pane
/// answered bound (tolerated) still splits in place with no notice.
#[test]
fn a_split_spawned_on_a_satellite_waits_for_its_attach() {
    for (result, instance) in [
        (SpawnResult::Ok(edge_pane()), None),
        (
            SpawnResult::OkBound {
                id: edge_pane(),
                instance: edge_token(),
            },
            Some(edge_token()),
        ),
    ] {
        let (outcome, h) = split_reply(
            satellite_split(None),
            FrameKind::ResourceSpawned {
                request_id: 9,
                result,
            },
        );
        assert_eq!(h.leaves(), vec![tid(1)], "nothing splits before the attach");
        assert_eq!(h.mirror.focused_resource, Some(tid(1)));
        assert!(!outcome.layout_replaced && !outcome.emit_set_metadata && !h.belled());
        let [ParkedAdopt::Split(split)] = outcome.adopt_spawned.as_slice() else {
            panic!("expected one parked split: {:?}", outcome.adopt_spawned);
        };
        assert_eq!(
            split.adopt,
            Some(SpawnedPane {
                id: edge_pane(),
                instance
            })
        );
        assert_eq!(split.host, SplitHost::Satellite(SatelliteHost::new("edge")));
        assert_eq!(split.focused_at_request, tid(1));
    }

    let (outcome, h) = split_reply(
        parked_split(SplitHost::Attached, None),
        FrameKind::ResourceSpawned {
            request_id: 9,
            result: SpawnResult::OkBound {
                id: tid(2),
                instance: edge_token(),
            },
        },
    );
    assert_eq!(h.leaves(), vec![tid(1), tid(2)]);
    assert!(outcome.notices.is_empty(), "a local split raises no notice");
}

/// The parked satellite split, or an opened existing satellite pane, lands
/// beside the local leaf, focused and saved, once the attach succeeds.
#[test]
fn a_satellite_split_applies_when_its_attach_succeeds() {
    let mut existing = satellite_split(None);
    existing.open_existing = Some(edge_pane());
    for split in [satellite_split(Some(edge_pane())), existing] {
        let (outcome, h) = split_reply(split, command_ok(9));
        assert_eq!(h.leaves(), vec![tid(1), edge_pane()]);
        assert_eq!(h.mirror.focused_resource, Some(edge_pane()));
        assert!(outcome.layout_replaced && outcome.emit_set_metadata && outcome.reflow_panes);
        assert!(outcome.notices.is_empty() && !h.belled() && outcome.kill_orphans.is_empty());
        assert!(h.mirror.pending_splits.is_empty());
    }
}

/// A refused open of an existing satellite pane leaves the layout alone and
/// does not kill a pane this client did not spawn.
#[test]
fn a_refused_open_of_an_existing_satellite_pane_does_not_kill_it() {
    let mut split = satellite_split(None);
    split.open_existing = Some(edge_pane());
    let (outcome, h) = split_reply(split, unreachable_refusal(9));
    assert_eq!(h.leaves(), vec![tid(1)]);
    assert!(outcome.kill_orphans.is_empty());
    assert!(h.belled());
    assert!(h.mirror.pending_splits.is_empty());
}

/// A refused satellite split attach leaves no dead split, bells, names the
/// host, and kills the spawned pane unless no kill could reach it.
#[test]
fn a_spawned_satellite_split_refusal_bells_and_names_the_host() {
    for (frame, killed) in [
        (unreachable_refusal(9), Vec::new()),
        (
            FrameKind::Error {
                request_id: Some(9),
                code: ErrorCode::TerminalNotFound,
                message: "no such terminal".to_owned(),
            },
            vec![edge_pane()],
        ),
    ] {
        let (outcome, h) = split_reply(satellite_split(Some(edge_pane())), frame);
        assert_eq!(h.leaves(), vec![tid(1)]);
        assert_eq!(h.mirror.focused_resource, Some(tid(1)));
        assert!(!outcome.emit_set_metadata && !outcome.layout_replaced);
        assert!(h.belled());
        assert_eq!(outcome.notices.len(), 1);
        assert!(
            outcome.notices[0]
                .text
                .contains("split onto satellite edge"),
            "{}",
            outcome.notices[0].text
        );
        assert_eq!(outcome.kill_orphans, killed);
        assert!(h.mirror.pending_splits.is_empty());
    }
}

/// A satellite split whose relayed spawn is refused bells and names the host.
#[test]
fn a_satellite_split_whose_spawn_is_refused_names_the_host() {
    use phux_protocol::wire::frame::SpawnError;
    let (outcome, h) = split_reply(
        satellite_split(None),
        FrameKind::ResourceSpawned {
            request_id: 9,
            result: SpawnResult::Err(SpawnError::SatelliteUnreachable(
                "satellite edge link is down".to_owned(),
            )),
        },
    );
    assert_eq!(h.leaves(), vec![tid(1)]);
    assert!(h.belled());
    assert_eq!(outcome.notices.len(), 1);
    assert!(
        outcome.notices[0]
            .text
            .contains("could not split onto satellite edge: satellite edge link is down"),
        "{}",
        outcome.notices[0].text
    );
    assert!(outcome.adopt_spawned.is_empty());
}

/// A hub without host-aware spawns opened the split on itself: it applies
/// locally and the notice says the pane is on this host.
#[test]
fn a_satellite_split_on_an_older_hub_says_it_opened_on_this_host() {
    let (outcome, h) = split_reply(
        parked_split(
            SplitHost::AttachedInsteadOf(SatelliteHost::new("edge")),
            None,
        ),
        spawned(tid(2)),
    );
    assert_eq!(h.leaves(), vec![tid(1), tid(2)]);
    assert!(outcome.adopt_spawned.is_empty());
    assert_eq!(outcome.notices.len(), 1);
    let text = &outcome.notices[0].text;
    assert!(
        text.contains("on this host") && text.contains("edge"),
        "{text}"
    );
}

/// A split parked across a window switch lands beside its source, in that
/// pane's window, without moving the visible window or focus.
#[test]
fn a_split_parked_across_a_window_switch_lands_beside_its_source() {
    let mut ws = Workspace::single(tid(1));
    ws.add_window("2".to_owned(), tid(5));
    let (outcome, h) = split_reply_in(
        ws,
        tid(5),
        satellite_split(Some(edge_pane())),
        command_ok(9),
    );
    assert_eq!(
        window_leaves(&h.mirror.workspace, 0),
        vec![tid(1), edge_pane()]
    );
    assert_eq!(window_leaves(&h.mirror.workspace, 1), vec![tid(5)]);
    assert_eq!(h.mirror.workspace.active, 1, "the window on screen stays");
    assert_eq!(
        h.mirror.focused_resource,
        Some(tid(5)),
        "focus is not stolen"
    );
    assert!(outcome.emit_set_metadata);
    assert!(!h.belled());
}

/// The source pane closed while its split waited: the split is dropped with
/// a bell and notice (satellite attach or local spawn alike) and the
/// unreferenced spawned pane is killed.
#[test]
fn a_split_whose_source_pane_closed_is_dropped() {
    for (split, frame, spawned_pane) in [
        (
            satellite_split(Some(edge_pane())),
            command_ok(9),
            edge_pane(),
        ),
        (
            parked_split(SplitHost::Attached, None),
            spawned(tid(2)),
            tid(2),
        ),
    ] {
        let (outcome, h) = split_reply_in(Workspace::single(tid(3)), tid(3), split, frame);
        assert_eq!(h.leaves(), vec![tid(3)]);
        assert_eq!(h.mirror.focused_resource, Some(tid(3)));
        assert!(!outcome.emit_set_metadata && !outcome.layout_replaced);
        assert!(h.belled());
        assert_eq!(outcome.notices.len(), 1);
        assert!(
            outcome.notices[0].text.contains("split dropped"),
            "{}",
            outcome.notices[0].text
        );
        assert_eq!(outcome.kill_orphans, vec![spawned_pane]);
        assert!(h.mirror.pending_splits.is_empty());
    }
}

/// `SatelliteUnreachable` greys the satellite pane and keeps its leaf.
#[test]
fn satellite_unreachable_greys_the_pane_and_keeps_its_leaf() {
    let mut ws = Workspace::single(tid(1));
    beside(&mut ws, &edge_pane());
    let mut h = H::on(ws, &[&tid(1), &edge_pane()]);
    h.mirror.focused_resource = Some(edge_pane());
    let outcome = h.send(FrameKind::Error {
        request_id: None,
        code: ErrorCode::SatelliteUnreachable,
        message: "satellite edge is unreachable: link is down".to_owned(),
    });
    assert!(h.mirror.panes[&edge_pane()].satellite_down);
    assert!(!h.mirror.panes[&tid(1)].satellite_down);
    assert_eq!(h.leaves(), vec![tid(1), edge_pane()]);
    assert!(outcome.chrome_dirty && !outcome.layout_replaced);
    assert_eq!(outcome.notices.len(), 1);
}

// ---- close, detach, and the empty state ------------------------------------

/// The last pane closing detaches the client, carrying its status (`None`
/// for a signal death, so the CLI can say "killed").
#[test]
fn last_pane_closed_detaches_the_client() {
    for status in [Some(0), None] {
        let pane = tid(1);
        let mut h = H::on(Workspace::single(pane.clone()), &[&pane]);
        let outcome = h.send(closed(&pane, status));
        assert!(outcome.exit);
        assert_eq!(
            outcome.exit_reason,
            Some(AttachEnd::LastPaneClosed {
                exit_status: status
            })
        );
        assert!(h.mirror.workspace.windows.is_empty());
        assert!(!h.mirror.panes.contains_key(&pane));
    }
}

/// Closing one of several panes keeps the attach: the leaf folds out, focus
/// re-anchors, and a repaint + broadcast + survivor reflow is requested. A
/// non-zero or signal death raises a Warn notice; a clean exit, or a close
/// this client requested (whose marker is drained), does not.
#[test]
fn closing_one_of_several_panes_keeps_the_client_attached() {
    let (left, right) = (tid(1), tid(2));
    for (pane, status, expected, notice) in [
        (&left, Some(0), false, None),
        (&left, Some(137), false, Some("pane 1: exited 137")),
        (
            &right,
            None,
            false,
            Some("pane 2: killed (signal or unknown)"),
        ),
        (&left, Some(137), true, None),
    ] {
        let mut h = H::on(ws1(split2(1, 2, 1)), &[&left, &right]);
        if expected {
            h.mirror.expected_closes.insert(pane.clone());
        }
        let outcome = h.send(closed(pane, status));
        assert!(!outcome.exit);
        assert_eq!(h.mirror.workspace.windows.len(), 1);
        assert!(outcome.layout_replaced && outcome.emit_set_metadata && outcome.reflow_panes);
        assert_eq!(
            outcome.notices.first().map(|n| n.text.as_str()),
            notice,
            "{pane:?} {status:?}"
        );
        if notice.is_some() {
            assert_eq!(outcome.notices.len(), 1);
            assert_eq!(outcome.notices[0].severity, NoticeSeverity::Warn);
        }
        assert!(
            h.mirror.expected_closes.is_empty(),
            "the expectation is consumed"
        );
    }
    let mut h = H::on(ws1(split2(1, 2, 1)), &[&left, &right]);
    h.send(closed(&left, Some(0)));
    assert_eq!(
        h.mirror.focused_resource,
        Some(right),
        "focus re-anchors onto the survivor"
    );
}

#[test]
fn closing_the_mru_pane_clears_stale_history() {
    let (left, right) = (tid(1), tid(2));
    let mut h = H::on(ws1(split2(1, 2, 1)), &[&left, &right]);
    let mut history = crate::attach::focus::FocusHistory::with_previous(right.clone());
    let before = h.mirror.focused_resource.clone();
    h.send(closed(&right, Some(0)));
    history.observe(before, h.mirror.focused_resource.as_ref());
    history.repair(h.mirror.focused_resource.as_ref(), &h.mirror.workspace);
    assert_eq!(history.previous(), None);
}

/// `DETACHED` exits with the server's stated reason, including none.
#[test]
fn detached_carries_the_servers_reason_into_the_exit() {
    for reason in [
        None,
        Some(DetachReason::Requested),
        Some(DetachReason::ServerShutdown),
        Some(DetachReason::Replaced),
    ] {
        let mut h = H::on(Workspace::single(tid(1)), &[&tid(1)]);
        let outcome = h.send(FrameKind::Detached {
            reason,
            message: "diagnostic only".to_owned(),
        });
        assert!(outcome.exit);
        assert_eq!(outcome.exit_reason, Some(AttachEnd::Detached { reason }));
    }
}

/// ADR-0105: the last pane of a keep-empty session closing keeps the attach,
/// leaves the empty state, and tombstones the dead layout. The mark follows
/// `keep_empty/v1` broadcasts for this session only.
#[test]
fn keep_empty_mark_keeps_the_attach_when_the_last_pane_closes() {
    use phux_protocol::wire::frame::SESSION_KEEP_EMPTY_KEY;
    let pane = tid(1);
    let mut h = H::on(Workspace::single(pane.clone()), &[&pane]);
    h.mirror.session_name = "work".to_owned();
    let mark =
        |value: &[u8]| meta_changed(Scope::Global, SESSION_KEEP_EMPTY_KEY, Some(value.to_vec()));
    h.send(mark(b"other\0true"));
    assert!(
        !h.mirror.keep_empty_session,
        "another session's mark is not ours"
    );
    h.send(mark(b"work\0true"));
    assert!(h.mirror.keep_empty_session);

    let outcome = h.send(closed(&pane, Some(0)));
    assert!(!outcome.exit && outcome.exit_reason.is_none());
    assert!(outcome.clear_layout, "the dead layout is tombstoned");
    assert!(outcome.layout_replaced, "the empty state is painted");
    assert!(h.mirror.workspace.windows.is_empty());
    assert_eq!(h.mirror.focused_resource, None);
    assert!(!h.mirror.panes.contains_key(&pane));
}

/// A session-rename broadcast updates our status name only when it names us,
/// and always reports the pair for the peer graph.
#[test]
fn session_rename_broadcast_updates_this_clients_status_name() {
    use phux_protocol::wire::frame::{SESSION_NAME_KEY, encode_session_rename};
    let mut h = H::on(Workspace::single(tid(1)), &[&tid(1)]);
    h.mirror.session_name = "work".to_owned();
    let rename = |from, to| {
        meta_changed(
            Scope::Global,
            SESSION_NAME_KEY,
            Some(encode_session_rename(from, to)),
        )
    };
    let pair = |o: &FrameOutcome| o.session_rename.clone();
    let outcome = h.send(rename("work", "notes"));
    assert_eq!(h.mirror.session_name, "notes");
    assert_eq!(
        pair(&outcome),
        Some(("work".to_owned(), "notes".to_owned()))
    );
    let outcome = h.send(rename("other", "elsewhere"));
    assert_eq!(
        h.mirror.session_name, "notes",
        "a peer rename does not overwrite our name"
    );
    assert_eq!(
        pair(&outcome),
        Some(("other".to_owned(), "elsewhere".to_owned()))
    );
}

/// Attaching to an already-empty session starts in the empty state (no slot
/// for the sentinel focus), and a persisted layout of only dead panes is
/// discarded rather than attached.
#[test]
fn attaching_to_an_empty_session_starts_and_stays_empty() {
    let sid = SessionId::new(4);
    let snapshot = SessionSnapshot::new(sid, WindowId::new(0), ResourceId::local(0))
        .with_sessions(vec![SessionInfo::new(sid, "parked").with_keep_empty(true)]);
    let mut h = H::on(Workspace::single(tid(9)), &[]);
    let outcome = h.send(attached(snapshot, 3));
    assert!(h.mirror.keep_empty_session);
    assert_eq!(h.mirror.session_name, "parked");
    assert!(h.mirror.workspace.windows.is_empty());
    assert_eq!(h.mirror.focused_resource, None);
    assert!(
        h.mirror.panes.is_empty(),
        "the sentinel focus must not get a slot"
    );
    assert!(outcome.subscribe_layout);
    assert_eq!(outcome.own_client_id, Some(ClientId::new(3)));

    h.layout_request = Some(7);
    let outcome = h.send(FrameKind::MetadataValue {
        request_id: 7,
        value: Some(Workspace::single(tid(9)).encode_cbor().unwrap()),
    });
    assert!(outcome.layout_get_answered);
    assert!(outcome.attach_panes.is_empty(), "no dead pane is attached");
    assert!(h.mirror.workspace.windows.is_empty());
    assert_eq!(h.mirror.focused_resource, None);
}

// ---- agent events and notices ----------------------------------------------

/// ADR-0035 `Asked` raises the (possibly unfocused) pane's attention and
/// dirties the chrome once; a repeat changes nothing; an unknown pane is
/// dropped without allocating a slot.
#[test]
fn asked_event_sets_attention_and_dirties_chrome_once() {
    let (left, right) = (tid(1), tid(2));
    let mut h = H::on(ws1(split2(1, 2, 1)), &[&left, &right]);
    assert!(h.send(event(&right, asked())).chrome_dirty);
    assert!(h.mirror.panes[&right].attention && !h.mirror.panes[&left].attention);
    assert!(!h.send(event(&right, asked())).chrome_dirty);
    assert!(h.mirror.panes[&right].attention);

    let unknown = tid(9);
    let mut h = H::on(Workspace::single(left.clone()), &[&left]);
    assert!(!h.send(event(&unknown, asked())).chrome_dirty);
    assert!(!h.mirror.panes.contains_key(&unknown));
}

/// A mirror slot does not make a peer's question local.
#[test]
fn peer_asked_event_with_a_cached_slot_routes_to_foreign_attention() {
    let peer = tid(9);
    let mut h = H::on(Workspace::single(tid(1)), &[&tid(1), &peer]);
    let outcome = h.send(event(&peer, asked()));
    assert_eq!(outcome.foreign_attention, Some(peer.clone()));
    assert!(
        h.mirror.panes[&peer].attention,
        "retain until ownership is known"
    );
    assert!(!outcome.chrome_dirty);
}

/// Cwd and command-exit events land in the slot and dirty the chrome only
/// on change; unknown panes and activity-only events change nothing.
#[test]
fn cwd_and_exit_events_update_the_slot_and_coalesce() {
    let pane = tid(1);
    let mut h = H::on(Workspace::single(pane.clone()), &[&pane]);
    let cwd = |dir: &str| AgentEvent::CwdChanged {
        cwd: dir.to_owned(),
    };
    let exit = |code| AgentEvent::CommandFinished {
        exit_code: Some(code),
    };
    for (ev, dirty) in [
        (cwd("/tmp/work"), true),
        (cwd("/tmp/work"), false),
        (exit(0), true),
        (exit(0), false),
        (exit(127), true),
        (AgentEvent::Idle, false),
    ] {
        assert_eq!(
            h.send(event(&pane, ev.clone())).chrome_dirty,
            dirty,
            "{ev:?}"
        );
    }
    assert_eq!(h.mirror.panes[&pane].cwd.as_deref(), Some("/tmp/work"));
    assert_eq!(h.mirror.panes[&pane].last_exit, Some(127));

    let unknown = tid(9);
    assert!(!h.send(event(&unknown, cwd("/x"))).chrome_dirty);
    assert!(!h.send(event(&unknown, exit(1))).chrome_dirty);
    assert!(!h.mirror.panes.contains_key(&unknown));
}

fn control_event(holder: Option<ClientId>) -> AgentEvent {
    use phux_protocol::wire::frame::{ControlAction, ResourceLifecycle};
    AgentEvent::TerminalControl {
        lifecycle: ResourceLifecycle::Running,
        exit_status: None,
        input_holder: holder,
        action: if holder.is_some() {
            ControlAction::Acquired
        } else {
            ControlAction::Released
        },
        actor: holder,
    }
}

/// A focused-pane input-holder TRANSITION raises a notice; the attach-time
/// initial state (first control event) and an unchanged holder do not.
#[test]
fn focused_holder_transition_yields_a_notice_and_initial_state_does_not() {
    let pane = tid(1);
    let holder = ClientId::new(9);
    let mut h = H::on(Workspace::single(pane.clone()), &[&pane]);
    let initial = h.send(event(&pane, control_event(Some(holder))));
    assert!(initial.chrome_dirty && initial.notices.is_empty());
    assert_eq!(h.mirror.panes[&pane].input_holder, Some(holder));

    let released = h.send(event(&pane, control_event(None)));
    assert_eq!(released.notices.len(), 1);
    assert_eq!(released.notices[0].severity, NoticeSeverity::Info);
    assert_eq!(released.notices[0].text, "input: wheel released");
    let seized = h.send(event(&pane, control_event(Some(holder))));
    assert_eq!(seized.notices.len(), 1);
    assert_eq!(seized.notices[0].text, "input: c9 took the wheel");
    assert!(
        h.send(event(&pane, control_event(Some(holder))))
            .notices
            .is_empty()
    );
}

/// An unfocused pane's holder transition folds the badge but raises no
/// notice (the slot is scoped to the pane being typed into).
#[test]
fn unfocused_holder_transition_yields_no_notice() {
    let (focused, background) = (tid(1), tid(2));
    let holder = ClientId::new(4);
    let mut h = H::on(Workspace::single(focused.clone()), &[&focused, &background]);
    h.send(event(&background, control_event(None)));
    let outcome = h.send(event(&background, control_event(Some(holder))));
    assert!(outcome.chrome_dirty && outcome.notices.is_empty());
    assert_eq!(h.mirror.panes[&background].input_holder, Some(holder));
}

/// No `ErrorCode`, correlated or not, ends the attach (SPEC §9: termination
/// is `DETACHED` plus transport close). Uncorrelated codes raise one Warn
/// notice naming the code; `SatelliteUnreachable` uses the degraded wording;
/// correlated ones stay on their request path. Swept from the wire tables.
#[test]
fn no_error_code_is_fatal_in_the_attached_phase() {
    let codes: Vec<ErrorCode> = (0..=u16::MAX).filter_map(ErrorCode::from_wire).collect();
    assert!(!codes.is_empty());
    for code in codes {
        for request_id in [None, Some(11_u32)] {
            let mut h = H::on(Workspace::single(tid(1)), &[&tid(1)]);
            let outcome = h
                .try_send(FrameKind::Error {
                    request_id,
                    code,
                    message: "the pane fell over".to_owned(),
                })
                .unwrap_or_else(|e| {
                    panic!("ERROR {code:?} ({request_id:?}) ended the attach: {e:?}")
                });
            assert!(!outcome.exit, "{code:?} ({request_id:?})");
            match (request_id, code) {
                (Some(_), _) => assert!(outcome.notices.is_empty(), "{code:?}"),
                (None, ErrorCode::SatelliteUnreachable) => assert_eq!(
                    outcome.notices[0].text,
                    "federation degraded: the pane fell over"
                ),
                (None, _) => {
                    assert_eq!(outcome.notices.len(), 1, "{code:?}");
                    assert_eq!(outcome.notices[0].severity, NoticeSeverity::Warn);
                    let text = &outcome.notices[0].text;
                    assert!(
                        text.contains("the pane fell over") && text.contains(&format!("{code:?}")),
                        "{text}"
                    );
                }
            }
        }
    }
}

// ---- agent metadata --------------------------------------------------------

/// ADR-0040: a `phux.agent/v1` broadcast decodes into the index and flags
/// chrome; an identical record does not; a tombstone clears it; malformed
/// bytes are no record at all.
#[test]
fn agent_metadata_broadcast_updates_index_and_tombstone_clears_it() {
    use phux_protocol::wire::frame::RESOURCE_AGENT_KEY;
    let pane = tid(1);
    let mut h = H::on(Workspace::single(pane.clone()), &[&pane]);
    let record = |value: Option<&[u8]>| {
        meta_changed(
            Scope::Resource(pane.clone()),
            RESOURCE_AGENT_KEY,
            value.map(<[u8]>::to_vec),
        )
    };
    assert!(!h.send(record(Some(b"not json at all"))).agent_meta_changed);
    assert!(h.mirror.agent_meta.records.is_empty());

    let blocked = br#"{"name":"reviewer","state":"blocked"}"#;
    assert!(h.send(record(Some(blocked))).agent_meta_changed);
    let stored = &h.mirror.agent_meta.records[&pane];
    assert_eq!(stored.name, "reviewer");
    assert_eq!(
        stored.state,
        phux_client::agent_meta::AgentMetaState::Blocked
    );
    assert!(!h.send(record(Some(blocked))).agent_meta_changed);
    assert!(h.send(record(None)).agent_meta_changed);
    assert!(!h.mirror.agent_meta.records.contains_key(&pane));
}

/// ADR-0040: a `GET_METADATA` reply is correlated through the pending map;
/// an absent key resolves it without inventing a record.
#[test]
fn agent_metadata_get_reply_is_correlated_by_request_id() {
    let pane = tid(1);
    let mut h = H::on(Workspace::single(pane.clone()), &[&pane]);
    h.mirror.agent_meta.pending.insert(77, pane.clone());
    let outcome = h.send(FrameKind::MetadataValue {
        request_id: 77,
        value: Some(br#"{"name":"codex","kind":"codex","state":"working"}"#.to_vec()),
    });
    assert!(outcome.agent_meta_changed);
    assert!(h.mirror.agent_meta.pending.is_empty());
    assert_eq!(h.mirror.agent_meta.records[&pane].name, "codex");

    h.mirror.agent_meta.pending.insert(78, pane);
    assert!(
        h.send(FrameKind::MetadataValue {
            request_id: 78,
            value: None
        })
        .agent_meta_changed
    );
    assert!(h.mirror.agent_meta.records.is_empty());
}

/// The config-reload doorbell rings on a Global non-tombstone broadcast only.
#[test]
fn config_reload_doorbell_flags_reload_and_ignores_tombstones() {
    use phux_protocol::wire::frame::CONFIG_RELOAD_KEY;
    let mut h = H::on(Workspace::single(tid(1)), &[&tid(1)]);
    let outcome = h.send(meta_changed(
        Scope::Global,
        CONFIG_RELOAD_KEY,
        Some(b"1234-99".to_vec()),
    ));
    assert!(outcome.config_reload);
    assert!(!outcome.layout_replaced && !outcome.agent_meta_changed);
    assert!(
        !h.send(meta_changed(Scope::Global, CONFIG_RELOAD_KEY, None))
            .config_reload
    );
    assert!(
        !h.send(meta_changed(
            Scope::Resource(tid(9)),
            CONFIG_RELOAD_KEY,
            Some(b"5678-99".to_vec())
        ))
        .config_reload
    );
}

// ---- resource kinds: AgentSession children ride the attach as streams ------

/// One Terminal pane plus an `AgentSession` bound to it: no grid, no window,
/// a parent, and an agent facet.
fn mixed_kind_snapshot(pane: &ResourceId, agent: &ResourceId) -> SessionSnapshot {
    let (window, session) = (WindowId::new(1), SessionId::new(1));
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

/// A client attached to [`mixed_kind_snapshot`] of panes 1 and 2.
fn kind_attached() -> (H, FrameOutcome) {
    let mut h = H::new().with_viewport((100, 30));
    let outcome = h.send(attached(mixed_kind_snapshot(&tid(1), &tid(2)), 1));
    (h, outcome)
}

#[test]
fn attach_participants_and_agent_sessions_split_a_mixed_kind_snapshot() {
    let (pane, agent) = (tid(1), tid(2));
    let snapshot = mixed_kind_snapshot(&pane, &agent);
    let participants = attach_participants(&snapshot);
    assert_eq!(
        participants,
        vec![pane],
        "only the Terminal is a barrier participant"
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
    let (pane, agent) = (tid(1), tid(2));
    let (h, outcome) = kind_attached();
    assert_eq!(
        h.mirror.panes.keys().collect::<Vec<_>>(),
        vec![&pane],
        "no slot for a 0x0 stream"
    );
    assert_eq!(
        h.mirror.engine_kernel.resource_kind(&agent),
        Some(ResourceKind::AgentSession)
    );
    let view = h
        .mirror
        .engine_kernel
        .agent_session(&agent)
        .expect("declared");
    assert_eq!(view.parent, Some(&pane));
    assert_eq!(view.state.provider.as_deref(), Some("claude"));
    assert_eq!(
        view.state.status,
        phux_client_core::session::agent_stream::AgentSessionStatus::Working
    );
    assert!(outcome.pane_cwds.iter().all(|(id, _)| id != &agent));
    assert_eq!(h.leaves(), vec![pane], "the layout holds the pane alone");
}

#[test]
fn an_agent_stream_bootstraps_without_a_slot_and_dirties_the_chrome() {
    let (pane, agent) = (tid(1), tid(2));
    let (mut h, _) = kind_attached();
    let begin = h
        .try_send(FrameKind::BootstrapBegin {
            terminal_id: agent.clone(),
            stream_id: stream(),
            bootstrap_id: bootstrap(),
            profile: phux_protocol::BootstrapStreamProfile::AgentEventsJsonlV1,
            cols: 0,
            rows: 0,
            // L1 §4.8: base_seq is the record counter at the cut.
            base_seq: 1,
        })
        .expect("agent BEGIN at 0x0 is legal");
    assert!(!begin.chrome_dirty);
    h.send(chunk_frame(
        &agent,
        b"{\"seq\":1,\"ts_ms\":1,\"type\":\"ask\",\"data\":{}}\n",
    ));
    let ready = h.send(ready_frame(&agent));
    assert!(ready.chrome_dirty, "the published log changes the sidebar");
    assert!(
        !ready.layout_replaced,
        "no pane repaint for a record stream"
    );
    let live = h.send(output_frame(
        &agent,
        2,
        b"{\"seq\":2,\"ts_ms\":2,\"type\":\"stop\",\"data\":{}}\n",
    ));
    assert!(live.chrome_dirty);
    assert!(
        !h.mirror.panes.contains_key(&agent),
        "a stream never seeds a slot"
    );

    let rows = crate::attach::agent_rows::agent_session_rows(&h.mirror.engine_kernel);
    let under_pane = &rows[&pane];
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
    let (pane, agent) = (tid(1), tid(2));
    let (mut h, _) = kind_attached();
    let before = h.mirror.workspace.clone();
    let outcome = h.send(FrameKind::ResourceClosed {
        terminal_id: agent.clone(),
        exit_status: None,
        reason: CloseReason::ParentClosed,
        signal: None,
    });
    assert!(!outcome.exit && outcome.chrome_dirty);
    assert!(!outcome.layout_replaced && !outcome.emit_set_metadata && !outcome.reflow_panes);
    assert!(
        outcome.notices.is_empty(),
        "no pane-exit notice for a stream"
    );
    assert_eq!(h.mirror.workspace, before);
    assert!(h.mirror.panes.contains_key(&pane));
    assert!(h.mirror.engine_kernel.agent_session(&agent).is_none());
    assert!(crate::attach::agent_rows::agent_session_rows(&h.mirror.engine_kernel).is_empty());
}

#[test]
fn a_layout_naming_an_agent_session_is_refused() {
    let (mut h, _) = kind_attached();
    let mut bad = Workspace::single(tid(1));
    bad.windows[0].state = split2(1, 2, 1);
    let error = h
        .try_send(layout_changed(1, Some(bad.encode_cbor().unwrap())))
        .expect_err("non-terminal layout must be refused");
    assert!(error.to_string().contains("is not a terminal resource"));
    assert_eq!(h.leaves(), vec![tid(1)]);
}

#[test]
fn a_live_spawned_agent_session_is_declared_and_attached_as_a_stream() {
    let (pane, late) = (tid(1), tid(3));
    let (mut h, _) = kind_attached();
    let spawn = |parent: ResourceId| AgentEvent::ResourceSpawned {
        kind: ResourceKind::AgentSession,
        parent: Some(parent),
    };
    let outcome = h.send(event(&late, spawn(pane.clone())));
    assert_eq!(outcome.attach_panes, vec![late.clone()]);
    assert!(outcome.chrome_dirty && !outcome.foreign_pane_set_dirty);
    assert!(!h.mirror.panes.contains_key(&late));
    assert_eq!(
        h.mirror
            .engine_kernel
            .agent_session(&late)
            .expect("declared")
            .parent,
        Some(&pane)
    );

    // A child of a pane this client does not hold is a peer's business.
    let outcome = h.send(event(&tid(4), spawn(tid(99))));
    assert!(outcome.attach_panes.is_empty() && outcome.foreign_pane_set_dirty);
    assert!(h.mirror.engine_kernel.agent_session(&tid(4)).is_none());
}

// ---- ADR-0147: the floating plugin overlay ----------------------------------

/// A client on one layout pane with a floating overlay spawn parked as
/// request 9.
fn floating_parked() -> H {
    let mut h = H::on(Workspace::single(tid(1)), &[&tid(1)]);
    h.mirror
        .pending_floating
        .insert(9, "Agent Board".to_owned());
    h
}

/// The overlay's spawn reply seeds a floating slot in no window, leaves the
/// layout and its focus alone, and asks for a full repaint.
#[test]
fn a_floating_spawn_reply_seeds_a_floating_slot_outside_the_layout() {
    let mut h = floating_parked();
    let before = h.mirror.workspace.clone();
    let outcome = h.send(spawned(tid(2)));
    assert!(outcome.layout_replaced && outcome.size_floating && !outcome.emit_set_metadata);
    assert_eq!(
        h.mirror.panes[&tid(2)].floating.as_deref(),
        Some("Agent Board")
    );
    assert_eq!(h.mirror.workspace, before, "no window adopts the overlay");
    assert_eq!(h.mirror.focused_resource, Some(tid(1)));
    assert_eq!(h.mirror.paint_focus(), Some(tid(2)));
    assert!(h.mirror.pending_floating.is_empty());
}

/// A refused spawn bells with the reason; an overlay racing one already open
/// is killed rather than stacked.
#[test]
fn a_floating_spawn_that_cannot_open_bells_or_is_killed() {
    let mut h = floating_parked();
    let refused = h.send(FrameKind::ResourceSpawned {
        request_id: 9,
        result: SpawnResult::Err(phux_protocol::wire::frame::SpawnError::SpawnFailed(
            "no such file".to_owned(),
        )),
    });
    assert_eq!(refused.notices.len(), 1);
    assert!(refused.notices[0].text.contains("Agent Board"));
    assert!(refused.notices[0].text.contains("no such file"));
    assert!(h.out.contains(&0x07), "bells");

    let mut h = floating_parked();
    h.send(spawned(tid(2)));
    h.mirror.pending_floating.insert(9, "Second".to_owned());
    let raced = h.send(spawned(tid(3)));
    assert_eq!(raced.kill_orphans, vec![tid(3)]);
    assert!(h.mirror.expected_closes.contains(&tid(3)));
    assert!(!h.mirror.panes.contains_key(&tid(3)));
}

/// The overlay's process exiting closes it: no notice, no detach, the layout
/// untouched, and a repaint to uncover the panes beneath.
#[test]
fn the_floating_overlay_closes_with_its_process() {
    let mut h = floating_parked();
    h.send(spawned(tid(2)));
    let outcome = h.send(closed(&tid(2), Some(1)));
    assert!(outcome.layout_replaced && !outcome.exit);
    assert!(outcome.notices.is_empty(), "{:?}", outcome.notices);
    assert!(!h.mirror.panes.contains_key(&tid(2)));
    assert_eq!(h.mirror.workspace, Workspace::single(tid(1)));
    assert_eq!(h.mirror.paint_focus(), Some(tid(1)));
}

/// A retained overlay (ADR-0124) closes on `Exited` and its Terminal is
/// killed, silently.
#[test]
fn a_retained_floating_overlay_is_killed_when_its_process_exits() {
    use phux_protocol::wire::frame::{ControlAction, ResourceLifecycle};
    let mut h = floating_parked();
    h.send(spawned(tid(2)));
    let outcome = h.send(event(
        &tid(2),
        AgentEvent::TerminalControl {
            lifecycle: ResourceLifecycle::Exited,
            exit_status: Some(0),
            input_holder: None,
            action: ControlAction::Exited,
            actor: None,
        },
    ));
    assert_eq!(outcome.kill_orphans, vec![tid(2)]);
    assert!(outcome.layout_replaced);
    assert!(h.mirror.expected_closes.contains(&tid(2)));
    assert!(!h.mirror.panes.contains_key(&tid(2)));
}

/// Under the overlay, a layout pane's output reaches its mirror but not the
/// screen; the overlay's own output paints inside its box.
#[test]
fn output_under_the_floating_overlay_waits_and_the_overlay_paints_in_its_box() {
    let (pane, overlay) = (tid(1), tid(2));
    let mut h = H::published(
        Workspace::single(pane.clone()),
        &[(&pane, 80, 24, b""), (&overlay, 80, 24, b"")],
    );
    h.mirror
        .panes
        .get_mut(&overlay)
        .expect("overlay slot")
        .floating = Some("Board".to_owned());
    let outcome = h.output(&pane, b"under the box");
    assert!(h.out.is_empty(), "{:?}", h.out_str());
    assert_eq!(outcome.authoritative_damage, vec![pane.clone()]);
    assert_eq!(h.cell(&pane, 0, 0), Some('u'));

    h.output(&overlay, b"on top");
    let inner = crate::attach::floating::floating_box(crate::attach::paint::content_rect(
        (80, 24),
        None,
        None,
    ))
    .inner;
    let painted = h.out_str();
    assert!(strip_csi(&painted).contains("on top"), "{painted:?}");
    let origin = format!("\x1b[{};{}H", inner.y + 1, inner.x + 1);
    assert!(
        painted.contains(&origin),
        "paints at the box origin: {painted:?}"
    );
}
