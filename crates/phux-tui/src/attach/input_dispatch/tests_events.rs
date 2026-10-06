//! Event-routing tests: overlay key interception, sidebar and bar
//! clicks, mouse forwarding, set-pane, and predictive echo gating.
#![allow(clippy::expect_used, reason = "tests")]

use std::collections::HashSet;

use phux_config::vocab::ACTION_NAMES;
use phux_protocol::ResourceId;
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton};
use phux_protocol::wire::frame::{Command, FrameKind};

use crate::attach::input_replay::{InputReplayJournal, ReplayDisposition};
use crate::attach::paint::{SidebarEdge, SidebarReservation, content_rect};
use crate::attach::render::ReplicaWalk;
use crate::layout::Rect;
use crate::predict::{PredictionState, PredictiveConfig};
use crate::render::Theme;
use crate::render::chrome::sidebar::SidebarTarget;
use crate::render::chrome::status_bar::{BarInset, Position, StatusBarPainter, make_context};

use super::args::*;
use super::ctx::*;
use super::dispatch::*;
use super::test_support::*;

// ---- overlays and the resolver -----------------------------------------------

/// Regression: while an overlay is up the resolver is bypassed, so the
/// leader chord (and the key after it) reach the overlay as literal input.
#[tokio::test]
async fn overlay_active_prefix_key_reaches_overlay_not_resolver() {
    let keys = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let mut env = Env::new(CtxFixture::default()).with_default_bindings();
    env.fx
        .overlays
        .push(Box::new(crate::render::overlay::RecordingOverlay {
            keys: keys.clone(),
        }));
    let leader = InputEvent::Key(KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::A,
        mods: ModSet::CTRL,
        consumed_mods: ModSet::CTRL,
        composing: false,
        text: None,
        unshifted_codepoint: Some(u32::from(b'a')),
    });
    env.dispatch(vec![leader, press(PhysicalKey::X, Some("x"))])
        .await;
    let received = keys.borrow();
    assert_eq!(received.len(), 2, "{received:?}");
    assert_eq!(received[0].key, PhysicalKey::A);
    assert!(received[0].mods.contains(ModSet::CTRL));
    assert_eq!(received[1].key, PhysicalKey::X);
}

/// With the prefix pending under a passthrough popup (which-key or first-use
/// guidance), the next chord dismisses it AND runs its binding (`C-a d`
/// detaches); Esc dismisses and cancels the prefix, so a following `d` is a
/// plain keystroke.
#[tokio::test]
async fn passthrough_popups_never_eat_the_pending_chord() {
    for (events, onboarding, detaches) in [
        (vec![press(PhysicalKey::D, Some("d"))], false, true),
        (vec![press(PhysicalKey::D, Some("d"))], true, true),
        (
            vec![
                press(PhysicalKey::Escape, None),
                press(PhysicalKey::D, Some("d")),
            ],
            false,
            false,
        ),
    ] {
        let cfg = default_cfg();
        let mut env = Env::new(CtxFixture::default()).with_default_bindings();
        let prefix = phux_config::keybind::parse_chord(&cfg.keybindings.prefix).expect("prefix");
        let resolver = env.resolver.as_mut().expect("resolver");
        assert_eq!(resolver.feed(prefix), phux_config::keybind::Feed::Partial);
        let theme = Theme::default();
        if onboarding {
            env.fx
                .overlays
                .push(Box::new(crate::render::overlay::ToastOverlay::passthrough(
                    super::super::onboarding::ONBOARDING_TITLE,
                    super::super::onboarding::hint_lines(Some(&cfg.keybindings), true),
                    &theme,
                )));
        } else {
            env.fx.overlays.push(Box::new(
                crate::render::overlay::WhichKeyOverlay::from_config(&cfg.keybindings, &theme),
            ));
        }
        let sent = env.dispatch(events).await;
        assert!(!env.fx.overlays.is_active(), "the popup is dismissed");
        assert!(!env.resolver.as_ref().expect("resolver").is_pending());
        assert_eq!(sent.detach, detaches, "onboarding={onboarding}");
    }
}

/// A kitty-mode host reports Caps Lock on the prefix chord (`CSI 97;69u`);
/// the binding must still fire, as it does for the legacy `0x01`.
#[tokio::test]
async fn prefix_chord_matches_with_caps_lock_reported() {
    let cfg = default_cfg();
    let prefix = phux_config::keybind::parse_chord(&cfg.keybindings.prefix).expect("prefix");
    let mut env = Env::new(CtxFixture::default()).with_default_bindings();
    let leader = InputEvent::Key(KeyEvent {
        action: KeyAction::Press,
        key: prefix.key,
        mods: prefix.modifiers | ModSet::CAPS_LOCK,
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    });
    let sent = env
        .dispatch(vec![leader, press(PhysicalKey::D, Some("d"))])
        .await;
    assert!(sent.detach, "prefix + d must detach with Caps Lock on");
}

#[tokio::test]
async fn copy_mode_page_scroll_mutates_focused_terminal_viewport() {
    let mut replay = Vec::new();
    for n in 0..10 {
        replay.extend_from_slice(format!("line{n:02}\r\n").as_bytes());
    }
    let mut env = Env::new(CtxFixture::default()).published(&[(&tid(1), 8, 4, &replay)]);
    env.fx.viewport = (8, 4);
    let visible = |env: &mut Env<'_>| -> String {
        let terminal = super::super::pane_state::published_terminal(&env.fx.engine_kernel, &tid(1))
            .expect("published");
        let slot = env.panes.get_mut(&tid(1)).expect("pane");
        (0..6)
            .filter_map(|col| {
                slot.renderer
                    .read_grapheme_string_at(ReplicaWalk::for_test(terminal), 0, col)
                    .expect("read cell")
            })
            .collect()
    };
    let before = visible(&mut env);
    env.fx
        .overlays
        .push(Box::new(crate::render::overlay::CopyModeOverlay::new(
            0, 0, 8, 4,
        )));
    let sent = env.dispatch(vec![press(PhysicalKey::PageUp, None)]).await;
    assert!(sent.repainted, "scrolling copy-mode triggers a repaint");
    assert_ne!(
        before,
        visible(&mut env),
        "the scroll reaches the focused viewport"
    );
}

