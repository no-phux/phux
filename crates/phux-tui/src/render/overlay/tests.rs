use super::*;
use phux_protocol::input::key::{KeyAction, ModSet, PhysicalKey};

fn key(k: PhysicalKey) -> KeyEvent {
    KeyEvent {
        action: KeyAction::Press,
        key: k,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    }
}

fn action(name: &str) -> phux_config::keybind::ResolvedAction {
    phux_config::keybind::ResolvedAction {
        action: name.to_owned(),
        args: std::collections::BTreeMap::new(),
    }
}

/// A test overlay: optionally Esc-dismissable, optionally bounded, and
/// painting `text` at `rect`'s origin plus an `OUTSIDE` sentinel at the
/// viewport origin when bounded.
#[derive(Default)]
struct Probe {
    esc_dismisses: bool,
    rect: Option<Rect>,
    text: &'static str,
}

impl Probe {
    fn esc() -> Self {
        Self {
            esc_dismisses: true,
            ..Self::default()
        }
    }

    fn bounded(rect: Rect) -> Self {
        Self {
            rect: Some(rect),
            text: "INSIDE",
            ..Self::default()
        }
    }
}

impl RenderOverlay for Probe {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        let style = ratatui::style::Style::default();
        match self.rect {
            Some(rect) => {
                buf.set_string(0, 0, "OUTSIDE", style);
                buf.set_string(rect.x, rect.y, self.text, style);
            }
            None => {
                buf.set_string(area.x, area.y, self.text, style);
            }
        }
    }
    fn handle_key(&mut self, key: &KeyEvent) -> OverlayCommand {
        if self.esc_dismisses && key.key == PhysicalKey::Escape {
            OverlayCommand::Dismiss
        } else {
            OverlayCommand::Stay
        }
    }
    fn bounds(&self, _area: Rect) -> Option<Rect> {
        self.rect
    }
}

fn live_fleet_list() -> SelectList {
    SelectList::new(
        "agent fleet",
        vec![SelectItem::new("stale-row", action("focus-pane"))],
        &Theme::default(),
    )
    .with_live_key("agent-fleet")
}

#[test]
fn keys_reach_only_the_top_and_dismiss_pops_one() {
    let mut s = OverlayState::new();
    assert!(!s.is_active());
    s.push(Box::new(Probe::esc()));
    s.handle_key(&key(PhysicalKey::A));
    assert!(s.is_active(), "non-Esc stays");
    // A stay-forever top shields the Esc-dismiss overlay beneath it.
    s.push(Box::new(Probe::default()));
    s.handle_key(&key(PhysicalKey::Escape));
    assert_eq!(s.depth(), 2);
    s.dismiss();
    assert_eq!(s.depth(), 1, "dismiss pops one");
    s.handle_key(&key(PhysicalKey::Escape));
    assert!(!s.is_active());
}

#[test]
fn session_management_drill_down_keeps_picker_until_explicitly_closed() {
    let mut target = action("switch-session");
    target
        .args
        .insert("id".to_owned(), toml::Value::Integer(42));
    target
        .args
        .insert("name".to_owned(), toml::Value::String("build".to_owned()));
    let list = SelectList::new(
        "Sessions",
        vec![SelectItem::new("build", target.clone())],
        &Theme::default(),
    )
    .with_session_management();
    let mut stack = OverlayState::new();
    stack.push(Box::new(list));
    let OverlayOutcome::RunAction(manage) = stack.handle_key(&key(PhysicalKey::Tab)) else {
        panic!("management")
    };
    assert_eq!(manage.action, "session-actions");
    let mut expected_args = target.args.clone();
    expected_args.insert(
        "switch-action".to_owned(),
        toml::Value::String("switch-session".to_owned()),
    );
    assert_eq!(manage.args, expected_args);
    assert_eq!(
        stack.depth(),
        1,
        "Tab leaves the inventory below the drill-down"
    );
    stack.push(Box::new(Probe::esc()));
    stack.handle_key(&key(PhysicalKey::Escape));
    assert_eq!(stack.depth(), 1);
    assert_eq!(
        stack.handle_key(&key(PhysicalKey::Enter)),
        OverlayOutcome::RunAction(target)
    );
    assert_eq!(stack.depth(), 0, "Enter retains normal switching semantics");
}

