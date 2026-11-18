use super::*;
use crate::theme::contrast;

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
    // WCAG AA for every word-carrying ink (§1.2 D4).
    const TEXT_FLOOR: f32 = 4.5;
    for dark in [true, false] {
        let t = tokens_for(dark, None);
        let r = &t.roles;
        let readable = [
            ("text", r.text.text),
            ("dim", r.text.dim),
            ("failed", r.status.failed),
            ("done", r.status.done),
            ("bell", r.status.bell),
            ("working", r.status.working),
            ("needs you", r.status.needs_you),
        ];
        for ground in [r.layer.ground, r.layer.sidebar, r.layer.raised] {
            for (role, ink) in readable {
                let got = contrast(ink.paint(), ground.paint());
                assert!(got >= TEXT_FLOOR, "{role}, dark {dark}: {got}");
            }
        }
    }
}
