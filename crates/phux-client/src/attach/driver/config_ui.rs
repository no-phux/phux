//! Config-derived driver state: the lenient keybind resolver, the
//! which-key popup, the status-bar painter build, and the in-place
//! config reload.

use std::collections::HashMap;
use std::time::Duration;

#[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
use phux_protocol::caps::BootstrapCapabilities;
use phux_protocol::ids::{ClientId, TerminalId};

use crate::attach::paint::{SidebarReservation, StatusBarPaint, paint_full_frame};
use crate::attach::pane_state::{AttachKernel, PaneSlot, VcsIndex};
use crate::attach::plugin_actions::PluginActionEntry;
use crate::attach::plugin_panes;
use crate::attach::server_frame::AgentMetaIndex;
use crate::layout::Workspace;
use crate::render::ChromeBreakpoints;
use crate::render::chrome::sidebar::SidebarPainter;
use crate::render::chrome::status_bar::{Notice, StatusBarPainter};
use crate::render::overlay::OverlayState;

use super::chrome::refresh_window_chrome;
use super::overlay_paint::paint_active_overlay;

/// phux-4li.5: build a [`phux_config::keybind::Resolver`] from a
/// keybindings snapshot (post phux-r82.5: the plugin-merged one, so
/// manifest `keys` chords resolve like user bindings — the merge already
/// validated each contributed chord, so a plugin can't poison this
/// build).
///
/// phux-i0e8.3.4: the build is **lenient per binding** — a resolver
/// always comes back, and each diagnostic disables exactly the binding
/// it names. Before this, one malformed chord failed the whole build and
/// silently disabled EVERY binding, including `detach`. Diagnostics are
/// logged here; the caller surfaces them as a visible status-bar error
/// line ([`keybind_error_line`]). Config reload deliberately stays
/// all-or-nothing instead (`crate::attach::reload`, docs/consumers/tui.md §4.3).
pub(super) fn build_resolver_from(
    kb: &phux_config::KeybindingsCfg,
) -> (
    phux_config::keybind::Resolver,
    Vec<phux_config::keybind::BindingDiagnostic>,
) {
    let (resolver, diagnostics) = phux_config::keybind::Resolver::new_lenient(kb);
    for diag in &diagnostics {
        tracing::warn!(binding = %diag.binding, error = %diag.error, "keybinding disabled");
    }
    (resolver, diagnostics)
}

/// phux-i0e8.3.4: format the lenient resolver's diagnostics as the
/// one-line status-bar error strip. Names the first offending chord, the
/// reason, how many more bindings (if any) were also disabled, and the
/// actionable next step (`phux config check`). Empty input formats to an
/// empty string (callers gate on non-empty diagnostics).
pub(super) fn keybind_error_line(diags: &[phux_config::keybind::BindingDiagnostic]) -> String {
    let Some(first) = diags.first() else {
        return String::new();
    };
    let more = diags.len() - 1;
    if more == 0 {
        format!(
            "keybinding \"{}\" disabled: {} (run: phux config check)",
            first.binding, first.error
        )
    } else {
        format!(
            "keybinding \"{}\" disabled: {} (+{more} more; run: phux config check)",
            first.binding, first.error
        )
    }
}

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

