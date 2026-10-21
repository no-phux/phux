//! `ATTACHED` and each pane's bootstrap: the session graph and first geometry.

use std::collections::HashSet;

use phux_protocol::ids::{ClientId, ResourceId};
use phux_protocol::wire::frame::SessionLifetime;

use super::windows::focused_session_name;
use super::{FrameCtx, FrameOutcome, KernelRoute, is_terminal};
use crate::attach::outcome::AttachError;
use crate::attach::paint::content_rect;
use crate::attach::pane_state::{PaneSlot, published_terminal};
use crate::attach::session_mirror::session_lifetime;
use crate::layout::{ArrangementDoc, Rect};

/// `ATTACHED` carries the session graph; cells arrive via each bootstrap.
/// The outcome asks the driver to fetch and subscribe the layout key.
pub(super) fn handle_attached<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
    initial_client_id: ClientId,
) -> Result<FrameOutcome, AttachError> {
    // The focused session's lifetime and members decide what an emptied
    // arrangement does, and an empty session has no pane to focus at all.
    let focused = snapshot
        .sessions
        .iter()
        .find(|s| s.id == snapshot.focused_session);
    *ctx.lifetime = focused.map_or(SessionLifetime::Ephemeral, session_lifetime);
    *ctx.session_key = focused.and_then(|s| s.key);
    *ctx.members = session_terminals(snapshot);
    if focused_session_is_empty(snapshot) {
        return Ok(attach_empty_session(ctx, snapshot, initial_client_id));
    }
    // Capture the initial focused pane so subsequent INPUT_* frames
    // know where to route.
    let bootstrap = snapshot.focused_resource.clone();
    tracing::debug!(
        terminal_id = ?bootstrap,
        "ATTACHED: seeding focused_resource from snapshot"
    );
    *ctx.focused_resource = Some(bootstrap.clone());
    // Single-pane seed; the persisted layout replaces it when present.
    *ctx.workspace = ArrangementDoc::single(bootstrap.clone());
    // Seed mirrors at server-advertised sizes before output can race the
    // bootstrap (VT interpretation is geometry-sensitive). AgentSessions
    // have no grid and get no slot.
    for pane in snapshot.resources.iter().filter(|pane| is_terminal(pane)) {
        if let std::collections::hash_map::Entry::Vacant(v) = ctx.panes.entry(pane.id.clone()) {
            let slot = v.insert(PaneSlot::new_with_size(pane.cols, pane.rows)?);
            // Seed the pane's cwd from the snapshot (the
            // spawn cwd); `cwd_changed` events refine it live.
            slot.cwd.clone_from(&pane.cwd);
            // ADR-0124: a pane retained after its process exited arrives
            // read-only, with the exit the snapshot reports.
            slot.lifecycle = pane.lifecycle;
            slot.exited = pane
                .exit
                .as_ref()
                .map(crate::attach::pane_state::ExitMark::from_facet);
        }
    }
    // Hand the per-pane cwds up to the driver so the
    // sidebar can derive each window's VCS branch client-side.
    let pane_cwds: Vec<(ResourceId, String)> = snapshot
        .resources
        .iter()
        .filter(|pane| is_terminal(pane))
        .filter_map(|p| p.cwd.clone().map(|cwd| (p.id.clone(), cwd)))
        .collect();
    // The focused pane gets a slot even if the graph omitted it.
    if let std::collections::hash_map::Entry::Vacant(v) = ctx.panes.entry(bootstrap) {
        let content = content_rect(
            ctx.viewport_dims,
            ctx.status_bar.as_ref().map(|p| p.position()),
            ctx.sidebar,
        );
        v.insert(PaneSlot::new_with_size(content.w, content.h)?);
    }
    *ctx.session_name = focused_session_name(snapshot);
    // The session graph for the session picker.
    let session_cache = (snapshot.sessions.clone(), snapshot.focused_session);
    Ok(FrameOutcome {
        subscribe_layout: true,
        sessions: Some(session_cache),
        inventory: Some((snapshot.windows.clone(), snapshot.resources.clone())),
        // ADR-0033: cache our own ClientId so the supervisory badge can
        // distinguish "you hold the wheel" from another client.
        own_client_id: Some(initial_client_id),
        pane_cwds,
        ..FrameOutcome::default()
    })
}

