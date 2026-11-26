//! Actual Machines view, not the separate UIKit connection controller.
use super::*;

mod behavior;

fn fixture(size: (u32, u32), phone: bool, dark: bool, scale: f32) -> Runtime<Desk> {
    let mut desk = crate::test_support::window().with_phone(phone);
    desk.machines.open = true;
    desk.settings.theme = crate::theme::find(if dark { "phux-dark" } else { "phux-light" });
    if let Some(phone) = &mut desk.phone {
        phone.keyboard = false;
    }
    let mut rt = crate::test_support::runtime(
        desk,
        crate::ui::view,
        (size.0 as f32, size.1 as f32),
        Env::new(dark, scale),
    );
    rt.set_srgb_blend(true);
    rt.set_system_reduced_motion(true);
    settle(&mut rt);
    let key = format!("machine-color-{LOCAL}");
    tap(&mut rt, &key);
    rt
}

fn settle(rt: &mut Runtime<Desk>) {
    rt.settle_all();
    rt.frame(1.0);
    rt.frame(1.0);
}

fn tap(rt: &mut Runtime<Desk>, key: &str) {
    let e = crate::ui::element(rt, key);
    let [x, y, w, h] = e.rect;
    let point = (x + w / 2.0, y + h / 2.0);
    rt.touch(1, rune::TouchPhase::Started, point.0, point.1, 0.1);
    rt.frame(0.1);
    rt.touch(1, rune::TouchPhase::Ended, point.0, point.1, 0.2);
    settle(rt);
}

fn keyboard_focus(rt: &mut Runtime<Desk>, key: &str) {
    // As in reconnect's regressions: Tx::focus alone does not request reveal.
    rt.update(|_, tx| tx.focus(key, false));
    settle(rt);
    rt.key(rune::KeyEvent {
        key: rune::Key::Tab,
        mods: rune::Mods {
            shift: true,
            ..rune::Mods::default()
        },
    });
    settle(rt);
    rt.key(crate::keys::plain(rune::Key::Tab));
    settle(rt);
}

fn save(
    gpu: &rune::render::Gpu,
    renderer: &mut rune::render::Renderer,
    rt: &mut Runtime<Desk>,
    size: (u32, u32),
    stem: &str,
) {
    let pixels = crate::ui::offscreen_as_app_with_renderer(gpu, renderer, rt, size);
    if let Some(dir) = std::env::var_os("RUNE_SETTINGS_SNAP") {
        crate::test_support::visual::write(
            &std::path::Path::new(&dir).join(format!("{stem}.png")),
            rt,
            size,
            &pixels,
            stem,
        );
    }
}

#[test]
fn machine_colour_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    for (form, size, phone, scale) in [
        ("desktop", (1100, 720), false, 1),
        ("desktop-min", (640, 360), false, 1),
        ("phone", (393, 852), true, 1),
        ("small", (320, 568), true, 1),
        ("phone", (393, 852), true, 2),
        ("small", (320, 568), true, 2),
    ] {
        for (theme, dark) in [("light", false), ("dark", true)] {
            for (case, largest, keyboard) in [
                ("ordinary", false, false),
                ("large", true, false),
                ("keyboard", false, true),
                ("large-keyboard", true, true),
            ] {
                if !phone && case != "ordinary" {
                    continue;
                }
                let mut rt = fixture(size, phone, dark, scale as f32);
                assert_eq!(rt.state.machines.picking.as_deref(), Some(LOCAL));
                rt.update(|d, _| {
                    d.set_accessibility(
                        false,
                        if largest {
                            "UICTContentSizeCategoryAccessibilityXXXL"
                        } else {
                            "UICTContentSizeCategoryL"
                        },
                    );
                    if let Some(phone) = &mut d.phone {
                        phone.keyboard = keyboard;
                    }
                });
                settle(&mut rt);
                let mut renderer = rune::render::Renderer::new(
                    &gpu.device,
                    rune::render::OFFSCREEN_FORMAT.remove_srgb_suffix(),
                );
                renderer.set_srgb_blend(true);
                let stem = format!("machine-colour-{form}-{case}-{theme}-{scale}x");
                let pixels = (size.0 * scale, size.1 * scale);
                save(&gpu, &mut renderer, &mut rt, pixels, &stem);
                keyboard_focus(&mut rt, &format!("machine-swatch-{LOCAL}-auto"));
                save(
                    &gpu,
                    &mut renderer,
                    &mut rt,
                    pixels,
                    &format!("{stem}-focused"),
                );
            }
        }
    }
}