/// Copy-mode search end to end through the dispatcher: `/needle` Enter
/// searches the pane's own loaded history, scrolls the hit into view, and
/// selects it; `n` wraps to the next hit and `N` goes back.
#[tokio::test]
async fn copy_mode_search_jumps_between_hits_in_scrollback() {
    let mut replay = Vec::new();
    for n in 0..30 {
        let word = if n == 3 || n == 12 { "needle" } else { "line" };
        replay.extend_from_slice(format!("{word}{n:02}\r\n").as_bytes());
    }
    let mut env = Env::new(CtxFixture::default()).published(&[(&tid(1), 10, 5, &replay)]);
    env.fx.viewport = (10, 5);
    let top_line = |env: &mut Env<'_>| -> String {
        let terminal = super::super::pane_state::published_terminal(&env.fx.engine_kernel, &tid(1))
            .expect("published");
        let slot = env.panes.get_mut(&tid(1)).expect("pane");
        (0..8)
            .filter_map(|col| {
                slot.renderer
                    .read_grapheme_string_at(ReplicaWalk::for_test(terminal), 0, col)
                    .expect("read cell")
            })
            .collect()
    };
    env.fx
        .overlays
        .push(Box::new(crate::render::overlay::CopyModeOverlay::new(
            4, 0, 10, 5,
        )));
    let typed = |text: &str| -> Vec<InputEvent> {
        text.chars()
            .map(|ch| press(PhysicalKey::A, Some(&ch.to_string())))
            .collect()
    };

    let mut search = typed("/needle");
    search.push(press(PhysicalKey::Enter, None));
    let sent = env.dispatch(search).await;
    assert!(sent.repainted, "a search moves the cursor: repaint");
    // From the live bottom, forward wraps to the oldest hit (row 3) and
    // centers it: rows 1..=5 show, the hit on row 2.
    assert_eq!(
        env.fx.overlays.copy_search_status().as_deref(),
        Some("/needle 1/2")
    );
    assert_eq!(top_line(&mut env), "line01");
    let sel = env.fx.overlays.copy_selection().expect("copy-mode");
    assert_eq!(
        (sel.start_row, sel.start_col, sel.end_row, sel.end_col),
        (2, 0, 2, 5)
    );

    env.dispatch(vec![press(PhysicalKey::N, Some("n"))]).await;
    assert_eq!(
        env.fx.overlays.copy_search_status().as_deref(),
        Some("/needle 2/2")
    );
    assert_eq!(top_line(&mut env), "line10");

    env.dispatch(vec![InputEvent::Key(KeyEvent {
        mods: ModSet::SHIFT,
        ..key_event(PhysicalKey::N, Some("N"))
    })])
    .await;
    assert_eq!(
        env.fx.overlays.copy_search_status().as_deref(),
        Some("/needle 1/2")
    );

    env.dispatch(typed("/nothing-here")).await;
    env.dispatch(vec![press(PhysicalKey::Enter, None)]).await;
    assert_eq!(
        env.fx.overlays.copy_search_status().as_deref(),
        Some("/nothing-here: no match")
    );
    assert!(env.fx.overlays.is_active(), "a miss keeps copy-mode open");
}

fn key_event(key: PhysicalKey, text: Option<&str>) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: text.map(ToOwned::to_owned),
        unshifted_codepoint: None,
    }
}

/// Bracketed paste into a prompt or picker fills its text (controls
/// stripped) without submitting, dismissing, or reaching the pane.
#[tokio::test]
async fn bracketed_paste_populates_modal_text_without_reaching_the_pane() {
    type Case<'a> = (Box<dyn RenderOverlay>, &'a [u8], &'a str, Option<&'a str>);
    use crate::render::overlay::{
        OverlayOutcome, PromptOverlay, RenderOverlay, SelectItem, SelectList,
    };
    let theme = Theme::default();
    let picker = SelectList::new(
        "Sessions",
        vec![
            SelectItem::new("other", bare_action("wrong-choice")),
            SelectItem::new("kitten", bare_action("chosen-session")),
        ],
        &theme,
    );
    let cases: [Case<'_>; 2] = [
        (
            Box::new(PromptOverlay::rename_window("", &theme)),
            b"build\r\n\t jobs\x1b\x08",
            "rename-window",
            Some("build jobs"),
        ),
        (Box::new(picker), b"k\tit\n", "chosen-session", None),
    ];
    for (modal, payload, expected_action, expected_name) in cases {
        let mut env = two_pane_env(&[], (0, 0));
        env.fx.overlays.push(modal);
        let mut framed = b"\x1b[200~".to_vec();
        framed.extend_from_slice(payload);
        framed.extend_from_slice(b"\x1b[201~");
        let events = crate::attach::input::StdinParser::new().feed(&framed);
        let sent = env.dispatch(events).await;
        assert!(
            sent.frames.is_empty(),
            "paste leaked to the pane: {:?}",
            sent.frames
        );
        assert!(
            env.fx.overlays.is_active(),
            "pasted controls must not submit or dismiss"
        );
        let InputEvent::Key(enter) = press(PhysicalKey::Enter, None) else {
            unreachable!()
        };
        let OverlayOutcome::RunAction(action) = env.fx.overlays.handle_key(&enter) else {
            panic!("pasted text must remain until a real Enter");
        };
        assert_eq!(action.action, expected_action);
        if let Some(name) = expected_name {
            assert_eq!(
                action.args.get("name").and_then(toml::Value::as_str),
                Some(name)
            );
        }
    }
}

// ---- sidebar hit targets -------------------------------------------------------

fn str_arg(r: &phux_config::keybind::ResolvedAction, key: &str) -> Option<String> {
    r.args.get(key)?.as_str().map(str::to_owned)
}

fn strip(h: u16) -> Rect {
    Rect {
        x: 0,
        y: 0,
        w: 28,
        h,
    }
}

/// Window blocks commit `select-window`, the footer `new-window`, the
/// collapse corner `toggle-sidebar`, and headers their management views;
/// blank rows and the separator commit nothing.
#[test]
fn sidebar_click_action_maps_rows_to_registry_actions() {
    // 22 body rows: Agents at 0, Sessions at 11, footer at 22.
    let quiet = targets(0, 2, 1);
    let hit = |x, y| sidebar_click_action(strip(23), &quiet, x, y);
    let window = hit(4, 15).expect("window row");
    assert_eq!(
        (window.action.as_str(), index_arg(&window)),
        ("select-window", Some(1))
    );
    for (x, y, action) in [
        (4, 22, "new-window"),
        (27, 22, "toggle-sidebar"),
        (4, 0, "agent-fleet"),
        (4, 11, "session-picker"),
    ] {
        let resolved = hit(x, y).expect(action);
        assert_eq!(resolved.action, action);
    }
    assert!(hit(4, 10).is_none() && hit(27, 0).is_none());
}

/// ADR-0148: a plugin section row focuses its pane; the section header is
/// inert.
#[test]
fn sidebar_plugin_rows_commit_focus_pane() {
    use crate::render::chrome::sidebar_sections::{PluginSection, PluginSectionRow, PluginShape};
    let section = PluginSection {
        title: "Builds".to_owned(),
        rows: 3,
        entries: vec![
            PluginSectionRow {
                text: "lint".to_owned(),
                window: 0,
                pane: 0,
            },
            PluginSectionRow {
                text: "test".to_owned(),
                window: 1,
                pane: 2,
            },
        ],
    };
    let mut t = targets(0, 1, 1);
    t.counts.plugin = PluginShape::of(std::slice::from_ref(&section));
    t.plugin = vec![vec![(0, 0), (1, 2)]];
    // 22 body rows: the 4-row band leaves 18, so Agents 0-8, band 9-12.
    let click = sidebar_click_action(strip(23), &t, 4, 11).expect("plugin row");
    assert_eq!(click.action, "focus-pane");
    assert_eq!(
        (usize_arg(&click, "window"), usize_arg(&click, "pane")),
        (Some(1), Some(2))
    );
    assert!(
        sidebar_click_action(strip(23), &t, 4, 9).is_none(),
        "header"
    );
    assert!(
        sidebar_click_action(strip(23), &t, 4, 12).is_none(),
        "blank"
    );
}