/// ADR-0105: the focused session holds no terminals (none listed AND a focus
/// the graph does not list, so an unreported count does not read as empty).
///
/// A server that reports each resource's session (some local resource
/// carries the `session` facet, ADR-0161 §7; the rule of
/// `phux_client::resource::reports_sessions`) synthesizes its windows: the
/// session is empty when no Terminal names it, as its synthesized window
/// count would say. An older server reports only its window count.
fn focused_session_is_empty(snapshot: &phux_protocol::wire::info::SessionSnapshot) -> bool {
    let focus_is_listed = snapshot
        .resources
        .iter()
        .any(|r| r.id == snapshot.focused_resource);
    let Some(focused) = snapshot
        .sessions
        .iter()
        .find(|s| s.id == snapshot.focused_session)
    else {
        return false;
    };
    let reports_sessions = snapshot
        .resources
        .iter()
        .any(|info| info.id.is_local() && info.session.is_some());
    let holds_nothing = if reports_sessions {
        phux_client::resource::terminals_in(snapshot, focused.id)
            .next()
            .is_none()
    } else {
        focused.is_empty()
    };
    holds_nothing && !focus_is_listed
}

/// The focused session's Terminal-kind members: the panes the attach
/// bootstraps, arranged or not.
fn session_terminals(snapshot: &phux_protocol::wire::info::SessionSnapshot) -> HashSet<ResourceId> {
    crate::attach::server_frame::engine_route::attach_participants(snapshot)
        .into_iter()
        .collect()
}

/// ADR-0105: ATTACHED to a session with no windows: start empty; the graph,
/// client id, and layout subscription are recorded as usual.
fn attach_empty_session<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
    initial_client_id: ClientId,
) -> FrameOutcome {
    tracing::debug!("ATTACHED: session has no windows; starting in the empty state");
    *ctx.focused_resource = None;
    *ctx.workspace = ArrangementDoc::default();
    *ctx.session_name = focused_session_name(snapshot);
    FrameOutcome {
        subscribe_layout: true,
        sessions: Some((snapshot.sessions.clone(), snapshot.focused_session)),
        inventory: Some((snapshot.windows.clone(), snapshot.resources.clone())),
        own_client_id: Some(initial_client_id),
        layout_replaced: true,
        ..FrameOutcome::default()
    }
}

/// Size (or create) the pane's slot at the geometry `BOOTSTRAP_BEGIN`
/// advertises; an `AgentSession` (0x0) seeds nothing.
pub(super) fn seed_bootstrap_geometry<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal_id: ResourceId,
    cols: u16,
    rows: u16,
) -> Result<FrameOutcome, AttachError> {
    if ctx.is_agent_session(&terminal_id) {
        return Ok(FrameOutcome::default());
    }
    let slot = match ctx.panes.entry(terminal_id) {
        std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(PaneSlot::new_with_size(cols, rows)?)
        }
    };
    slot.geometry = (cols.max(1), rows.max(1));
    Ok(FrameOutcome::default())
}

/// Record a bootstrap chunk on the span; staging has nothing to paint.
pub(super) fn record_bootstrap_chunk<W: crate::attach::RenderSink>(
    ctx: &FrameCtx<'_, W>,
    terminal_id: &ResourceId,
    payload_len: usize,
) -> FrameOutcome {
    ctx.frame_span
        .record("terminal_id", tracing::field::debug(terminal_id));
    ctx.frame_span.record("bytes", payload_len);
    FrameOutcome::default()
}

