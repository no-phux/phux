use phux_design::Params;

use super::*;

#[test]
fn system_terminal_is_the_house_palette() {
    let dark = Theme::system_terminal(true).expect("dark installs the house terminal");
    let light = Theme::system_terminal(false).expect("so does light");
    let house = phux_design::tokens::TERMINAL;
    assert_eq!(
        dark.background,
        [house.dark.bg.r, house.dark.bg.g, house.dark.bg.b]
    );
    assert!(light.background.iter().all(|&c| c > 0xc0), "a light ground");
}

#[test]
fn ids_are_distinct_and_found() {
    let themes = themes();
    for (at, theme) in themes.iter().enumerate() {
        assert_eq!(find(theme.id), Some(theme));
        assert!(themes[at + 1..].iter().all(|t| t.id != theme.id));
    }
    assert_eq!(find("gone"), None);
    assert_eq!(themes[0].id, "phux", "the house terminal leads");
}

#[test]
fn stored_ids_survive_the_catalog() {
    assert_eq!(find("midnight").map(|t| t.id), Some("phux"));
    assert_eq!(find("catppuccin").map(|t| t.id), Some("catppuccin-mocha"));
    assert!(find("gruvbox-material").is_none(), "mobile's alone");
}

#[test]
fn every_theme_knows_its_mode() {
    let lut = Params::house().srgb_lut;
    for theme in themes() {
        let input = crate::ui::tokens::theme_input(theme);
        let lum = phux_design::solve::lum(input.bg, lut);
        assert_eq!(lum <= 0.18, theme.dark, "{}", theme.name);
    }
}

#[test]
fn any_accent_gets_a_readable_ink() {
    for hex in [0xffffff, 0x000000, 0x808080, 0xffcc00, 0x0a84ff] {
        let fill = color(hex);
        assert!(contrast(on_fill(fill), fill) >= 4.5, "{hex:06x}");
    }
}

#[test]
fn a_theme_reaches_the_engine_as_bytes() {
    let t = find("nord").unwrap().terminal();
    assert_eq!(t.background, [0x2e, 0x34, 0x40]);
    assert_eq!(t.ansi16[1], [0xbf, 0x61, 0x6a]);
}