/// A queue row commits a LOCAL focus or a CROSS-SESSION re-attach (a
/// graph row by resource identity, never fabricated indices); a roster row
/// switches session (by host when it has one; unreachable is inert); an
/// overflow row opens the fleet dashboard.
#[test]
fn sidebar_queue_and_roster_rows_commit_their_own_actions() {
    let t = targets(2, 2, 0);
    let local = sidebar_click_action(strip(23), &t, 4, 1).expect("queue row 0");
    assert_eq!(
        (local.action.as_str(), index_arg(&local)),
        ("select-window", Some(1))
    );
    let peer = sidebar_click_action(strip(23), &t, 4, 2).expect("queue row 1");
    assert_eq!(peer.action, "switch-session");
    assert_eq!(str_arg(&peer, "name").as_deref(), Some("peer-1"));
    assert_eq!(
        (usize_arg(&peer, "window"), usize_arg(&peer, "pane")),
        (Some(2), Some(3))
    );
    assert!(!peer.args.contains_key("resource"));

    let mut graph = t;
    graph.needs_you[1] = SidebarTarget::Session {
        name: "peer".to_owned(),
        id: Some(phux_protocol::ids::SessionId::new(2)),
        window: None,
        pane: None,
        resource: Some(ResourceId::local(10)),
    };
    let click = sidebar_click_action(strip(23), &graph, 4, 2).expect("graph row");
    assert_eq!(str_arg(&click, "resource").as_deref(), Some("@10"));
    assert!(
        !click.args.contains_key("window") && !click.args.contains_key("pane"),
        "{:?}",
        click.args
    );

    let mut t = targets(0, 2, 2);
    let space = sidebar_click_action(strip(23), &t, 4, 12).expect("roster row");
    assert_eq!(space.action, "switch-session");
    assert_eq!(str_arg(&space, "name").as_deref(), Some("space-0"));
    assert!(!space.args.contains_key("pane"));
    t.roster[0].as_mut().expect("row").host = Some("devbox".to_owned());
    let host = sidebar_click_action(strip(23), &t, 4, 13).expect("host row");
    assert_eq!(str_arg(&host, "host").as_deref(), Some("devbox"));
    t.roster[0] = None;
    assert!(
        sidebar_click_action(strip(23), &t, 4, 13).is_none(),
        "unreachable host is inert"
    );

    let t = targets(12, 1, 1);
    assert!(
        (0..23)
            .filter_map(|y| sidebar_click_action(strip(23), &t, 4, y))
            .any(|r| r.action == "agent-fleet")
    );
}

/// Every action a sidebar click commits is a dispatched action name.
#[test]
fn sidebar_click_actions_are_dispatched_names() {
    let t = targets(9, 3, 2);
    let narrow = Rect {
        x: 0,
        y: 0,
        w: 20,
        h: 23,
    };
    for y in 0..23 {
        for x in [2u16, 19] {
            if let Some(resolved) = sidebar_click_action(narrow, &t, x, y) {
                assert!(
                    ACTION_NAMES.contains(&resolved.action.as_str()),
                    "{}",
                    resolved.action
                );
            }
        }
    }
}

/// A left-docked 20-column sidebar over an 80-column viewport and a
/// two-window workspace ("1", "two"); window rows sit at 14 and 15.
fn sidebar_env(
    height: u16,
    sidebar_targets: crate::render::chrome::sidebar::SidebarTargets,
) -> Env<'static> {
    let mut fx = CtxFixture::default();
    fx.workspace.add_window("two".to_owned(), tid(2));
    fx.workspace.select(0);
    fx.viewport = (80, height);
    fx.sidebar = Some(SidebarReservation {
        edge: SidebarEdge::Left,
        width: 20,
    });
    fx.sidebar_enabled = true;
    fx.sidebar_targets = Some(sidebar_targets);
    fx.bar = Some(Position::Bottom);
    Env::new(fx)
}

fn window_names(env: &Env<'_>) -> Vec<String> {
    env.fx
        .workspace
        .windows
        .iter()
        .map(|w| w.name.clone())
        .collect()
}

fn drop_at(env: &Env<'_>) -> Option<usize> {
    match &env.fx.drag {
        Some(DragGrab::Window(grab)) => grab.drop_at,
        _ => None,
    }
}

/// End-to-end sidebar clicks: a window block selects it (right press also
/// opens its menu), `+ new` parks a spawn, blank rows are consumed (right
/// press there opens the session menu).
#[tokio::test]
async fn sidebar_clicks_run_their_affordances() {
    for (event, active, overlay, pending) in [
        (left(MouseAction::Press, 3, 15), 1, false, 0),
        (left(MouseAction::Press, 3, 23), 0, false, 1),
        (left(MouseAction::Press, 3, 10), 0, false, 0),
        (right_press(3, 15), 1, true, 0),
        (right_press(3, 10), 0, true, 0),
    ] {
        let mut env = sidebar_env(24, targets(0, 2, 1));
        env.dispatch(vec![event.clone()]).await;
        assert_eq!(env.fx.workspace.active, active, "{event:?}");
        assert_eq!(env.fx.overlays.is_active(), overlay, "{event:?}");
        assert_eq!(env.fx.pending_windows.len(), pending, "{event:?}");
    }
}

#[tokio::test]
async fn short_sidebar_overflow_clicks_open_both_navigation_overlays() {
    let table = targets(3, 2, 2);
    for (y, action) in [(2, "agent-fleet"), (3, "session-picker")] {
        let short = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 7,
        };
        assert_eq!(
            sidebar_click_action(short, &table, 4, y)
                .expect(action)
                .action,
            action
        );
        let mut env = sidebar_env(7, table.clone());
        env.dispatch(vec![left(MouseAction::Press, 4, y)]).await;
        assert!(env.fx.overlays.is_active(), "{action} opens an overlay");
        assert!(env.fx.pending_windows.is_empty());
    }
}

