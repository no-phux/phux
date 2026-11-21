use super::*;
use crate::test_util::{key_event, press_text as press, rendered};
use phux_protocol::input::mouse::{MouseAction, MouseButton};

fn typ(p: &mut PromptOverlay, ch: char) -> OverlayCommand {
    // PhysicalKey is irrelevant for text input; the buffer reads `text`.
    p.handle_key(&press(PhysicalKey::A, Some(&ch.to_string())))
}

fn committed_name(p: &mut PromptOverlay) -> Option<toml::Value> {
    let OverlayCommand::Commit(action) = p.handle_key(&press(PhysicalKey::Enter, None)) else {
        panic!("expected Commit");
    };
    action.args.get("name").cloned()
}

/// Where `needle` is painted lowest in `buf`, as a cell (the buttons sit
/// below a title that may repeat the verb).
fn find(buf: &Buffer, needle: &str) -> Option<(u16, u16)> {
    let area = buf.area;
    (0..area.height).rev().find_map(|y| {
        let row: String = (0..area.width).map(|x| buf[(x, y)].symbol()).collect();
        let at = row.find(needle)?;
        let x = row[..at].chars().count();
        Some((u16::try_from(x).ok()?, y))
    })
}

fn click(p: &mut PromptOverlay, (x, y): (u16, u16)) -> OverlayCommand {
    let at = |action| MouseEvent {
        action,
        button: MouseButton::Left,
        mods: ModSet::empty(),
        x: f64::from(x),
        y: f64::from(y),
    };
    assert_eq!(
        p.handle_mouse(&at(MouseAction::Press)),
        OverlayCommand::Stay
    );
    p.handle_mouse(&at(MouseAction::Release))
}

/// The committing button names what Enter does in each prompt.
#[test]
fn the_buttons_name_the_verb_and_the_placeholder_shows_while_empty() {
    let rendered = |p: &PromptOverlay| rendered(p, 80, 24);
    let theme = Theme::default();
    let window = rendered(&PromptOverlay::rename_window("a", &theme));
    assert!(window.contains("Rename tab"), "{window}");
    assert!(window.contains("Rename") && window.contains("Cancel"));
    assert!(rendered(&PromptOverlay::rename_session("a", &theme)).contains("Rename"));
    let fresh = rendered(&PromptOverlay::new_session(&theme));
    assert!(fresh.contains("Create"), "{fresh}");
    assert!(fresh.contains("a name for it"), "placeholder:\n{fresh}");
    let mut typed = PromptOverlay::new_session(&theme);
    typ(&mut typed, 'x');
    assert!(
        !rendered(&typed).contains("a name for it"),
        "the placeholder goes with the first character"
    );
}

#[test]
fn bounds_cover_the_painted_dialog() {
    let p = PromptOverlay::rename_window("build", &Theme::default());
    let area = Rect::new(0, 0, 80, 24);
    let mut buf = Buffer::empty(area);
    p.render(area, &mut buf);
    let bounds = p.bounds(area).expect("a modal");
    let painted = |x, y| buf[(x, y)].symbol() != " ";
    for y in 0..area.height {
        for x in 0..area.width {
            if painted(x, y) {
                assert!(
                    bounds.contains(ratatui::layout::Position::new(x, y)),
                    "({x},{y}) painted outside {bounds:?}"
                );
            }
        }
    }
    assert!(painted(bounds.x, bounds.y), "the border's corner");
    assert!(find(&buf, "build").is_some());
}

#[test]
fn typing_then_enter_commits_resolved_action() {
    let mut p = PromptOverlay::rename_window("", &Theme::default());
    for ch in ['b', 'u', 'i', 'l', 'd'] {
        assert_eq!(typ(&mut p, ch), OverlayCommand::Stay);
    }
    let cmd = p.handle_key(&press(PhysicalKey::Enter, None));
    let OverlayCommand::Commit(action) = cmd else {
        panic!("expected Commit, got {cmd:?}");
    };
    assert_eq!(action.action, "rename-window");
    assert_eq!(
        action.args.get("name"),
        Some(&toml::Value::String("build".to_owned()))
    );
}