/// Refresh the pane's chrome caches from the replica the bootstrap just
/// published, and report the repaint the barrier release permits.
pub(super) fn handle_bootstrap_ready<W: crate::attach::RenderSink>(
    ctx: &mut FrameCtx<'_, W>,
    terminal_id: &ResourceId,
    route: KernelRoute,
) -> Result<FrameOutcome, AttachError> {
    ctx.frame_span
        .record("terminal_id", tracing::field::debug(terminal_id));
    let terminal = published_terminal(ctx.engine_kernel, terminal_id).ok_or_else(|| {
        AttachError::Protocol(format!("BOOTSTRAP_READY did not publish {terminal_id:?}"))
    })?;
    let slot = ctx
        .panes
        .get_mut(terminal_id)
        .ok_or_else(|| AttachError::Protocol("READY without pane slot".to_owned()))?;
    let title_changed = slot.title_changed(terminal);
    slot.update_sync_output(terminal, tokio::time::Instant::now());
    let damaged = route.damaged(terminal_id);
    Ok(FrameOutcome {
        layout_replaced: damaged,
        authoritative_damage: damaged.then(|| terminal_id.clone()).into_iter().collect(),
        chrome_dirty: damaged && title_changed,
        history_request: route.history_request,
        ..FrameOutcome::default()
    })
}

/// Whether this frame's applied bytes may reach stdout: not under a modal,
/// not for an earlier frame of a burst, not inside a sync-output block.
pub(super) const fn paint_permitted(
    overlay_active: bool,
    defer_paint: bool,
    sync_output_active: bool,
) -> bool {
    !overlay_active && !defer_paint && !sync_output_active
}

/// The rect a first-seen pane will paint into (zoom-honoring), else the
/// content rect; sizes its mirror.
pub(super) fn initial_pane_dims<W: crate::attach::RenderSink>(
    ctx: &FrameCtx<'_, W>,
    terminal_id: &ResourceId,
    content: Rect,
) -> (u16, u16) {
    ctx.workspace
        .render_group(ctx.zoomed.as_ref())
        .and_then(|ls| {
            crate::attach::paint::tiled_rect(ls.as_ref(), content, ctx.viewport_dims, terminal_id)
                .map(|r| (r.w, r.h))
        })
        .unwrap_or((content.w, content.h))
}

#[cfg(test)]
mod empty_session_tests {
    use super::focused_session_is_empty;
    use phux_protocol::ids::{ResourceId, SessionId, SessionKey, WindowId};
    use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

    fn snapshot(session: SessionInfo, resources: Vec<ResourceInfo>) -> SessionSnapshot {
        SessionSnapshot::new(SessionId::new(1), WindowId::new(0), ResourceId::local(0))
            .with_sessions(vec![session])
            .with_resources(resources)
    }

    /// From a server that reports resource sessions, a session is empty
    /// when no Terminal names it, whatever its synthesized window count says;
    /// an `AgentSession` alone does not make it non-empty.
    #[test]
    fn a_reporting_server_counts_terminal_members_not_windows() {
        use phux_protocol::ResourceKind;
        let session = SessionInfo::new(SessionId::new(1), "work")
            .with_window_count(1)
            .with_key(Some(SessionKey::new([1; 16]).expect("key")));
        let elsewhere = ResourceInfo::new(ResourceId::local(5), WindowId::new(9), 80, 24)
            .with_session(Some(SessionId::new(2)));
        assert!(focused_session_is_empty(&snapshot(
            session.clone(),
            vec![elsewhere.clone()]
        )));
        let mut agent = ResourceInfo::new(ResourceId::local(7), WindowId::new(0), 0, 0)
            .with_session(Some(SessionId::new(1)));
        agent.kind = ResourceKind::AgentSession;
        assert!(focused_session_is_empty(&snapshot(
            session.clone(),
            vec![elsewhere.clone(), agent.clone()]
        )));
        let member = ResourceInfo::new(ResourceId::local(6), WindowId::new(0), 80, 24)
            .with_session(Some(SessionId::new(1)));
        assert!(!focused_session_is_empty(&snapshot(
            session.with_window_count(0),
            vec![elsewhere, agent, member]
        )));
    }

    /// An older server's session is empty by its window count.
    #[test]
    fn an_unkeyed_session_is_empty_by_its_window_count() {
        let empty = SessionInfo::new(SessionId::new(1), "work").with_window_count(0);
        assert!(focused_session_is_empty(&snapshot(empty, vec![])));
        let full = SessionInfo::new(SessionId::new(1), "work").with_window_count(1);
        assert!(!focused_session_is_empty(
            &snapshot(full, vec![]).with_windows(vec![WindowInfo::new(
                WindowId::new(1),
                SessionId::new(1),
                "w",
            )])
        ));
    }
}
