use super::*;
use crate::theme::{DIM_BAR, TEXT_BAR, contrast};

#[test]
fn the_bare_window_drops_the_chrome_and_keeps_the_sheets() {
    let mut desk = crate::test_support::settings_window();
    desk.settings_open = false;
    let mut rt = rune::Runtime::new(desk, view, (900.0, 600.0), rune::Env::new(false, 1.0));
    rt.frame(0.0);
    assert!(rt.presented("sidebar-toggle").is_some());
    rt.update(|d, _| d.bare = Some(crate::desk::bare::Bare::test("tui", "work")));
    rt.frame(1.0);
    assert!(rt.presented("sidebar-toggle").is_none(), "no title bar");
    assert!(rt.presented("sidebar-host").is_none(), "no sidebar");
    let stage = rt.presented("stage").expect("the pane").0;
    assert!(stage[2] >= 900.0 - 0.5, "edge to edge: {stage:?}");
    rt.update(|d, _| d.settings_open = true);
    rt.frame(2.0);
    assert!(
        rt.presented("settings-close").is_some(),
        "sheets still show"
    );
    // With nothing connected, the chord again simply leaves.
    rt.update(|d, _| d.toggle_bare());
    rt.frame(3.0);
    assert!(rt.presented("sidebar-toggle").is_some());
}

#[test]
fn the_system_appearance_clears_the_bars_too() {
    for dark in [true, false] {
        let p = ink(dark, rune::oklch(0.6, 0.15, 250.0), None);
        assert!(Rc::ptr_eq(
            &p.tokens,
            &ink(dark, rune::oklch(0.6, 0.15, 250.0), None).tokens
        ));
        let marks = p.marks;
        let readable = [
            ("text", p.text),
            ("danger", p.danger),
            ("alert", marks.alert),
            ("done", marks.done),
            ("bell", marks.bell),
            ("working", marks.working),
            ("good", marks.good),
        ];
        for ground in [p.bg, p.sidebar, p.raised] {
            assert!(contrast(p.dim, ground) >= DIM_BAR, "dim, dark {dark}");
            for (role, ink) in readable {
                let got = contrast(ink, ground);
                assert!(got >= TEXT_BAR, "{role}, dark {dark}: {got}");
            }
        }
    }
}
