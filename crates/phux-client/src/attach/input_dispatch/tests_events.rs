//! Event-routing tests: overlay key interception, sidebar and bar
//! clicks, mouse forwarding, set-pane, and predictive echo gating.
#![allow(clippy::expect_used, reason = "tests")]

//! Input dispatcher: translates parser-emitted events into wire frames
//! or layout-action effects.
//!
//! Owns the resolver-intercept path (prefix chord → `ResolvedAction` →
//! mutate the active window of the `Workspace`), the predict overlay's
//! keystroke feed, and the parked-spawn bookkeeping (`PendingSplit` /
//! `PendingWindow`) that bridges a local `split-pane` / `new-window`
//! chord to its remote `SPAWN_TERMINAL` reply.

use std::collections::{HashMap, HashSet};

use phux_protocol::TerminalId;
use phux_protocol::input::InputEvent;
use phux_protocol::input::key::PhysicalKey;
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::wire::frame::FrameKind;

use crate::attach::connection::Connection;
use crate::attach::focus::FocusHistory;
use crate::attach::paint::{SidebarReservation, content_rect};
use crate::attach::pane_state::{AttentionNavigation, PaneSlot};
use crate::attach::render::ReplicaWalk;
use crate::layout::Workspace;
use crate::predict::Overlay;
use crate::render::overlay::OverlayState;
use crate::render::{ChromeBreakpoints, Theme};

use super::ACTION_NAMES;
use super::args::*;
use super::ctx::*;
use super::dispatch::*;
use super::effects::*;
use super::run_action::*;
use super::test_support::*;

// The `RecordingOverlay` test double lives in `crate::render::overlay`
// because implementing `RenderOverlay::render` names ratatui types, which
// the boundary guard confines to `render/`.

/// Regression (wave-hunt/client-tui): while an overlay is active the
/// keybind resolver must be bypassed, so the leader prefix key (and any
/// mid-chord key) reaches the overlay as literal input instead of being
/// swallowed by the resolver.
///
/// Pre-fix: feeding `C-a` (the default leader) while an overlay was up
/// returned `ChordOutcome::Partial`, hit `continue`, and the key never
/// reached the overlay — *and* left the resolver mid-chord so it
/// intercepted the next key too. A user typing a name into the rename
/// prompt that contained the leader chord lost characters.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "the fixture's DispatchCtx grows a line per composed feature (wave-2 + wave-2.5); the scenario itself is one flow"
)]
async fn overlay_active_prefix_key_reaches_overlay_not_resolver() {
    use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

    let cfg = phux_config::parse_str(
        phux_config::DEFAULT_CONFIG_TOML,
        std::path::Path::new("default.toml"),
    )
    .expect("default config parses");
    let mut resolver =
        phux_config::keybind::Resolver::new(&cfg.keybindings).expect("resolver builds");

    // Record what the overlay receives.
    let keys = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut overlays = OverlayState::new();
    overlays.push(Box::new(crate::render::overlay::RecordingOverlay {
        keys: keys.clone(),
    }));

    let (a, _b) = tokio::net::UnixStream::pair().expect("uds pair");
    let mut conn = Connection::from_stream(a);
    let mut out: Vec<u8> = Vec::new();
    let mut workspace = Workspace::single(tid(1));
    let mut focused_pane = Some(tid(1));
    let mut detach_pending = false;
    let mut predict = PredictionState::new(crate::predict::PredictiveConfig::disabled(), 80, 24);
    let overlay = Overlay;
    let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
    let mut next_request_id = 1;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    let theme = Theme::default();
    let mut switch_request = None;
    let mut session_name = String::new();

    // The default leader is `C-a`. Feed it, then a printable key.
    let leader = KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::A,
        mods: ModSet::CTRL,
        consumed_mods: ModSet::CTRL,
        composing: false,
        text: None,
        unshifted_codepoint: Some(u32::from(b'a')),
    };
    let letter = KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::X,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: Some("x".to_owned()),
        unshifted_codepoint: Some(u32::from(b'x')),
    };

    let mut zoomed = None;
    let mut sidebar_enabled = false;
    let mut drag: Option<DragGrab> = None;
    let mut reload_request = false;
    let mut mouse_optout: std::collections::HashSet<TerminalId> = std::collections::HashSet::new();
    let fleet_agent_meta = HashMap::new();
    let mut fleet_vcs = crate::attach::pane_state::VcsIndex::default();
    let mut engine_kernel = test_engine_kernel();
    // phux-k0cw: the strip's shape comes from the painted target
    // table now, not from the workspace, so a fixture that wants
    // hit-testable window rows must declare them.
    let sidebar_targets = targets(0, workspace.windows.len(), 0);
    let mut ctx = DispatchCtx {
        engine_kernel: &mut engine_kernel,
        resolver: Some(&mut resolver),
        focus_history: FocusHistory::default(),
        workspace: &mut workspace,
        viewport: (80, 24),
        cell_px: (1, 1),
        next_request_id: &mut next_request_id,
        spawn_initial_size_supported: true,
        pending_splits: &mut pending_splits,
        pending_windows: &mut pending_windows,
        expected_closes: &mut HashSet::new(),
        overlays: &mut overlays,
        keybindings: None,
        theme: &theme,
        sessions: &[],
        foreign_layouts: &HashMap::new(),
        foreign_agents: &HashMap::new(),
        focused_session: None,
        session_name: &mut session_name,
        switch_request: &mut switch_request,
        zoomed: &mut zoomed,
        sidebar: None,
        sidebar_enabled: &mut sidebar_enabled,
        sidebar_width: 20,
        chrome: ChromeBreakpoints::default(),
        sidebar_targets: &sidebar_targets,
        bar: None,
        status_bar: None,
        drag: &mut drag,
        mouse_optout: &mut mouse_optout,
        attention_navigation: &mut AttentionNavigation::default(),
        plugin_actions: &[],
        plugin_panes: &[],
        plugin_tx: None,
        reload_request: &mut reload_request,
        agent_meta: &fleet_agent_meta,
        vcs: &mut fleet_vcs,
    };
    dispatch_input_events(
        &mut out,
        &mut conn,
        vec![InputEvent::Key(leader), InputEvent::Key(letter)],
        &mut focused_pane,
        &mut detach_pending,
        &mut predict,
        &overlay,
        &mut panes,
        &mut ctx,
    )
    .await
    .expect("dispatch");

    let received = keys.borrow();
    assert_eq!(
        received.len(),
        2,
        "both the leader chord and the following key must reach the overlay; got {received:?}",
    );
    assert_eq!(received[0].key, PhysicalKey::A);
    assert!(received[0].mods.contains(ModSet::CTRL));
    assert_eq!(received[1].key, PhysicalKey::X);
}

// -- which-key popup passthrough (phux-foz.2) --------------------------

/// Drive `dispatch_input_events` with the given events against a
/// resolver already pending at the prefix and either which-key or
/// onboarding on the overlay stack. Returns `(overlays_active_after,
/// detach_pending, resolver_pending_after)`.
#[allow(
    clippy::future_not_send,
    clippy::too_many_lines,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
async fn dispatch_with_passthrough_popup(
    events: Vec<InputEvent>,
    onboarding: bool,
) -> (bool, bool, bool) {
    let cfg = phux_config::parse_str(
        phux_config::DEFAULT_CONFIG_TOML,
        std::path::Path::new("default.toml"),
    )
    .expect("default config parses");
    let mut resolver =
        phux_config::keybind::Resolver::new(&cfg.keybindings).expect("resolver builds");
    // Walk to the pending-prefix state the popup describes.
    let prefix = phux_config::keybind::parse_chord(&cfg.keybindings.prefix).expect("prefix parses");
    assert_eq!(resolver.feed(prefix), phux_config::keybind::Feed::Partial);
    assert!(resolver.pending_at_prefix());

    let theme = Theme::default();
    let mut overlays = OverlayState::new();
    if onboarding {
        overlays.push(Box::new(crate::render::overlay::ToastOverlay::passthrough(
            super::super::onboarding::ONBOARDING_TITLE,
            super::super::onboarding::hint_lines(Some(&cfg.keybindings)),
            &theme,
        )));
    } else {
        overlays.push(Box::new(
            crate::render::overlay::WhichKeyOverlay::from_config(&cfg.keybindings, &theme),
        ));
    }
    assert!(overlays.top_is_passthrough());

    let (a, _b) = tokio::net::UnixStream::pair().expect("uds pair");
    let mut conn = Connection::from_stream(a);
    let mut out: Vec<u8> = Vec::new();
    let mut workspace = Workspace::single(tid(1));
    let mut focused_pane = Some(tid(1));
    let mut detach_pending = false;
    let mut predict = PredictionState::new(crate::predict::PredictiveConfig::disabled(), 80, 24);
    let overlay = Overlay;
    let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
    let mut next_request_id = 1;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    let mut switch_request = None;
    let mut session_name = String::new();
    let mut zoomed = None;
    let mut sidebar_enabled = false;
    let mut drag: Option<DragGrab> = None;
    let mut reload_request = false;
    let mut mouse_optout: std::collections::HashSet<TerminalId> = std::collections::HashSet::new();
    let fleet_agent_meta = HashMap::new();
    let mut fleet_vcs = crate::attach::pane_state::VcsIndex::default();
    let mut engine_kernel = test_engine_kernel();
    // phux-k0cw: the strip's shape comes from the painted target
    // table now, not from the workspace, so a fixture that wants
    // hit-testable window rows must declare them.
    let sidebar_targets = targets(0, workspace.windows.len(), 0);
    let mut ctx = DispatchCtx {
        engine_kernel: &mut engine_kernel,
        resolver: Some(&mut resolver),
        focus_history: FocusHistory::default(),
        workspace: &mut workspace,
        viewport: (80, 24),
        cell_px: (1, 1),
        next_request_id: &mut next_request_id,
        spawn_initial_size_supported: true,
        pending_splits: &mut pending_splits,
        pending_windows: &mut pending_windows,
        expected_closes: &mut HashSet::new(),
        overlays: &mut overlays,
        keybindings: Some(&cfg.keybindings),
        theme: &theme,
        sessions: &[],
        foreign_layouts: &HashMap::new(),
        foreign_agents: &HashMap::new(),
        focused_session: None,
        session_name: &mut session_name,
        switch_request: &mut switch_request,
        zoomed: &mut zoomed,
        sidebar: None,
        sidebar_enabled: &mut sidebar_enabled,
        sidebar_width: 20,
        chrome: ChromeBreakpoints::default(),
        sidebar_targets: &sidebar_targets,
        bar: None,
        status_bar: None,
        drag: &mut drag,
        mouse_optout: &mut mouse_optout,
        attention_navigation: &mut AttentionNavigation::default(),
        plugin_actions: &[],
        plugin_panes: &[],
        plugin_tx: None,
        reload_request: &mut reload_request,
        agent_meta: &fleet_agent_meta,
        vcs: &mut fleet_vcs,
    };
    dispatch_input_events(
        &mut out,
        &mut conn,
        events,
        &mut focused_pane,
        &mut detach_pending,
        &mut predict,
        &overlay,
        &mut panes,
        &mut ctx,
    )
    .await
    .expect("dispatch");
    (overlays.is_active(), detach_pending, resolver.is_pending())
}