/// The session prompts ignored `C-u`, `C-w`, `C-a`/`C-e`: a pre-filled
/// name had to be erased one Backspace at a time.
#[test]
fn readline_keys_edit_the_prompt() {
    let ctrl = |key| key_event(key, ModSet::CTRL, None);

    let mut p = PromptOverlay::rename_session("old name", &Theme::default());
    assert_eq!(p.handle_key(&ctrl(PhysicalKey::U)), OverlayCommand::Stay);
    for ch in "new".chars() {
        typ(&mut p, ch);
    }
    assert_eq!(
        committed_name(&mut p),
        Some(toml::Value::String("new".to_owned()))
    );

    let mut p = PromptOverlay::rename_session("work tmp", &Theme::default());
    p.handle_key(&ctrl(PhysicalKey::W));
    p.handle_key(&ctrl(PhysicalKey::A));
    typ(&mut p, 'x');
    p.handle_key(&ctrl(PhysicalKey::E));
    typ(&mut p, 'y');
    assert_eq!(
        committed_name(&mut p),
        Some(toml::Value::String("xwork y".to_owned()))
    );

    // C-u kills only what is before the cursor; C-k what is after.
    let mut p = PromptOverlay::rename_session("abcd", &Theme::default());
    for key in [PhysicalKey::B, PhysicalKey::B] {
        p.handle_key(&ctrl(key));
    }
    p.handle_key(&ctrl(PhysicalKey::U));
    assert_eq!(
        committed_name(&mut p),
        Some(toml::Value::String("cd".to_owned()))
    );
    let mut p = PromptOverlay::rename_session("abcd", &Theme::default());
    p.handle_key(&ctrl(PhysicalKey::A));
    p.handle_key(&ctrl(PhysicalKey::F));
    p.handle_key(&ctrl(PhysicalKey::K));
    assert_eq!(
        committed_name(&mut p),
        Some(toml::Value::String("a".to_owned()))
    );
}

/// Movement and deletion step over whole characters, and Ctrl+Backspace
/// (how the legacy BS byte decodes) deletes like Backspace.
#[test]
fn edits_respect_char_boundaries() {
    let plain = |key| key_event(key, ModSet::empty(), None);
    let ctrl = |key| key_event(key, ModSet::CTRL, None);
    let mut p = PromptOverlay::rename_session("héllo", &Theme::default());
    p.handle_key(&plain(PhysicalKey::Home));
    p.handle_key(&plain(PhysicalKey::ArrowRight));
    p.handle_key(&ctrl(PhysicalKey::D));
    p.handle_key(&plain(PhysicalKey::End));
    p.handle_key(&ctrl(PhysicalKey::Backspace));
    p.handle_key(&ctrl(PhysicalKey::H));
    typ(&mut p, 'ü');
    assert_eq!(
        committed_name(&mut p),
        Some(toml::Value::String("hlü".to_owned()))
    );
}

/// Backspace edits; Esc, an empty Enter, and an Enter after only
/// control text (which never enters the buffer) all cancel.
#[test]
fn backspace_edits_and_empty_or_escape_cancels() {
    let mut p = PromptOverlay::rename_window("ab", &Theme::default());
    assert_eq!(
        p.handle_key(&press(PhysicalKey::Backspace, None)),
        OverlayCommand::Stay
    );
    assert_eq!(
        committed_name(&mut p),
        Some(toml::Value::String("a".to_owned()))
    );

    let theme = Theme::default();
    let mut escaped = PromptOverlay::rename_window("x", &theme);
    assert_eq!(
        escaped.handle_key(&press(PhysicalKey::Escape, None)),
        OverlayCommand::Dismiss
    );
    let mut empty = PromptOverlay::rename_window("", &theme);
    assert_eq!(typ(&mut empty, '\t'), OverlayCommand::Stay);
    assert_eq!(
        empty.handle_key(&press(PhysicalKey::Enter, None)),
        OverlayCommand::Dismiss
    );
}