/// Dragging the strip's rule (column 19) resizes it to follow the pointer,
/// clamped so the strip keeps a name and the panes keep `min_pane_cols`;
/// release ends the grab, and focus loss abandons it so the next press acts.
#[tokio::test]
async fn dragging_the_sidebar_edge_resizes_the_strip() {
    let grab = left(MouseAction::Press, 19, 5);
    let cases = [
        (
            vec![
                grab.clone(),
                left(MouseAction::Motion, 25, 5),
                left(MouseAction::Motion, 29, 6),
                left(MouseAction::Release, 29, 6),
            ],
            30,
            false,
            0,
        ),
        (
            vec![
                grab.clone(),
                left(MouseAction::Motion, 25, 5),
                InputEvent::Focus(FocusEvent::Lost),
                // The next press acts: it selects (and picks up) its row.
                left(MouseAction::Press, 3, 15),
            ],
            26,
            true,
            1,
        ),
        (
            vec![grab.clone(), left(MouseAction::Motion, 75, 5)],
            40,
            true,
            0,
        ),
        (
            vec![grab, left(MouseAction::Motion, 2, 5)],
            super::chrome_drag::MIN_DRAGGED_SIDEBAR_COLS,
            true,
            0,
        ),
    ];
    for (events, width, live, active) in cases {
        let mut env = sidebar_env(24, targets(0, 2, 1));
        env.dispatch(events).await;
        assert_eq!(env.fx.sidebar_width, width);
        assert_eq!(env.fx.drag.is_some(), live);
        assert_eq!(env.fx.workspace.active, active);
        assert!(env.fx.sidebar_enabled);
    }
    // The rule's bottom corner is the collapse chevron, not a handle.
    let mut env = sidebar_env(24, targets(0, 2, 1));
    env.dispatch(vec![left(MouseAction::Press, 19, 23)]).await;
    assert!(!env.fx.sidebar_enabled && env.fx.drag.is_none());
    assert_eq!(env.fx.sidebar_width, 20);
}

/// Width math for both docks, and a strip floor that yields to a config
/// already narrower than the default floor.
#[test]
fn dragged_sidebar_width_tracks_the_pointer_within_both_floors() {
    use super::chrome_drag::{WidthFloors, dragged_sidebar_width};
    let floors = WidthFloors {
        strip: 16,
        panes: 40,
    };
    for (cols, edge, x, width) in [
        (80, SidebarEdge::Left, 29, Some(30)),
        (80, SidebarEdge::Right, 50, Some(30)),
        (80, SidebarEdge::Left, 79, Some(40)),
        (50, SidebarEdge::Left, 20, None),
    ] {
        assert_eq!(dragged_sidebar_width(cols, edge, x, floors), width);
    }
    let narrow = WidthFloors {
        strip: 12,
        panes: 40,
    };
    assert_eq!(
        dragged_sidebar_width(80, SidebarEdge::Left, 11, narrow),
        Some(12)
    );
}

/// A window row pressed and released over another row moves into that slot,
/// still active; released anywhere else (or clicked) it stays put. Motion
/// over another row marks the drop slot; release or focus loss clears it.
#[tokio::test]
async fn dragging_a_sidebar_window_row_reorders_windows() {
    let grab = left(MouseAction::Press, 3, 14);
    let over = left(MouseAction::Motion, 3, 15);
    let cases = [
        (
            vec![
                grab.clone(),
                over.clone(),
                left(MouseAction::Release, 3, 15),
            ],
            ["two", "1"],
            None,
            false,
        ),
        (
            vec![grab.clone(), left(MouseAction::Release, 3, 14)],
            ["1", "two"],
            None,
            false,
        ),
        (
            vec![grab.clone(), left(MouseAction::Release, 3, 5)],
            ["1", "two"],
            None,
            false,
        ),
        (
            vec![grab.clone(), left(MouseAction::Release, 50, 15)],
            ["1", "two"],
            None,
            false,
        ),
        (
            vec![grab.clone(), over.clone()],
            ["1", "two"],
            Some(1),
            true,
        ),
        (
            vec![grab, over, InputEvent::Focus(FocusEvent::Lost)],
            ["1", "two"],
            None,
            false,
        ),
    ];
    for (events, order, marker, live) in cases {
        let mut env = sidebar_env(24, targets(0, 2, 1));
        env.dispatch(events).await;
        assert_eq!(window_names(&env), order);
        assert_eq!(drop_at(&env), marker);
        assert_eq!(env.fx.drag.is_some(), live);
        if order == ["two", "1"] {
            assert_eq!(
                env.fx.workspace.active, 1,
                "the dragged window stays active"
            );
        }
    }
}

// ---- status-bar window tabs ------------------------------------------------------

/// A painter with the `windows` widget, fed "0:bash 1:vim" (window 0 on
/// columns 0..=5, the separator on 6, window 1 on 7..=11) and painted once
/// so its hit-test cache is populated.
fn painted_windows_bar(position: Position) -> StatusBarPainter {
    use phux_config::widget::{StatusBar, WidgetRegistry, WindowInfo};
    let cfg = phux_config::StatusCfg {
        left: vec![phux_config::Widget::Bare("windows".into())],
        ..Default::default()
    };
    let bar = StatusBar::build(&cfg, &WidgetRegistry::with_builtins()).expect("bar builds");
    let mut painter = StatusBarPainter::new(bar, position);
    let tab = |name: &str, active| WindowInfo {
        name: name.to_owned(),
        active,
        zoomed: false,
        attention: false,
        branch: None,
        exited: None,
        badge: None,
    };
    painter.set_windows(vec![tab("bash", true), tab("vim", false)]);
    paint(&mut painter, 80);
    painter
}

fn paint(painter: &mut StatusBarPainter, cols: u16) {
    painter
        .paint(
            &mut Vec::new(),
            BarInset::NONE,
            cols,
            24,
            &make_context("", std::time::SystemTime::UNIX_EPOCH),
        )
        .expect("paint");
}

/// A two-window workspace with a painted bar at `position` (or a reserved
/// row with no painter).
fn bar_env(position: Position, with_painter: bool) -> Env<'static> {
    let mut fx = CtxFixture::default();
    fx.workspace.add_window("two".to_owned(), tid(2));
    fx.workspace.select(0);
    fx.bar = Some(position);
    fx.status_bar = with_painter.then(|| painted_windows_bar(position));
    Env::new(fx)
}

/// Bar-row clicks: a left press on window 1's tab selects it (bottom or top
/// dock); a right press also opens its menu, and off the tabs opens the
/// session menu; a non-tab cell or a missing painter is consumed. Nothing
/// reaches a pane.
#[tokio::test]
async fn bar_clicks_select_tabs_and_never_reach_a_pane() {
    let cases = [
        (
            Position::Bottom,
            true,
            left(MouseAction::Press, 8, 23),
            1,
            false,
        ),
        (
            Position::Top,
            true,
            left(MouseAction::Press, 8, 0),
            1,
            false,
        ),
        (
            Position::Bottom,
            true,
            left(MouseAction::Press, 6, 23),
            0,
            false,
        ),
        (
            Position::Bottom,
            true,
            left(MouseAction::Press, 40, 23),
            0,
            false,
        ),
        (Position::Bottom, true, right_press(8, 23), 1, true),
        (Position::Bottom, true, right_press(40, 23), 0, true),
        (
            Position::Bottom,
            false,
            left(MouseAction::Press, 8, 23),
            0,
            false,
        ),
    ];
    for (position, painter, event, active, overlay) in cases {
        let mut env = bar_env(position, painter);
        let sent = env.dispatch(vec![event.clone()]).await;
        assert_eq!(env.fx.workspace.active, active, "{event:?}");
        assert_eq!(env.fx.overlays.is_active(), overlay, "{event:?}");
        assert!(sent.frames.is_empty(), "{event:?}: {:?}", sent.frames);
    }
}

