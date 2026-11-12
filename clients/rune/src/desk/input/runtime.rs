//! Clipboard commands over a native field never address the terminal behind it.
use super::{Modal, Shortcut};
use rune::{Key, KeyEvent, Mods};

#[test]
fn find_clipboard_commands_cannot_escape_to_the_terminal() {
    let (client, mut rt) = crate::grid::budget::input_fixture();
    rt.state.prefs = rune::prefs::Prefs::memory();
    let board = rune::clipboard::memory();
    board.set_text("clipboard sentinel");
    rt.use_clipboard(&board);
    rt.update(|d, tx| {
        tx.instant();
        d.open_find();
        d.find_query = rune::TextEdit::new("field sentinel");
        d.find_query.focused = true;
    });
    rt.frame(0.0);
    rt.frame(1.0);
    let (x, y) = rt.find("find-query").expect("find field");
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(0.0);
    assert!(rt.state.find_query.focused);
    let _ = client.take_outbound();

    // On Linux these used to be native editing chords. Rune now passes
    // Ctrl+Shift+C/V/A to Desk::key; plain Ctrl+C/V/A still edit the field.
    // On macOS these extended chords must be harmless too.
    for c in ['v', 'a', 'c'] {
        rt.key(KeyEvent {
            key: Key::Command(c),
            mods: Mods {
                cmd: true,
                ctrl: true,
                shift: true,
                ..Mods::default()
            },
        });
        rt.frame(0.0);
    }
    // Linux also binds Shift+Insert to app Paste.
    rt.key(KeyEvent {
        key: Key::Insert,
        mods: Mods {
            shift: true,
            ..Mods::default()
        },
    });
    rt.frame(0.0);
    // Menu/palette dispatch shares the same modal gate on every platform.
    for action in [Shortcut::Paste, Shortcut::SelectAll, Shortcut::Copy] {
        rt.update(|d, tx| d.modal_command(Modal::FindField, action, tx));
        rt.frame(0.0);
        assert!(
            client.take_outbound().is_empty(),
            "{action:?} addressed the terminal"
        );
    }
    assert_eq!(rt.state.find_query.text, "field sentinel");
    assert_eq!(
        board.text().as_deref().map(String::as_str),
        Some("clipboard sentinel")
    );
    assert!(rt.state.pending.is_none());
    assert!(
        client.take_outbound().is_empty(),
        "field commands sent terminal input"
    );

    // Native field editing still works, without addressing the terminal.
    rt.key(KeyEvent {
        key: Key::Command('a'),
        mods: Mods {
            cmd: true,
            ctrl: !cfg!(target_os = "macos"),
            ..Mods::default()
        },
    });
    rt.paste("native field paste");
    rt.frame(0.0);
    assert_eq!(rt.state.find_query.text, "native field paste");
    assert!(client.take_outbound().is_empty());
}

#[test]
fn native_field_ink_follows_live_themes_in_both_forms() {
    for phone in [false, true] {
        let (_, mut rt) = crate::grid::budget::input_fixture();
        rt.state.prefs = rune::prefs::Prefs::memory();
        rt.state.phone = phone.then(crate::desk::phone::Phone::default);
        rt.set_viewport(
            if phone {
                (393.0, 852.0)
            } else {
                (1100.0, 720.0)
            },
            2.0,
        );
        for theme in ["catppuccin-mocha", "catppuccin-latte"] {
            let theme = crate::theme::find(theme).expect("theme");
            rt.update(|d, tx| {
                tx.instant();
                d.settings.theme = Some(theme);
                d.palette_open = true;
                d.palette_query = rune::TextEdit::new("field ink");
                d.palette_query.focused = true;
            });
            rt.frame(0.0);
            let (x, y) = rt.find("palette-query").expect("palette field");
            rt.pointer_down(x, y);
            rt.pointer_up(x, y);
            rt.frame(0.0);
            let field = rt
                .elements()
                .into_iter()
                .find(|e| e.key.as_deref() == Some("palette-query"))
                .expect("field");
            let caret = field.caret.expect("focused caret");
            let ink = theme.roles().text;
            let drawn = rt
                .instances
                .iter()
                .find(|i| {
                    // A thin terminal glyph can share this x coordinate;
                    // only the rectangle inside the field is its caret.
                    i.params[1] == rune::render::KIND_RECT
                        && (i.rect[0] / 2.0 - caret[0]).abs() < 1.0
                        && i.rect[1] / 2.0 >= caret[1] - 1.0
                        && (i.rect[1] + i.rect[3]) / 2.0 <= caret[1] + caret[3] + 1.0
                        && i.rect[2] <= 4.0
                        && i.rect[3] > 10.0
                })
                .expect("drawn caret");
            assert_eq!(
                drawn.fill,
                ink.premul(1.0),
                "{} phone={phone} caret",
                theme.id
            );
            rt.key(KeyEvent {
                key: Key::Command('a'),
                mods: Mods {
                    cmd: true,
                    ctrl: !cfg!(target_os = "macos"),
                    ..Mods::default()
                },
            });
            rt.frame(0.0);
            assert!(
                rt.instances
                    .iter()
                    .any(|i| i.fill == ink.alpha(0.2).premul(1.0)),
                "{} phone={phone} selection follows the foreground",
                theme.id
            );
        }
    }
}

#[test]
fn native_field_context_menu_edits_only_its_find_field_in_both_forms() {
    for phone in [false, true] {
        let (client, mut rt) = crate::grid::budget::input_fixture();
        rt.state.prefs = rune::prefs::Prefs::memory();
        rt.state.phone = phone.then(crate::desk::phone::Phone::default);
        rt.set_viewport(
            if phone {
                (393.0, 852.0)
            } else {
                (1100.0, 720.0)
            },
            2.0,
        );
        let board = rune::clipboard::memory();
        board.set_text("clipboard sentinel");
        rt.use_clipboard(&board);
        rt.update(|d, tx| {
            tx.instant();
            d.open_find();
            d.find_query = rune::TextEdit::new("field sentinel");
            d.find_query.focused = true;
        });
        rt.frame(0.0);
        // Phone stage measurement retargets its camera after the first
        // frame; advance its spring before hit-testing the Find field.
        for _ in 0..120 {
            rt.frame(1.0 / 60.0);
        }
        let (x, y) = rt.find("find-query").expect("find field");
        rt.pointer_down(x, y);
        rt.pointer_up(x, y);
        rt.frame(0.0);
        rt.update(|d, _| d.find_query.set_selection(0, d.find_query.text.len()));
        rt.frame(0.0);
        let _ = client.take_outbound();
        rt.secondary_down(x, y);
        rt.secondary_up(x, y);
        rt.frame(0.0);
        let menu = rt.popup_menu().expect("native field menu");
        assert!(menu.find("copy").expect("Copy").is_enabled());
        assert!(menu.find("paste").is_some());
        assert!(rt.state.pointer.menu.is_none(), "not the terminal's menu");
        rt.resolve_popup(Some("copy")).expect("copy the field");
        rt.frame(0.0);
        assert_eq!(
            board.text().as_deref().map(String::as_str),
            Some("field sentinel")
        );

        rt.secondary_down(x, y);
        rt.secondary_up(x, y);
        rt.frame(0.0);
        rt.resolve_popup(Some("cut")).expect("cut the field");
        rt.frame(0.0);
        assert!(rt.state.find_query.text.is_empty());
        assert!(rt.state.find_query.focused);
        assert!(
            client.take_outbound().is_empty(),
            "menu sent terminal input"
        );
    }
}
