//! Config diagnostics/reload and prefix-popup timing and modal isolation.

use super::chrome_frames::palette_overlay;
use super::*;

/// One malformed chord disables only itself; detach survives.
#[test]
fn attach_resolver_survives_one_bad_chord_and_keeps_detach() {
    let cfg = phux_config::parse_str(
        "[keybindings.prefix-table]\n\"q-\" = \"kill-pane\"\nd = \"detach\"\n",
        Path::new("test.toml"),
    )
    .expect("parses");
    let (mut resolver, diags) = build_resolver_from(&cfg.keybindings);
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].binding, "q-");
    let prefix = phux_config::keybind::parse_chord(&cfg.keybindings.prefix).expect("prefix");
    assert_eq!(resolver.feed(prefix), phux_config::keybind::Feed::Partial);
    match resolver.feed(phux_config::keybind::parse_chord("d").expect("chord")) {
        phux_config::keybind::Feed::Resolved(ra) => assert_eq!(ra.action, "detach"),
        other => panic!("detach must survive one bad chord, got {other:?}"),
    }
}

/// The error lines name the first bad chord, count the rest, and point at
/// `phux config check` (the diagnosing verb, not `config show`).
#[test]
fn config_error_lines_name_the_problem_and_point_at_config_check() {
    let diags = |toml: &str| {
        let cfg = phux_config::parse_str(toml, Path::new("test.toml")).expect("parses");
        build_resolver_from(&cfg.keybindings).1
    };
    let one = keybind_error_line(&diags(
        "[keybindings.prefix-table]\n\"q-\" = \"kill-pane\"\n",
    ));
    assert!(
        one.contains("\"q-\"") && one.contains("run: phux config check"),
        "{one}"
    );
    assert!(!one.contains("more;"), "{one}");
    let three = keybind_error_line(&diags(
        "[keybindings.prefix-table]\n\"q-\" = \"kill-pane\"\n\"w-\" = \"kill-pane\"\n\"e-\" = \"kill-pane\"\n",
    ));
    assert!(
        three.contains("\"e-\"") && three.contains("+2 more; run: phux config check"),
        "{three}"
    );
    assert_eq!(keybind_error_line(&[]), "");

    let line = config_error_line(&"boom");
    assert!(
        line.contains("config error: boom") && line.contains("phux config check"),
        "{line}"
    );
    assert!(!line.contains("config show"), "{line}");
}

/// A resolver from the shipped defaults, walked to the pending-prefix state.
fn pending_resolver() -> phux_config::keybind::Resolver {
    let cfg = default_cfg();
    let mut r = phux_config::keybind::Resolver::new(&cfg.keybindings).expect("resolver");
    let prefix = phux_config::keybind::parse_chord(&cfg.keybindings.prefix).expect("prefix");
    assert_eq!(r.feed(prefix), phux_config::keybind::Feed::Partial);
    r
}

/// The which-key deadline arms once at `now + delay` and keeps that anchor;
/// an early chord, a disabled config, or an overlay disarms it.
#[test]
fn which_key_deadline_arms_once_and_disarms_on_resolve_disable_or_overlay() {
    let now = tokio::time::Instant::now();
    let delay = Duration::from_millis(600);
    let mut deadline = None;
    update_which_key_deadline(&mut deadline, true, true, false, now, delay);
    assert_eq!(deadline, Some(now + delay));
    update_which_key_deadline(&mut deadline, true, true, false, now + delay / 2, delay);
    assert_eq!(deadline, Some(now + delay), "anchor survives re-passes");
    update_which_key_deadline(&mut deadline, false, true, false, now, delay);
    assert_eq!(deadline, None, "an early chord suppresses the popup");

    update_which_key_deadline(&mut deadline, true, false, false, now, delay);
    assert_eq!(deadline, None, "disabled in config");
    update_which_key_deadline(&mut deadline, true, true, true, now, delay);
    assert_eq!(deadline, None, "a modal owns input");
    update_which_key_deadline(&mut deadline, true, true, false, now, delay);
    update_which_key_deadline(&mut deadline, true, true, true, now, delay);
    assert_eq!(deadline, None, "an overlay appearing disarms");
}

/// The timeout pushes a passthrough popup and leaves the prefix pending; it
/// declines with no pending prefix or over a modal.
#[test]
fn which_key_timeout_pushes_a_passthrough_popup_only_when_pending() {
    let cfg = default_cfg();
    let theme = crate::render::Theme::default();
    let resolver = pending_resolver();
    let mut overlays = OverlayState::new();
    assert!(push_which_key_overlay(
        &mut overlays,
        &resolver,
        &cfg.keybindings,
        &theme
    ));
    assert!(
        overlays.top_is_passthrough(),
        "the popup can never eat a chord"
    );
    assert!(resolver.pending_at_prefix(), "the pending prefix survives");

    let idle = phux_config::keybind::Resolver::new(&cfg.keybindings).expect("resolver");
    let mut overlays = OverlayState::new();
    assert!(!push_which_key_overlay(
        &mut overlays,
        &idle,
        &cfg.keybindings,
        &theme
    ));
    assert!(!overlays.is_active());

    let mut overlays = OverlayState::new();
    overlays.push(palette_overlay());
    assert!(!push_which_key_overlay(
        &mut overlays,
        &resolver,
        &cfg.keybindings,
        &theme
    ));
    assert_eq!(overlays.depth(), 1, "nothing stacked on the modal");
}

/// A config reload is never half-applied: a malformed file keeps the old
/// theme and sidebar painter and stacks one toast; a valid one swaps the theme
/// into the settings and a fresh sidebar painter and stacks nothing.
#[test]
fn config_reload_swaps_whole_or_keeps_the_old_config_and_toasts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    let defaults = phux_config::parse_with_defaults("", Path::new("test.toml")).expect("parses");
    let mut settings = crate::settings::TuiSettings::tolerant_from(&defaults);
    let old_theme = settings.theme;
    let mut overlays = OverlayState::new();
    let mut painter = SidebarPainter::new(old_theme);

    std::fs::write(&path, "this is [not valid toml").expect("write config");
    assert!(!adopt_config_reload(
        &mut settings,
        &mut overlays,
        &mut painter,
        &path
    ));
    assert_eq!(settings.theme, old_theme);
    assert_eq!(*painter.theme(), old_theme);
    assert_eq!(overlays.depth(), 1, "the failure is toasted");

    let mut overlays = OverlayState::new();
    std::fs::write(&path, "[theme]\naccent = \"#ff0000\"\n").expect("write config");
    assert!(adopt_config_reload(
        &mut settings,
        &mut overlays,
        &mut painter,
        &path
    ));
    let red = ratatui::style::Color::Rgb(0xff, 0, 0);
    assert_eq!(settings.theme.accent, red);
    assert_eq!(painter.theme().accent, red, "the painter is rebuilt");
    assert!(!overlays.is_active());
}
