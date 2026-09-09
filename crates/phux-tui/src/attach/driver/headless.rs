//! The headless one-shot composite (`phux snapshot --rendered`) and its
//! completion barrier.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use phux_client_core::engine::ghostty::GhosttyAdapter;
use phux_client_core::history::HistoryCacheConfig;
use phux_client_core::session::{EffectBuffer as KernelEffectBuffer, SessionKernel};
#[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
use phux_protocol::caps::BootstrapCapabilities;
use phux_protocol::caps::ServerFeature;
use phux_protocol::ids::{ResourceId, SessionId};
use phux_protocol::wire::frame::{AttachTarget, FrameKind, Scope};

use crate::attach::actions::{PendingSplit, PendingWindow};
use crate::attach::connection::Connection;
use crate::attach::outcome::AttachError;
use crate::attach::paint::{SidebarReservation, sidebar_reservation};
use crate::attach::pane_state::{PaneSlot, VcsIndex};
use crate::attach::server_frame::{AgentMetaIndex, FrameOutcome, handle_server_frame};
use crate::layout::Workspace;
use crate::predict::{Overlay, PredictionState, PredictiveConfig};
use crate::render::chrome::sidebar::SidebarPainter;
use crate::render::chrome::status_bar::StatusBarPainter;
use phux_client::agent_meta::RESOURCE_AGENT_KEY;
use phux_client::layout_ops::{DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, layout_key};

use super::chrome::{agent_entries, window_infos};
use super::session_io::{
    attach_client_caps, attach_client_name, send_attach, send_terminal_replies,
    take_terminal_replies, wait_for_attached,
};
use crate::settings::TuiSettings;

type HeadlessHistoryGeneration = (
    ResourceId,
    phux_protocol::StreamId,
    phux_protocol::BootstrapId,
);

#[derive(Debug, Default)]
pub(super) struct HeadlessCompletion {
    attach_ready: bool,
    pending_history: HashSet<HeadlessHistoryGeneration>,
    pending_layout: Option<u32>,
}

impl HeadlessCompletion {
    pub(super) fn new(pending_layout: Option<u32>) -> Self {
        Self {
            pending_layout,
            ..Self::default()
        }
    }

    pub(super) fn observe_frame(&mut self, frame: &FrameKind, attach_id: u32) {
        match frame {
            FrameKind::AttachReady {
                attach_id: ready_id,
            } if *ready_id == attach_id => self.attach_ready = true,
            FrameKind::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
                next_cursor: None,
                ..
            }
            | FrameKind::HistoryTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            }
            | FrameKind::HistoryRejected {
                terminal_id,
                stream_id,
                bootstrap_id,
                ..
            } => {
                self.pending_history
                    .remove(&(terminal_id.clone(), *stream_id, *bootstrap_id));
            }
            FrameKind::MetadataValue { request_id, .. }
                if self.pending_layout == Some(*request_id) =>
            {
                self.pending_layout = None;
            }
            _ => {}
        }
    }

    pub(super) fn note_history_request(
        &mut self,
        terminal_id: &ResourceId,
        stream_id: phux_protocol::StreamId,
        bootstrap_id: phux_protocol::BootstrapId,
    ) {
        self.pending_history
            .insert((terminal_id.clone(), stream_id, bootstrap_id));
    }
    pub(super) fn restart_attach(&mut self) {
        self.attach_ready = false;
        self.pending_history.clear();
    }

    pub(super) fn is_complete(&self, agent_metadata_complete: bool) -> bool {
        self.attach_ready
            && self.pending_history.is_empty()
            && self.pending_layout.is_none()
            && agent_metadata_complete
    }
}

/// The next opaque native history page to pull, as the frame dispatcher
/// reports it.
type HistoryPageRequest = (
    ResourceId,
    phux_protocol::StreamId,
    phux_protocol::BootstrapId,
    bytes::Bytes,
    u32,
    u32,
);

