use super::*;

#[test]
fn glass_ink_tracks_its_terminal_ground_not_the_os_appearance() {
    for (ground, expected) in [(Color::BLACK, Color::WHITE), (Color::WHITE, Color::BLACK)] {
        let ink = ground_ink(ground, Color::WHITE, None);
        assert_eq!(
            crate::theme::on_fill(ink.text),
            crate::theme::on_fill(expected)
        );
        assert!(crate::theme::contrast(ink.text, ground) >= 4.5);
    }
}

#[test]
fn a_phone_takes_its_sheets_from_the_theme_or_the_ground_never_the_system() {
    let mut desk = Desk::settings_test_fixture().with_phone(true);
    desk.settings.theme = None;
    desk.settings_open = false;
    // No terminal yet: the black one starts on, so a light-mode phone
    // does not open on a white page between a dark launch and terminal.
    assert_eq!(terminal_ground(&desk), Color::BLACK);
    assert_eq!(edge_color(&desk), Color::BLACK);
    let sheets = ink(&desk, Color::WHITE);
    assert_eq!(crate::theme::on_fill(sheets.raised), Color::WHITE);
    let light = crate::theme::all().find(|theme| !theme.dark);
    desk.settings.theme = light;
    let sheets = ink(&desk, Color::WHITE);
    assert_eq!(crate::theme::on_fill(sheets.raised), Color::BLACK);
}

#[test]
fn the_active_tab_is_brought_whole_into_the_strip() {
    let mut desk = Desk::settings_test_fixture().with_phone(true);
    desk.settings_open = false;
    desk.tabs.list = (0..6u8)
        .map(|i| crate::desk::Tab {
            id: [i; 16],
            name: format!("a tab with a long name {i}"),
            resources: Vec::new(),
            active: i == 5,
        })
        .collect();
    let mut rt = rune::Runtime::new(
        desk,
        crate::ui::view,
        (393.0, 852.0),
        rune::Env::new(false, 1.0),
    );
    rt.frame(0.0);
    let strip = rt.presented("phone-tabs").expect("the strip").0;
    let before = rt.presented("phone-tab-5").expect("the last tab").0;
    assert!(
        before[0] + before[2] > strip[0] + strip[2],
        "six tabs overflow the strip"
    );
    rt.update(|d, tx| d.reveal_active_tab(tx));
    for frame in 1..120 {
        rt.frame(f64::from(frame) / 60.0);
    }
    let tab = rt.presented("phone-tab-5").expect("the last tab").0;
    assert!(
        tab[0] >= strip[0] && tab[0] + tab[2] <= strip[0] + strip[2] + 0.5,
        "the active tab spans {}..{}, the strip {}..{}",
        tab[0],
        tab[0] + tab[2],
        strip[0],
        strip[0] + strip[2]
    );
}