/// The refresh walks the whole stack, so a modal on top cannot shield a
/// live list beneath it; without one it reports no change.
#[test]
fn refresh_items_reaches_a_live_overlay_anywhere_in_the_stack() {
    let fresh = vec![SelectItem::new("fresh-row", action("focus-pane"))];
    let mut s = OverlayState::new();
    assert!(!s.refresh_items("agent-fleet", &fresh));
    s.push(Box::new(live_fleet_list()));
    s.push(Box::new(Probe::esc()));
    assert!(s.refresh_items("agent-fleet", &fresh));
    let mut s = OverlayState::new();
    s.push(Box::new(Probe::esc()));
    assert!(!s.refresh_items("agent-fleet", &fresh));
}

#[test]
fn paint_composes_the_stack_bottom_up() {
    let mut s = OverlayState::new();
    let mut buf = Vec::new();
    s.paint(&mut buf, (80, 24)).expect("paint");
    assert!(buf.is_empty(), "inactive paints nothing");
    s.push(Box::new(Probe {
        text: "bottom",
        ..Probe::default()
    }));
    s.push(Box::new(Probe {
        rect: None,
        text: "to",
        ..Probe::default()
    }));
    let mut out = Vec::new();
    s.paint(&mut out, (20, 5)).expect("paint");
    let txt = String::from_utf8_lossy(&out);
    assert!(out.starts_with(b"\x1b[?25l"), "cursor hidden first");
    assert!(txt.contains("tottom"), "top paints over bottom: {txt:?}");
}

/// Records the breakpoints it is stamped with, exposed through `bounds`.
#[derive(Default)]
struct BreakpointProbe(Option<ChromeBreakpoints>);

impl RenderOverlay for BreakpointProbe {
    fn render(&self, _area: Rect, _buf: &mut Buffer) {}
    fn handle_key(&mut self, _key: &KeyEvent) -> OverlayCommand {
        OverlayCommand::Stay
    }
    fn set_breakpoints(&mut self, bp: ChromeBreakpoints) {
        self.0 = Some(bp);
    }
    fn bounds(&self, _area: Rect) -> Option<Rect> {
        self.0
            .map(|bp| Rect::new(0, 0, bp.compact_cols, bp.compact_rows))
    }
}

/// `push` stamps the configured breakpoints on the way in, and a reload
/// re-stamps overlays already open.
#[test]
fn breakpoints_reach_pushed_and_already_stacked_overlays() {
    let viewport = Rect::new(0, 0, 200, 60);
    let mut s = OverlayState::new();
    s.push(Box::new(BreakpointProbe::default()));
    assert_eq!(
        s.active_bounds(viewport),
        Some(Rect::new(0, 0, 64, 18)),
        "shipped"
    );
    s.set_breakpoints(ChromeBreakpoints {
        compact_cols: 100,
        compact_rows: 40,
        min_pane_cols: 30,
    });
    assert_eq!(s.active_bounds(viewport), Some(Rect::new(0, 0, 100, 40)));
    s.dismiss();
    s.push(Box::new(BreakpointProbe::default()));
    assert_eq!(s.active_bounds(viewport), Some(Rect::new(0, 0, 100, 40)));
}

#[test]
fn active_bounds_unions_bounded_overlays_and_yields_to_full_screen() {
    let mut s = OverlayState::new();
    let viewport = Rect::new(0, 0, 40, 20);
    s.push(Box::new(Probe::bounded(Rect::new(2, 2, 4, 4))));
    assert_eq!(s.active_bounds(viewport), Some(Rect::new(2, 2, 4, 4)));
    s.push(Box::new(Probe::bounded(Rect::new(10, 8, 4, 4))));
    assert_eq!(s.active_bounds(viewport), Some(Rect::new(2, 2, 12, 10)));
    s.push(Box::new(Probe::esc()));
    assert_eq!(
        s.active_bounds(viewport),
        None,
        "any full-screen overlay wins"
    );
}

