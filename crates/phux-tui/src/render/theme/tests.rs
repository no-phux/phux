use super::*;
use std::collections::BTreeMap;

use phux_design::Params;

mod contrast;
mod lint;

/// ADR-0101: the settings page's theme rows (also the vocabulary `phux
/// config check` validates `[theme]` against) are exactly the slots the
/// renderer reads, no more and no fewer.
#[test]
fn slot_specs_name_every_slot() {
    let mut theme = Theme::default();
    let mut seen = std::collections::BTreeSet::new();
    for spec in SLOT_SPECS {
        assert_eq!(spec.table(), "theme", "{}", spec.key);
        assert!(
            theme.slot_mut(spec.leaf()).is_some(),
            "SLOT_SPECS names `{}`, which Theme does not read",
            spec.key
        );
        assert!(seen.insert(spec.leaf()), "duplicate row {}", spec.key);
    }
    // Every colour field is a row: probe the field count through Debug,
    // which lists one `name: Color` pair per field before the `term`.
    let debug = format!("{:?}", Theme::default());
    let colours = debug.split(", term:").next().unwrap_or_default();
    let field_count = colours.matches(": ").count();
    assert_eq!(
        field_count,
        SLOT_SPECS.len(),
        "Theme has {field_count} colour fields but SLOT_SPECS has {} rows",
        SLOT_SPECS.len()
    );
}

/// `phux config check` judges a slot colour with
/// [`phux_config::theme_slots::is_color`]; it must accept exactly what
/// [`parse_color`] does, or check either cries wolf or misses a colour the
/// renderer drops.
#[test]
fn config_check_colour_syntax_agrees_with_the_renderer() {
    let names = [
        "reset",
        "black",
        "red",
        "green",
        "yellow",
        "blue",
        "magenta",
        "cyan",
        "gray",
        "grey",
        "silver",
        "white",
        "darkgray",
        "dark-grey",
        "Dark Gray",
        "lightred",
        "light_red",
        "LIGHT-RED",
        "brightred",
        "bright-blue",
        "brightblack",
        "lightblack",
        "brightwhite",
        "lightwhite",
        "lightgray",
        "light-silver",
        "lightgreen",
        "lightyellow",
        "lightmagenta",
        "lightcyan",
        "Cyan",
        " cyan ",
        "c-y-a-n",
    ];
    let junk = [
        "",
        " ",
        "teal",
        "orange",
        "darkred",
        "lightreset",
        "#",
        "#12345",
        "#1234567",
        "#gggggg",
        "#GGGGGG",
        "#AbCdEf",
        "#abcdef",
        "#+1+2+3",
        "123456",
        "0x12",
        "rgb(1,2,3)",
        "+5",
        "-1",
        "007",
        "1.5",
        "theme.accent",
        "#12 456",
        "#ééé",
    ];
    let numbers = (0..=300).map(|n| n.to_string());
    for spec in names
        .iter()
        .chain(&junk)
        .map(|s| (*s).to_owned())
        .chain(numbers)
    {
        assert_eq!(
            phux_config::theme_slots::is_color(&spec),
            parse_color(&spec).is_some(),
            "{spec:?}"
        );
    }
}

#[test]
fn color_to_string_round_trips_through_the_parser() {
    for color in [
        Color::Rgb(0x7a, 0xa2, 0xf7),
        Color::Indexed(12),
        Color::Reset,
        Color::Cyan,
        Color::LightRed,
    ] {
        let text = color_to_string(color);
        assert_eq!(
            Color::from_str(&text).expect("renders a parseable color"),
            color,
            "{text}"
        );
    }
}

fn cfg(pairs: &[(&str, &str)]) -> phux_config::ThemeCfg {
    let mut slots = BTreeMap::new();
    for (k, v) in pairs {
        slots.insert((*k).to_owned(), (*v).to_owned());
    }
    phux_config::ThemeCfg { slots }
}