/// The config-derived chrome a rendered snapshot composites against.
struct HeadlessChrome {
    /// The columns the window sidebar reserves, or `None` when it is off.
    sidebar: Option<SidebarReservation>,
    /// The theme the sidebar strip paints with.
    sidebar_theme: crate::render::Theme,
    /// The status-bar painter, absent when the config disables it.
    status_bar: Option<StatusBarPainter>,
}

/// Fold `[sidebar]`, `[chrome]`, `[theme]`, and `[status]` in exactly as a
/// live attach does: the same tolerant [`TuiSettings`] load.
///
/// phux-4h5a: read `[sidebar]` so `phux snapshot --rendered` shows the
/// strip exactly as a live attach would. Disabled (the default) folds to
/// `None`, keeping the rendered frame byte-identical to the pre-sidebar one.
/// phux-huhi: the same `[chrome]` thresholds a live attach folds in, so a
/// rendered snapshot yields the sidebar at the width the user configured.
fn headless_chrome(viewport_dims: (u16, u16)) -> HeadlessChrome {
    let settings = TuiSettings::load_tolerant();
    let sidebar = sidebar_reservation(
        viewport_dims.0,
        settings.sidebar.enabled,
        settings.sidebar.width,
        settings.sidebar.edge,
        settings.chrome.min_pane_cols,
    );
    HeadlessChrome {
        sidebar,
        sidebar_theme: settings.theme,
        status_bar: settings.status_bar,
    }
}

/// The session-scoped state the headless composite ingests frames into.
///
/// The live attach loop keeps the same set as `main_loop` locals; the
/// composite holds them in one place because every frame it drains goes
/// through the same twenty-two-argument `handle_server_frame` call, twice.
struct HeadlessSession {
    /// The client-side libghostty session kernel the frames feed.
    engine_kernel: SessionKernel<GhosttyAdapter>,
    /// Effects the kernel emits per ingested frame.
    kernel_effects: KernelEffectBuffer,
    /// Throwaway sink: `defer_paint = true` emits no VT, but
    /// `handle_server_frame` still needs a `Write`.
    sink: Vec<u8>,
    /// The pane mirrors, keyed by Terminal.
    panes: HashMap<ResourceId, PaneSlot>,
    /// The multi-pane layout the composite tiles against.
    workspace: Workspace,
    /// The pane the composited frame draws as focused.
    focused_resource: Option<ResourceId>,
    /// The zoomed pane, if the layout carries one.
    zoomed: Option<ResourceId>,
    /// The session name, learned from ATTACHED.
    session_name: String,
    /// The status-bar painter, absent when the config disables it.
    status_bar: Option<StatusBarPainter>,
    /// The sidebar reservation the panes tile inside of.
    sidebar: Option<SidebarReservation>,
    /// The theme the sidebar strip paints with.
    sidebar_theme: crate::render::Theme,
    /// The caller-supplied viewport; there is no TTY to ask.
    viewport_dims: (u16, u16),
    /// Prediction state, disabled for a one-shot composite.
    predict: PredictionState,
    /// The overlay stack, empty for a one-shot composite.
    overlay: Overlay,
    /// Splits this client asked for, keyed by request id.
    pending_splits: HashMap<u32, PendingSplit>,
    /// Windows this client asked for, keyed by request id.
    pending_windows: HashMap<u32, PendingWindow>,
    /// phux-i0e8.2.2: headless composite dispatches no kill actions, so the
    /// expected-close set stays empty; threaded for the shared signature.
    expected_closes: HashSet<ResourceId>,
    /// ADR-0040: one-shot `phux.agent/v1` reads so the composited window
    /// labels prefer structured agent records, matching a live attach.
    agent_meta: AgentMetaIndex,
    /// phux-p4vp: pane cwd + branch memo so the composited sidebar carries
    /// the same branch lines a live attach would.
    vcs: VcsIndex,
}