/// phux-foz.2 requirement 3 (execute path): with the which-key popup
/// up and the prefix pending, the next key must dismiss the popup AND
/// execute its prefix-table binding exactly as if the popup had never
/// appeared — the popup eats nothing.
#[tokio::test]
async fn which_key_popup_next_chord_dismisses_and_executes() {
    use phux_protocol::input::key::PhysicalKey;
    // Default prefix table binds `d` = detach.
    let (overlay_active, detach_pending, resolver_pending) =
        dispatch_with_passthrough_popup(vec![press(PhysicalKey::D, Some("d"))], false).await;
    assert!(!overlay_active, "the chord must dismiss the popup");
    assert!(
        detach_pending,
        "the chord must still execute its binding (C-a d = detach)"
    );
    assert!(!resolver_pending, "the chord resolved; nothing pending");
}

/// phux-foz.2 requirement 3 (cancel path): Esc dismisses the popup
/// and cancels the pending prefix — the binding does NOT run, and a
/// following prefix-table key is a plain keystroke for the pane.
#[tokio::test]
async fn which_key_popup_esc_cancels_the_prefix() {
    use phux_protocol::input::key::PhysicalKey;
    let (overlay_active, detach_pending, resolver_pending) = dispatch_with_passthrough_popup(
        vec![
            press(PhysicalKey::Escape, None),
            // With the prefix cancelled, `d` must NOT resolve to detach.
            press(PhysicalKey::D, Some("d")),
        ],
        false,
    )
    .await;
    assert!(!overlay_active, "Esc must dismiss the popup");
    assert!(!resolver_pending, "Esc must cancel the pending prefix");
    assert!(
        !detach_pending,
        "after Esc, `d` is a plain pane keystroke, not `C-a d`"
    );
}

/// First-use guidance disappears without taxing the intended key: this
/// drives the real dispatcher and proves the same `d` both dismisses the
/// notice and completes the pending detach binding.
#[tokio::test]
async fn onboarding_dismissal_passes_the_intended_key_through() {
    use phux_protocol::input::key::PhysicalKey;
    let (overlay_active, detach_pending, resolver_pending) =
        dispatch_with_passthrough_popup(vec![press(PhysicalKey::D, Some("d"))], true).await;
    assert!(!overlay_active, "the input must dismiss the guidance");
    assert!(
        detach_pending,
        "the dismissing key must still run its action"
    );
    assert!(!resolver_pending, "the binding must resolve normally");
}

#[allow(
    clippy::too_many_lines,
    reason = "full copy-mode page-up/page-down round trip needs a complete DispatchCtx fixture, which grows a line per composed feature (phux-foz.9 sidebar_agents, phux-foz.12 status-bar lend)"
)]
#[tokio::test]
async fn copy_mode_page_scroll_mutates_focused_terminal_viewport() {
    use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};

    fn visible_prefix(
        kernel: &super::super::pane_state::AttachKernel,
        panes: &mut HashMap<TerminalId, PaneSlot>,
        id: &TerminalId,
        row: u16,
    ) -> String {
        let slot = panes.get_mut(id).expect("pane");
        let terminal =
            super::super::pane_state::published_terminal(kernel, id).expect("published terminal");
        (0..6)
            .filter_map(|col| {
                slot.renderer
                    .read_grapheme_string_at(ReplicaWalk::for_test(terminal), row, col)
                    .expect("read cell")
            })
            .collect()
    }

    let (a, _b) = tokio::net::UnixStream::pair().expect("uds pair");
    let mut conn = Connection::from_stream(a);
    let mut out: Vec<u8> = Vec::new();
    let mut workspace = Workspace::single(tid(1));
    let mut focused_pane = Some(tid(1));
    let mut detach_pending = false;
    let mut predict = PredictionState::new(crate::predict::PredictiveConfig::disabled(), 8, 4);
    let overlay = Overlay;
    let mut replay = Vec::new();
    for n in 0..10 {
        replay.extend_from_slice(format!("line{n:02}\r\n").as_bytes());
    }
    let (mut engine_kernel, _, mut panes) =
        super::super::pane_state::published_test_state(&[(&tid(1), 8, 4, &replay)]);

    let before = visible_prefix(&engine_kernel, &mut panes, &tid(1), 0);

    let mut overlays = OverlayState::new();
    overlays.push(Box::new(crate::render::overlay::CopyModeOverlay::new(
        0, 0, 8, 4,
    )));
    let mut next_request_id = 1;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    let theme = Theme::default();
    let mut switch_request = None;
    let mut session_name = String::new();
    let mut zoomed = None;
    let mut sidebar_enabled = false;
    let mut drag: Option<DragGrab> = None;
    let mut reload_request = false;
    let mut mouse_optout: std::collections::HashSet<TerminalId> = std::collections::HashSet::new();
    let fleet_agent_meta = HashMap::new();
    let mut fleet_vcs = crate::attach::pane_state::VcsIndex::default();
    // phux-k0cw: the strip's shape comes from the painted target
    // table now, not from the workspace, so a fixture that wants
    // hit-testable window rows must declare them.
    let sidebar_targets = targets(0, workspace.windows.len(), 0);
    let mut ctx = DispatchCtx {
        engine_kernel: &mut engine_kernel,
        resolver: None,
        focus_history: FocusHistory::default(),
        workspace: &mut workspace,
        viewport: (8, 4),
        cell_px: (1, 1),
        next_request_id: &mut next_request_id,
        spawn_initial_size_supported: true,
        pending_splits: &mut pending_splits,
        pending_windows: &mut pending_windows,
        expected_closes: &mut HashSet::new(),
        overlays: &mut overlays,
        keybindings: None,
        theme: &theme,
        sessions: &[],
        foreign_layouts: &HashMap::new(),
        foreign_agents: &HashMap::new(),
        focused_session: None,
        session_name: &mut session_name,
        switch_request: &mut switch_request,
        zoomed: &mut zoomed,
        sidebar: None,
        sidebar_enabled: &mut sidebar_enabled,
        sidebar_width: 20,
        chrome: ChromeBreakpoints::default(),
        sidebar_targets: &sidebar_targets,
        bar: None,
        status_bar: None,
        drag: &mut drag,
        mouse_optout: &mut mouse_optout,
        attention_navigation: &mut AttentionNavigation::default(),
        plugin_actions: &[],
        plugin_panes: &[],
        plugin_tx: None,
        reload_request: &mut reload_request,
        agent_meta: &fleet_agent_meta,
        vcs: &mut fleet_vcs,
    };
    let page_up = KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::PageUp,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    };

    let changed = dispatch_input_events(
        &mut out,
        &mut conn,
        vec![InputEvent::Key(page_up)],
        &mut focused_pane,
        &mut detach_pending,
        &mut predict,
        &overlay,
        &mut panes,
        &mut ctx,
    )
    .await
    .expect("dispatch");

    let after = visible_prefix(&engine_kernel, &mut panes, &tid(1), 0);
    assert!(changed, "scrolling copy-mode should trigger a repaint");
    assert_ne!(
        before, after,
        "dispatch should apply copy-mode scroll to the focused pane viewport"
    );
}

// ---------- phux-fce4: sidebar hit targets ----------

fn str_arg(r: &phux_config::keybind::ResolvedAction, key: &str) -> Option<String> {
    r.args.get(key)?.as_str().map(str::to_owned)
}

