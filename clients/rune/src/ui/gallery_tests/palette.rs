//! The command palette through the real renderer, over the gallery's
//! populated server (agents working, blocked and done): its empty list, a
//! typed query, no match, and the Themes and Sessions modes, on two
//! desktop windows and both phone widths, dark and light, and on the phone
//! with its keyboard up and at the largest text. `RUNE_PALETTE_SNAP=DIR`
//! writes each frame as `DIR/NAME.png` with its geometry and accessibility
//! tree beside it as `DIR/NAME.json`.

use rune::Runtime;

use super::{Locale, fixture, settle, window_on};
use crate::desk::Desk;
use crate::keys::Shortcut;

const SCALE: f32 = 2.0;

/// Each window: its size and whether it is the phone form.
const WINDOWS: [((f32, f32), bool); 4] = [
    ((1100.0, 720.0), false),
    ((640.0, 360.0), false),
    ((393.0, 852.0), true),
    ((320.0, 568.0), true),
];

/// Each state the palette is painted in.
const STATES: [&str; 5] = ["empty", "typed", "no-match", "themes", "sessions"];

fn arrange(d: &mut Desk, state: &str, tx: &mut rune::Tx) {
    match state {
        "themes" => d.shortcut(Shortcut::ChangeTheme, tx),
        "sessions" => d.shortcut(Shortcut::SwitchSession, tx),
        _ => d.toggle_palette(),
    }
    let query = match state {
        "typed" => "re",
        "no-match" => "deploy staging",
        _ => return,
    };
    d.palette_query.replace_text(query);
    d.palette_edited();
}

/// The palette in `state`, open and settled, the phone's keyboard down
/// unless `keyboard`.
pub(super) fn palette_window(
    size: (f32, f32),
    phone: bool,
    dark: bool,
    state: &str,
    keyboard: bool,
) -> Runtime<Desk> {
    let mut rt = window_on(size, dark, None, Locale::En, phone);
    rt.set_system_reduced_motion(true);
    settle(&mut rt);
    let state = state.to_owned();
    rt.update(move |d, tx| {
        if let Some(phone) = d.phone.as_mut() {
            phone.keyboard = keyboard;
        }
        arrange(d, &state, tx);
    });
    settle(&mut rt);
    rt
}

fn snap(gpu: &rune::render::Gpu, rt: &mut Runtime<Desk>, size: (f32, f32), name: &str) {
    rt.set_srgb_blend(true);
    settle(rt);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a fixed size"
    )]
    let px = ((size.0 * SCALE) as u32, (size.1 * SCALE) as u32);
    let pixels = crate::ui::offscreen_as_app(gpu, rt, px);
    assert!(
        pixels.as_chunks::<4>().0.iter().any(|p| p[0] != p[1]),
        "{name} painted nothing but grey"
    );
    if let Some(dir) = std::env::var_os("RUNE_PALETTE_SNAP") {
        let dir = std::path::Path::new(&dir);
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(format!("{name}.png"));
        crate::test_support::visual::write(&path, rt, px, &pixels, name);
    }
}

#[test]
fn palette_visual_review_frames() {
    let gpu = rune::render::Gpu::headless();
    for (size, phone) in WINDOWS {
        let form = if phone { "phone" } else { "desktop" };
        let tag = format!("{form}-{}x{}", size.0, size.1);
        for dark in [true, false] {
            let tone = if dark { "dark" } else { "light" };
            for state in STATES {
                let mut rt = palette_window(size, phone, dark, state, false);
                snap(&gpu, &mut rt, size, &format!("{tag}-{state}-{tone}"));
            }
            if !phone {
                continue;
            }
            for state in ["empty", "typed"] {
                let mut rt = palette_window(size, phone, dark, state, true);
                snap(
                    &gpu,
                    &mut rt,
                    size,
                    &format!("{tag}-{state}-keyboard-{tone}"),
                );
                let mut rt = palette_window(size, phone, dark, state, false);
                rt.update(|d, _| {
                    d.set_text_scale(fixture::LARGEST_TEXT);
                });
                snap(
                    &gpu,
                    &mut rt,
                    size,
                    &format!("{tag}-{state}-largest-{tone}"),
                );
            }
        }
    }
}