impl HeadlessSession {
    /// Seed the composite's state around an already-negotiated kernel.
    fn new(
        engine_kernel: SessionKernel<GhosttyAdapter>,
        chrome: HeadlessChrome,
        viewport_dims: (u16, u16),
    ) -> Self {
        Self {
            engine_kernel,
            kernel_effects: KernelEffectBuffer::new(),
            sink: Vec::new(),
            panes: HashMap::new(),
            workspace: Workspace::default(),
            focused_resource: None,
            zoomed: None,
            session_name: String::new(),
            status_bar: chrome.status_bar,
            sidebar: chrome.sidebar,
            sidebar_theme: chrome.sidebar_theme,
            viewport_dims,
            predict: PredictionState::new(
                PredictiveConfig::disabled(),
                viewport_dims.0,
                viewport_dims.1,
            ),
            overlay: Overlay,
            pending_splits: HashMap::new(),
            pending_windows: HashMap::new(),
            expected_closes: HashSet::new(),
            agent_meta: AgentMetaIndex::default(),
            vcs: VcsIndex::default(),
        }
    }

    /// Feed one frame through the same dispatcher the live attach loop uses.
    ///
    /// `defer_paint = true` throughout: the pane mirrors ingest, and stdout
    /// stays silent until the single compose pass.
    fn ingest(
        &mut self,
        frame: FrameKind,
        focused_session: Option<SessionId>,
        layout_get_request_id: Option<u32>,
    ) -> Result<FrameOutcome, AttachError> {
        handle_server_frame(
            &mut self.engine_kernel,
            &mut self.kernel_effects,
            &mut self.sink,
            frame,
            &mut self.panes,
            &mut self.workspace,
            &mut self.focused_resource,
            &mut self.zoomed,
            &mut self.session_name,
            focused_session,
            self.status_bar.as_mut(),
            self.sidebar,
            self.viewport_dims,
            &mut self.predict,
            &self.overlay,
            layout_get_request_id,
            &mut self.pending_splits,
            &mut self.pending_windows,
            &mut self.expected_closes,
            &mut self.agent_meta,
            false,
            true,
        )
    }

    /// ADR-0040: pipeline one `phux.agent/v1` GET per pane (no SUBSCRIBE —
    /// this is a one-shot composite). Replies drain through the settle loop
    /// and land in `agent_meta.records`. Request ids start high above the
    /// layout GET's `1` so the two reply streams cannot collide.
    #[allow(
        clippy::future_not_send,
        reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
    )]
    async fn request_agent_records(&mut self, conn: &mut Connection) -> Result<(), AttachError> {
        let mut req_id: u32 = 1000;
        for id in self.panes.keys() {
            self.agent_meta.pending.insert(req_id, id.clone());
            conn.send(&FrameKind::GetMetadata {
                request_id: req_id,
                scope: Scope::Resource(id.clone()),
                key: RESOURCE_AGENT_KEY.to_owned(),
            })
            .await?;
            req_id = req_id.wrapping_add(1);
        }
        Ok(())
    }

    /// Whether every one-shot `phux.agent/v1` reply has landed.
    fn agent_metadata_complete(&self) -> bool {
        self.agent_meta.pending.is_empty()
    }

    /// Seed the window/tab strip exactly as the live loop does before its
    /// first bar paint, then compose the assembled frame against the render
    /// layout (honoring zoom).
    fn compose(&mut self) -> phux_core::screen::RenderedFrame {
        use std::time::SystemTime;

        let windows = window_infos(
            &self.workspace,
            &self.panes,
            self.zoomed.as_ref(),
            &self.agent_meta.records,
            &mut self.vcs,
        );
        if let Some(sb) = self.status_bar.as_mut() {
            sb.set_windows(windows.clone());
        }
        // phux-4h5a: feed the same window list into the strip painter so the
        // composited frame shows the sidebar tabs when `[sidebar]` is enabled.
        let mut sidebar_painter = SidebarPainter::new(self.sidebar_theme);
        sidebar_painter.set_windows(windows);
        // phux-foz.9: and the attention queue, from the same record index +
        // title fallback a live attach renders.
        //
        // phux-k0cw: LOCAL rows only, and no roster at all. A capture must be
        // reproducible from one session's state; sweeping the server for peer
        // layouts would make the same command emit different bytes depending on
        // what else happened to be running at the time. The composite has no
        // subscriptions and no event loop to keep such a sweep honest anyway.
        sidebar_painter.set_needs_you(agent_entries(
            &self.workspace,
            &self.panes,
            &self.agent_meta,
            &crate::attach::agent_rows::agent_session_rows(&self.engine_kernel),
        ));

        let layout_state = self
            .workspace
            .render_window(self.zoomed.as_ref())
            .map_or_else(
                crate::layout::LayoutState::default,
                std::borrow::Cow::into_owned,
            );
        crate::attach::rendered::compose_full_frame_cells(
            &layout_state,
            &mut self.panes,
            &self.engine_kernel,
            self.focused_resource.as_ref(),
            self.viewport_dims,
            self.status_bar.as_ref(),
            self.sidebar,
            Some(&sidebar_painter),
            &self.session_name,
            SystemTime::now(),
            &crate::render::theme::Theme::default(),
        )
    }
}

