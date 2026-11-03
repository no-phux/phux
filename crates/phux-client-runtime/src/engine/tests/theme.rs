use super::*;

fn grey_theme(base: u8) -> TerminalTheme {
    TerminalTheme {
        ansi16: std::array::from_fn(|i| [base + u8::try_from(i).expect("16 entries"); 3]),
        foreground: [250; 3],
        background: [base; 3],
        cursor: [200; 3],
    }
}

/// ADR-0157: a theme set before attach colours the first frame, an
/// application's OSC 4 in the stream still wins for its entry, and a live
/// theme change republishes the visible terminal with the new palette.
#[test]
fn the_terminal_theme_colours_published_frames_and_follows_live_changes() {
    let (owner, publication) = owner();
    owner.set_terminal_theme(Some(grey_theme(100)));
    let terminal = id(1);
    attach(&owner, &terminal, b"\x1b]4;3;rgb:03/03/03\x1b\\themed");
    let first = publication.acquire(&terminal).expect("published");
    let rgb = |c: crate::publication::Rgb| [c.r, c.g, c.b];
    assert_eq!(rgb(first.colors.palette[1]), [101; 3]);
    assert_eq!(rgb(first.colors.palette[3]), [3; 3], "the app's OSC 4 wins");
    assert_eq!(rgb(first.colors.background), [100; 3]);
    assert!(first.colors.has_background);

    owner.set_terminal_theme(Some(grey_theme(40)));
    let second = publication.acquire(&terminal).expect("republished");
    assert!(second.generation > first.generation);
    assert_eq!(rgb(second.colors.palette[1]), [41; 3]);
    assert_eq!(rgb(second.colors.palette[3]), [3; 3]);
    assert_eq!(rgb(second.colors.background), [40; 3]);

    owner.set_terminal_theme(None);
    let cleared = publication.acquire(&terminal).expect("republished");
    assert!(
        !cleared.colors.has_background,
        "clearing hands defaults back to the renderer"
    );
    assert_eq!(
        rgb(cleared.colors.background),
        [0; 3],
        "not the last theme's"
    );
    assert_eq!(rgb(cleared.colors.foreground), [255; 3]);
}