/// The slot map is a system, not a bag of colours: slots
/// that share a token must stay equal, and the pairs the old palette
/// conflated (chord and working, done and focus) must stay apart.
#[test]
fn shared_register_slots_stay_in_step() {
    for t in [
        Theme::default(),
        Theme::house(&Outer {
            hinted_mode: Some(Mode::Light),
            ..Outer::default()
        }),
    ] {
        assert_eq!(t.title, t.text, "titles are body text");
        assert_eq!(t.chord, t.text, "a chord is body text, drawn bold");
        assert_eq!(t.section_header, t.dim);
        assert_eq!(t.pane_title, t.dim);
        assert_eq!(t.sidebar_section, t.border, "headers and rules: faint");
        assert_eq!(t.divider, t.border, "every rule is one material");
        assert_eq!(t.agent_blocked, t.attention, "needs you is one tone");
        assert_eq!(t.error, t.agent_failed, "an error is the failed tone");
        assert_eq!(t.divider_focus, t.accent);
        assert_eq!(t.pane_title_focus, t.accent);
        assert_eq!(t.action, t.accent);
        assert_ne!(t.chord, t.agent_working, "chords never read as working");
        assert_ne!(t.agent_done, t.accent, "done never reads as focus");
        assert_eq!(t.shadow, Color::Reset, "a sheet, not a window");
    }
}

/// Every settings-page slot is overridable in every color syntax (an
/// unknown-slot warning would otherwise silently eat one); other slots
/// keep their defaults, and unknown or unparseable entries change nothing.
#[test]
fn every_slot_is_overridable_and_bad_entries_are_ignored() {
    for spec in SLOT_SPECS {
        let t = Theme::from_cfg(&cfg(&[(spec.leaf(), "#123456")]), &[]);
        assert_eq!(
            t.slot(spec.leaf()),
            Some(Color::Rgb(0x12, 0x34, 0x56)),
            "{}",
            spec.key
        );
    }
    let t = Theme::from_cfg(
        &cfg(&[("accent", "magenta"), ("chord", "12"), ("surface", "reset")]),
        &[],
    );
    assert_eq!(
        (t.accent, t.chord, t.surface),
        (Color::Magenta, Color::Indexed(12), Color::Reset)
    );
    assert_eq!(t.dim, Theme::default().dim);
    for bad in [("not_a_slot", "red"), ("accent", "definitely-not-a-color")] {
        assert_eq!(
            Theme::from_cfg(&cfg(&[bad]), &[]),
            Theme::default(),
            "{bad:?}"
        );
    }
    assert_eq!(
        Theme::from_cfg(&phux_config::ThemeCfg::default(), &[]),
        Theme::default()
    );
}

const LIGHT_COLORS: &str = "mode = \"light\"\naccent = \"#1e66f5\"\nselection = \"#ccd0da\"\n\
    muted = \"#acb0be\"\nbackground = \"#eff1f5\"\n\
    lighter_background = \"#dce0e8\"\nforeground = \"#4c4f69\"\nbright_foreground = \"#4c4f69\"\n\
    red = \"#d20f39\"\nyellow = \"#df8e1d\"\ngreen = \"#40a02b\"\ncyan = \"#179299\"\n\
    blue = \"#1e66f5\"\nmagenta = \"#ea76cb\"\n";

/// A scratch theme directory holding `colors.toml`, removed on drop.
struct ThemeDir(std::path::PathBuf);

impl ThemeDir {
    fn new(tag: &str, colors: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "phux-tui-theme-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join("colors.toml"), colors).expect("write colors.toml");
        Self(dir)
    }

    fn file(&self) -> String {
        self.0.join("colors.toml").display().to_string()
    }
}

impl Drop for ThemeDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The solver's roles for the colours `path` holds.
fn roles_of(path: &str) -> phux_design::Roles {
    let colors = phux_config::theme::load_file(std::path::Path::new(path)).expect("loads");
    let rgb = |c: phux_config::theme::Rgb| Rgb8 {
        r: c.r,
        g: c.g,
        b: c.b,
    };
    let input = phux_design::Theme {
        bg: rgb(colors.background()),
        fg: rgb(colors.foreground()),
        ansi: colors.ansi16().map(rgb),
        cursor: Some(rgb(colors.cursor())),
        accent: Some(rgb(colors.accent())),
        selection_bg: Some(rgb(colors.selection())),
        dark: Some(colors.mode == ThemeMode::Dark),
    };
    phux_design::derive(&input, &Params::house())
}

const fn ink(c: Rgba8) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}