/// With a TOP bar, the bottom row is pane content: the click forwards.
#[tokio::test]
async fn bar_claim_leaves_pane_content_alone() {
    let mut env = bar_env(Position::Top, true);
    let sent = env.dispatch(vec![left(MouseAction::Press, 8, 23)]).await;
    assert_eq!(env.fx.workspace.active, 0);
    assert!(
        matches!(sent.frames.as_slice(), [FrameKind::InputMouse { terminal_id, .. }] if *terminal_id == tid(1)),
        "{:?}",
        sent.frames
    );
}

/// A tab dragged onto another tab moves into its slot (still active); off
/// the bar or back on itself it stays. Motion marks the slot; release or
/// focus loss clears it. Nothing reaches a pane.
#[tokio::test]
async fn dragging_a_tab_reorders_windows() {
    let grab = left(MouseAction::Press, 2, 23);
    let over = left(MouseAction::Motion, 9, 23);
    let cases = [
        (
            vec![
                grab.clone(),
                left(MouseAction::Motion, 5, 23),
                over.clone(),
                left(MouseAction::Release, 9, 23),
            ],
            ["two", "1"],
            None,
        ),
        (
            vec![grab.clone(), left(MouseAction::Release, 9, 10)],
            ["1", "two"],
            None,
        ),
        (
            vec![grab.clone(), left(MouseAction::Release, 3, 23)],
            ["1", "two"],
            None,
        ),
        (
            vec![grab.clone(), left(MouseAction::Release, 40, 23)],
            ["1", "two"],
            None,
        ),
        (vec![grab.clone(), over.clone()], ["1", "two"], Some(1)),
        (
            vec![grab, over, InputEvent::Focus(FocusEvent::Lost)],
            ["1", "two"],
            None,
        ),
    ];
    for (events, order, marker) in cases {
        let mut env = bar_env(Position::Bottom, true);
        let sent = env.dispatch(events).await;
        assert_eq!(window_names(&env), order);
        assert_eq!(drop_at(&env), marker);
        assert!(
            !sent
                .frames
                .iter()
                .any(|f| matches!(f, FrameKind::InputMouse { .. })),
            "{:?}",
            sent.frames
        );
        if order == ["two", "1"] {
            assert_eq!(env.fx.workspace.active, 1);
        }
    }
}

/// A tab column commits a dispatched `select-window`; non-tab columns and a
/// missing painter commit nothing; every painted navigation hint dispatches.
#[test]
fn bar_click_action_maps_tabs_and_hints_to_dispatched_actions() {
    use phux_config::widget::{CellHit, StatusBar, WidgetRegistry};
    let painter = painted_windows_bar(Position::Bottom);
    let resolved = bar_click_action(Some(&painter), 8).expect("tab column hits");
    assert_eq!(
        (resolved.action.as_str(), index_arg(&resolved)),
        ("select-window", Some(1))
    );
    assert!(ACTION_NAMES.contains(&resolved.action.as_str()));
    assert!(
        bar_click_action(Some(&painter), 6).is_none()
            && bar_click_action(Some(&painter), 40).is_none()
    );
    assert!(bar_click_action(None, 8).is_none());

    let cfg = phux_config::StatusCfg {
        center: vec![phux_config::Widget::Bare("help-hints".into())],
        ..Default::default()
    };
    let bar = StatusBar::build(&cfg, &WidgetRegistry::with_builtins()).expect("bar builds");
    let mut painter = StatusBarPainter::new(bar, Position::Bottom);
    paint(&mut painter, 100);
    let mut actions = std::collections::BTreeSet::new();
    for x in 0..100 {
        if matches!(painter.hit_at(x), Some(CellHit::Action(_))) {
            let resolved = bar_click_action(Some(&painter), x).expect("painted action dispatches");
            assert!(resolved.args.is_empty());
            actions.insert(resolved.action);
        }
    }
    let expected = [
        "command-palette",
        "copy-mode",
        "session-picker",
        "settings",
        "show-help",
    ];
    assert_eq!(actions, expected.into_iter().map(str::to_owned).collect());
}

// ---- pane mouse routing ------------------------------------------------------------

/// [`two_pane_workspace`] at 80x24 (no bar or sidebar), with a published
/// 39x24 replica per `(id, vt)` seed and `cell_px` cells.
fn two_pane_env(seed_vt: &[(ResourceId, &[u8])], cell_px: (u16, u16)) -> Env<'static> {
    let mut fx = CtxFixture::default();
    fx.workspace = two_pane_workspace();
    fx.cell_px = cell_px;
    let entries: Vec<_> = seed_vt.iter().map(|(id, vt)| (id, 39, 24, *vt)).collect();
    Env::new(fx).published(&entries)
}

/// The divider column, found by hit-testing rather than hardcoding rounding.
fn divider_x() -> u16 {
    use crate::multi_pane::{RouteDecision, route_mouse_event};
    let workspace = two_pane_workspace();
    let ls = workspace.active_window().expect("window");
    let content = content_rect((80, 24), None, None);
    (0..80u16)
        .find(|&x| {
            let probe = mev(MouseAction::Press, MouseButton::Left, f64::from(x), 5.0);
            matches!(
                route_mouse_event(ls, content, (80, 24), &probe),
                RouteDecision::Divider { .. }
            )
        })
        .expect("a two-pane split has a divider column")
}

/// A mouse event over pane 2 (column 70, row 5).
fn over_pane_2(action: MouseAction, button: MouseButton) -> InputEvent {
    mouse(action, button, 70.0, 5.0)
}

const TRACKING: &[u8] = b"\x1b[?1049h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h";

/// ADR-0124: a pane retained after its process exited takes no input; its
/// live neighbour still does.
#[tokio::test]
async fn input_to_a_retained_pane_is_dropped_by_the_dispatcher() {
    for (exited, events, forwarded) in [
        (
            tid(1),
            vec![
                press(PhysicalKey::A, Some("a")),
                press(PhysicalKey::Enter, None),
            ],
            false,
        ),
        (tid(2), vec![press(PhysicalKey::A, Some("a"))], true),
    ] {
        let mut env = two_pane_env(&[(tid(1), b"final screen"), (tid(2), b"")], (8, 16));
        env.panes.get_mut(&exited).expect("slot").exited =
            Some(super::super::pane_state::ExitMark {
                status: Some(0),
                signal: None,
            });
        let sent = env.dispatch(events).await;
        if forwarded {
            assert!(
                matches!(sent.frames.as_slice(), [FrameKind::InputKey { terminal_id, .. }] if *terminal_id == tid(1)),
                "{:?}",
                sent.frames
            );
        } else {
            assert!(sent.frames.is_empty(), "{:?}", sent.frames);
        }
    }
}

