use super::*;

#[test]
fn a_missing_font_family_falls_back_to_the_default() {
    assert_eq!(mono_family(" "), FAMILIES.1);
    assert_eq!(
        mono_family(" Menlo\" "),
        format!("\"Menlo\", {}", FAMILIES.1)
    );
    assert_eq!(
        mono_family("Berkeley Mono, ,Symbols Nerd Font"),
        format!("\"Berkeley Mono\", \"Symbols Nerd Font\", {}", FAMILIES.1)
    );
    let width = |mono: &str| {
        let _scope = rune::text::with_families(FAMILIES.0, mono);
        Metrics::new(DEFAULT_FONT_SIZE, 2.0).cell_w
    };
    assert_eq!(width(&mono_family("No Such Font")), width(FAMILIES.1));
}

#[test]
fn cells_land_on_device_pixels() {
    for scale in [1.0, 2.0, 3.0] {
        let m = Metrics::new(DEFAULT_FONT_SIZE, scale);
        let px = m.cell_w * scale;
        assert!(
            (px - px.round()).abs() < 1e-3,
            "{px} device px wide at {scale}x"
        );
        let px = m.cell_h * scale;
        assert!(
            (px - px.round()).abs() < 1e-3,
            "{px} device px tall at {scale}x"
        );
    }
}

#[test]
fn fitting_floors_and_never_reaches_zero() {
    let m = Metrics::new(DEFAULT_FONT_SIZE, 2.0);
    let (cols, rows) = m.fit([m.cell_w * 80.5, m.cell_h * 24.9]);
    assert_eq!((cols, rows), (80, 24));
    assert_eq!(m.fit([0.0, 0.0]), (1, 1));
}

#[test]
fn hit_testing_clamps_to_the_grid() {
    let m = Metrics::new(DEFAULT_FONT_SIZE, 2.0);
    assert_eq!(m.cell_at(-5.0, -5.0, 80, 24), (0, 0));
    assert_eq!(m.cell_at(m.cell_w * 3.5, m.cell_h * 2.5, 80, 24), (3, 2));
    assert_eq!(m.cell_at(1e6, 1e6, 80, 24), (79, 23));
}

#[test]
fn colors_decode_srgb() {
    let white = color(Rgb {
        r: 255,
        g: 255,
        b: 255,
    });
    assert!((white.r - 1.0).abs() < 1e-6);
    let mid = color(Rgb {
        r: 128,
        g: 128,
        b: 128,
    });
    assert!(mid.r > 0.2 && mid.r < 0.23, "linear {}", mid.r);
}