/// `[theme] file` selects a palette; the solver derives every slot from
/// it per the documented map, and explicit slot keys still win (ADR-0026
/// layering).
#[test]
fn a_selected_theme_maps_onto_the_slots_and_overrides_still_win() {
    let dir = ThemeDir::new("light", LIGHT_COLORS);
    let file = dir.file();
    let t = Theme::from_cfg(&cfg(&[("file", &file), ("error", "#000001")]), &[]);
    let roles = roles_of(&file);
    assert_eq!(roles.mode, Mode::Light);
    assert_eq!(t.accent, ink(roles.accent.accent_text));
    assert_eq!(t.surface, ink(roles.layer.sidebar));
    assert_eq!(t.text, ink(roles.text.text));
    assert_eq!(t.dim, ink(roles.text.dim));
    assert_eq!(t.border, ink(roles.text.faint));
    assert_eq!(t.attention, ink(roles.status.needs_you));
    assert_eq!(t.agent_working, ink(roles.status.working));
    assert_eq!(t.agent_done, ink(roles.status.done));
    assert_eq!(t.agent_failed, ink(roles.status.failed));
    assert_eq!(t.selection_bg, ink(roles.selection.bg));
    assert_eq!(t.selection_fg, ink(roles.selection.fg));
    assert_eq!(
        t.error,
        Color::Rgb(0, 0, 1),
        "an explicit slot beats the theme"
    );
    assert!(Theme::resolve(&cfg(&[("file", &file)]), &[]).is_ok());
}

/// A dark theme fills panels with the solver's sidebar step off its own
/// background; a selection that does not load is a warning for the
/// tolerant build and a refusal for the strict one.
#[test]
fn dark_surface_and_missing_theme_handling() {
    let dark = LIGHT_COLORS.replace("mode = \"light\"", "mode = \"dark\"");
    let dir = ThemeDir::new("dark", &dark);
    let t = Theme::from_cfg(&cfg(&[("file", &dir.file())]), &[]);
    let roles = roles_of(&dir.file());
    assert_eq!(roles.mode, Mode::Dark);
    assert_eq!(t.surface, ink(roles.layer.sidebar));
    assert_ne!(t.surface, ink(roles.layer.ground), "a panel is a step off");

    let missing = cfg(&[("name", "no-such-theme-xyz"), ("accent", "#123456")]);
    let tolerant = Theme::from_cfg(&missing, &[]);
    assert_eq!(tolerant.accent, Color::Rgb(0x12, 0x34, 0x56));
    assert_eq!(tolerant.dim, Theme::default().dim);
    assert!(Theme::resolve(&missing, &[]).is_err());
}

/// Omarchy's Vantablack: every colour a grey, the accent a mid grey.
const VANTABLACK: &str = "mode = \"dark\"\naccent = \"#8d8d8d\"\nselection = \"#1a1a1a\"\n\
    muted = \"#7a7a7a\"\nbackground = \"#000000\"\nlighter_background = \"#1a1a1a\"\n\
    foreground = \"#ffffff\"\nbright_foreground = \"#ffffff\"\nlight_foreground = \"#ececec\"\n\
    red = \"#a4a4a4\"\nyellow = \"#cecece\"\norange = \"#b9b9b9\"\ngreen = \"#b6b6b6\"\n\
    cyan = \"#b0b0b0\"\nblue = \"#8d8d8d\"\nmagenta = \"#9b9b9b\"\nbrown = \"#5c5c5c\"\n";

/// A grey accent can only stand out by lightness. When it solves quieter
/// than unfocused labels, focus takes body text, so the focused pane is
/// never the dimmest one; a hued accent is left alone.
#[test]
fn a_grey_accent_never_makes_focus_the_quietest_label() {
    let dir = ThemeDir::new("vantablack", VANTABLACK);
    let roles = roles_of(&dir.file());
    assert!(
        roles.recedes(roles.accent.accent_text),
        "the fixture still reproduces the inversion"
    );
    let t = Theme::from_cfg(&cfg(&[("file", &dir.file())]), &[]);
    assert_eq!(t.pane_title_focus, ink(roles.text.text));
    assert_eq!(t.divider_focus, ink(roles.text.text));
    assert_eq!(t.pane_title, ink(roles.text.dim));

    let hued = ThemeDir::new("hued", LIGHT_COLORS);
    let t = Theme::from_cfg(&cfg(&[("file", &hued.file())]), &[]);
    assert_eq!(t.accent, ink(roles_of(&hued.file()).accent.accent_text));
}