/// A second press during a divider drag is consumed (no focus move, no
/// forward), and the release that follows still ends the drag.
#[tokio::test]
async fn a_press_during_a_divider_drag_is_consumed() {
    let grab = mouse(
        MouseAction::Press,
        MouseButton::Left,
        f64::from(divider_x()),
        5.0,
    );
    let mut env = two_pane_env(&[], (1, 1));
    let sent = env
        .dispatch(vec![
            grab.clone(),
            over_pane_2(MouseAction::Press, MouseButton::Left),
        ])
        .await;
    assert!(env.fx.drag.is_some());
    assert_eq!(env.focused, Some(tid(1)));
    assert!(sent.frames.is_empty(), "{:?}", sent.frames);

    let mut env = two_pane_env(&[], (1, 1));
    env.dispatch(vec![
        grab,
        over_pane_2(MouseAction::Press, MouseButton::Right),
        over_pane_2(MouseAction::Release, MouseButton::Left),
    ])
    .await;
    assert!(env.fx.drag.is_none());
}

/// `set-pane mouse off` is per pane: a press in an opted-out pane focuses it
/// but forwards nothing (nor opens a menu); an opted-in sibling still gets
/// its `INPUT_MOUSE` in pane-local coordinates.
#[tokio::test]
async fn mouse_opt_out_is_per_pane() {
    let mut env = two_pane_env(&[(tid(2), b"")], (1, 1));
    env.fx.mouse_optout = HashSet::from([tid(2)]);
    let sent = env
        .dispatch(vec![over_pane_2(MouseAction::Press, MouseButton::Left)])
        .await;
    assert_eq!(env.focused, Some(tid(2)));
    assert!(sent.frames.is_empty(), "{:?}", sent.frames);
    let sent = env
        .dispatch(vec![over_pane_2(MouseAction::Press, MouseButton::Right)])
        .await;
    assert!(sent.frames.is_empty() && !env.fx.overlays.is_active());

    let mut env = two_pane_env(&[], (1, 1));
    env.fx.mouse_optout = HashSet::from([tid(1)]);
    let sent = env
        .dispatch(vec![over_pane_2(MouseAction::Press, MouseButton::Left)])
        .await;
    assert_eq!(env.focused, Some(tid(2)));
    match sent.frames.as_slice() {
        [FrameKind::InputMouse { terminal_id, event }] => {
            assert_eq!(*terminal_id, tid(2));
            assert!(
                event.x < f64::from(divider_x()),
                "pane-local x: {}",
                event.x
            );
        }
        other => panic!("expected one INPUT_MOUSE, got {other:?}"),
    }
}

fn primary_scrollback_vt(lines: usize) -> Vec<u8> {
    (0..lines)
        .flat_map(|i| format!("line-{i:03}\r\n").into_bytes())
        .collect()
}

/// Wheel routing by the pane's screen and mode state: the alt screen (even
/// with prior primary history) synthesizes 3 arrows per notch unless the app
/// turned alternate scroll off or tracks the mouse (then it forwards); the
/// primary screen scrolls locally, forwarding when there is nothing to
/// scroll (no history, or already at the live tail).
#[tokio::test]
async fn wheel_routing_follows_the_panes_screen_and_modes() {
    enum Want {
        Arrows(PhysicalKey),
        Forward(MouseButton),
        Nothing,
    }
    let history = primary_scrollback_vt(40);
    let mut alt_with_history = history.clone();
    alt_with_history.extend_from_slice(b"\x1b[?1049h");
    let (up, down) = (MouseButton::Four, MouseButton::Five);
    let cases: [(&[u8], MouseButton, Want); 7] = [
        (b"\x1b[?1049h", up, Want::Arrows(PhysicalKey::ArrowUp)),
        (b"\x1b[?1049h", down, Want::Arrows(PhysicalKey::ArrowDown)),
        (&alt_with_history, up, Want::Arrows(PhysicalKey::ArrowUp)),
        (b"\x1b[?1049h\x1b[?1007l", up, Want::Forward(up)),
        (TRACKING, up, Want::Forward(up)),
        (&history, up, Want::Nothing),
        (&history, down, Want::Forward(down)),
    ];
    for (vt, button, want) in cases {
        let mut env = two_pane_env(&[(tid(2), vt)], (1, 1));
        let sent = env
            .dispatch(vec![over_pane_2(MouseAction::Press, button)])
            .await;
        match want {
            Want::Arrows(key) => {
                assert_eq!(sent.frames.len(), 3, "{:?}", sent.frames);
                for frame in &sent.frames {
                    assert!(
                        matches!(frame, FrameKind::InputKey { terminal_id, event } if *terminal_id == tid(2) && event.key == key),
                        "{frame:?}"
                    );
                }
            }
            Want::Forward(b) => assert!(
                matches!(sent.frames.as_slice(), [FrameKind::InputMouse { terminal_id, event }] if *terminal_id == tid(2) && event.button == b),
                "{:?}",
                sent.frames
            ),
            Want::Nothing => assert!(sent.frames.is_empty(), "{:?}", sent.frames),
        }
    }
    // An empty history forwards too rather than eating a no-op scroll.
    let mut env = two_pane_env(&[(tid(2), b"")], (1, 1));
    let sent = env
        .dispatch(vec![over_pane_2(MouseAction::Press, up)])
        .await;
    assert!(
        matches!(sent.frames.as_slice(), [FrameKind::InputMouse { .. }]),
        "{:?}",
        sent.frames
    );
}

/// Ctrl-left drag over a mouse-tracking TUI is the host-selection escape
/// hatch: it enters copy mode instead of reaching the app.
#[tokio::test]
async fn ctrl_left_press_in_mouse_tracking_pane_starts_host_copy() {
    let mut event = mev(MouseAction::Press, MouseButton::Left, 70.0, 5.0);
    event.mods = ModSet::CTRL;
    let mut env = two_pane_env(&[(tid(2), TRACKING)], (1, 1));
    let sent = env.dispatch(vec![InputEvent::Mouse(event)]).await;
    assert_eq!(env.focused, Some(tid(2)));
    assert!(sent.frames.is_empty() && env.fx.overlays.is_active());
}

/// ADR-0058: a right press over a pane that has not asked for the mouse
/// focuses it and opens the hover-tracking pane menu; a mouse-tracking app
/// owns the button, so it forwards instead. Clicking away closes the menu
/// and schedules the repaint that erases it.
#[tokio::test]
async fn right_press_opens_the_pane_menu_unless_the_app_owns_the_mouse() {
    let mut env = two_pane_env(&[(tid(2), b"")], (1, 1));
    let sent = env
        .dispatch(vec![over_pane_2(MouseAction::Press, MouseButton::Right)])
        .await;
    assert_eq!(env.focused, Some(tid(2)));
    assert!(env.fx.overlays.is_active() && env.fx.overlays.wants_pointer_hover());
    assert!(sent.frames.is_empty(), "{:?}", sent.frames);
    let sent = env
        .dispatch(vec![mouse(MouseAction::Press, MouseButton::Left, 2.0, 2.0)])
        .await;
    assert!(!env.fx.overlays.is_active() && sent.repainted);

    let mut env = two_pane_env(&[(tid(2), b"\x1b[?1000h\x1b[?1006h")], (1, 1));
    let sent = env
        .dispatch(vec![over_pane_2(MouseAction::Press, MouseButton::Right)])
        .await;
    assert!(!env.fx.overlays.is_active());
    assert!(
        matches!(sent.frames.as_slice(), [FrameKind::InputMouse { terminal_id, event }] if *terminal_id == tid(2) && event.button == MouseButton::Right),
        "{:?}",
        sent.frames
    );
}

