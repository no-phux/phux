//! The palette's own rows: options in one list, the picked one the
//! keyboard's cursor, its rules where the separator tests look for them.

use rune::{Env, Runtime};

use crate::desk::Desk;

fn palette(dark: bool) -> Runtime<Desk> {
    let mut rt = crate::test_support::runtime(
        crate::test_support::window(),
        crate::ui::view,
        (1100.0, 720.0),
        Env::new(dark, 2.0),
    );
    rt.on_key = Some(Desk::key);
    rt.frame(0.0);
    rt.update(|d, _| d.toggle_palette());
    rt.frame(0.0);
    rt
}

#[test]
fn rows_are_options_in_one_list_and_the_picked_row_is_selected() {
    for dark in [true, false] {
        let rt = palette(dark);
        let list = crate::ui::element(&rt, "palette-results");
        assert_eq!(list.role, "listbox");
        let first = crate::ui::element(&rt, "palette-row-0");
        assert_eq!(first.role, "option");
        assert!(first.label.as_deref().is_some_and(|l| !l.is_empty()));
        let tree = rt.accessibility_json();
        let text = tree.to_string();
        assert!(text.contains("palette-row-1"), "more than one row");
        for key in ["palette-header-rule", "palette-footer-rule"] {
            assert!(rt.find(key).is_some(), "{key}");
        }
    }
}

#[test]
fn the_arrows_move_the_picked_row() {
    let mut rt = palette(true);
    assert_eq!(rt.state.palette_picked, 0);
    rt.key(rune::KeyEvent {
        key: rune::Key::Down,
        mods: rune::Mods::default(),
    });
    rt.frame(0.0);
    assert_eq!(rt.state.palette_picked, 1, "the field keeps the arrows");
    assert!(rt.state.palette_open);
}