/// phux-nz4.5 / phux-9vf: load the on-disk config and build a
/// [`StatusBarPainter`] from `[status]`.
///
/// A malformed config never blocks attach — but it no longer vanishes
/// silently either. On a load or build failure we surface a visible
/// error line (`StatusBarPainter::error_line`) on the bar row pointing
/// the user at `phux config check` for the full diagnostic, instead of
/// dropping to an empty bar with only a `tracing::warn` nobody sees
/// (keybindings degrade separately, per binding — see
/// [`build_resolver_from`]). Returns `None`
/// only when the config is valid and the bar would be empty (no widgets
/// configured) — callers short-circuit on that.
/// phux-foz.5: perform one explicit live config reload and repaint.
///
/// Re-runs the layered config loader ([`crate::attach::reload::reload_in_place`])
/// and, on success, swaps the driver's config-derived state — keybindings
/// snapshot, resolver, theme, status bar, plugin-action rows, which-key
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
    reason = "the config-derived slots and the repaint context are driver-loop locals threaded by reference, same shape as the paint helpers"
)]
pub(super) fn handle_config_reload<W: crate::attach::RenderSink>(
    out: &mut W,
    keybindings_snapshot: &mut Option<phux_config::KeybindingsCfg>,
    resolver: &mut Option<phux_config::keybind::Resolver>,
    theme: &mut crate::render::Theme,
    chrome: &mut ChromeBreakpoints,
    status_bar: &mut Option<StatusBarPainter>,
    sidebar_painter: &mut SidebarPainter,
    plugin_actions: &mut Vec<PluginActionEntry>,
    plugin_panes: &mut Vec<plugin_panes::PluginPaneEntry>,
    which_key_enabled: &mut bool,
    which_key_delay: &mut Duration,
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
    match crate::attach::reload::reload_in_place(
        &phux_config::loader::config_path(),
        keybindings_snapshot,
        resolver,
        theme,
        chrome,
        status_bar,
        plugin_actions,
        plugin_panes,
        which_key_enabled,
        which_key_delay,
    ) {
        Ok(()) => {
            tracing::info!("config reloaded in place");
            // phux-huhi: the new `[chrome]` thresholds reach the overlay
            // stack immediately, including any modal already open.
            overlays.set_breakpoints(*chrome);
            // Fresh painters carry the new theme and start cache-cold so
            // the repaint below recolors the whole chrome. The attention
            // chip color rides the theme (phux-foz.1).
            *sidebar_painter = SidebarPainter::new(*theme);
            if let Some(sb) = status_bar.as_mut() {
                sb.set_attention_color(theme.attention);
            }
            refresh_window_chrome(
                status_bar.as_mut(),
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
                    status_bar.as_mut(),
                    sidebar,
                    Some(sidebar_painter),
                    session_name,
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
                theme,
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
            status_bar.as_mut(),
            sidebar,
            Some(&mut *sidebar_painter),
            session_name,
            theme,
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

pub(super) fn build_status_bar_painter() -> Option<StatusBarPainter> {
    let cfg = match phux_config::loader::load() {
        Ok(c) => c,
        Err(err) => {
            tracing::warn!(error = %err, "phux-config load failed; surfacing on status bar");
            return Some(StatusBarPainter::error_line(config_error_line(&err)));
        }
    };
    let manifests = if cfg.plugins.is_empty() {
        Vec::new()
    } else {
        let config_path = phux_config::loader::config_path();
        phux_config::plugin::load_enabled_manifests(&config_path, &cfg.plugins)
    };
    // phux-i0e8.6.1: the composition itself (plugin `[[widgets]]` merge,
    // bar build, `[status] position`, prefix) is shared with the reload
    // path via `reload::compose_status_bar` so the two cannot drift.
    // Only the error POLICY differs, and it stays here: startup degrades
    // a build failure to the error-line painter so a broken config never
    // blocks attach; a reload instead fails atomically and keeps the
    // previous config.
    match crate::attach::reload::compose_status_bar(&cfg, &manifests) {
        Ok(painter) => painter,
        Err(err) => {
            tracing::warn!(error = %err, "status-bar build failed; surfacing on status bar");
            Some(StatusBarPainter::error_line(config_error_line(&err)))
        }
    }
}

/// phux-9vf: format a one-line, on-screen config error for the status
/// bar: the `Display` of the error plus the actionable next step.
/// The remedy is `phux config check` — the verb that diagnoses, with
/// key paths and layer attribution — not `config show`, which only
/// renders the effective config (phux-i0e8.3.5).
pub(super) fn config_error_line(err: &impl std::fmt::Display) -> String {
    format!("config error: {err} (run: phux config check)")
}