/// Pull any persisted multi-pane layout for this session so dividers +
/// tiling match a live attach. One-shot, so we GET but do not SUBSCRIBE.
///
/// Returns the request id the completion barrier waits on, or `None` when
/// there is no layout to ask for.
async fn request_layout(
    conn: &mut Connection,
    subscribe_layout: bool,
    focused_session: Option<SessionId>,
) -> Result<Option<u32>, AttachError> {
    if !subscribe_layout {
        return Ok(None);
    }
    let Some(session) = focused_session else {
        return Ok(None);
    };
    let req_id = 1;
    conn.send(&FrameKind::GetMetadata {
        request_id: req_id,
        scope: Scope::Group(DEFAULT_GROUP_ID),
        key: layout_key(session),
    })
    .await?;
    Ok(Some(req_id))
}

/// Re-attach after the engine asked for a rebootstrap, restarting the
/// completion barrier under the new attach id.
async fn restart_attach(
    conn: &mut Connection,
    session_name: &str,
    completion: &mut HeadlessCompletion,
) -> Result<u32, AttachError> {
    if session_name.is_empty() {
        return Err(AttachError::Protocol(
            "engine requested rebootstrap before ATTACHED named the session".to_owned(),
        ));
    }
    let attach_id = send_attach(conn, AttachTarget::ByName(session_name.to_owned())).await?;
    completion.restart_attach();
    Ok(attach_id)
}

/// Ask for the history page the engine requested, recording the cursor chain
/// the completion barrier then waits on.
async fn request_history_page(
    conn: &mut Connection,
    completion: &mut HeadlessCompletion,
    request: HistoryPageRequest,
) -> Result<(), AttachError> {
    let (terminal_id, stream_id, bootstrap_id, cursor, max_bytes, max_rows) = request;
    completion.note_history_request(&terminal_id, stream_id, bootstrap_id);
    conn.send(&FrameKind::HistoryRequest {
        terminal_id,
        stream_id,
        bootstrap_id,
        cursor,
        max_bytes,
        max_rows,
    })
    .await
}

/// Drain frames until the completion barrier reports the composite whole.
///
/// A rendered snapshot is valid only after the server's aggregate barrier
/// and all work it unlocked has drained. `ATTACH_READY` can be queued before
/// the requested post-READY history pages or one-shot metadata replies.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
async fn drain_until_settled(
    conn: &mut Connection,
    session: &mut HeadlessSession,
    completion: &mut HeadlessCompletion,
    attach_id: &mut u32,
    focused_session: Option<SessionId>,
    layout_get_request_id: Option<u32>,
    terminal_reply_supported: bool,
) -> Result<(), AttachError> {
    loop {
        let frame = conn.recv().await?;
        completion.observe_frame(&frame, *attach_id);
        let mut outcome = session.ingest(frame, focused_session, layout_get_request_id)?;
        send_terminal_replies(
            conn,
            take_terminal_replies(&mut outcome, terminal_reply_supported),
        )
        .await?;
        if outcome.resync_required {
            *attach_id = restart_attach(conn, &session.session_name, completion).await?;
            continue;
        }
        if let Some(request) = outcome.history_request {
            request_history_page(conn, completion, request).await?;
        }
        if completion.is_complete(session.agent_metadata_complete()) {
            return Ok(());
        }
    }
}

