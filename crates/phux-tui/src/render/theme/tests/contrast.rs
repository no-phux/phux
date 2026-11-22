//! The contrast floor at every depth, the 16-colour map, `NO_COLOR`, and the ground.

use super::*;

fn rgb_of(c: Color) -> Rgb8 {
    match c {
        Color::Rgb(r, g, b) => Rgb8 { r, g, b },
        Color::Indexed(i) => xterm_rgb(i),
        other => panic!("contrast is only defined for rgb or indexed slots: {other:?}"),
    }
}

fn contrast(a: Color, b: Color) -> f64 {
    phux_design::solve::contrast(rgb_of(a), rgb_of(b), Params::house().srgb_lut)
}

/// The text and rule slots: everything but the fills and the shadow.
fn text_slots(t: &Theme) -> Vec<(&'static str, Color)> {
    let mut copy = *t;
    copy.slots_mut()
        .into_iter()
        .filter(|(key, _)| !matches!(*key, "surface" | "selection_bg" | "shadow"))
        .filter(|(key, _)| *key != "selection_fg")
        .map(|(key, c)| (key, *c))
        .collect()
}

/// The house look in both modes, and every catalog theme through the same
/// solver: the themes a user is most likely to pick.
fn solved_themes(outer: &Outer) -> Vec<(String, Theme)> {
    let mut all = vec![
        ("house-dark".to_owned(), Theme::house(outer)),
        (
            "house-light".to_owned(),
            Theme::house(&Outer {
                hinted_mode: Some(Mode::Light),
                ..*outer
            }),
        ),
    ];
    for entry in &phux_design::catalog().themes {
        all.push((
            entry.id.to_owned(),
            Theme::from_input(&entry.input(), outer),
        ));
    }
    all
}

/// The accessibility floor: every text or rule slot clears WCAG AA (4.5:1)
/// against `surface`, and selection text against its bed, for the house
/// look and every catalog theme.
#[test]
fn contrast_floor_is_met() {
    for (id, t) in solved_themes(&Outer::default()) {
        for (name, slot) in text_slots(&t) {
            let ratio = contrast(slot, t.surface);
            assert!(
                ratio >= 4.5,
                "{id}: {name} is {ratio:.2}:1 against surface; the floor is 4.5:1"
            );
        }
        let ratio = contrast(t.selection_fg, t.selection_bg);
        assert!(ratio >= 4.5, "{id}: selection text is {ratio:.2}:1");
    }
}

/// The same floor after snapping to the 256 palette, against the snapped
/// surface: quantising never costs a slot its bar.
#[test]
fn the_256_palette_keeps_the_floor() {
    let outer = Outer {
        depth: Depth::Ansi256,
        ..Outer::default()
    };
    for (id, t) in solved_themes(&outer) {
        for (name, slot) in std::iter::once(("surface", t.surface)).chain(text_slots(&t)) {
            let Color::Indexed(i) = slot else {
                panic!("{id}: {name} is not on the palette: {slot:?}");
            };
            assert!(i >= 16, "{id}: {name} uses a user-remapped index {i}");
        }
        for (name, slot) in text_slots(&t) {
            let ratio = contrast(slot, t.surface);
            assert!(ratio >= 4.5, "{id}: {name} is {ratio:.2}:1 at 256 colours");
        }
        let ratio = contrast(t.selection_fg, t.selection_bg);
        assert!(
            ratio >= 4.5,
            "{id}: selection is {ratio:.2}:1 at 256 colours"
        );
    }
}

/// The three rungs stay ordered. Focus must read as brighter than
/// recessive text, which must read as brighter than the rules — a
/// retune that clears the floor by flattening the hierarchy would
/// otherwise pass `contrast_floor_is_met` and look wrong.
#[test]
fn the_recessive_rungs_stay_ordered() {
    for (id, t) in solved_themes(&Outer::default()).into_iter().take(2) {
        let bg = t.surface;
        let rules = contrast(t.divider, bg);
        let recessive = contrast(t.dim, bg);
        let focus = contrast(t.accent, bg);
        assert!(
            rules < recessive,
            "{id}: rules ({rules:.2}) must recede behind recessive text ({recessive:.2})"
        );
        assert!(
            recessive < focus,
            "{id}: recessive text ({recessive:.2}) must recede behind focus ({focus:.2})"
        );
    }
}