/// The pure click→action mapping: window blocks commit
/// `select-window { index }`, the footer rows `new-window` and
/// `command-palette`, the collapse corner `toggle-sidebar`, and
/// header/blank/separator cells nothing.
#[test]
fn sidebar_click_action_maps_rows_to_registry_actions() {
    // Left-docked 20-column strip over a 24-row viewport with a status
    // bar: rows 0..=22, footer on rows 21 (new) and 22 (menu). With a
    // quiet queue row 0 is the `here` header (phux-k0cw), so window 1's
    // block sits on rows 3-4.
    let strip = crate::layout::Rect {
        x: 0,
        y: 0,
        w: 20,
        h: 23,
    };
    let quiet = targets(0, 2, 0);
    // Window 1's name row (y = 3) and branch row (y = 4) both select it.
    for y in [3, 4] {
        let resolved = sidebar_click_action(strip, &quiet, 4, y).expect("window row hits");
        assert_eq!(resolved.action, "select-window");
        assert_eq!(index_arg(&resolved), Some(1));
    }
    let new = sidebar_click_action(strip, &quiet, 4, 21).expect("new row hits");
    assert_eq!(new.action, "new-window");
    assert!(new.args.is_empty());
    let menu = sidebar_click_action(strip, &quiet, 4, 22).expect("menu row hits");
    assert_eq!(menu.action, "command-palette");
    // phux-foz.9: the collapse chevron in the bottom corner.
    let collapse = sidebar_click_action(strip, &quiet, 19, 22).expect("collapse corner hits");
    assert_eq!(collapse.action, "toggle-sidebar");
    assert!(collapse.args.is_empty());
    // Header row, blank padding row, and the separator column (outside
    // the chevron corner) commit nothing.
    assert!(sidebar_click_action(strip, &quiet, 4, 0).is_none());
    assert!(sidebar_click_action(strip, &quiet, 4, 10).is_none());
    assert!(sidebar_click_action(strip, &quiet, 19, 0).is_none());
}

/// phux-k0cw: a queue row commits a LOCAL focus or a CROSS-SESSION
/// re-attach depending on the row, and a roster row switches session.
/// The two are deliberately different commits — the distinction is the
/// whole reason the target table is snapshotted per paint.
#[test]
fn sidebar_queue_and_roster_rows_commit_their_own_actions() {
    let strip = crate::layout::Rect {
        x: 0,
        y: 0,
        w: 20,
        h: 23,
    };
    // 2 queued + 2 windows: rows 0 header, 1-2 queue, 3 gap, 4 `here`.
    let t = targets(2, 2, 0);
    let local = sidebar_click_action(strip, &t, 4, 1).expect("queue row 0 hits");
    assert_eq!(local.action, "select-window", "a local row stays local");
    assert_eq!(index_arg(&local), Some(1));

    let peer = sidebar_click_action(strip, &t, 4, 2).expect("queue row 1 hits");
    assert_eq!(peer.action, "switch-session");
    assert_eq!(str_arg(&peer, "name").as_deref(), Some("peer-1"));
    assert_eq!(usize_arg(&peer, "window"), Some(2));
    assert_eq!(usize_arg(&peer, "pane"), Some(3));
    assert!(
        sidebar_click_action(strip, &t, 4, 0).is_none(),
        "the queue header is inert"
    );

    // 2 windows + 2 peers: rows 0 `here`, 1-4 blocks, 5 gap,
    // 6 `spaces`, 7-8 roster.
    let t = targets(0, 2, 2);
    assert!(
        sidebar_click_action(strip, &t, 4, 6).is_none(),
        "the spaces header is inert"
    );
    let space = sidebar_click_action(strip, &t, 4, 7).expect("roster row hits");
    assert_eq!(space.action, "switch-session");
    assert_eq!(str_arg(&space, "name").as_deref(), Some("space-0"));
    assert!(
        !space.args.contains_key("pane"),
        "a roster click names a session, not a pane"
    );

    // The overflow row hands off to the dashboard.
    let t = targets(9, 1, 0);
    let overflow = (0..strip.h)
        .filter_map(|y| sidebar_click_action(strip, &t, 4, y))
        .find(|r| r.action == "agent-fleet");
    assert!(
        overflow.is_some(),
        "an overflow row opens the fleet dashboard"
    );
}

/// Every action a sidebar click can commit must be a dispatched action
/// name — the same lockstep the palette registry test enforces.
#[test]
fn sidebar_click_actions_are_dispatched_names() {
    let strip = crate::layout::Rect {
        x: 0,
        y: 0,
        w: 20,
        h: 23,
    };
    // Every zone populated, so the sweep reaches every commit shape.
    let t = targets(9, 3, 2);
    for y in 0..strip.h {
        for x in [2u16, 19] {
            if let Some(resolved) = sidebar_click_action(strip, &t, x, y) {
                assert!(
                    ACTION_NAMES.contains(&resolved.action.as_str()),
                    "sidebar committed `{}`, which run_action does not dispatch",
                    resolved.action,
                );
            }
        }
    }
}

fn left_press_at(x: u16, y: u16) -> InputEvent {
    use phux_protocol::input::key::ModSet;
    InputEvent::Mouse(MouseEvent {
        action: MouseAction::Press,
        button: MouseButton::Left,
        mods: ModSet::empty(),
        x: f64::from(x),
        y: f64::from(y),
    })
}

/// phux-wrnm: the same press with the right button.
fn right_press_at(x: u16, y: u16) -> InputEvent {
    use phux_protocol::input::key::ModSet;
    InputEvent::Mouse(MouseEvent {
        action: MouseAction::Press,
        button: MouseButton::Right,
        mods: ModSet::empty(),
        x: f64::from(x),
        y: f64::from(y),
    })
}

/// Drive `dispatch_input_events` with a left-docked sidebar reservation
/// and one mouse event; returns `(active_window, overlay_active,
/// pending_window_count)` so the callers can assert each affordance's
/// end-to-end effect.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
async fn dispatch_sidebar_click(ev: InputEvent) -> (usize, bool, usize) {
    let (a, _b) = tokio::net::UnixStream::pair().expect("uds pair");
    let mut conn = Connection::from_stream(a);
    let mut out: Vec<u8> = Vec::new();
    let mut workspace = Workspace::single(tid(1));
    workspace.add_window("two".to_owned(), tid(2));
    workspace.select(0);
    let mut focused_pane = Some(tid(1));
    let mut detach_pending = false;
    let mut predict = PredictionState::new(crate::predict::PredictiveConfig::disabled(), 80, 24);
    let overlay = Overlay;
    let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
    let mut next_request_id = 1;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    let mut overlays = OverlayState::new();
    let theme = Theme::default();
    let mut switch_request = None;
    let mut session_name = String::new();
    let mut zoomed = None;
    let mut sidebar_enabled = true;
    let mut drag: Option<DragGrab> = None;
    let mut mouse_optout: std::collections::HashSet<TerminalId> = std::collections::HashSet::new();
    let mut reload_request = false;
    let fleet_agent_meta = HashMap::new();
    let mut fleet_vcs = crate::attach::pane_state::VcsIndex::default();
    let mut engine_kernel = test_engine_kernel();
    // phux-k0cw: the strip's shape comes from the painted target
    // table now, not from the workspace, so a fixture that wants
    // hit-testable window rows must declare them.
    let sidebar_targets = targets(0, workspace.windows.len(), 0);
    let mut ctx = DispatchCtx {
        engine_kernel: &mut engine_kernel,
        resolver: None,
        focus_history: FocusHistory::default(),
        workspace: &mut workspace,
        viewport: (80, 24),
        cell_px: (1, 1),
        next_request_id: &mut next_request_id,
        spawn_initial_size_supported: true,
        pending_splits: &mut pending_splits,
        pending_windows: &mut pending_windows,
        expected_closes: &mut HashSet::new(),
        overlays: &mut overlays,
        keybindings: None,
        theme: &theme,
        sessions: &[],
        foreign_layouts: &HashMap::new(),
        foreign_agents: &HashMap::new(),
        focused_session: None,
        session_name: &mut session_name,
        switch_request: &mut switch_request,
        zoomed: &mut zoomed,
        sidebar: Some(SidebarReservation {
            edge: super::super::paint::SidebarEdge::Left,
            width: 20,
        }),
        sidebar_enabled: &mut sidebar_enabled,
        sidebar_width: 20,
        chrome: ChromeBreakpoints::default(),
        sidebar_targets: &sidebar_targets,
        bar: Some(crate::render::chrome::status_bar::Position::Bottom),
        status_bar: None,
        drag: &mut drag,
        mouse_optout: &mut mouse_optout,
        attention_navigation: &mut AttentionNavigation::default(),
        plugin_actions: &[],
        plugin_panes: &[],
        plugin_tx: None,
        reload_request: &mut reload_request,
        agent_meta: &fleet_agent_meta,
        vcs: &mut fleet_vcs,
    };
    dispatch_input_events(
        &mut out,
        &mut conn,
        vec![ev],
        &mut focused_pane,
        &mut detach_pending,
        &mut predict,
        &overlay,
        &mut panes,
        &mut ctx,
    )
    .await
    .expect("dispatch");
    (
        workspace.active,
        overlays.is_active(),
        pending_windows.len(),
    )
}

/// A left press on the second window's block switches to it — the
/// mouse route runs the same `select-window` a keybinding would.
#[tokio::test]
async fn sidebar_click_on_window_block_selects_it() {
    // phux-qtw8: the strip is full-height (h = 24 in a 24-row viewport)
    // even with a bar docked. Row 0 is the spaces header (phux-foz.9), so
    // window 1's name row is y=3.
    let (active, overlay_active, pending) = dispatch_sidebar_click(left_press_at(3, 3)).await;
    assert_eq!(active, 1, "clicking window 1's block must select it");
    assert!(!overlay_active);
    assert_eq!(pending, 0);
}

