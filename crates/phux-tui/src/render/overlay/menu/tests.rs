#![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

use super::*;

fn area(x: u16, y: u16, w: u16, h: u16) -> crate::layout::Rect {
    crate::layout::Rect { x, y, w, h }
}

fn action(name: &str) -> ResolvedAction {
    crate::attach::resolved(name, std::collections::BTreeMap::new())
}

/// split-pane, toggle-zoom, a separator, kill-pane.
fn rows() -> Vec<MenuRow> {
    vec![
        MenuRow::item("Split right", action("split-pane")).secondary(Some("C-a |".to_owned())),
        MenuRow::item("Zoom", action("toggle-zoom")),
        MenuRow::Separator,
        MenuRow::item("Close pane", action("kill-pane")),
    ]
}

fn menu_in(anchor: (u16, u16), area: crate::layout::Rect) -> ContextMenu {
    ContextMenu::new("pane", rows(), anchor, area, &Theme::default())
}

fn menu_at(x: u16, y: u16) -> ContextMenu {
    menu_in((x, y), area(0, 0, 80, 24))
}

fn mouse(action: MouseAction, button: MouseButton, x: u16, y: u16) -> MouseEvent {
    MouseEvent {
        action,
        button,
        mods: ModSet::empty(),
        x: f64::from(x),
        y: f64::from(y),
    }
}

fn press(x: u16, y: u16) -> MouseEvent {
    mouse(MouseAction::Press, MouseButton::Left, x, y)
}

use crate::test_util::press as key;

fn committed(cmd: OverlayCommand) -> String {
    match cmd {
        OverlayCommand::Commit(a) => a.action,
        other => panic!("expected a commit, got {other:?}"),
    }
}

fn painted(menu: &ContextMenu, cols: u16, rows_h: u16) -> String {
    let area = Rect::new(0, 0, cols, rows_h);
    let mut buf = Buffer::empty(area);
    menu.render(area, &mut buf);
    (0..rows_h)
        .flat_map(|y| (0..cols).map(move |x| (x, y)))
        .map(|(x, y)| buf[(x, y)].symbol().to_owned())
        .collect()
}

#[test]
fn placement_anchors_flips_and_clamps_into_the_area() {
    let r = menu_at(10, 4).rect();
    assert_eq!((r.x, r.y, r.height), (10, 4, 6), "corner on the anchor");

    // Bottom-right: flips up and left, still touching the anchor.
    let r = menu_at(79, 23).rect();
    assert_eq!((r.x + r.width - 1, r.y + r.height - 1), (79, 23));

    let r = menu_in((7, 2), area(0, 0, 8, 3)).rect();
    assert!(r.x + r.width <= 8 && r.y + r.height <= 3, "{r:?}");

    // Never over a left sidebar or a top status bar.
    let r = menu_in((3, 0), area(20, 1, 60, 23)).rect();
    assert!(r.x >= 20 && r.y >= 1, "{r:?}");

    let titled = ContextMenu::new(
        "a very long window name indeed",
        rows(),
        (0, 0),
        area(0, 0, 80, 24),
        &Theme::default(),
    );
    assert!(titled.rect().width >= 32, "the title fits");
}

/// The opening click's release lands on the border corner and leaves
/// the menu up; a release or press on a row commits it; a press on the
/// border or separator is inert; any press outside dismisses.
#[test]
fn pointer_input_commits_rows_and_dismisses_outside() {
    let mut menu = menu_at(10, 4);
    assert_eq!(
        menu.handle_mouse(&mouse(MouseAction::Release, MouseButton::Left, 10, 4)),
        OverlayCommand::Stay
    );
    assert_eq!(
        committed(menu.handle_mouse(&mouse(MouseAction::Release, MouseButton::Left, 12, 5))),
        "split-pane"
    );
    assert_eq!(
        committed(menu_at(10, 4).handle_mouse(&press(12, 8))),
        "kill-pane"
    );
    assert_eq!(
        menu.handle_mouse(&press(12, 7)),
        OverlayCommand::Stay,
        "separator"
    );
    assert_eq!(
        menu.handle_mouse(&press(10, 5)),
        OverlayCommand::Stay,
        "border"
    );
    for button in [MouseButton::Left, MouseButton::Right] {
        assert_eq!(
            menu_at(10, 4).handle_mouse(&mouse(MouseAction::Press, button, 2, 2)),
            OverlayCommand::Dismiss
        );
    }

    let mut menu = menu_at(10, 4);
    menu.handle_mouse(&mouse(MouseAction::Motion, MouseButton::Left, 12, 6));
    assert_eq!(
        committed(menu.handle_key(&key(PhysicalKey::Enter))),
        "toggle-zoom",
        "hover"
    );
    menu.handle_mouse(&mouse(MouseAction::Press, MouseButton::Four, 12, 5));
    assert_eq!(menu.model.selected, 0, "wheel up");
    menu.handle_mouse(&mouse(MouseAction::Press, MouseButton::Five, 12, 5));
    assert_eq!(menu.model.selected, 1, "wheel down");
}

