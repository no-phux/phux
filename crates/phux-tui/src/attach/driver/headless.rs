//! The headless one-shot composite (`phux snapshot --rendered`) and its
//! completion barrier.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use phux_client_core::engine::ghostty::GhosttyAdapter;
use phux_client_core::history::HistoryCacheConfig;
use phux_client_core::session::SessionKernel;
use phux_protocol::ids::{ResourceId, SessionId};
use phux_protocol::wire::frame::{AttachTarget, FrameKind, Scope};

use crate::attach::chrome_ctx::ChromeCtx;
use crate::attach::connection::Connection;
use crate::attach::outcome::AttachError;
use crate::attach::paint::{SidebarReservation, sidebar_reservation};
use crate::attach::pane_state::VcsIndex;
use crate::attach::server_frame::{FrameEnv, FrameOutcome, handle_server_frame};
use crate::attach::session_mirror::SessionMirror;
use crate::predict::{PredictionState, PredictiveConfig};
use crate::render::chrome::sidebar::SidebarPainter;
use crate::render::chrome::status_bar::StatusBarPainter;
use phux_client::agent_meta::RESOURCE_AGENT_KEY;
use phux_client::layout_ops::{DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID, layout_key};

use super::chrome::{agent_entries, window_infos};
use super::session_io::{
    attach_client_caps, attach_client_name, send_attach_without_size_vote, wait_for_attached,
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
            // Any answer settles the outstanding request. A page with a next
            // cursor stays pending only if the engine asks for that cursor
            // (`note_history_request` re-arms it): the client pulls history
            // lazily (ADR-0119) and stops once its prefetch window is full,
            // so waiting for the chain's end would time out on deep history.
            FrameKind::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
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
    /// The configured `[theme]`: the sidebar strip and the dividers paint
    /// with it, as on the glass.
    theme: crate::render::Theme,
    /// The status-bar painter, absent when the config disables it.
    status_bar: Option<StatusBarPainter>,
}

/// Fold `[sidebar]`, `[chrome]`, `[theme]`, and `[status]` in through the same
/// tolerant [`TuiSettings`] load a live attach uses, so a rendered snapshot
/// matches the glass.
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
        theme: settings.theme,
        status_bar: settings.status_bar,
    }
}