/// SPEC input.md §3.1: forwarded positions are surface pixels, scaled from
/// pane-local cells at the send boundary.
#[tokio::test]
async fn forwarded_input_mouse_scales_cells_to_surface_pixels() {
    let mut env = two_pane_env(&[(tid(2), b"\x1b[?1000h\x1b[?1006h")], (8, 16));
    let sent = env
        .dispatch(vec![over_pane_2(MouseAction::Press, MouseButton::Left)])
        .await;
    // Pane 2 starts one column right of the divider and one row under the rail.
    let x = (70.0 - f64::from(divider_x()) - 1.0) * 8.0;
    let y = (5.0 - 1.0) * 16.0;
    match sent.frames.as_slice() {
        [FrameKind::InputMouse { event, .. }] => {
            assert!(
                (event.x - x).abs() < f64::EPSILON && (event.y - y).abs() < f64::EPSILON,
                "{event:?}"
            );
        }
        other => panic!("expected one INPUT_MOUSE, got {other:?}"),
    }
}

// ---- acknowledged input --------------------------------------------------------------

fn journal() -> std::cell::RefCell<InputReplayJournal> {
    let journal = std::cell::RefCell::new(InputReplayJournal::new());
    assert!(
        journal
            .borrow_mut()
            .begin_connection(Some(&[0xAB; 16]), true)
            .is_empty()
    );
    journal
}

fn apply_input_events(frame: &FrameKind) -> (u32, &[InputEvent]) {
    match frame {
        FrameKind::Command {
            request_id,
            command: Command::ApplyInput { events, .. },
        } => (*request_id, events),
        other => panic!("expected APPLY_INPUT, got {other:?}"),
    }
}

/// Pastes and the Enter after them go out one `APPLY_INPUT` at a time in
/// terminal order; an oversized paste is refused visibly.
#[tokio::test]
async fn dispatch_queues_paste_then_paste_then_enter_in_terminal_order() {
    use phux_protocol::input::paste::{PasteEvent, PasteTrust};
    let journal = journal();
    let paste = |data: &[u8]| {
        InputEvent::Paste(PasteEvent {
            trust: PasteTrust::Trusted,
            data: data.to_vec(),
        })
    };
    let mut env = two_pane_env(&[], (8, 16));
    env.journal = Some(&journal);
    let sent = env
        .dispatch(vec![
            paste(b"A"),
            paste(b"B"),
            paste(&vec![b'x'; 70 * 1024]),
            press(PhysicalKey::Enter, None),
        ])
        .await;
    assert_eq!(sent.frames.len(), 1, "only A is sent before its reply");
    let overflow = journal.borrow_mut().take_reports();
    assert_eq!(overflow.len(), 1);
    assert_eq!(overflow[0].disposition, ReplayDisposition::Refused);
    let (mut request, events) = apply_input_events(&sent.frames[0]);
    assert!(matches!(events[0], InputEvent::Paste(_)));
    for (next_id, is_paste) in [(50, true), (60, false)] {
        journal
            .borrow_mut()
            .resolve(request, &phux_protocol::wire::frame::CommandResult::Ok)
            .expect("resolve");
        let (_, frames) = journal.borrow_mut().next_frames(&mut { next_id });
        assert_eq!(frames.len(), 1);
        let (id, events) = apply_input_events(&frames[0]);
        assert_eq!(matches!(events[0], InputEvent::Paste(_)), is_paste);
        request = id;
    }
}

/// A failed send leaves the frame it failed on uncertain and rolls the
/// never-handed-over rest back to refused.
#[tokio::test]
async fn replay_send_failure_rolls_back_frames_not_handed_to_the_connection() {
    use crate::attach::input_replay::mint_input_operation_id;
    use phux_protocol::input::paste::{PasteEvent, PasteTrust};
    let journal = journal();
    for terminal in 1..=3 {
        let data = terminal.to_string().into_bytes();
        journal
            .borrow_mut()
            .submit(
                mint_input_operation_id(),
                tid(terminal),
                vec![InputEvent::Paste(PasteEvent {
                    trust: PasteTrust::Trusted,
                    data,
                })],
            )
            .expect("queue independent terminal");
    }
    let (_, frames) = journal.borrow_mut().next_frames(&mut 1);
    assert_eq!(frames.len(), 3);
    let (stream, peer) = tokio::net::UnixStream::pair().expect("uds pair");
    drop(peer);
    let mut conn = crate::attach::connection::Connection::from_stream(stream);
    send_replay_frames(&mut conn, &journal, &frames)
        .await
        .expect_err("A send fails");
    let dispositions: Vec<_> = journal
        .borrow_mut()
        .drain_unresolved("the connection send failed")
        .iter()
        .map(|report| report.disposition)
        .collect();
    assert_eq!(
        dispositions,
        [
            ReplayDisposition::Unknown,
            ReplayDisposition::Refused,
            ReplayDisposition::Refused
        ]
    );
}

// ---- set-pane ---------------------------------------------------------------------

/// `set-pane mouse` accepts on/off/toggle and booleans; a missing or unknown
/// value, or no focused pane, bells.
#[test]
fn set_pane_mouse_updates_the_opt_out() {
    let mut f = fx(crate::layout::Workspace::single(tid(1)));
    let set = |value: toml::Value| act("set-pane", &[("mouse", value)]);
    for (value, opted_out) in [
        ("off".into(), true),
        ("on".into(), false),
        ("toggle".into(), true),
        ("toggle".into(), false),
        (false.into(), true),
        (true.into(), false),
    ] {
        let effects = f.run(&set(value));
        assert!(!effects.bell);
        assert_eq!(f.mouse_optout.contains(&tid(1)), opted_out);
    }
    assert!(f.run(&bare_action("set-pane")).bell);
    assert!(f.run(&set("sideways".into())).bell);
    assert!(f.mouse_optout.is_empty());

    let mut f = fx(crate::layout::Workspace::default());
    assert!(f.run(&set("off".into())).bell);
    assert!(f.mouse_optout.is_empty());
}

// ---- predictive echo alt-screen gate --------------------------------------------------

/// The gate reads DEC private modes: main screen predicts; `?1049h` and the
/// legacy `?1047h` are app mode; `?1049l` returns to the main screen.
#[test]
fn alt_screen_gate_tracks_dec_private_modes() {
    let terminal = |vt: &[u8]| {
        let mut terminal = libghostty_vt::Terminal::new(80, 24).expect("terminal");
        terminal
            .set_scrollback_max_lines(Some(100))
            .expect("terminal");
        terminal.vt_write(vt);
        terminal
    };
    for (vt, alt) in [
        (&b""[..], false),
        (b"\x1b[?1049h", true),
        (b"\x1b[?1049h\x1b[?1049l", false),
        (b"\x1b[?1047h", true),
    ] {
        assert_eq!(terminal_in_alt_screen(&terminal(vt)), alt, "{vt:?}");
    }
}

