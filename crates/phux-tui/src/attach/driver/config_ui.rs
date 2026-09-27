//! The driver's config-facing seams: the which-key popup, the attach-time
//! notice, and the in-place config reload. The config-derived state itself
//! is `crate::settings::TuiSettings`.

use std::collections::HashMap;
use std::time::Duration;

use phux_protocol::ids::{ClientId, ResourceId};

use crate::attach::paint::{SidebarReservation, StatusBarPaint, paint_full_frame};
use crate::attach::pane_state::{AttachKernel, PaneSlot, VcsIndex};
use crate::attach::server_frame::AgentMetaIndex;
use crate::layout::Workspace;
use crate::render::chrome::sidebar::SidebarPainter;
use crate::render::chrome::status_bar::{Notice, StatusBarPainter};
use crate::render::overlay::OverlayState;
use crate::settings::TuiSettings;

use super::chrome::refresh_window_chrome;
use super::overlay_paint::paint_active_overlay;

/// (Dis)arm the which-key deadline for one loop pass: armed only while the
/// resolver is pending at the prefix, the popup is enabled, and no overlay is
/// up; an armed deadline keeps its original anchor.
pub(super) fn update_which_key_deadline(
    deadline: &mut Option<tokio::time::Instant>,
    pending_at_prefix: bool,
    enabled: bool,
    overlay_active: bool,
    now: tokio::time::Instant,
    delay: Duration,
) {
    if enabled && pending_at_prefix && !overlay_active {
        deadline.get_or_insert(now + delay);
    } else {
        *deadline = None;
    }
}

/// Push the which-key popup when the timeout fires, re-checking the arming
/// conditions. Never touches the resolver, so the next chord still completes.
pub(super) fn push_which_key_overlay(
    overlays: &mut OverlayState,
    resolver: Option<&phux_config::keybind::Resolver>,
    keybindings: Option<&phux_config::KeybindingsCfg>,
    theme: &crate::render::Theme,
) -> bool {
    if overlays.is_active() {
        return false;
    }
    if !resolver.is_some_and(phux_config::keybind::Resolver::pending_at_prefix) {
        return false;
    }
    let Some(kb) = keybindings else {
        return false;
    };
    tracing::debug!("which-key: prefix hesitation timeout; showing popup");
    overlays.push(Box::new(
        crate::render::overlay::WhichKeyOverlay::from_config(kb, theme),
    ));
    true
}

/// Reload the config in place and repaint. On success the reloadable
/// settings swap and the sidebar painter is rebuilt cache-cold under the new
/// theme; on any failure the old config stays and a toast shows the error.
/// Reached from the `reload-config` action and the CLI doorbell.
#[allow(
    clippy::too_many_arguments,
    reason = "the settings and the repaint context are driver-loop locals threaded by reference, same shape as the paint helpers"
)]
pub(super) fn handle_config_reload<W: crate::attach::RenderSink>(
    out: &mut W,
    settings: &mut TuiSettings,
    sidebar_painter: &mut SidebarPainter,
    overlays: &mut OverlayState,
    workspace: &Workspace,
    panes: &mut HashMap<ResourceId, PaneSlot>,
    engine_kernel: &AttachKernel,
    focused_resource: Option<&ResourceId>,
    zoomed: Option<&ResourceId>,
    own_client_id: Option<ClientId>,
    agent_meta: &AgentMetaIndex,
    vcs: &mut VcsIndex,
    // A reload rebuilds the sidebar painter cache-cold, so the
    // cross-session zones must be re-projected with it or the strip comes
    // back with an empty queue and roster until the next peer push.
    peers: crate::attach::sidebar_zones::PeerInputs<'_>,
    viewport_dims: (u16, u16),
    sidebar: Option<SidebarReservation>,
    session_name: &str,
) -> StatusBarPaint {
    let mut painted = StatusBarPaint::NotPublished;
    match settings.reload_in_place(&phux_config::loader::config_path()) {
        Ok(()) => {
            tracing::info!("config reloaded in place");
            // The new `[chrome]` thresholds reach the overlay
            // stack immediately, including any modal already open.
            overlays.set_breakpoints(settings.chrome);
            // ADR-0101: an open settings page that just edited a theme slot
            // shows the new palette rather than the one it was born with.
            overlays.set_theme(&settings.theme);
            // A fresh painter carries the new theme and repaints everything.
            *sidebar_painter = SidebarPainter::new(settings.theme);
            refresh_window_chrome(
                settings.status_bar.as_mut(),
                sidebar_painter,
                workspace,
                panes,
                focused_resource,
                zoomed,
                own_client_id,
                agent_meta,
                vcs,
                &crate::attach::agent_rows::agent_session_rows(engine_kernel),
                peers,
            );
            if !overlays.is_active()
                && let Some(ls) = workspace.render_window(zoomed).as_deref()
            {
                painted = paint_full_frame(
                    out,
                    ls,
                    panes,
                    engine_kernel,
                    focused_resource,
                    viewport_dims,
                    settings.status_bar.as_mut(),
                    sidebar,
                    Some(sidebar_painter),
                    session_name,
                    &settings.theme,
                );
            }
        }
        Err(msg) => {
            // Keep the old config and surface the failure as a toast.
            tracing::warn!(error = %msg, "config reload failed; keeping previous config");
            overlays.push(Box::new(crate::render::overlay::ToastOverlay::new(
                "Config reload failed - previous config kept",
                vec![
                    msg,
                    String::new(),
                    "Fix the file and reload again (run: phux config check)".to_owned(),
                ],
                &settings.theme,
            )));
        }
    }
    if overlays.is_active() {
        painted = paint_active_overlay(
            out,
            overlays,
            workspace,
            panes,
            engine_kernel,
            focused_resource,
            zoomed,
            viewport_dims,
            settings.status_bar.as_mut(),
            sidebar,
            Some(&mut *sidebar_painter),
            session_name,
            &settings.theme,
        );
    }
    painted
}

/// Seed the attach-time notice (e.g. "re-attached after server restart");
/// true when the painter took it, a tracing line when there is no painter.
pub(super) fn apply_initial_notice(
    status_bar: Option<&mut StatusBarPainter>,
    notice: Option<Notice>,
) -> bool {
    let Some(notice) = notice else {
        return false;
    };
    if let Some(sb) = status_bar {
        sb.set_notice(notice, std::time::Instant::now())
    } else {
        tracing::info!(
            severity = ?notice.severity,
            text = %notice.text,
            "attach-time notice dropped: no status bar configured",
        );
        false
    }
}