/// The session-scoped state the headless composite ingests frames into (the
/// live loop keeps the same [`SessionMirror`] on `SessionLoop`).
struct HeadlessSession {
    /// The mirror the frames fold into. Prediction is disabled and no kill
    /// is ever dispatched, so its predictor and close set stay empty.
    mirror: SessionMirror,
    /// Throwaway sink: `defer_paint = true` emits no VT, but
    /// `handle_server_frame` still needs a `Write`.
    sink: Vec<u8>,
    /// The status-bar painter, absent when the config disables it.
    status_bar: Option<StatusBarPainter>,
    /// The sidebar reservation the panes tile inside of.
    sidebar: Option<SidebarReservation>,
    /// The configured `[theme]` the chrome paints with.
    theme: crate::render::Theme,
    /// The caller-supplied viewport; there is no TTY to ask.
    viewport_dims: (u16, u16),
    /// Pane cwd + branch memo so the composited sidebar carries
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
        let predict = PredictionState::new(
            PredictiveConfig::disabled(),
            viewport_dims.0,
            viewport_dims.1,
        );
        Self {
            mirror: SessionMirror::new(engine_kernel, predict),
            sink: Vec::new(),
            status_bar: chrome.status_bar,
            sidebar: chrome.sidebar,
            theme: chrome.theme,
            viewport_dims,
            vcs: VcsIndex::default(),
        }
    }

    /// Feed one frame through the live dispatcher with `defer_paint = true`:
    /// mirrors ingest, stdout stays silent until the single compose pass.
    fn ingest(
        &mut self,
        frame: FrameKind,
        focused_session: Option<SessionId>,
        layout_get_request_id: Option<u32>,
    ) -> Result<FrameOutcome, AttachError> {
        let env = FrameEnv {
            focused_session,
            status_bar: self.status_bar.as_mut(),
            sidebar: self.sidebar,
            viewport_dims: self.viewport_dims,
            pending_layout_request: layout_get_request_id,
            overlay_active: false,
            defer_paint: true,
        };
        handle_server_frame(&mut self.mirror, env, &mut self.sink, frame)
    }

    /// ADR-0040: one `phux.agent/v1` GET per pane (no SUBSCRIBE), with request
    /// ids far above the layout GET's so the replies cannot collide.
    #[allow(
        clippy::future_not_send,
        reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
    )]
    async fn request_agent_records(&mut self, conn: &mut Connection) -> Result<(), AttachError> {
        let mut req_id: u32 = 1000;
        for id in self.mirror.panes.keys() {
            self.mirror.agent_meta.pending.insert(req_id, id.clone());
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
        self.mirror.agent_meta.pending.is_empty()
    }

    /// Seed the window/tab strip exactly as the live loop does before its
    /// first bar paint, then compose the assembled frame against the render
    /// layout (honoring zoom).
    fn compose(&mut self) -> phux_core::screen::RenderedFrame {
        use std::time::SystemTime;

        let mirror = &mut self.mirror;
        let mut windows = window_infos(
            &mirror.workspace,
            &mirror.panes,
            mirror.zoomed.as_ref(),
            &mirror.agent_meta.records,
            &mut self.vcs,
        );
        let local = agent_entries(
            &mirror.workspace,
            &mirror.panes,
            &mirror.agent_meta,
            &crate::attach::agent_rows::agent_session_rows(&mirror.engine_kernel),
            &crate::attach::review::ReviewIndex::new(),
        );
        super::chrome::badge_windows(&mut windows, &mirror.workspace, &local, &self.theme);
        if let Some(sb) = self.status_bar.as_mut() {
            sb.set_windows(windows.clone());
        }
        // Feed the same window list into the strip painter so the
        // composited frame shows the sidebar tabs when `[sidebar]` is enabled.
        let mut sidebar_painter = SidebarPainter::new(self.theme);
        sidebar_painter.set_windows(windows);
        let mut session = crate::render::chrome::sidebar::SessionRosterEntry {
            name: mirror.session_name.clone(),
            host: "this server".to_owned(),
            active: true,
            selectable: true,
            ..Default::default()
        };
        crate::attach::sidebar_zones::summarize_local_agents(&mut session, &local);
        sidebar_painter.set_roster(vec![session]);
        // Local rows and the current session only: a capture must not depend
        // on what else happened to be running on the server.
        sidebar_painter.set_needs_you(local);

        let layout_state = mirror
            .workspace
            .render_window(mirror.zoomed.as_ref())
            .map_or_else(
                crate::layout::LayoutState::default,
                std::borrow::Cow::into_owned,
            );
        let chrome = ChromeCtx {
            viewport: self.viewport_dims,
            sidebar: self.sidebar,
            status_bar: self.status_bar.as_mut(),
            sidebar_painter: Some(&mut sidebar_painter),
            session_name: &mirror.session_name,
            theme: &self.theme,
        };
        crate::attach::rendered::compose_full_frame_cells(
            &chrome,
            &layout_state,
            &mut mirror.panes,
            &mirror.engine_kernel,
            mirror.focused_resource.as_ref(),
            SystemTime::now(),
        )
    }
}

/// GET (not SUBSCRIBE) this session's persisted layout; returns the request
/// id the completion barrier waits on, or `None` with nothing to ask for.
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
    let attach_id =
        send_attach_without_size_vote(conn, AttachTarget::ByName(session_name.to_owned())).await?;
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

/// Drain frames until the completion barrier reports the composite whole:
/// `ATTACH_READY` can precede the history pages and metadata it unlocked.
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
) -> Result<(), AttachError> {
    loop {
        let frame = conn.recv().await?;
        completion.observe_frame(&frame, *attach_id);
        let outcome = session.ingest(frame, focused_session, layout_get_request_id)?;
        if outcome.resync_required {
            *attach_id = restart_attach(conn, &session.mirror.session_name, completion).await?;
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

/// Headless one-shot (`phux snapshot --rendered`): the composited view as cells.
///
/// Attaches and ingests the session and its layout through the live client
/// render path. No raw mode, no alt screen, no
/// VT: mirrors ingest with `defer_paint`, then one compose pass at the
/// caller's viewport. Completion waits for `ATTACH_READY`, every requested
/// history chain, and every metadata reply; the deadline is an error, never
/// a partial frame.
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
    let history_config = HistoryCacheConfig {
        request_max_bytes: negotiated.limits.max_history_page_bytes(),
        ..HistoryCacheConfig::default()
    };
    let engine_kernel = SessionKernel::with_history_config(
        GhosttyAdapter::new(negotiated.limits),
        negotiated.profile,
        history_config,
    );
    let mut attach_id = send_attach_without_size_vote(&mut conn, target).await?;
    let attached = wait_for_attached(&mut conn, attach_id).await?;

    let viewport_dims = (cols.max(1), rows.max(1));
    let mut session =
        HeadlessSession::new(engine_kernel, headless_chrome(viewport_dims), viewport_dims);

    // Replay ATTACHED once. The composite never subscribes, so its only
    // layout input is the GET answer it asked for.
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
        ),
    )
    .await;
    drop(conn);
    settled.map_err(|_| {
        AttachError::Protocol(format!(
            "headless attach {attach_id} timed out before ATTACH_READY, history, and metadata completed"
        ))
    })??;

    Ok(session.compose())
}