/// A left press on `+ new` parks a `new-window` spawn (the reply opens
/// the window), exactly like the `new-window` chord.
#[tokio::test]
async fn sidebar_click_on_new_parks_a_window_spawn() {
    // The footer is bottom-anchored: `+ new` is the strip's second-to-last
    // row, y = 22 of a full-height 24-row strip (phux-qtw8).
    let (active, overlay_active, pending) = dispatch_sidebar_click(left_press_at(3, 22)).await;
    assert_eq!(active, 0, "spawn is parked; no window switch yet");
    assert!(!overlay_active);
    assert_eq!(pending, 1, "new-window spawn must be parked");
}

/// A left press on `= menu` opens the command palette overlay — the
/// session/plugin menu built from the action registry.
///
/// phux-qtw8: `= menu` is the strip's last row, which is also the bar row —
/// the strip owns its columns there, and the bar has yielded them. The
/// strip hit-tests first, so the click reaches the footer, not the bar.
#[tokio::test]
async fn sidebar_click_on_menu_opens_the_command_palette() {
    let (active, overlay_active, pending) = dispatch_sidebar_click(left_press_at(3, 23)).await;
    assert_eq!(active, 0);
    assert!(overlay_active, "menu click must push the palette overlay");
    assert_eq!(pending, 0);
}

/// Pointer events over the strip never leak into pane routing: a press
/// on a blank row is consumed, mutating nothing.
#[tokio::test]
async fn sidebar_consumes_clicks_on_blank_rows() {
    let (active, overlay_active, pending) = dispatch_sidebar_click(left_press_at(3, 10)).await;
    assert_eq!(active, 0);
    assert!(!overlay_active);
    assert_eq!(pending, 0);
}

/// phux-wrnm: a right press on a window block selects that window (a
/// menu acts on what you pointed at) and then opens its window menu.
#[tokio::test]
async fn sidebar_right_press_on_a_window_block_selects_it_and_opens_its_menu() {
    let (active, overlay_active, pending) = dispatch_sidebar_click(right_press_at(3, 3)).await;
    assert_eq!(active, 1, "right-clicking window 1's block selects it");
    assert!(overlay_active, "and opens the window menu for it");
    assert_eq!(pending, 0);
}

/// phux-wrnm: every other cell of the strip is session chrome, so a
/// right press there opens the session menu rather than doing nothing.
#[tokio::test]
async fn sidebar_right_press_on_blank_chrome_opens_the_session_menu() {
    let (active, overlay_active, pending) = dispatch_sidebar_click(right_press_at(3, 10)).await;
    assert_eq!(active, 0, "no window was pointed at, so none is selected");
    assert!(overlay_active, "the session menu opens");
    assert_eq!(pending, 0);
}

// ---------- phux-foz.12: status-bar window-tab hit targets ----------

/// Build a status-bar painter with the `windows` widget in the left
/// slot (the default config's layout), fed `bash`/`vim` tabs and
/// painted once at `cols x rows` so its cached strip — the click
/// hit-test source — is populated. The strip reads "0:bash 1:vim":
/// window 0 on columns 0..=5, the separator on 6, window 1 on 7..=11.
fn painted_windows_bar(
    position: crate::render::chrome::status_bar::Position,
    cols: u16,
    rows: u16,
) -> crate::render::chrome::status_bar::StatusBarPainter {
    use crate::render::chrome::status_bar::{StatusBarPainter, make_context};
    use phux_config::widget::{StatusBar, WidgetRegistry, WindowInfo};
    let cfg = phux_config::StatusCfg {
        left: vec![phux_config::Widget::Bare("windows".into())],
        ..Default::default()
    };
    let bar = StatusBar::build(&cfg, &WidgetRegistry::with_builtins()).expect("bar builds");
    let mut painter = StatusBarPainter::new(bar, position);
    painter.set_windows(vec![
        WindowInfo {
            name: "bash".to_owned(),
            active: true,
            zoomed: false,
            attention: false,
            branch: None,
        },
        WindowInfo {
            name: "vim".to_owned(),
            active: false,
            zoomed: false,
            attention: false,
            branch: None,
        },
    ]);
    let mut sink = Vec::new();
    painter
        .paint(
            &mut sink,
            crate::render::chrome::status_bar::BarInset::NONE,
            cols,
            rows,
            &make_context("", std::time::SystemTime::UNIX_EPOCH),
        )
        .expect("paint");
    painter
}

/// Drive `dispatch_input_events` with a two-window workspace, no
/// sidebar, and a painted status bar at `position`; returns
/// `(active_window, frames_the_peer_received)` so callers can assert
/// both the select effect and that nothing leaked to a pane.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
async fn dispatch_bar_click(
    ev: InputEvent,
    position: crate::render::chrome::status_bar::Position,
    with_painter: bool,
) -> (usize, Vec<FrameKind>, bool) {
    let painter = painted_windows_bar(position, 80, 24);
    let (a, b) = tokio::net::UnixStream::pair().expect("uds pair");
    let mut conn = Connection::from_stream(a);
    let mut peer = Connection::from_stream(b);
    let mut out: Vec<u8> = Vec::new();
    let mut workspace = Workspace::single(tid(1));
    workspace.add_window("two".to_owned(), tid(2));
    workspace.select(0);
    let mut focused_pane = Some(tid(1));
    let mut detach_pending = false;
    let mut predict = PredictionState::new(crate::predict::PredictiveConfig::disabled(), 80, 24);
    let overlay = Overlay;
    let mut panes: HashMap<TerminalId, PaneSlot> = HashMap::new();
    let mut next_request_id = 1;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    let mut overlays = OverlayState::new();
    let theme = Theme::default();
    let mut switch_request = None;
    let mut session_name = String::new();
    let mut zoomed = None;
    let mut sidebar_enabled = false;
    let mut drag: Option<DragGrab> = None;
    let mut mouse_optout: std::collections::HashSet<TerminalId> = std::collections::HashSet::new();
    let mut reload_request = false;
    let fleet_agent_meta = HashMap::new();
    let mut fleet_vcs = crate::attach::pane_state::VcsIndex::default();
    {
        let mut engine_kernel = test_engine_kernel();
        // phux-k0cw: the strip's shape comes from the painted target
        // table now, not from the workspace, so a fixture that wants
        // hit-testable window rows must declare them.
        let sidebar_targets = targets(0, workspace.windows.len(), 0);
        let mut ctx = DispatchCtx {
            engine_kernel: &mut engine_kernel,
            resolver: None,
            focus_history: FocusHistory::default(),
            workspace: &mut workspace,
            viewport: (80, 24),
            cell_px: (1, 1),
            next_request_id: &mut next_request_id,
            spawn_initial_size_supported: true,
            pending_splits: &mut pending_splits,
            pending_windows: &mut pending_windows,
            expected_closes: &mut HashSet::new(),
            overlays: &mut overlays,
            keybindings: None,
            theme: &theme,
            sessions: &[],
            foreign_layouts: &HashMap::new(),
            foreign_agents: &HashMap::new(),
            focused_session: None,
            session_name: &mut session_name,
            switch_request: &mut switch_request,
            zoomed: &mut zoomed,
            sidebar: None,
            sidebar_enabled: &mut sidebar_enabled,
            sidebar_width: 20,
            chrome: ChromeBreakpoints::default(),
            sidebar_targets: &sidebar_targets,
            bar: Some(position),
            status_bar: with_painter.then_some(&painter),
            drag: &mut drag,
            mouse_optout: &mut mouse_optout,
            attention_navigation: &mut AttentionNavigation::default(),
            plugin_actions: &[],
            plugin_panes: &[],
            plugin_tx: None,
            reload_request: &mut reload_request,
            agent_meta: &fleet_agent_meta,
            vcs: &mut fleet_vcs,
        };
        dispatch_input_events(
            &mut out,
            &mut conn,
            vec![ev],
            &mut focused_pane,
            &mut detach_pending,
            &mut predict,
            &overlay,
            &mut panes,
            &mut ctx,
        )
        .await
        .expect("dispatch");
    }
    // Same drain discipline as `dispatch_mouse_two_pane`: close the
    // writer so the peer's recv loop terminates on EOF.
    drop(conn);
    let mut received = Vec::new();
    loop {
        let next = tokio::time::timeout(PEER_DRAIN_DEADLINE, peer.recv())
            .await
            .expect("timed out draining the peer connection");
        match next {
            Ok(frame) => received.push(frame),
            Err(_) => break,
        }
    }
    (workspace.active, received, overlays.is_active())
}

/// A left press on window 1's tab in the BOTTOM bar (the user-reported
/// dogfood case) selects it — the same `select-window` a keybinding or
/// sidebar click runs — and forwards nothing to a pane.
#[tokio::test]
async fn bar_click_on_window_tab_selects_it() {
    use crate::render::chrome::status_bar::Position;
    // "0:bash 1:vim" — column 8 is inside window 1's tab; bottom bar
    // row of a 24-row viewport is y = 23.
    let (active, received, _) =
        dispatch_bar_click(left_press_at(8, 23), Position::Bottom, true).await;
    assert_eq!(active, 1, "clicking window 1's tab must select it");
    assert!(
        received.is_empty(),
        "a bar-row click must not reach a pane; got {received:?}"
    );
}

/// The same tab click works with the bar docked at the TOP (phux-foz.8):
/// the claimed row is y = 0 and the pane content below is untouched.
#[tokio::test]
async fn bar_click_honors_top_placement() {
    use crate::render::chrome::status_bar::Position;
    let (active, received, _) = dispatch_bar_click(left_press_at(8, 0), Position::Top, true).await;
    assert_eq!(active, 1, "top-docked tab click must select window 1");
    assert!(received.is_empty());
}