/// The floating-modal invariant: only cells inside the clip (and its
/// shadow) are emitted, so the panes around the box stay untouched.
#[test]
fn paint_clipped_emits_only_the_box_and_its_shadow() {
    let rect = Rect::new(5, 3, 10, 4);
    let mut s = OverlayState::new();
    s.push(Box::new(Probe::bounded(rect)));
    let paint = |shadow| {
        let mut out = Vec::new();
        s.paint_clipped(&mut out, (40, 12), Rect::new(0, 0, 40, 12), rect, shadow)
            .expect("paint");
        String::from_utf8_lossy(&out).into_owned()
    };
    let plain = paint(Color::Reset);
    assert!(
        plain.contains("INSIDE") && !plain.contains("OUTSIDE"),
        "{plain:?}"
    );
    assert!(plain.contains("\x1b[4;6H"), "clip-origin CUP: {plain:?}");
    assert!(
        !plain.contains("\x1b[1;") && !plain.contains("\x1b[8;"),
        "{plain:?}"
    );

    // The shadow row sits below the box, one cell in, in the shadow bg.
    let shadowed = paint(Color::Rgb(20, 20, 30));
    assert!(shadowed.contains("\x1b[8;7H"), "{shadowed:?}");
    assert!(shadowed.contains("48;2;20;20;30"), "{shadowed:?}");
    assert!(!shadowed.contains("OUTSIDE"), "{shadowed:?}");
}

#[test]
fn unicode_vt_paint_keeps_borders_on_grid_in_full_and_clipped_paths() {
    use crate::attach::render::{ReplicaWalk, TerminalRenderer};
    use libghostty_vt::Terminal;
    let mut buf = Buffer::empty(Rect::new(0, 0, 24, 4));
    buf.set_string(
        3,
        1,
        "│构建工具 cafe\u{301}│",
        ratatui::style::Style::default(),
    );
    for clipped in [false, true] {
        let mut out = Vec::new();
        if clipped {
            emit_row_span(&mut out, &buf, 1, 3, 20).expect("clipped paint");
        } else {
            emit_buffer(&mut out, &buf).expect("full paint");
        }
        let mut terminal = Terminal::new(24, 4).expect("terminal");
        terminal
            .set_scrollback_max_lines(Some(0))
            .expect("terminal");
        terminal.vt_write(&out);
        let mut renderer = TerminalRenderer::new().expect("renderer");
        for (col, ch) in [
            (3, '│'),
            (4, '构'),
            (6, '建'),
            (8, '工'),
            (10, '具'),
            (17, '│'),
        ] {
            assert_eq!(
                renderer
                    .read_grapheme_at(ReplicaWalk::for_test(&terminal), 1, col)
                    .expect("cell"),
                Some(ch),
                "clipped={clipped}, col={col}"
            );
        }
    }
}

/// A context menu pinned in an 80x24 viewport, holding a destructive row.
fn pinned_menu() -> ContextMenu {
    ContextMenu::new(
        "pane",
        vec![
            MenuRow::item("Close pane", action("kill-pane")),
            MenuRow::item("Zoom", action("toggle-zoom")),
        ],
        (70, 18),
        crate::layout::Rect {
            x: 0,
            y: 0,
            w: 80,
            h: 24,
        },
        &Theme::default(),
    )
}

/// A resize drops a pointer-pinned menu (it could otherwise commit
/// `kill-pane` invisibly on Enter) anywhere in the stack, keeps reflowing
/// overlays, and then hands survivors the new pane size.
#[test]
fn a_resize_drops_pinned_overlays_and_resizes_survivors() {
    let mut s = OverlayState::new();
    assert!(!s.dismiss_stale_on_resize(), "empty: no change");
    s.push(Box::new(pinned_menu()));
    s.push(Box::new(SelectList::new(
        "command palette",
        vec![SelectItem::new("Close pane", action("kill-pane"))],
        &Theme::default(),
    )));
    assert!(s.dismiss_stale_on_resize());
    assert_eq!(s.depth(), 1, "only the pinned menu is dropped");
    assert!(!s.wants_pointer_hover());
    assert!(!s.dismiss_stale_on_resize(), "nothing stale left");
    s.dismiss();
    assert_eq!(s.handle_key(&key(PhysicalKey::Enter)), OverlayOutcome::None);

    s.push(Box::new(pinned_menu()));
    s.push(Box::new(CopyModeOverlay::new(20, 70, 80, 24)));
    assert!(s.dismiss_stale_on_resize());
    s.on_viewport_resize(60, 18);
    let sel = s.copy_selection().expect("copy-mode survived");
    assert!(sel.end_row < 18 && sel.end_col < 60, "{sel:?}");
}