#[cfg(test)]
mod sidebar_tests {
    use super::*;

    #[test]
    fn headless_blocked_agent_has_matching_session_summary() {
        use phux_client::agent_meta::{AgentMetaState, AgentRecord};
        let kernel = SessionKernel::new(
            GhosttyAdapter::new(phux_protocol::BootstrapLimits::default()),
            phux_protocol::BootstrapProfile::SynthesizedVtRaw,
        );
        let chrome = HeadlessChrome {
            sidebar: Some(SidebarReservation {
                edge: crate::attach::paint::SidebarEdge::Left,
                width: 32,
            }),
            theme: crate::render::Theme::default(),
            status_bar: None,
        };
        let mut session = HeadlessSession::new(kernel, chrome, (100, 24));
        let id = ResourceId::local(1);
        session.mirror.workspace = crate::layout::Workspace::single(id.clone());
        session.mirror.session_name = "work".to_owned();
        session.mirror.agent_meta.records.insert(
            id,
            AgentRecord {
                name: "reviewer".to_owned(),
                state: AgentMetaState::Blocked,
                ..Default::default()
            },
        );
        let frame = session.compose();
        let rows: Vec<String> = frame
            .cells
            .chunks(100)
            .map(|row| {
                row[..32]
                    .iter()
                    .map(|cell| cell.grapheme.as_str())
                    .collect()
            })
            .collect();
        assert!(rows[1].contains("reviewer"), "{rows:?}");
        assert!(rows[12].contains("this server"), "{rows:?}");
        assert!(
            rows[13].contains("work") && rows[13].contains("●1"),
            "{rows:?}"
        );
    }

    /// The composite's dividers paint with the configured theme, as the
    /// glass's do, not the built-in default.
    #[test]
    fn headless_dividers_use_the_configured_theme() {
        use phux_core::screen::CellColor;
        use ratatui::style::Color;
        let kernel = SessionKernel::new(
            GhosttyAdapter::new(phux_protocol::BootstrapLimits::default()),
            phux_protocol::BootstrapProfile::SynthesizedVtRaw,
        );
        let theme = crate::render::Theme {
            divider: Color::Rgb(1, 2, 3),
            divider_focus: Color::Rgb(4, 5, 6),
            ..crate::render::Theme::default()
        };
        let chrome = HeadlessChrome {
            sidebar: None,
            theme,
            status_bar: None,
        };
        let mut session = HeadlessSession::new(kernel, chrome, (80, 24));
        let (left, right) = (ResourceId::local(1), ResourceId::local(2));
        let mut workspace = crate::layout::Workspace::single(left.clone());
        let tree = workspace
            .active_window()
            .and_then(|window| window.tree.clone())
            .expect("tree");
        workspace.active_window_mut().expect("window").tree = Some(
            crate::layout::split_at(
                &tree,
                &left,
                &right,
                crate::layout::SplitDir::Horizontal,
                0.5,
            )
            .expect("split"),
        );
        session.mirror.workspace = workspace;
        let frame = session.compose();
        let themed = |cell: &phux_core::screen::RenderedCell| {
            matches!(
                cell.style.fg,
                CellColor::Rgb { r: 1, g: 2, b: 3 } | CellColor::Rgb { r: 4, g: 5, b: 6 }
            )
        };
        assert!(
            frame.cells.iter().any(themed),
            "no divider cell carries the configured theme"
        );
    }
}