/// A click on the bar row that misses every tab (separator, blank
/// padding, another widget's cells) is consumed as chrome: no select,
/// no forward, exactly the pre-claim no-op.
#[tokio::test]
async fn bar_click_on_non_tab_cell_is_a_noop() {
    use crate::render::chrome::status_bar::Position;
    // Column 6 is the tab separator; column 40 is blank padding.
    for x in [6, 40] {
        let (active, received, _) =
            dispatch_bar_click(left_press_at(x, 23), Position::Bottom, true).await;
        assert_eq!(active, 0, "col {x} must not select");
        assert!(
            received.is_empty(),
            "col {x} must not forward; got {received:?}"
        );
    }
}

/// With a TOP bar, a click on the bottom row is pane content — the bar
/// claim must not intercept it (it forwards to the pane under it).
#[tokio::test]
async fn bar_claim_leaves_pane_content_alone() {
    use crate::render::chrome::status_bar::Position;
    let (active, received, _) = dispatch_bar_click(left_press_at(8, 23), Position::Top, true).await;
    assert_eq!(active, 0, "a pane click must not select a window");
    match received.as_slice() {
        [FrameKind::InputMouse { terminal_id, .. }] => assert_eq!(*terminal_id, tid(1)),
        other => panic!("expected the click to forward to the pane, got {other:?}"),
    }
}

/// phux-wrnm: a right press on a tab selects that window and opens its
/// window menu — the tab-bar equivalent of right-clicking a browser tab.
#[tokio::test]
async fn bar_right_press_on_a_tab_selects_it_and_opens_the_window_menu() {
    use crate::render::chrome::status_bar::Position;
    let (active, received, overlay) =
        dispatch_bar_click(right_press_at(8, 23), Position::Bottom, true).await;
    assert_eq!(active, 1, "right-clicking a tab selects its window");
    assert!(overlay, "and opens that window's menu");
    assert!(received.is_empty(), "nothing reaches a pane: {received:?}");
}

/// phux-wrnm: off the tabs the bar is session chrome — the session name,
/// the widgets, the padding — so a right press there opens the session
/// menu instead of being swallowed.
#[tokio::test]
async fn bar_right_press_off_the_tabs_opens_the_session_menu() {
    use crate::render::chrome::status_bar::Position;
    // Column 40 is blank padding on the "0:bash 1:vim" strip.
    let (active, received, overlay) =
        dispatch_bar_click(right_press_at(40, 23), Position::Bottom, true).await;
    assert_eq!(active, 0, "no tab pointed at ⇒ no window switch");
    assert!(overlay, "the session menu opens");
    assert!(received.is_empty(), "nothing reaches a pane: {received:?}");
}

/// A bar reservation without a lent painter (headless paths, stale
/// fixtures) still claims the row safely: consumed, no panic, no select.
#[tokio::test]
async fn bar_click_without_painter_is_consumed() {
    use crate::render::chrome::status_bar::Position;
    let (active, received, _) =
        dispatch_bar_click(left_press_at(8, 23), Position::Bottom, false).await;
    assert_eq!(active, 0);
    assert!(received.is_empty());
}

/// The pure click->action mapping mirrors `sidebar_click_action`: a tab
/// column commits `select-window { index }`; non-tab columns and a
/// missing painter commit nothing — and the committed name must be a
/// dispatched action (the palette-registry lockstep).
#[test]
fn bar_click_action_maps_tab_columns_to_select_window() {
    use crate::render::chrome::status_bar::Position;
    let painter = painted_windows_bar(Position::Bottom, 80, 24);
    let resolved = bar_click_action(Some(&painter), 8).expect("tab column hits");
    assert_eq!(resolved.action, "select-window");
    assert_eq!(index_arg(&resolved), Some(1));
    assert!(
        ACTION_NAMES.contains(&resolved.action.as_str()),
        "bar committed `{}`, which run_action does not dispatch",
        resolved.action,
    );
    assert!(bar_click_action(Some(&painter), 6).is_none(), "separator");
    assert!(bar_click_action(Some(&painter), 40).is_none(), "padding");
    assert!(bar_click_action(None, 8).is_none(), "no painter");
}

// -- phux-npb3: per-pane mouse opt-out + drag double-press hardening ---
// (reuses the `two_pane_workspace` fixture defined for the resize-pane
// dispatch tests above.)

/// Build a mouse event in outer-viewport cell coordinates.
fn mev(action: MouseAction, button: MouseButton, x: f64, y: f64) -> MouseEvent {
    MouseEvent {
        action,
        button,
        mods: phux_protocol::input::key::ModSet::empty(),
        x,
        y,
    }
}

/// The divider column of [`two_pane_workspace`] at viewport 80x24 (no
/// bar, no sidebar), found by hit-testing rather than hardcoding the
/// rasterizer's rounding.
fn two_pane_divider_x() -> u16 {
    use crate::multi_pane::{RouteDecision, route_mouse_event};
    let workspace = two_pane_workspace();
    let ls = workspace.active_window().expect("active window");
    let content = content_rect((80, 24), None, None);
    (0..80u16)
        .find(|&x| {
            matches!(
                route_mouse_event(
                    ls,
                    content,
                    (80, 24),
                    &mev(MouseAction::Press, MouseButton::Left, f64::from(x), 5.0),
                ),
                RouteDecision::Divider { .. }
            )
        })
        .expect("a two-pane split has a divider column")
}

/// Drive `dispatch_input_events` with `events` against
/// [`two_pane_workspace`] (viewport 80x24, no bar / sidebar), seeding
/// the per-pane opt-out set with `seed_optout`. Returns every frame the
/// peer end of the connection received plus the post-dispatch drag,
/// focus, and opt-out state.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
async fn dispatch_mouse_two_pane(
    events: Vec<InputEvent>,
    seed_optout: &[TerminalId],
) -> (
    Vec<FrameKind>,
    Option<DragGrab>,
    Option<TerminalId>,
    std::collections::HashSet<TerminalId>,
) {
    dispatch_mouse_two_pane_with(events, seed_optout, &[], (1, 1)).await
}

/// [`dispatch_mouse_two_pane`] with pane slots: each `(id, vt)` entry
/// allocates a [`PaneSlot`] mirror and feeds it `vt` (mode seeds like
/// `?1049h`), so the wheel branch's mode gates are exercised against
/// real libghostty state. `cell_px` is the ctx cell geometry for the
/// SPEC §3.1 cells→pixels send-boundary scaling.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
async fn dispatch_mouse_two_pane_with(
    events: Vec<InputEvent>,
    seed_optout: &[TerminalId],
    seed_vt: &[(TerminalId, &[u8])],
    cell_px: (u16, u16),
) -> (
    Vec<FrameKind>,
    Option<DragGrab>,
    Option<TerminalId>,
    std::collections::HashSet<TerminalId>,
) {
    let mut overlays = OverlayState::new();
    let (frames, drag, focused, optout, _repaint) =
        dispatch_mouse_two_pane_into(&mut overlays, events, seed_optout, seed_vt, cell_px).await;
    (frames, drag, focused, optout)
}

/// [`dispatch_mouse_two_pane_with`] against a caller-owned overlay
/// stack, so a test can assert what the batch pushed (phux-wrnm: the
/// right-click menus) as well as what it sent.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
async fn dispatch_mouse_two_pane_into(
    overlays: &mut OverlayState,
    events: Vec<InputEvent>,
    seed_optout: &[TerminalId],
    seed_vt: &[(TerminalId, &[u8])],
    cell_px: (u16, u16),
) -> (
    Vec<FrameKind>,
    Option<DragGrab>,
    Option<TerminalId>,
    std::collections::HashSet<TerminalId>,
    bool,
) {
    let mut workspace = two_pane_workspace();
    let (a, b) = tokio::net::UnixStream::pair().expect("uds pair");
    let mut conn = Connection::from_stream(a);
    let mut peer = Connection::from_stream(b);
    let mut out: Vec<u8> = Vec::new();
    let mut focused_pane = Some(tid(1));
    let mut detach_pending = false;
    let mut predict = PredictionState::new(crate::predict::PredictiveConfig::disabled(), 80, 24);
    let overlay = Overlay;
    let entries: Vec<_> = seed_vt
        .iter()
        .map(|(id, bytes)| (id, 39, 24, *bytes))
        .collect();
    let (mut engine_kernel, _, mut panes) =
        super::super::pane_state::published_test_state(&entries);
    let mut next_request_id = 1;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    let theme = Theme::default();
    let mut switch_request = None;
    let mut session_name = String::new();
    let mut zoomed = None;
    let mut sidebar_enabled = false;
    let mut drag: Option<DragGrab> = None;
    let mut mouse_optout: std::collections::HashSet<TerminalId> =
        seed_optout.iter().cloned().collect();
    let mut reload_request = false;
    let fleet_agent_meta = HashMap::new();
    let mut fleet_vcs = crate::attach::pane_state::VcsIndex::default();
    let repainted;
    {
        // phux-k0cw: the strip's shape comes from the painted target
        // table now, not from the workspace, so a fixture that wants
        // hit-testable window rows must declare them.
        let sidebar_targets = targets(0, workspace.windows.len(), 0);
        let mut ctx = DispatchCtx {
            engine_kernel: &mut engine_kernel,
            resolver: None,
            focus_history: FocusHistory::default(),
            workspace: &mut workspace,
            viewport: (80, 24),
            cell_px,
            next_request_id: &mut next_request_id,
            spawn_initial_size_supported: true,
            pending_splits: &mut pending_splits,
            pending_windows: &mut pending_windows,
            expected_closes: &mut HashSet::new(),
            overlays,
            keybindings: None,
            theme: &theme,
            sessions: &[],
            foreign_layouts: &HashMap::new(),
            foreign_agents: &HashMap::new(),
            focused_session: None,
            session_name: &mut session_name,
            switch_request: &mut switch_request,
            zoomed: &mut zoomed,
            sidebar: None,
            sidebar_enabled: &mut sidebar_enabled,
            sidebar_width: 20,
            chrome: ChromeBreakpoints::default(),
            sidebar_targets: &sidebar_targets,
            bar: None,
            status_bar: None,
            drag: &mut drag,
            mouse_optout: &mut mouse_optout,
            attention_navigation: &mut AttentionNavigation::default(),
            plugin_actions: &[],
            plugin_panes: &[],
            plugin_tx: None,
            reload_request: &mut reload_request,
            agent_meta: &fleet_agent_meta,
            vcs: &mut fleet_vcs,
        };
        repainted = dispatch_input_events(
            &mut out,
            &mut conn,
            events,
            &mut focused_pane,
            &mut detach_pending,
            &mut predict,
            &overlay,
            &mut panes,
            &mut ctx,
        )
        .await
        .expect("dispatch");
    }
    // Close the writer so the peer's drain terminates: once the buffered
    // frames are consumed, `recv` sees the EOF and returns Disconnected.
    // (`try_recv` is not used here — tokio's non-blocking read reports
    // WouldBlock until the reactor has observed readiness, which this
    // freshly-paired socket never awaited.)
    drop(conn);
    let mut received = Vec::new();
    loop {
        let next = tokio::time::timeout(PEER_DRAIN_DEADLINE, peer.recv())
            .await
            .expect("timed out draining the peer connection");
        match next {
            Ok(frame) => received.push(frame),
            Err(_) => break, // EOF after the writer dropped
        }
    }
    (received, drag, focused_pane, mouse_optout, repainted)
}

