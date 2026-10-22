//! Terminal theme application.

use super::*;

const fn grey(v: u8) -> RgbColor {
    RgbColor { r: v, g: v, b: v }
}

fn theme() -> TerminalTheme {
    TerminalTheme {
        ansi16: std::array::from_fn(|i| grey(100 + u8::try_from(i).expect("16 entries"))),
        foreground: grey(250),
        background: grey(10),
        cursor: grey(200),
    }
}

fn entry(terminal: &GhosttyTerminal<'_, '_>, index: u8) -> RgbColor {
    terminal
        .color_palette()
        .expect("palette")
        .get(PaletteIndex(index))
}

/// The theme is a default, not an override: an app's OSC 4 survives the
/// theme being (re)applied, OSC 104 reverts to the theme, RIS keeps it,
/// and clearing restores libghostty's built-ins.
#[test]
fn the_theme_is_a_default_apps_still_override() {
    let mut terminal = GhosttyTerminal::new(10, 3).expect("terminal");
    let builtin_red = entry(&terminal, 1);
    terminal.vt_write(b"\x1b]4;1;rgb:01/01/01\x1b\\");
    apply_terminal_theme(&mut terminal, Some(&theme())).expect("theme");
    assert_eq!(entry(&terminal, 1), grey(1), "OSC 4 override survives");
    assert_eq!(entry(&terminal, 2), grey(102));
    assert_eq!(terminal.fg_color().expect("fg"), Some(grey(250)));
    assert_eq!(terminal.bg_color().expect("bg"), Some(grey(10)));
    terminal.vt_write(b"\x1b]104;1\x1b\\");
    assert_eq!(
        entry(&terminal, 1),
        grey(101),
        "OSC 104 reverts to the theme"
    );
    terminal.vt_write(b"\x1bc");
    assert_eq!(entry(&terminal, 2), grey(102), "RIS keeps the theme");
    apply_terminal_theme(&mut terminal, None).expect("clear");
    assert_eq!(entry(&terminal, 1), builtin_red);
    assert_eq!(terminal.fg_color().expect("fg"), None);
}

/// A synthesized replica gets the adapter's theme when its bootstrap
/// finishes, after the bootstrap bytes, so the bytes' own OSC 4 still
/// wins over the theme for that entry.
#[test]
fn finishing_a_bootstrap_applies_the_adapter_theme() {
    let mut adapter = GhosttyAdapter::new(BootstrapLimits::default());
    adapter.set_theme(Some(theme()));
    let mut replica = adapter
        .start_replica(
            BootstrapStreamProfile::SynthesizedVtStateSync,
            CanonicalGeometry::new(10, 3).expect("geometry"),
        )
        .expect("replica");
    let mut effects = EngineEffectBuffer::new();
    adapter
        .apply_bootstrap_chunk(&mut replica, b"\x1b]4;3;rgb:03/03/03\x1b\\", &mut effects)
        .expect("chunk");
    adapter
        .finish_bootstrap(&mut replica, &mut effects)
        .expect("finish");
    let terminal = replica.terminal().expect("terminal");
    assert_eq!(entry(terminal, 3), grey(3));
    assert_eq!(entry(terminal, 4), grey(104));
    assert_eq!(terminal.bg_color().expect("bg"), Some(grey(10)));
}
