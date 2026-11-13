//! The quick terminal's rows: a switch, and a recorder that registers what
//! it records at once and keeps the old chord when that's refused.

use rune::{PhysicalInput, PhysicalKeyCode, ShortcutError};

use super::*;

fn shows(rt: &Runtime<Desk>, words: &str) -> bool {
    rt.elements()
        .iter()
        .any(|e| e.text.as_deref().is_some_and(|t| t.contains(words)))
}

fn press(rt: &mut Runtime<Desk>, key: &str) {
    rt.update(|_, tx| tx.scroll_into_view("set-quick-chord"));
    rt.frame(1.0);
    let (x, y) = rt.find(key).unwrap_or_else(|| panic!("missing {key}"));
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(0.0);
}

#[test]
fn the_quick_terminal_has_a_switch_and_a_live_recorder() {
    let gpu = rune::render::Gpu::headless();
    let mut renderer = rune::render::Renderer::new(&gpu.device, rune::render::OFFSCREEN_FORMAT);
    let mut rt = runtime((1100.0, 720.0), false);
    rt.update(|d, tx| {
        tx.instant();
        d.settings_category(Category::Terminal);
    });
    rt.frame(0.0);
    assert!(rt.find("set-quick-terminal").is_none(), "no panel, no rows");
    rt.update(|d, _| d.quick_fixture(true));
    rt.frame(0.0);
    assert!(rt.find("set-quick-terminal").is_some());
    render(&gpu, &mut renderer, &mut rt, (1100, 720), "quick-terminal");

    // A recorded chord is registered at once. This panel never ran, so it's
    // refused: the old chord stays, and the row says why.
    press(&mut rt, "set-quick-chord");
    assert!(rt.state.quick_chord.recorder.is_recording());
    let ctrl = Mods {
        ctrl: true,
        ..Mods::default()
    };
    rt.physical_input(PhysicalInput::Key {
        key: PhysicalKeyCode::KeyJ,
        mods: ctrl,
        pressed: true,
        repeat: false,
    });
    rt.frame(0.0);
    let recorder = &rt.state.quick_chord.recorder;
    assert_eq!(recorder.error(), Some(&ShortcutError::NotRunning));
    let kept = recorder.shortcut().map(ToString::to_string);
    assert_eq!(kept.as_deref(), Some("ctrl+backquote"));
    assert!(shows(&rt, "couldn't be registered"));
    render(&gpu, &mut renderer, &mut rt, (1100, 720), "quick-refused");

    // Off, the recorder takes no clicks; set in the file, both rows say so.
    rt.update(|d, _| {
        d.quick_chord.recorder.cancel();
        d.quick_chord.on = false;
    });
    press(&mut rt, "set-quick-chord");
    assert!(!rt.state.quick_chord.recorder.is_recording());
    rt.update(|d, _| d.file.quick_terminal = Some("ctrl+backquote".into()));
    rt.frame(0.0);
    rt.update(|d, _| d.toggle_quick_terminal());
    assert!(!rt.state.quick_chord.on, "the file's choice stands");
    assert!(shows(&rt, "Set in config.toml"));
}
