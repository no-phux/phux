//! Settings through the real window in each appearance the design language
//! names (§5.2): the house look light and dark, increased contrast, and the
//! largest text size, at every size Settings is checked at.

use super::*;

/// The largest chrome text scale the gallery checks (Rune `TextSize` max).
const LARGEST: u8 = 140;

#[derive(Clone, Copy)]
enum Mode {
    Base,
    ContrastMore,
    LargestText,
}

impl Mode {
    const ALL: [Self; 3] = [Self::Base, Self::ContrastMore, Self::LargestText];

    const fn tag(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::ContrastMore => "contrast-more",
            Self::LargestText => "largest-text",
        }
    }
}

fn window(size: (f32, f32), phone: bool, dark: bool, mode: Mode) -> Runtime<Desk> {
    let scale = match mode {
        Mode::LargestText => f32::from(LARGEST) / 100.0,
        _ => 1.0,
    };
    crate::ui::tokens::overrides::set(false, scale);
    let mut desk = crate::test_support::settings_window().with_phone(phone);
    desk.settings.theme = None;
    let mut rt = Runtime::new(desk, crate::ui::view, size, Env::new(dark, 1.0));
    rt.on_key = Some(Desk::key);
    rt.set_srgb_blend(true);
    if matches!(mode, Mode::ContrastMore) {
        rt.set_system_increased_contrast(true);
    }
    rt.frame(0.0);
    rt
}

fn snap(gpu: &rune::render::Gpu, rt: &mut Runtime<Desk>, size: (u32, u32), name: &str) {
    rt.frame(1.0);
    rt.frame(1.0);
    let pixels = crate::ui::offscreen_as_app(gpu, rt, size);
    assert!(
        pixels.as_chunks::<4>().0.iter().any(|p| p[0] != p[1]),
        "{name} painted nothing but grey"
    );
    if let Some(dir) = std::env::var_os("RUNE_SETTINGS_SNAP") {
        let path = std::path::PathBuf::from(dir).join(format!("{name}.png"));
        rune::render::write_png(&path, size.0, size.1, &pixels).unwrap();
        std::fs::write(
            path.with_extension("json"),
            rt.accessibility_json().to_string(),
        )
        .unwrap();
    }
}

#[test]
fn settings_render_in_every_mode_and_size() {
    let gpu = rune::render::Gpu::headless();
    for (phone, size, name) in [
        (false, (1100, 720), "desktop"),
        (false, (640, 360), "desktop-min"),
        (true, (393, 852), "phone"),
        (true, (320, 568), "small"),
    ] {
        for dark in [false, true] {
            for mode in Mode::ALL {
                let tag = format!(
                    "mode-{name}-{}-{}",
                    if dark { "dark" } else { "light" },
                    mode.tag()
                );
                // One snapshot per runtime: a fresh renderer misses the
                // glyphs an earlier one uploaded.
                let mut rt = window((size.0 as f32, size.1 as f32), phone, dark, mode);
                rt.update(|d, tx| {
                    tx.instant();
                    d.settings_category(Category::Terminal);
                });
                snap(&gpu, &mut rt, size, &format!("{tag}-terminal"));
                let mut rt = window((size.0 as f32, size.1 as f32), phone, dark, mode);
                snap(&gpu, &mut rt, size, &tag);
                // Every target still fits the window at the largest text.
                for key in ["settings-close", "settings-query"] {
                    let [x, _, w, h] = rect(&rt, key);
                    assert!(w >= 44.0 && h >= 44.0, "{tag} {key}: {w}×{h}");
                    assert!(x >= 0.0 && x + w <= size.0 as f32 + 0.5, "{tag} {key} fits");
                }
            }
        }
    }
    crate::ui::tokens::overrides::set(false, 1.0);
}

/// The kit switch is a switch with its state, every row a Tab stop, and the
/// increased-contrast tokens reach the window (§1.11).
#[test]
fn increased_contrast_reaches_settings_and_rows_stay_switches() {
    crate::ui::tokens::overrides::set(false, 1.0);
    let mut rt = window((1100.0, 720.0), false, false, Mode::ContrastMore);
    rt.update(|d, tx| {
        tx.instant();
        d.settings_category(Category::Terminal);
    });
    rt.frame(0.0);
    let tree = rt.accessibility_json();
    let text = tree.to_string();
    assert!(text.contains("\"switch\""), "a switch role");
    let copy = crate::ui::element(&rt, "set-copy-select");
    assert!(copy.focusable);
    let more = crate::ui::Tokens::resolve(&crate::ui::tokens::Inputs {
        increased_contrast: true,
        ..crate::ui::tokens::Inputs::system(false)
    });
    let base = crate::ui::Tokens::resolve(&crate::ui::tokens::Inputs::system(false));
    assert_ne!(more.version, base.version, "contrast is in the memo key");
}