/// 16 colours: the semantic map, chosen per background, and
/// never ANSI 8 as ink on a dark ground.
#[test]
fn sixteen_colours_follow_the_semantic_map() {
    let dark_unknown = Theme::house(&Outer {
        depth: Depth::Ansi16,
        ..Outer::default()
    });
    let dark_known = Theme::house(&Outer {
        depth: Depth::Ansi16,
        background: Some(Rgb8::hex(0x00_2b36)),
        ..Outer::default()
    });
    let light = Theme::house(&Outer {
        depth: Depth::Ansi16,
        background: Some(Rgb8::hex(0xfd_f6e3)),
        ..Outer::default()
    });
    assert_eq!(dark_unknown.text, Color::Reset);
    assert_eq!(
        dark_unknown.dim,
        Color::Reset,
        "dim on an unknown dark ground"
    );
    assert_eq!(dark_known.dim, Color::Gray, "ANSI 7 on a known dark ground");
    assert_eq!(dark_known.border, Color::Gray);
    assert_eq!(light.border, Color::DarkGray, "ANSI 8 on light only");
    for t in [dark_unknown, dark_known, light] {
        assert_eq!(t.accent, Color::Cyan);
        assert_eq!(t.attention, Color::Yellow);
        assert_eq!(t.agent_failed, Color::Red);
        assert_eq!(t.agent_done, Color::Green);
        assert_eq!(t.surface, Color::Reset, "panels are the terminal's own");
        assert_eq!(t.ground_fill(), Color::Reset);
    }
    for t in [dark_unknown, dark_known] {
        assert_eq!(t.agent_working, Color::LightBlue);
        for (name, slot) in text_slots(&t) {
            assert_ne!(slot, Color::DarkGray, "{name}: ANSI 8 on a dark ground");
        }
    }
    assert_eq!(light.agent_working, Color::Blue);
}

/// `NO_COLOR`: every slot is the terminal's own, and a selection carries
/// itself with reverse video.
#[test]
fn no_color_is_glyph_word_and_bold_only() {
    let t = Theme::house(&Outer {
        depth: Depth::Mono,
        ..Outer::default()
    });
    let mut copy = t;
    for (name, slot) in copy.slots_mut() {
        assert_eq!(*slot, Color::Reset, "{name}");
    }
    assert!(
        t.on_selection(Style::default())
            .add_modifier
            .contains(Modifier::REVERSED)
    );
    assert!(
        !Theme::default()
            .on_selection(Style::default())
            .add_modifier
            .contains(Modifier::REVERSED)
    );
    assert_eq!(t.identity(Color::Rgb(1, 2, 3)), Color::Reset);
}

/// The ground chrome text sits on: solved against when the terminal
/// reported it (rules then sit on `Reset`), filled with `surface` when not.
#[test]
fn an_unknown_ground_fills_the_rules_and_a_reported_one_is_solved_against() {
    let unknown = Theme::default();
    assert!(!unknown.term.ground_known);
    assert_eq!(unknown.ground_fill(), unknown.surface);
    assert_eq!(
        unknown,
        Theme::from_roles(&Roles::house(Mode::Dark), &Outer::default(), false)
    );

    let bg = Rgb8::hex(0x00_2b36);
    let reported = Theme::house(&Outer {
        background: Some(bg),
        foreground: Some(Rgb8::hex(0x83_9496)),
        ..Outer::default()
    });
    assert!(reported.term.ground_known);
    assert_eq!(reported.ground_fill(), Color::Reset);
    let ground = Color::Rgb(bg.r, bg.g, bg.b);
    for (name, slot) in text_slots(&reported) {
        let ratio = contrast(slot, ground);
        assert!(
            ratio >= 4.5,
            "{name} is {ratio:.2}:1 on the reported ground"
        );
    }

    let light = Theme::house(&Outer {
        background: Some(Rgb8::hex(0xff_ffff)),
        ..Outer::default()
    });
    assert!(
        contrast(light.text, Color::Rgb(255, 255, 255)) >= 4.5,
        "a light report solves a light look"
    );

    // A selected theme counts its ground as known only when the terminal
    // reported that very background.
    let entry = phux_design::catalog().find("phux").expect("house entry");
    let matching = Outer {
        background: Some(entry.bg),
        ..Outer::default()
    };
    assert!(
        Theme::from_input(&entry.input(), &matching)
            .term
            .ground_known
    );
    assert!(
        !Theme::from_input(&entry.input(), &Outer::default())
            .term
            .ground_known
    );
}