/// Headless one-shot: attach, ingest the session's snapshot + layout, and
/// return the client's composited multi-pane view as dense structured cells
/// (`phux snapshot --rendered`, phux-l5xa).
///
/// Unlike the side-effect-free `GET_SCREEN` read, this **attaches** (R2): it
/// drives the same client render path the live attach loop uses, so the
/// returned frame is what the human's glass would show — pane content tiled
/// per the layout, dividers, and the status bar, composited. But it never
/// installs raw mode or an alt screen and never paints VT: frames feed the
/// pane mirrors with `defer_paint = true` (mirrors ingest, stdout is
/// suppressed), then ONE `rendered::compose_full_frame_cells` pass
/// assembles the frame. There is no TTY, so the viewport `(cols, rows)` is
/// caller-supplied.
///
/// Completion policy (R3): after the ATTACHED replay and one-shot metadata
/// requests, frames are drained until the matching `ATTACH_READY`, every
/// requested history cursor chain, and every required metadata reply complete.
/// The overall deadline is an error, never partial or blank success.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
pub async fn run_headless_rendered(
    socket: &Path,
    target: AttachTarget,
    cols: u16,
    rows: u16,
) -> Result<phux_core::screen::RenderedFrame, AttachError> {
    /// Hard cap on waiting for the matching aggregate attach barrier.
    const ATTACH_READY_DEADLINE: Duration = Duration::from_secs(3);

    // Headless rendering is a local-socket path by signature, so it offers no
    // compression — see `attach_client_caps`.
    let client_caps = attach_client_caps(None, &crate::attach::Dial::uds(socket));
    let mut conn =
        Connection::connect_with_hello(socket, attach_client_name(), client_caps).await?;
    let negotiated = conn.negotiated_bootstrap().ok_or_else(|| {
        AttachError::Protocol("headless attach lacks negotiated bootstrap".to_owned())
    })?;
    let terminal_reply_supported = negotiated
        .server_features
        .contains(ServerFeature::TerminalReply);
    let history_config = HistoryCacheConfig {
        request_max_bytes: negotiated.limits.max_history_page_bytes(),
        ..HistoryCacheConfig::default()
    };
    let engine_kernel = SessionKernel::with_history_config(
        GhosttyAdapter::new(negotiated.limits),
        negotiated.profile,
        history_config,
    );
    let mut attach_id = send_attach(&mut conn, target).await?;
    let attached = wait_for_attached(&mut conn, attach_id).await?;

    let viewport_dims = (cols.max(1), rows.max(1));
    let mut session =
        HeadlessSession::new(engine_kernel, headless_chrome(viewport_dims), viewport_dims);

    // Replay ATTACHED so the focused-pane + workspace bootstrap runs once.
    // phux-k0cw: no session is known yet (ATTACHED is what reports it), and
    // the headless composite never subscribes, so it never receives a layout
    // BROADCAST to adopt or reject — only the GET answer it asked for, which
    // takes the `MetadataValue` path.
    let outcome = session.ingest(attached, None, None)?;
    session.vcs.apply_snapshot(outcome.pane_cwds);
    let focused_session = outcome.sessions.map(|(_, focused)| focused);

    session.request_agent_records(&mut conn).await?;
    let layout_get_request_id =
        request_layout(&mut conn, outcome.subscribe_layout, focused_session).await?;

    let mut completion = HeadlessCompletion::new(layout_get_request_id);
    let settled = tokio::time::timeout(
        ATTACH_READY_DEADLINE,
        drain_until_settled(
            &mut conn,
            &mut session,
            &mut completion,
            &mut attach_id,
            focused_session,
            layout_get_request_id,
            terminal_reply_supported,
        ),
    )
    .await;
    settled.map_err(|_| {
        AttachError::Protocol(format!(
            "headless attach {attach_id} timed out before ATTACH_READY, history, and metadata completed"
        ))
    })??;

    Ok(session.compose())
}