/// A paste lands at the cursor as text, never as keys; so does key text
/// that is more than one character (an IME commit).
#[test]
fn pastes_and_multi_character_text_insert_literally() {
    let mut p = PromptOverlay::rename_window("ad", &Theme::default());
    p.handle_key(&press(PhysicalKey::ArrowLeft, None));
    p.handle_paste("b\u{1b}c");
    assert_eq!(
        typ(&mut p, 'x'),
        OverlayCommand::Stay,
        "typing continues after the paste"
    );
    p.handle_key(&press(PhysicalKey::A, Some("日本")));
    assert_eq!(
        committed_name(&mut p),
        Some(toml::Value::String("abcx日本d".to_owned()))
    );
}

/// Tab is inert, as it always was: it never moves focus off the field, so
/// the Enter after it still commits.
#[test]
fn tab_stays_inert_and_enter_still_commits() {
    let tab = press(PhysicalKey::Tab, Some("\t"));
    let mut p = PromptOverlay::rename_window("build", &Theme::default());
    assert_eq!(p.handle_key(&tab), OverlayCommand::Stay);
    typ(&mut p, 'x');
    assert_eq!(
        committed_name(&mut p),
        Some(toml::Value::String("buildx".to_owned()))
    );
}

/// Shift, Ctrl or Alt held on Enter still commits.
#[test]
fn a_modified_enter_commits() {
    for mods in [ModSet::SHIFT, ModSet::CTRL, ModSet::ALT] {
        let mut p = PromptOverlay::rename_window("build", &Theme::default());
        let OverlayCommand::Commit(action) =
            p.handle_key(&key_event(PhysicalKey::Enter, mods, None))
        else {
            panic!("{mods:?}+Enter commits");
        };
        assert_eq!(
            action.args.get("name"),
            Some(&toml::Value::String("build".to_owned()))
        );
    }
}

/// A layout change while the prompt is up marks its surface stale; the next
/// key re-declares it offscreen at the real (large) viewport without
/// writing outside the scratch buffer.
#[test]
fn a_key_after_a_layout_change_on_a_large_viewport_does_not_panic() {
    let area = Rect::new(0, 0, 160, 60);
    let mut p = PromptOverlay::new_session(&Theme::default());
    p.render(area, &mut Buffer::empty(area));
    p.on_viewport_resize(160, 58);
    assert_eq!(typ(&mut p, 'a'), OverlayCommand::Stay);
}

/// A viewport too short for the field paints the frame and nothing past it.
#[test]
fn a_tiny_viewport_paints_without_panicking() {
    for (w, h) in [(80, 3), (80, 1), (10, 3), (1, 1)] {
        for y in 0..h {
            let outer = Rect::new(0, y, w, h - y);
            let buf_area = Rect::new(0, 0, w, h);
            let p = PromptOverlay::new_session(&Theme::default());
            p.render(outer, &mut Buffer::empty(buf_area));
        }
    }
}

#[test]
fn clicking_the_buttons_presses_them_and_clicking_the_field_moves_the_cursor() {
    let area = Rect::new(0, 0, 80, 24);
    let painted = |p: &PromptOverlay| {
        let mut buf = Buffer::empty(area);
        p.render(area, &mut buf);
        buf
    };

    let mut p = PromptOverlay::rename_window("build", &Theme::default());
    let at = find(&painted(&p), "Rename").expect("the verb button");
    let OverlayCommand::Commit(action) = click(&mut p, (at.0 + 1, at.1)) else {
        panic!("the verb button commits");
    };
    assert_eq!(
        action.args.get("name"),
        Some(&toml::Value::String("build".to_owned()))
    );

    let mut p = PromptOverlay::rename_window("build", &Theme::default());
    let at = find(&painted(&p), "Cancel").expect("the cancel button");
    assert_eq!(click(&mut p, (at.0 + 1, at.1)), OverlayCommand::Dismiss);

    // A click before the `u` puts the cursor there: typing lands in it.
    let mut p = PromptOverlay::rename_window("build", &Theme::default());
    let at = find(&painted(&p), "build").expect("the field text");
    assert_eq!(click(&mut p, (at.0 + 1, at.1)), OverlayCommand::Stay);
    typ(&mut p, 'x');
    assert_eq!(
        committed_name(&mut p),
        Some(toml::Value::String("bxuild".to_owned()))
    );
}