/// phux-npb3 hardening: a second Press arriving while a divider drag is
/// active must be consumed — not fall through to normal routing, where
/// it would move focus and forward an `INPUT_MOUSE` mid-drag.
#[tokio::test]
async fn second_press_during_divider_drag_is_consumed() {
    let dx = f64::from(two_pane_divider_x());
    let (received, drag, focused, _) = dispatch_mouse_two_pane(
        vec![
            InputEvent::Mouse(mev(MouseAction::Press, MouseButton::Left, dx, 5.0)),
            InputEvent::Mouse(mev(MouseAction::Press, MouseButton::Left, 70.0, 5.0)),
        ],
        &[],
    )
    .await;
    assert!(drag.is_some(), "the divider press grabs a drag");
    assert_eq!(
        focused,
        Some(tid(1)),
        "a press mid-drag must not move focus"
    );
    assert!(
        received.is_empty(),
        "a press mid-drag must not forward to a pane; got {received:?}"
    );
}

/// The double-press guard must not eat the release that ends the drag.
#[tokio::test]
async fn release_after_guarded_press_still_ends_drag() {
    let dx = f64::from(two_pane_divider_x());
    let (_received, drag, _focused, _) = dispatch_mouse_two_pane(
        vec![
            InputEvent::Mouse(mev(MouseAction::Press, MouseButton::Left, dx, 5.0)),
            InputEvent::Mouse(mev(MouseAction::Press, MouseButton::Right, 70.0, 5.0)),
            InputEvent::Mouse(mev(MouseAction::Release, MouseButton::Left, 70.0, 5.0)),
        ],
        &[],
    )
    .await;
    assert!(
        drag.is_none(),
        "the release must still end the drag after a guarded press"
    );
}

/// phux-npb3 routing: a press inside an opted-out pane still
/// click-focuses it (chrome-level — that is also what makes the driver
/// drop outer capture), but no `INPUT_MOUSE` is synthesized for it.
#[tokio::test]
async fn press_in_opted_out_pane_focuses_but_does_not_forward() {
    let (received, _, focused, _) = dispatch_mouse_two_pane(
        vec![InputEvent::Mouse(mev(
            MouseAction::Press,
            MouseButton::Left,
            70.0,
            5.0,
        ))],
        &[tid(2)],
    )
    .await;
    assert_eq!(
        focused,
        Some(tid(2)),
        "click-to-focus still applies to an opted-out pane"
    );
    assert!(
        received.is_empty(),
        "an opted-out pane must receive no INPUT_MOUSE; got {received:?}"
    );
}

/// The opt-out is per-pane: a sibling that did NOT opt out still gets
/// its `INPUT_MOUSE` forwarded (with pane-local coordinates) while the
/// other pane sits in the opt-out set.
#[tokio::test]
async fn press_in_opted_in_sibling_still_forwards() {
    let dx = two_pane_divider_x();
    let (received, _, focused, _) = dispatch_mouse_two_pane(
        vec![InputEvent::Mouse(mev(
            MouseAction::Press,
            MouseButton::Left,
            70.0,
            5.0,
        ))],
        &[tid(1)], // the OTHER pane is opted out
    )
    .await;
    assert_eq!(focused, Some(tid(2)));
    match received.as_slice() {
        [FrameKind::InputMouse { terminal_id, event }] => {
            assert_eq!(*terminal_id, tid(2));
            assert!(
                event.x < f64::from(dx),
                "forwarded coordinates are pane-local; got x = {}",
                event.x
            );
        }
        other => panic!("expected exactly one INPUT_MOUSE, got {other:?}"),
    }
}

// -- phux-yyex: wheel routing per pane screen/mode state ---------------

/// Wheel over an alt-screen pane without mouse tracking synthesizes
/// arrow-key presses (xterm alternate scroll, DECSET 1007 — default ON
/// in libghostty): the alt screen has no scrollback, so the local
/// viewport scroll would be a silent no-op and the wheel would go dead.
#[tokio::test]
async fn wheel_in_alt_screen_pane_synthesizes_arrow_keys() {
    let (received, _, _, _) = dispatch_mouse_two_pane_with(
        vec![InputEvent::Mouse(mev(
            MouseAction::Press,
            MouseButton::Four,
            70.0,
            5.0,
        ))],
        &[],
        &[(tid(2), b"\x1b[?1049h")],
        (1, 1),
    )
    .await;
    assert_eq!(
        received.len(),
        3,
        "one wheel notch = 3 arrows: {received:?}"
    );
    for frame in &received {
        match frame {
            FrameKind::InputKey { terminal_id, event } => {
                assert_eq!(*terminal_id, tid(2));
                assert_eq!(event.key, PhysicalKey::ArrowUp);
            }
            other => panic!("expected INPUT_KEY, got {other:?}"),
        }
    }
}

/// Wheel-down in the same alt-screen pane maps to `ArrowDown`.
#[tokio::test]
async fn wheel_down_in_alt_screen_pane_synthesizes_arrow_down() {
    let (received, _, _, _) = dispatch_mouse_two_pane_with(
        vec![InputEvent::Mouse(mev(
            MouseAction::Press,
            MouseButton::Five,
            70.0,
            5.0,
        ))],
        &[],
        &[(tid(2), b"\x1b[?1049h")],
        (1, 1),
    )
    .await;
    assert_eq!(received.len(), 3);
    for frame in &received {
        match frame {
            FrameKind::InputKey { event, .. } => {
                assert_eq!(event.key, PhysicalKey::ArrowDown);
            }
            other => panic!("expected INPUT_KEY, got {other:?}"),
        }
    }
}

/// An app that opted out of alternate scroll (`?1007l`) gets neither
/// arrows nor a forwarded wheel — matching xterm, the wheel is inert on
/// an alt screen that asked for silence.
#[tokio::test]
async fn wheel_with_alt_scroll_off_sends_nothing() {
    let (received, _, _, _) = dispatch_mouse_two_pane_with(
        vec![InputEvent::Mouse(mev(
            MouseAction::Press,
            MouseButton::Four,
            70.0,
            5.0,
        ))],
        &[],
        &[(tid(2), b"\x1b[?1049h\x1b[?1007l")],
        (1, 1),
    )
    .await;
    assert!(received.is_empty(), "expected no frames, got {received:?}");
}

/// Wheel over a primary-screen pane without mouse tracking is consumed
/// by the local scrollback viewport — nothing crosses the wire.
#[tokio::test]
async fn wheel_in_primary_screen_pane_scrolls_locally() {
    let (received, _, _, _) = dispatch_mouse_two_pane_with(
        vec![InputEvent::Mouse(mev(
            MouseAction::Press,
            MouseButton::Four,
            70.0,
            5.0,
        ))],
        &[],
        &[(tid(2), b"")],
        (1, 1),
    )
    .await;
    assert!(received.is_empty(), "expected no frames, got {received:?}");
}

/// Wheel over a pane whose app tracks the mouse (Claude Code sets
/// `?1000h ?1002h ?1003h ?1006h`) forwards the wheel as `INPUT_MOUSE` so
/// the app scrolls itself.
#[tokio::test]
async fn wheel_in_mouse_tracking_pane_forwards_input_mouse() {
    let (received, _, _, _) = dispatch_mouse_two_pane_with(
        vec![InputEvent::Mouse(mev(
            MouseAction::Press,
            MouseButton::Four,
            70.0,
            5.0,
        ))],
        &[],
        &[(
            tid(2),
            b"\x1b[?1049h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h",
        )],
        (1, 1),
    )
    .await;
    match received.as_slice() {
        [FrameKind::InputMouse { terminal_id, event }] => {
            assert_eq!(*terminal_id, tid(2));
            assert_eq!(event.button, MouseButton::Four);
        }
        other => panic!("expected exactly one INPUT_MOUSE, got {other:?}"),
    }
}

