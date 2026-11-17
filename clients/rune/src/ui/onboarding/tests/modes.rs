//! Onboarding in each appearance the design language names (§5.2).

use super::*;

/// The largest chrome text scale the gallery checks (Rune `TextSize` max),
/// in percent.
const LARGEST: u8 = 140;

/// The house look in `dark`, at `size`, on `step`; increased contrast and
/// the text scale as asked.
fn house(step: Step, size: (f32, f32), dark: bool, more: bool, scale: f32) -> Runtime<Desk> {
    crate::ui::tokens::overrides::set(false, scale);
    let mut desk = crate::test_support::settings_window();
    desk.connection.resources.images = Some(rune::ImageCache::new(std::sync::Arc::new(|| {})));
    desk.settings_open = false;
    desk.settings.theme = None;
    let mut rt = Runtime::new(desk, crate::ui::view, size, Env::new(dark, 1.0));
    rt.on_key = Some(Desk::key);
    rt.set_srgb_blend(true);
    rt.set_system_increased_contrast(more);
    rt.update(|d, tx| {
        d.open_onboarding();
        d.pump_onboarding(tx);
        d.onboarding.skills = Probe::Ready(vec![
            skill("claude", "installed"),
            skill("codex", "missing"),
        ]);
        d.onboarding.step = step;
    });
    settle(&mut rt);
    rt
}

/// Each step in the house look's both modes, under increased contrast and
/// at the largest text, at the desktop and minimum window sizes. Onboarding
/// is a desktop window's; the phone has its own (iOS) flow.
#[test]
fn every_step_renders_in_every_mode() {
    let gpu = rune::render::Gpu::headless();
    let steps = [
        Step::Welcome,
        Step::Look,
        Step::Agents,
        Step::Migrate,
        Step::Machines,
        Step::Done,
    ];
    let modes = [
        ("base", false, 1.0),
        ("contrast-more", true, 1.0),
        ("largest-text", false, f32::from(LARGEST) / 100.0),
    ];
    for (size, name) in [
        ((1100.0, 720.0), "desktop"),
        ((640.0, 360.0), "desktop-min"),
    ] {
        for dark in [false, true] {
            for (mode, more, scale) in modes {
                for step in steps {
                    let mut rt = house(step, size, dark, more, scale);
                    let tag = format!(
                        "mode-{name}-{}-{mode}-{}",
                        if dark { "dark" } else { "light" },
                        step.id()
                    );
                    assert!(has(&rt, &format!("onboarding-{}", step.id())), "{tag}");
                    let (w, h) = (size.0 as u32, size.1 as u32);
                    let pixels = crate::ui::offscreen_as_app(&gpu, &mut rt, (w, h));
                    if let Some(dir) = std::env::var_os("RUNE_ONBOARDING_SNAP") {
                        let path = std::path::Path::new(&dir).join(format!("{tag}.png"));
                        std::fs::create_dir_all(&dir).unwrap();
                        rune::render::write_png(&path, w, h, &pixels).unwrap();
                    }
                }
            }
        }
    }
    crate::ui::tokens::overrides::set(false, 1.0);
}

/// Every choice on the Look step is a focusable radio, and the step's
/// actions are kit buttons in the Tab order.
#[test]
fn look_choices_are_radios_and_actions_are_tab_stops() {
    let rt = window(Step::Look);
    for key in ["look-auto", "look-light", "look-dark", "look-theme-phux"] {
        assert!(crate::ui::element(&rt, key).focusable, "{key}");
    }
    let json = rt.accessibility_json().to_string();
    assert!(json.contains("\"radio-button\""), "radio roles");
    for key in ["onboarding-continue", "onboarding-back"] {
        assert!(crate::ui::element(&rt, key).focusable, "{key}");
    }
}
