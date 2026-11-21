use phux_design::Params;

use super::*;

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