#[test]
fn keys_skip_separators_wrap_jump_and_dismiss() {
    let mut menu = menu_at(10, 4);
    menu.handle_key(&key(PhysicalKey::ArrowDown));
    menu.handle_key(&key(PhysicalKey::ArrowDown));
    assert_eq!(
        committed(menu.handle_key(&key(PhysicalKey::Enter))),
        "kill-pane"
    );
    let mut menu = menu_at(10, 4);
    menu.handle_key(&key(PhysicalKey::ArrowUp));
    assert_eq!(
        committed(menu.handle_key(&key(PhysicalKey::Enter))),
        "kill-pane",
        "wraps"
    );
    menu.handle_key(&key(PhysicalKey::Home));
    assert_eq!(menu.model.selected, 0);
    menu.handle_key(&key(PhysicalKey::End));
    assert_eq!(menu.model.selected, 3);
    assert_eq!(
        menu.handle_key(&key(PhysicalKey::Escape)),
        OverlayCommand::Dismiss
    );
}

/// Painted whole or not at all: after a resize a truncated box would
/// show rows that disagree with the hit-test (which reads the pinned
/// rect). The driver then drops it because it does not survive resize.
#[test]
fn a_menu_paints_whole_or_not_at_all() {
    let menu = menu_at(10, 4);
    let row: String = painted(&menu, 80, 24);
    assert!(row.contains("Split right") && row.contains("C-a |"));

    let menu = menu_at(70, 18);
    assert!(painted(&menu, 40, 10).trim().is_empty(), "fully clipped");
    assert!(
        painted(&menu, 60, 20).trim().is_empty(),
        "partially clipped"
    );
    assert!(painted(&menu, 80, 24).contains("Split right"));
    assert!(!menu.survives_resize());
}

/// Hover drives the highlight through ratcn: it follows the pointer
/// over rows, holds over the separator and the border, and paints the
/// selection bar on the hovered row without moving any other cell.
#[test]
fn hover_moves_the_highlight_and_keeps_the_layout() {
    let mut menu = menu_at(10, 4);
    let before = painted(&menu, 80, 24);
    let motion = |x, y| mouse(MouseAction::Motion, MouseButton::Unknown, x, y);
    menu.handle_mouse(&motion(14, 8));
    assert_eq!(menu.model.selected, 3, "hover onto Close pane");
    menu.handle_mouse(&motion(14, 7));
    assert_eq!(menu.model.selected, 3, "the separator keeps it");
    menu.handle_mouse(&motion(10, 6));
    assert_eq!(menu.model.selected, 3, "the border keeps it");
    assert_eq!(painted(&menu, 80, 24), before, "the same glyphs");

    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    menu.render(area, &mut buf);
    let theme = Theme::default();
    assert_eq!(buf[(12, 8)].bg, theme.selection_bg, "Close pane is lit");
    assert_ne!(buf[(12, 5)].bg, theme.selection_bg, "Split right is not");
}

/// Press-drag-release: hovering with the button held, then releasing
/// over a row, commits the row released on.
#[test]
fn press_drag_release_commits_the_row_released_on() {
    let mut menu = menu_at(10, 4);
    let drag = |x, y| mouse(MouseAction::Motion, MouseButton::Left, x, y);
    menu.handle_mouse(&drag(13, 5));
    menu.handle_mouse(&drag(13, 6));
    assert_eq!(
        committed(menu.handle_mouse(&mouse(MouseAction::Release, MouseButton::Left, 13, 6))),
        "toggle-zoom"
    );
}

#[test]
fn the_wheel_steps_and_wraps() {
    let mut menu = menu_at(10, 4);
    let wheel = |button| mouse(MouseAction::Press, button, 12, 5);
    menu.handle_mouse(&wheel(MouseButton::Four));
    assert_eq!(menu.model.selected, 3, "wraps to the bottom");
    menu.handle_mouse(&wheel(MouseButton::Five));
    assert_eq!(menu.model.selected, 0, "wraps to the top");
    menu.handle_mouse(&wheel(MouseButton::Five));
    menu.handle_mouse(&wheel(MouseButton::Five));
    assert_eq!(menu.model.selected, 3, "skips the separator");
}
