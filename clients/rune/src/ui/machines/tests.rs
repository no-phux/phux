//! The machines sheet's semantics: swatches are a radio group of focusable
//! radios, rows and their actions are Tab stops.

use rune::{Env, Runtime};

use crate::desk::Desk;
use crate::machines::LOCAL;

mod colour;

fn sheet(dark: bool) -> Runtime<Desk> {
    let mut desk = crate::test_support::window();
    desk.machines.open = true;
    desk.machines.picking = Some(LOCAL.to_owned());
    let mut rt =
        crate::test_support::runtime(desk, crate::ui::view, (1100.0, 720.0), Env::new(dark, 1.0));
    rt.set_srgb_blend(true);
    rt.frame(0.0);
    rt.frame(1.0);
    rt
}

#[test]
fn swatches_are_a_radio_group_of_focusable_radios() {
    let rt = sheet(false);
    let swatch = format!(
        "machine-swatch-{LOCAL}-{}",
        phux_config_core::host_color::PALETTE[0].0
    );
    for key in [
        swatch.as_str(),
        &format!("machine-swatch-{LOCAL}-auto"),
        &format!("machine-color-{LOCAL}"),
        &format!("machine-works-{LOCAL}"),
        &format!("machine-{LOCAL}"),
        "machine-add",
    ] {
        assert!(
            crate::ui::element(&rt, key).focusable,
            "{key} is a Tab stop"
        );
    }
    let json = rt.accessibility_json().to_string();
    assert!(
        json.contains("\"radio-group\""),
        "the swatches are one group"
    );
    assert!(json.contains("\"radio-button\""), "each swatch is a radio");
}

#[test]
fn the_sheet_paints_in_both_modes() {
    let gpu = rune::render::Gpu::headless();
    for dark in [false, true] {
        let mut rt = sheet(dark);
        let pixels = crate::ui::offscreen_as_app(&gpu, &mut rt, (1100, 720));
        assert!(pixels.as_chunks::<4>().0.iter().any(|p| p[0] != p[1]));
        if let Some(dir) = std::env::var_os("RUNE_SETTINGS_SNAP") {
            let name = format!("machines-{}.png", if dark { "dark" } else { "light" });
            let path = std::path::Path::new(&dir).join(name);
            rune::render::write_png(&path, 1100, 720, &pixels).unwrap();
        }
    }
}