// ---------- phux-wrnm: right-click context menus (ADR-0058) ----------

/// A right press over a pane whose app has NOT asked for the mouse
/// opens the pane menu and forwards nothing: the button belongs to the
/// client here, exactly as the left button does for drag-to-copy.
#[tokio::test]
async fn right_press_on_a_pane_opens_the_pane_menu() {
    let mut overlays = OverlayState::new();
    let (received, _, _, _, _) = dispatch_mouse_two_pane_into(
        &mut overlays,
        vec![InputEvent::Mouse(mev(
            MouseAction::Press,
            MouseButton::Right,
            70.0,
            5.0,
        ))],
        &[],
        &[(tid(2), b"")],
        (1, 1),
    )
    .await;
    assert!(overlays.is_active(), "the pane menu must be pushed");
    assert!(
        overlays.wants_pointer_hover(),
        "a menu hover-tracks the pointer, so the driver raises ?1003h",
    );
    assert!(
        received.is_empty(),
        "the right press is consumed by the menu; got {received:?}",
    );
}

/// Click-to-focus runs first, so the menu acts on the pane you pointed
/// at rather than the one that happened to hold focus.
#[tokio::test]
async fn right_press_focuses_the_pane_under_the_pointer_first() {
    let mut overlays = OverlayState::new();
    let (_, _, focused, _, _) = dispatch_mouse_two_pane_into(
        &mut overlays,
        vec![InputEvent::Mouse(mev(
            MouseAction::Press,
            MouseButton::Right,
            70.0,
            5.0,
        ))],
        &[],
        &[(tid(2), b"")],
        (1, 1),
    )
    .await;
    assert_eq!(focused, Some(tid(2)), "the right-clicked pane takes focus");
    assert!(overlays.is_active());
}

/// An inner program that turned mouse tracking on owns every button —
/// including the right one, which many TUIs bind to their own menu. The
/// client must forward rather than steal it (the keyboard `context-menu`
/// action is the way in for those panes).
#[tokio::test]
async fn right_press_in_a_mouse_tracking_pane_forwards_instead_of_opening_a_menu() {
    let mut overlays = OverlayState::new();
    let (received, _, _, _, _) = dispatch_mouse_two_pane_into(
        &mut overlays,
        vec![InputEvent::Mouse(mev(
            MouseAction::Press,
            MouseButton::Right,
            70.0,
            5.0,
        ))],
        &[],
        &[(tid(2), b"\x1b[?1000h\x1b[?1006h")],
        (1, 1),
    )
    .await;
    assert!(
        !overlays.is_active(),
        "the app owns the mouse; no menu may cover it",
    );
    match received.as_slice() {
        [FrameKind::InputMouse { terminal_id, event }] => {
            assert_eq!(*terminal_id, tid(2));
            assert_eq!(event.button, MouseButton::Right);
        }
        other => panic!("expected the right press to be forwarded, got {other:?}"),
    }
}

/// Clicking away closes the menu — and schedules the repaint that
/// erases it. Without the dismissal repaint the box stayed painted over
/// the panes until unrelated output happened to redraw them.
#[tokio::test]
async fn clicking_outside_the_menu_closes_it_and_repaints() {
    let mut overlays = OverlayState::new();
    let (_, _, _, _, repainted) = dispatch_mouse_two_pane_into(
        &mut overlays,
        vec![
            // Open at the far right of pane 2, then click back over pane 1.
            InputEvent::Mouse(mev(MouseAction::Press, MouseButton::Right, 70.0, 5.0)),
            InputEvent::Mouse(mev(MouseAction::Press, MouseButton::Left, 2.0, 2.0)),
        ],
        &[],
        &[(tid(2), b"")],
        (1, 1),
    )
    .await;
    assert!(!overlays.is_active(), "the click outside dismissed it");
    assert!(repainted, "dismissal must schedule a repaint");
}

/// A pane that opted out via `set-pane mouse off` gets no menu either:
/// the whole point of the opt-out is that the client stops claiming
/// that pane's pointer events (phux-npb3).
#[tokio::test]
async fn right_press_on_an_opted_out_pane_opens_nothing() {
    let mut overlays = OverlayState::new();
    let (received, _, _, _, _) = dispatch_mouse_two_pane_into(
        &mut overlays,
        vec![InputEvent::Mouse(mev(
            MouseAction::Press,
            MouseButton::Right,
            70.0,
            5.0,
        ))],
        &[tid(2)],
        &[(tid(2), b"")],
        (1, 1),
    )
    .await;
    assert!(!overlays.is_active());
    assert!(received.is_empty(), "opted out ⇒ nothing forwarded either");
}

/// SPEC input.md §3.1: forwarded `INPUT_MOUSE` positions are surface-space
/// pixels (`cell_index x cell_size`), scaled at the send boundary from
/// the dispatcher's pane-local cell coordinates.
#[tokio::test]
async fn forwarded_input_mouse_scales_cells_to_surface_pixels() {
    let dx = two_pane_divider_x();
    let (received, _, _, _) = dispatch_mouse_two_pane_with(
        vec![InputEvent::Mouse(mev(
            MouseAction::Press,
            MouseButton::Left,
            70.0,
            5.0,
        ))],
        &[],
        &[(tid(2), b"\x1b[?1000h\x1b[?1006h")],
        (8, 16),
    )
    .await;
    // Pane 2's content starts one column right of the divider.
    let expected_x = (70.0 - f64::from(dx) - 1.0) * 8.0;
    let expected_y = 5.0 * 16.0;
    match received.as_slice() {
        [FrameKind::InputMouse { event, .. }] => {
            assert!((event.x - expected_x).abs() < f64::EPSILON);
            assert!((event.y - expected_y).abs() < f64::EPSILON);
        }
        other => panic!("expected exactly one INPUT_MOUSE, got {other:?}"),
    }
}

/// Run a `set-pane` action against `workspace` with a caller-owned
/// opt-out set, returning the effects.
fn run_set_pane(
    mouse: Option<toml::Value>,
    workspace: &mut Workspace,
    mouse_optout: &mut std::collections::HashSet<TerminalId>,
) -> ActionEffects {
    let mut next_request_id = 100;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    let mut overlays = OverlayState::new();
    let theme = Theme::default();
    let mut switch_request = None;
    let mut session_name = String::new();
    let mut zoomed = None;
    let mut sidebar_enabled = false;
    let mut drag: Option<DragGrab> = None;
    let mut reload_request = false;
    let fleet_agent_meta = HashMap::new();
    let mut fleet_vcs = crate::attach::pane_state::VcsIndex::default();
    let mut engine_kernel = test_engine_kernel();
    // phux-k0cw: the strip's shape comes from the painted target
    // table now, not from the workspace, so a fixture that wants
    // hit-testable window rows must declare them.
    let sidebar_targets = targets(0, workspace.windows.len(), 0);
    let mut ctx = DispatchCtx {
        engine_kernel: &mut engine_kernel,
        resolver: None,
        focus_history: FocusHistory::default(),
        workspace,
        viewport: (80, 24),
        cell_px: (1, 1),
        next_request_id: &mut next_request_id,
        spawn_initial_size_supported: true,
        pending_splits: &mut pending_splits,
        pending_windows: &mut pending_windows,
        expected_closes: &mut HashSet::new(),
        overlays: &mut overlays,
        keybindings: None,
        theme: &theme,
        sessions: &[],
        foreign_layouts: &HashMap::new(),
        foreign_agents: &HashMap::new(),
        focused_session: None,
        session_name: &mut session_name,
        switch_request: &mut switch_request,
        zoomed: &mut zoomed,
        sidebar: None,
        sidebar_enabled: &mut sidebar_enabled,
        sidebar_width: 20,
        chrome: ChromeBreakpoints::default(),
        sidebar_targets: &sidebar_targets,
        bar: None,
        status_bar: None,
        drag: &mut drag,
        mouse_optout,
        attention_navigation: &mut AttentionNavigation::default(),
        plugin_actions: &[],
        plugin_panes: &[],
        plugin_tx: None,
        reload_request: &mut reload_request,
        agent_meta: &fleet_agent_meta,
        vcs: &mut fleet_vcs,
    };
    let mut action = bare_action("set-pane");
    if let Some(v) = mouse {
        action.args.insert("mouse".to_owned(), v);
    }
    let focused = ctx.workspace.active_window().and_then(|w| w.focus.clone());
    run_action(&action, &mut ctx, focused.as_ref(), &HashMap::new())
}

#[test]
fn set_pane_mouse_off_then_on_updates_optout() {
    let mut workspace = Workspace::single(tid(1));
    let mut optout = std::collections::HashSet::new();
    let effects = run_set_pane(
        Some(toml::Value::String("off".to_owned())),
        &mut workspace,
        &mut optout,
    );
    assert!(!effects.bell);
    assert!(optout.contains(&tid(1)), "`mouse = off` opts the pane out");

    let effects = run_set_pane(
        Some(toml::Value::String("on".to_owned())),
        &mut workspace,
        &mut optout,
    );
    assert!(!effects.bell);
    assert!(
        !optout.contains(&tid(1)),
        "`mouse = on` opts the pane back in"
    );
}

