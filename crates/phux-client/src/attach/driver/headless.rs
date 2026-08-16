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
use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::{AttachTarget, FrameKind, Scope};

use crate::agent_meta::TERMINAL_AGENT_KEY;
use crate::attach::actions::{PendingSplit, PendingWindow};
use crate::attach::connection::Connection;
use crate::attach::outcome::AttachError;
use crate::attach::paint::{SidebarEdge, sidebar_reservation};
use crate::attach::pane_state::{PaneSlot, VcsIndex};
use crate::attach::server_frame::{AgentMetaIndex, handle_server_frame};
use crate::layout::Workspace;
use crate::layout_ops::{DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, layout_key};
use crate::predict::{Overlay, PredictionState, PredictiveConfig};
use crate::render::ChromeBreakpoints;
use crate::render::chrome::sidebar::SidebarPainter;
use phux_config::SidebarPosition;

use super::chrome::{agent_entries, window_infos};
use super::config_ui::build_status_bar_painter;
use super::session_io::{
    attach_client_caps, attach_client_name, send_attach, send_terminal_replies,
    take_terminal_replies, wait_for_attached,
};

type HeadlessHistoryGeneration = (
    TerminalId,
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
        terminal_id: &TerminalId,
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
#[allow(
    clippy::too_many_lines,
    reason = "mirrors main_loop's session-scoped local setup before one ingest-and-compose; the ~12 &mut locals would otherwise be threaded through a context struct for a single caller"
)]
pub async fn run_headless_rendered(
    socket: &Path,
    target: AttachTarget,
    cols: u16,
    rows: u16,
) -> Result<phux_core::screen::RenderedFrame, AttachError> {
    use std::time::SystemTime;

    /// Hard cap on waiting for the matching aggregate attach barrier.
    const ATTACH_READY_DEADLINE: Duration = Duration::from_secs(3);

    let client_caps = attach_client_caps(None);
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
    let mut engine_kernel = SessionKernel::with_history_config(
        GhosttyAdapter::new(negotiated.limits),
        negotiated.profile,
        history_config,
    );
    let mut kernel_effects = KernelEffectBuffer::new();
    let mut attach_id = send_attach(&mut conn, target).await?;
    let attached = wait_for_attached(&mut conn, attach_id).await?;

    let viewport_dims = (cols.max(1), rows.max(1));
    // Throwaway sink: `defer_paint = true` emits no VT, but
    // `handle_server_frame` still needs a `Write`.
    let mut sink: Vec<u8> = Vec::new();
    let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
    let mut workspace = Workspace::default();
    let mut focused_pane: Option<TerminalId> = None;
    let mut zoomed: Option<TerminalId> = None;
    let mut session_name = String::new();
    let mut status_bar = build_status_bar_painter();
    // phux-4h5a: read `[sidebar]` so `phux snapshot --rendered` shows the
    // strip exactly as a live attach would. Disabled (the default) folds to
    // `None`, keeping the rendered frame byte-identical to the pre-sidebar one.
    let headless_cfg = phux_config::loader::load().ok();
    let sidebar_cfg = headless_cfg.as_ref().map(|c| c.sidebar.clone());
    // phux-huhi: the same `[chrome]` thresholds a live attach folds in, so a
    // rendered snapshot yields the sidebar at the width the user configured.
    let headless_chrome = headless_cfg
        .as_ref()
        .map_or_else(ChromeBreakpoints::default, |c| {
            ChromeBreakpoints::from_cfg(&c.chrome)
        });
    let sidebar = sidebar_cfg.as_ref().and_then(|c| {
        sidebar_reservation(
            viewport_dims.0,
            c.enabled,
            c.width,
            match c.position {
                SidebarPosition::Right => SidebarEdge::Right,
                SidebarPosition::Left => SidebarEdge::Left,
            },
            headless_chrome.min_pane_cols,
        )
    });
    let sidebar_theme = headless_cfg
        .as_ref()
        .map_or_else(crate::render::Theme::default, |c| {
            crate::render::Theme::from_cfg(&c.theme)
        });
    let mut predict = PredictionState::new(
        PredictiveConfig::disabled(),
        viewport_dims.0,
        viewport_dims.1,
    );
    let overlay = Overlay;
    let mut pending_splits: HashMap<u32, PendingSplit> = HashMap::new();
    let mut pending_windows: HashMap<u32, PendingWindow> = HashMap::new();
    let mut layout_get_request_id: Option<u32> = None;
    // ADR-0040: one-shot `phux.agent/v1` reads so the composited window
    // labels prefer structured agent records, matching a live attach.
    let mut agent_meta = AgentMetaIndex::default();
    // phux-p4vp: pane cwd + branch memo so the composited sidebar carries
    // the same branch lines a live attach would.
    let mut vcs = VcsIndex::default();
    // phux-i0e8.2.2: headless composite dispatches no kill actions, so the
    // expected-close set stays empty; threaded for the shared signature.
    let mut expected_closes: HashSet<TerminalId> = HashSet::new();

    // Replay ATTACHED so the focused-pane + workspace bootstrap runs once.
    // phux-k0cw: no session is known yet (ATTACHED is what reports it), and
    // the headless composite never subscribes, so it never receives a layout
    // BROADCAST to adopt or reject — only the GET answer it asked for, which
    // takes the `MetadataValue` path.
    let outcome = handle_server_frame(
        &mut engine_kernel,
        &mut kernel_effects,
        &mut sink,
        attached,
        &mut panes,
        &mut workspace,
        &mut focused_pane,
        &mut zoomed,
        &mut session_name,
        None,
        status_bar.as_mut(),
        sidebar,
        viewport_dims,
        &mut predict,
        &overlay,
        layout_get_request_id,
        &mut pending_splits,
        &mut pending_windows,
        &mut expected_closes,
        &mut agent_meta,
        false,
        true,
    )?;
    vcs.apply_snapshot(outcome.pane_cwds);
    let focused_session = outcome.sessions.map(|(_, focused)| focused);

    // ADR-0040: pipeline one `phux.agent/v1` GET per pane (no SUBSCRIBE —
    // this is a one-shot composite). Replies drain through the settle loop
    // below and land in `agent_meta.records`. Request ids start high above
    // the layout GET's `1` so the two reply streams cannot collide.
    {
        let mut req_id: u32 = 1000;
        for id in panes.keys() {
            agent_meta.pending.insert(req_id, id.clone());
            conn.send(&FrameKind::GetMetadata {
                request_id: req_id,
                scope: Scope::Terminal(id.clone()),
                key: TERMINAL_AGENT_KEY.to_owned(),
            })
            .await?;
            req_id = req_id.wrapping_add(1);
        }
    }

    // Pull any persisted multi-pane layout for this session so dividers +
    // tiling match a live attach. One-shot, so we GET but do not SUBSCRIBE.
    if outcome.subscribe_layout
        && let Some(session) = focused_session
    {
        let req_id = 1;
        layout_get_request_id = Some(req_id);
        conn.send(&FrameKind::GetMetadata {
            request_id: req_id,
            scope: Scope::Group(DEFAULT_GROUP_ID),
            key: layout_key(session),
        })
        .await?;
    }

    // A rendered snapshot is valid only after the server's aggregate barrier
    // and all work it unlocked has drained. ATTACH_READY can be queued before
    // the requested post-READY history pages or one-shot metadata replies.
    let mut completion = HeadlessCompletion::new(layout_get_request_id);
    tokio::time::timeout(ATTACH_READY_DEADLINE, async {
        loop {
            let frame = conn.recv().await?;
            completion.observe_frame(&frame, attach_id);
            let mut outcome = handle_server_frame(
                &mut engine_kernel,
                &mut kernel_effects,
                &mut sink,
                frame,
                &mut panes,
                &mut workspace,
                &mut focused_pane,
                &mut zoomed,
                &mut session_name,
                focused_session,
                status_bar.as_mut(),
                sidebar,
                viewport_dims,
                &mut predict,
                &overlay,
                layout_get_request_id,
                &mut pending_splits,
                &mut pending_windows,
                &mut expected_closes,
                &mut agent_meta,
                false,
                true,
            )?;
            send_terminal_replies(
                &mut conn,
                take_terminal_replies(&mut outcome, terminal_reply_supported),
            )
            .await?;
            if outcome.resync_required {
                if session_name.is_empty() {
                    return Err(AttachError::Protocol(
                        "engine requested rebootstrap before ATTACHED named the session".to_owned(),
                    ));
                }
                attach_id =
                    send_attach(&mut conn, AttachTarget::ByName(session_name.clone())).await?;
                completion.restart_attach();
                continue;
            }
            if let Some((
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                max_bytes,
                max_rows,
            )) = outcome.history_request
            {
                completion.note_history_request(&terminal_id, stream_id, bootstrap_id);
                conn.send(&FrameKind::HistoryRequest {
                    terminal_id,
                    stream_id,
                    bootstrap_id,
                    cursor,
                    max_bytes,
                    max_rows,
                })
                .await?;
            }
            if completion.is_complete(agent_meta.pending.is_empty()) {
                break;
            }
        }
        Ok::<(), AttachError>(())
    })
    .await
    .map_err(|_| {
        AttachError::Protocol(format!(
            "headless attach {attach_id} timed out before ATTACH_READY, history, and metadata completed"
        ))
    })??;

    // Seed the window/tab strip exactly as the live loop does before its
    // first bar paint, so the composited bar shows the windows.
    let windows = window_infos(
        &workspace,
        &panes,
        zoomed.as_ref(),
        &agent_meta.records,
        &mut vcs,
    );
    if let Some(sb) = status_bar.as_mut() {
        sb.set_windows(windows.clone());
    }
    // phux-4h5a: feed the same window list into the strip painter so the
    // composited frame shows the sidebar tabs when `[sidebar]` is enabled.
    let mut sidebar_painter = SidebarPainter::new(sidebar_theme);
    sidebar_painter.set_windows(windows);
    // phux-foz.9: and the attention queue, from the same record index +
    // title fallback a live attach renders.
    //
    // phux-k0cw: LOCAL rows only, and no roster at all. A capture must be
    // reproducible from one session's state; sweeping the server for peer
    // layouts would make the same command emit different bytes depending on
    // what else happened to be running at the time. The composite has no
    // subscriptions and no event loop to keep such a sweep honest anyway.
    sidebar_painter.set_needs_you(agent_entries(&workspace, &panes, &agent_meta));

    // Compose the assembled frame against the render layout (honoring zoom).
    let layout_state = workspace.render_window(zoomed.as_ref()).map_or_else(
        crate::layout::LayoutState::default,
        std::borrow::Cow::into_owned,
    );
    let frame = crate::attach::rendered::compose_full_frame_cells(
        &layout_state,
        &mut panes,
        &engine_kernel,
        focused_pane.as_ref(),
        viewport_dims,
        status_bar.as_ref(),
        sidebar,
        Some(&sidebar_painter),
        &session_name,
        SystemTime::now(),
    );
    Ok(frame)
}