/// Through the real dispatch path a printable key queues one prediction in
/// both screens; on the primary screen it displays at once, in an alt-screen
/// app it stays hidden until an echo confirms (ADR-0090).
#[tokio::test]
async fn dispatch_predicts_but_gates_display_in_alt_screen_apps() {
    for alt_screen in [false, true] {
        let vt: &[u8] = if alt_screen { b"\x1b[?1049h" } else { b"" };
        let mut env = Env::new(CtxFixture::default()).published(&[(&tid(1), 80, 24, vt)]);
        env.predict = PredictionState::new(PredictiveConfig::enabled(), 80, 24);
        env.dispatch(vec![press(PhysicalKey::A, Some("a"))]).await;
        assert_eq!(env.predict.pending_len(), 1);
        assert_eq!(env.predict.should_display(predict_now_ms()), !alt_screen);
        if alt_screen {
            assert!(!env.predict.echo_confirmed());
        }
    }
}

// ---- ADR-0147: the floating plugin overlay -----------------------------------

/// An env on pane 1 with a floating overlay `@9` open over it.
fn env_with_floating() -> Env<'static> {
    let mut env = Env::new(CtxFixture::default())
        .with_default_bindings()
        .published(&[(&tid(1), 80, 24, b""), (&tid(9), 60, 18, b"")]);
    env.panes.get_mut(&tid(9)).expect("overlay slot").floating = Some("Board".to_owned());
    env
}

fn ctrl_a() -> InputEvent {
    InputEvent::Key(KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::A,
        mods: ModSet::CTRL,
        consumed_mods: ModSet::CTRL,
        composing: false,
        text: None,
        unshifted_codepoint: Some(u32::from(b'a')),
    })
}

fn kills(frames: &[FrameKind]) -> Vec<ResourceId> {
    frames
        .iter()
        .filter_map(|frame| match frame {
            FrameKind::Command {
                command: Command::KillResource { terminal_id, .. },
                ..
            } => Some(terminal_id.clone()),
            _ => None,
        })
        .collect()
}

/// Keys and pastes reach the overlay, never the layout pane beneath it.
#[tokio::test]
async fn the_floating_overlay_takes_keyboard_input() {
    let mut env = env_with_floating();
    let sent = env.dispatch(vec![press(PhysicalKey::Q, Some("q"))]).await;
    assert!(
        matches!(sent.frames.as_slice(), [FrameKind::InputKey { terminal_id, .. }] if *terminal_id == tid(9)),
        "{:?}",
        sent.frames
    );
}

/// `kill-pane` dismisses the overlay and nothing else: its Terminal is
/// killed (silently), its slot goes at once, and the layout pane survives.
#[tokio::test]
async fn kill_pane_dismisses_only_the_floating_overlay() {
    let mut env = env_with_floating();
    let sent = env
        .dispatch(vec![ctrl_a(), press(PhysicalKey::X, Some("x"))])
        .await;
    assert_eq!(kills(&sent.frames), vec![tid(9)]);
    assert!(sent.repainted);
    assert!(!env.panes.contains_key(&tid(9)));
    assert!(env.fx.expected_closes.contains(&tid(9)));
    assert_eq!(env.fx.workspace, crate::layout::Workspace::single(tid(1)));
    // Input is back on the layout pane.
    let sent = env.dispatch(vec![press(PhysicalKey::Q, Some("q"))]).await;
    assert!(
        matches!(sent.frames.as_slice(), [FrameKind::InputKey { terminal_id, .. }] if *terminal_id == tid(1)),
        "{:?}",
        sent.frames
    );
}

/// Any other action dismisses the overlay first, then runs.
#[tokio::test]
async fn another_action_dismisses_the_overlay_then_runs() {
    let mut env = env_with_floating();
    let sent = env
        .dispatch(vec![ctrl_a(), press(PhysicalKey::C, Some("c"))])
        .await;
    assert_eq!(kills(&sent.frames), vec![tid(9)]);
    assert!(
        sent.frames
            .iter()
            .any(|frame| matches!(frame, FrameKind::SpawnResource { .. })),
        "new-window still spawns: {:?}",
        sent.frames
    );
}

/// Copy mode does not mutate the layout, but dismissing the floating box
/// must still ask the driver for a full repaint.
#[tokio::test]
async fn a_non_layout_action_repaints_when_it_dismisses_the_floating_overlay() {
    let mut env = env_with_floating();
    let sent = env
        .dispatch(vec![ctrl_a(), press(PhysicalKey::BracketLeft, Some("["))])
        .await;
    assert_eq!(kills(&sent.frames), vec![tid(9)]);
    assert!(!env.panes.contains_key(&tid(9)));
    assert!(env.fx.expected_closes.contains(&tid(9)));
    assert_eq!(env.fx.workspace, crate::layout::Workspace::single(tid(1)));
    assert!(
        env.fx.overlays.copy_selection().is_some(),
        "copy mode still opens"
    );
    assert!(
        sent.repainted,
        "dismissing the floating box needs a full repaint"
    );
}

/// Waiting for the initial layout read blocks the action, not the repaint
/// needed by the floating overlay it already dismissed.
#[tokio::test]
async fn a_blocked_layout_action_repaints_when_it_dismisses_the_floating_overlay() {
    let mut env = env_with_floating();
    env.fx.layout_read_complete = false;
    let sent = env
        .dispatch(vec![ctrl_a(), press(PhysicalKey::C, Some("c"))])
        .await;
    assert_eq!(kills(&sent.frames), vec![tid(9)]);
    assert_eq!(sent.frames.len(), 1, "new-window must stay blocked");
    assert!(!env.panes.contains_key(&tid(9)));
    assert_eq!(env.fx.workspace, crate::layout::Workspace::single(tid(1)));
    assert!(
        sent.repainted,
        "even a blocked action dismissed the floating box"
    );
}

/// The pointer inside the box reaches the overlay pane-local; a press
/// outside the box dismisses it.
#[tokio::test]
async fn the_pointer_inside_reaches_the_overlay_and_a_press_outside_dismisses() {
    let inner = crate::attach::floating::floating_box(content_rect((80, 24), None, None)).inner;
    let mut env = env_with_floating();
    let inside = InputEvent::Mouse(mev(
        MouseAction::Press,
        MouseButton::Left,
        f64::from(inner.x + 2),
        f64::from(inner.y + 1),
    ));
    let sent = env.dispatch(vec![inside]).await;
    match sent.frames.as_slice() {
        [FrameKind::InputMouse { terminal_id, event }] => {
            assert_eq!(*terminal_id, tid(9));
            assert!((event.x - 2.0).abs() < f64::EPSILON && (event.y - 1.0).abs() < f64::EPSILON);
        }
        other => panic!("expected one overlay mouse frame, got {other:?}"),
    }
    let outside = InputEvent::Mouse(mev(MouseAction::Press, MouseButton::Left, 0.0, 23.0));
    let sent = env.dispatch(vec![outside]).await;
    assert_eq!(kills(&sent.frames), vec![tid(9)]);
    assert!(sent.repainted && !env.panes.contains_key(&tid(9)));
}