#[test]
fn set_pane_toggle_flips_state() {
    let mut workspace = Workspace::single(tid(1));
    let mut optout = std::collections::HashSet::new();
    let toggle = || toml::Value::String("toggle".to_owned());
    run_set_pane(Some(toggle()), &mut workspace, &mut optout);
    assert!(optout.contains(&tid(1)), "first toggle opts out");
    run_set_pane(Some(toggle()), &mut workspace, &mut optout);
    assert!(!optout.contains(&tid(1)), "second toggle opts back in");
}

#[test]
fn set_pane_bool_arg_maps_to_on_off() {
    let mut workspace = Workspace::single(tid(1));
    let mut optout = std::collections::HashSet::new();
    run_set_pane(
        Some(toml::Value::Boolean(false)),
        &mut workspace,
        &mut optout,
    );
    assert!(optout.contains(&tid(1)), "`mouse = false` means off");
    run_set_pane(
        Some(toml::Value::Boolean(true)),
        &mut workspace,
        &mut optout,
    );
    assert!(!optout.contains(&tid(1)), "`mouse = true` means on");
}

#[test]
fn set_pane_missing_or_bad_mouse_arg_bells() {
    let mut workspace = Workspace::single(tid(1));
    let mut optout = std::collections::HashSet::new();
    let effects = run_set_pane(None, &mut workspace, &mut optout);
    assert!(effects.bell, "missing `mouse` arg bells");
    let effects = run_set_pane(
        Some(toml::Value::String("sideways".to_owned())),
        &mut workspace,
        &mut optout,
    );
    assert!(effects.bell, "unknown `mouse` value bells");
    assert!(optout.is_empty());
}

#[test]
fn set_pane_without_focused_pane_bells() {
    let mut workspace = Workspace::default();
    let mut optout = std::collections::HashSet::new();
    let effects = run_set_pane(
        Some(toml::Value::String("off".to_owned())),
        &mut workspace,
        &mut optout,
    );
    assert!(effects.bell, "no focused pane to set");
    assert!(optout.is_empty());
}

// ---- phux-51n6.1: predictive-echo full-screen-app (alt-screen) gate ----

use crate::predict::{PredictionState, PredictiveConfig};

/// A fresh shell-prompt pane (main screen) is not in app mode: the gate
/// must let prediction through.
#[test]
fn alt_screen_gate_false_on_main_screen() {
    let terminal = libghostty_vt::Terminal::new(libghostty_vt::TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: 100,
    })
    .expect("terminal");
    assert!(
        !terminal_in_alt_screen(&terminal),
        "a fresh pane sits on the main screen — predict here"
    );
}

/// Entering the alternate screen (`?1049h`, as vim/nvim/less/agent TUIs
/// do) trips the gate; leaving it (`?1049l`) clears it. The legacy
/// `?1047h` variant is caught too.
#[test]
fn alt_screen_gate_tracks_dec_private_modes() {
    let mut terminal = libghostty_vt::Terminal::new(libghostty_vt::TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: 100,
    })
    .expect("terminal");
    terminal.vt_write(b"\x1b[?1049h");
    assert!(
        terminal_in_alt_screen(&terminal),
        "1049h (save-cursor alt screen) is app mode"
    );
    terminal.vt_write(b"\x1b[?1049l");
    assert!(
        !terminal_in_alt_screen(&terminal),
        "1049l returns to the main screen — predict again"
    );

    let mut legacy = libghostty_vt::Terminal::new(libghostty_vt::TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: 100,
    })
    .expect("terminal");
    legacy.vt_write(b"\x1b[?1047h");
    assert!(
        terminal_in_alt_screen(&legacy),
        "1047h (legacy alt screen) is app mode too"
    );
}

/// Drive the REAL [`dispatch_input_events`] with one printable keystroke
/// against a focused pane, returning the predictor afterward so tests
/// can assert on both the queue and the ADR-0090 display policy. When
/// `alt_screen` is set, the pane's mirror is switched to the alternate
/// screen (`?1049h`) before dispatch, so the keystroke must queue a
/// prediction whose display stays confirmation-gated.
///
/// This exercises the true dispatch-site behaviour (the
/// `predict.set_alt_screen(...)` sync and the timestamped predict call)
/// end to end rather than re-stating it inline — so a refactor that
/// silently drops the screen-mode sync turns the alt-screen case red
/// instead of passing on a private copy of the predicate.
#[allow(
    clippy::future_not_send,
    reason = "client-side libghostty Terminal is !Send; ADR-0003 binds us to current-thread"
)]
async fn predict_state_after_key_dispatch(alt_screen: bool) -> PredictionState {
    use phux_protocol::input::key::PhysicalKey;

    let theme = Theme::default();
    let mut overlays = OverlayState::new();
    let (a, _b) = tokio::net::UnixStream::pair().expect("uds pair");
    let mut conn = Connection::from_stream(a);
    let mut out: Vec<u8> = Vec::new();
    let mut workspace = Workspace::single(tid(1));
    let mut focused_pane = Some(tid(1));
    let mut detach_pending = false;
    // Enabled predictor, fresh (un-suspended) — a printable insert at the
    // origin cursor is predictable, so the only thing standing between the
    // keystroke and a queued ghost is the app-mode gate under test.
    let mut predict = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
    let overlay = Overlay;
    // The focused replica carries the alt-screen signal the gate reads
    // via `terminal.mode()`. A fresh pane sits on the main screen (cooked
    // shell prompt); `?1049h` puts it in a full-screen app.
    let bootstrap: &[u8] = if alt_screen { b"\x1b[?1049h" } else { b"" };
    let (mut engine_kernel, _, mut panes) =
        super::super::pane_state::published_test_state(&[(&tid(1), 80, 24, bootstrap)]);

    let mut next_request_id = 1;
    let mut pending_splits = HashMap::new();
    let mut pending_windows = HashMap::new();
    let mut switch_request = None;
    let mut session_name = String::new();
    let mut zoomed = None;
    let mut sidebar_enabled = false;
    let mut drag: Option<DragGrab> = None;
    let mut reload_request = false;
    let mut mouse_optout: std::collections::HashSet<TerminalId> = std::collections::HashSet::new();
    let fleet_agent_meta = HashMap::new();
    let mut fleet_vcs = crate::attach::pane_state::VcsIndex::default();
    // phux-k0cw: the strip's shape comes from the painted target
    // table now, not from the workspace, so a fixture that wants
    // hit-testable window rows must declare them.
    let sidebar_targets = targets(0, workspace.windows.len(), 0);
    let mut ctx = DispatchCtx {
        engine_kernel: &mut engine_kernel,
        // No resolver: every key forwards straight through to the pane,
        // past the predict layer — no keybinding interception to muddy
        // the gate assertion.
        resolver: None,
        focus_history: FocusHistory::default(),
        workspace: &mut workspace,
        viewport: (80, 24),
        cell_px: (1, 1),
        next_request_id: &mut next_request_id,
        spawn_initial_size_supported: true,
        pending_splits: &mut pending_splits,
        pending_windows: &mut pending_windows,
        expected_closes: &mut HashSet::new(),
        overlays: &mut overlays,
        keybindings: None,
        theme: &theme,
        sessions: &[],
        foreign_layouts: &HashMap::new(),
        foreign_agents: &HashMap::new(),
        focused_session: None,
        session_name: &mut session_name,
        switch_request: &mut switch_request,
        zoomed: &mut zoomed,
        sidebar: None,
        sidebar_enabled: &mut sidebar_enabled,
        sidebar_width: 20,
        chrome: ChromeBreakpoints::default(),
        sidebar_targets: &sidebar_targets,
        bar: None,
        status_bar: None,
        drag: &mut drag,
        mouse_optout: &mut mouse_optout,
        attention_navigation: &mut AttentionNavigation::default(),
        plugin_actions: &[],
        plugin_panes: &[],
        plugin_tx: None,
        reload_request: &mut reload_request,
        agent_meta: &fleet_agent_meta,
        vcs: &mut fleet_vcs,
    };

    dispatch_input_events(
        &mut out,
        &mut conn,
        vec![press(PhysicalKey::A, Some("a"))],
        &mut focused_pane,
        &mut detach_pending,
        &mut predict,
        &overlay,
        &mut panes,
        &mut ctx,
    )
    .await
    .expect("dispatch");

    predict
}

/// Cooked shell prompt (main screen): driving the real dispatch path with
/// a printable key queues exactly one speculative ghost, displayable
/// immediately (no gate on the primary screen).
#[tokio::test]
async fn dispatch_predicts_key_at_cooked_prompt() {
    let predict = predict_state_after_key_dispatch(false).await;
    assert_eq!(
        predict.pending_len(),
        1,
        "main-screen prompt: the keystroke echoes speculatively"
    );
    assert!(
        predict.should_display(predict_now_ms()),
        "primary screen displays immediately"
    );
}

/// Full-screen app (alt screen via `?1049h`, as vim/nvim/less/an agent TUI
/// do): the same real dispatch path queues the prediction — reconcile
/// needs it to measure whether the app echoes — but the ADR-0090 display
/// policy keeps it hidden until a non-blank echo confirms. Dropping the
/// `set_alt_screen` sync at the dispatch site fails this.
#[tokio::test]
async fn dispatch_predicts_but_hides_in_alt_screen_app() {
    let predict = predict_state_after_key_dispatch(true).await;
    assert_eq!(
        predict.pending_len(),
        1,
        "alt-screen app: the keystroke still queues for reconciliation"
    );
    assert!(
        !predict.should_display(predict_now_ms()),
        "no echo evidence yet — the ghost must not display"
    );
    assert!(!predict.echo_confirmed());
}
