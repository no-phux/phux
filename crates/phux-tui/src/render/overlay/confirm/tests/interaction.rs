//! Keyboard, pointer, and compact layout behavior of confirmation dialogs.

use phux_protocol::input::mouse::{MouseAction, MouseButton};

use super::*;

#[test]
fn tab_moves_focus_to_the_approval_button_and_enter_presses_it() {
    let mut overlay = clipboard();
    let area = Rect::new(0, 0, 80, 24);
    overlay.render(area, &mut Buffer::empty(area));
    assert_eq!(
        overlay.handle_key(&press(PhysicalKey::Tab)),
        OverlayCommand::Stay
    );
    assert_eq!(
        overlay.handle_key(&press(PhysicalKey::Enter)),
        OverlayCommand::SetClipboard(ClipboardText("secret".to_owned()))
    );
}

#[test]
fn clicking_a_button_presses_it() {
    let mut overlay = clipboard();
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    overlay.render(area, &mut buf);
    let (x, y) = (0..area.height)
        .find_map(|y| {
            let row: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect();
            row.find("Allow").map(|x| (x, y))
        })
        .expect("the Allow button is painted");
    let at = |action| MouseEvent {
        action,
        button: MouseButton::Left,
        mods: ModSet::empty(),
        x: f64::from(u16::try_from(x).expect("fits") + 1),
        y: f64::from(y),
    };
    assert_eq!(
        overlay.handle_mouse(&at(MouseAction::Press)),
        OverlayCommand::Stay
    );
    assert_eq!(
        overlay.handle_mouse(&at(MouseAction::Release)),
        OverlayCommand::SetClipboard(ClipboardText("secret".to_owned()))
    );
}

#[test]
fn approval_requires_an_explicit_key_and_enter_initially_declines() {
    let held = paste(b"make\nmake install\n");
    let paste_confirm = ConfirmOverlay::paste(held.clone(), 2, false, &Theme::default());
    for mods in [ModSet::empty(), ModSet::SHIFT, ModSet::CAPS_LOCK] {
        let key = crate::test_util::key_event(PhysicalKey::Y, mods, None);
        assert_eq!(
            paste_confirm.clone().handle_key(&key),
            OverlayCommand::Paste(held.clone())
        );
        assert_eq!(
            clipboard().handle_key(&key),
            OverlayCommand::SetClipboard(ClipboardText("secret".to_owned()))
        );
    }
    for key in [PhysicalKey::Escape, PhysicalKey::N, PhysicalKey::Enter] {
        assert_eq!(
            paste_confirm.clone().handle_key(&press(key)),
            OverlayCommand::Dismiss
        );
        assert_eq!(
            kill_window(&[]).handle_key(&press(key)),
            OverlayCommand::Dismiss
        );
    }
    let event = |key, mods, text| crate::test_util::key_event(key, mods, text);
    let wrong_letter = event(PhysicalKey::Y, ModSet::empty(), Some("f"));
    assert_eq!(clipboard().handle_key(&wrong_letter), OverlayCommand::Stay);
    for mods in [ModSet::CTRL, ModSet::ALT, ModSet::SUPER] {
        for key in [PhysicalKey::Y, PhysicalKey::Enter] {
            assert_eq!(
                clipboard().handle_key(&event(key, mods, None)),
                OverlayCommand::Stay
            );
        }
    }
    assert_eq!(
        kill_window(&[]).handle_key(&press(PhysicalKey::A)),
        OverlayCommand::Stay
    );
}

#[test]
fn a_narrow_dialog_stacks_whole_actions_and_keeps_cancel_focused() {
    let mut overlay = kill_window(&[]);
    let text = rendered(&overlay, 20, 14);
    let lines: Vec<_> = text.lines().collect();
    let cancel = lines.iter().position(|line| line.contains("Cancel"));
    let kill = lines.iter().rposition(|line| line.contains("Kill tab"));
    assert!(
        matches!((cancel, kill), (Some(a), Some(b)) if a < b),
        "{text}"
    );
    assert_eq!(
        overlay.handle_key(&press(PhysicalKey::Enter)),
        OverlayCommand::Dismiss
    );
    assert_eq!(
        overlay.handle_key(&press(PhysicalKey::Tab)),
        OverlayCommand::Stay
    );
    assert_eq!(
        kill_args(overlay.handle_key(&press(PhysicalKey::Enter))).0,
        "kill-window"
    );
}
