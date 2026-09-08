//! The driver's config-facing seams: the which-key popup, the attach-time
//! notice, and the in-place config reload. The config-derived state itself
//! is `crate::settings::TuiSettings` (phux-u1tq.2).

use std::collections::HashMap;
use std::time::Duration;

#[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
use phux_protocol::caps::BootstrapCapabilities;
use phux_protocol::ids::{ClientId, TerminalId};

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

/// phux-foz.2: (dis)arm the which-key popup deadline for one loop pass.
///
/// Arms (`Some(now + delay)`) only while ALL of: the resolver is pending
/// exactly at the prefix, the popup is enabled in config, and no overlay
/// is already active (a modal owns the screen; and once the popup itself
/// is up, re-arming would re-push it forever). Re-invocations while armed
/// keep the ORIGINAL deadline (anchored, like `esc_deadline`) so other
/// select! arms firing cannot postpone the popup. Any pass that sees the
/// conditions no longer met — e.g. an early continuation chord resolved
/// the prefix — disarms, which is how a fast chord suppresses the popup.
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

/// phux-foz.2: push the which-key popup when the timeout fires.
///
/// Re-checks the arming conditions against the CURRENT state (the select!
/// arm may race a same-iteration resolver mutation) and pushes a
/// [`WhichKeyOverlay`] built from the same keybindings snapshot the help
/// overlay uses. Returns `true` iff the popup was pushed (the caller then
/// paints the overlay layer). Never touches the resolver: the pending
/// prefix must stay live so the next chord still completes normally.
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

/// phux-foz.5: perform one explicit live config reload and repaint.
///
/// Re-runs the layered config loader ([`TuiSettings::reload_in_place`])
/// and, on success, swaps the reloadable settings — keybindings snapshot,
/// resolver, theme, chrome breakpoints, status bar, plugin rows, which-key
/// knobs — in place, rebuilds the sidebar painter under the new theme
/// (cache-cold, so the repaint recolors everything), refreshes the window
/// chrome, and repaints. On ANY parse/validation failure the previous
/// config stays fully in effect and the error is surfaced as a
/// dismissable toast. Never crashes, never half-applies.
///
/// Reached from both reload surfaces: the `reload-config` action
/// (`DispatchCtx::reload_request`) and the `phux config reload` CLI
/// doorbell (`FrameOutcome::config_reload`).
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
    panes: &mut HashMap<TerminalId, PaneSlot>,
    engine_kernel: &AttachKernel,
    focused_pane: Option<&TerminalId>,
    zoomed: Option<&TerminalId>,
    own_client_id: Option<ClientId>,
    agent_meta: &AgentMetaIndex,
    vcs: &mut VcsIndex,
    // phux-k0cw: a reload rebuilds the sidebar painter cache-cold, so the
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
            // phux-huhi: the new `[chrome]` thresholds reach the overlay
            // stack immediately, including any modal already open.
            overlays.set_breakpoints(settings.chrome);
            // ADR-0101: an open settings page that just edited a theme slot
            // shows the new palette rather than the one it was born with.
            overlays.set_theme(&settings.theme);
            // A fresh sidebar painter carries the new theme and starts
            // cache-cold so the repaint below recolors the whole chrome
            // (the status bar's attention chip already rides the theme,
            // phux-foz.1, set when the settings were built).
            *sidebar_painter = SidebarPainter::new(settings.theme);
            refresh_window_chrome(
                settings.status_bar.as_mut(),
                sidebar_painter,
                workspace,
                panes,
                focused_pane,
                zoomed,
                own_client_id,
                agent_meta,
                vcs,
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
                    focused_pane,
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
            // Keep the old config (reload_in_place touched nothing) and
            // make the failure visible: a dismissable toast, mirroring
            // the plugin-action failure surface. The status bar, theme,
            // and every binding keep working exactly as before.
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
            focused_pane,
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

/// phux-i0e8.2.3: set a caller-supplied attach-time notice (the reconnect
/// loop's "re-attached after server restart") on the status-bar painter's
/// transient slot.
///
/// Returns `true` when the painter accepted it. Degrades to a `tracing`
/// line — never silently — when there is no painter, mirroring the
/// per-frame `FrameOutcome::notices` drain; the painter itself degrades
/// the empty-bar and persistent-error-line cases the same way inside
/// `set_notice`.
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
