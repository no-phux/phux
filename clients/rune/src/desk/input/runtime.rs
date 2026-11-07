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
                    (i.rect[0] / 2.0 - caret[0]).abs() < 1.0 && i.rect[2] <= 4.0 && i.rect[3] > 10.0
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
