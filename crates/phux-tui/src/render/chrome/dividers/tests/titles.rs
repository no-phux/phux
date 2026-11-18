//! Pane titles inset into the rail: clipping, badges, origins, and hostile text.

use super::*;

/// A title is inset into the rule above its pane, accented when focused;
/// an empty title draws nothing (no invented names).
#[test]
fn titles_inset_into_the_rail() {
    let content = railed(80, 24);
    let layout = split_layout(content);
    let s = render(&layout, content, Some("editor"));
    let cells: HashMap<(u16, u16), String> = painted_cells(&s)
        .into_iter()
        .map(|(x, y, sym)| ((x, y), sym))
        .collect();
    let expect = [(0, "─"), (1, " "), (2, "e"), (8, " ")];
    for (x, sym) in expect {
        assert_eq!(cells.get(&(x, 0)).map(String::as_str), Some(sym), "col {x}");
    }
    assert!(
        s.contains(&sgr_fg(theme().pane_title_focus)) && s.contains(&sgr_fg(theme().pane_title))
    );
    assert_eq!(
        render(&layout, content, Some("")),
        render(&layout, content, None)
    );
}

/// Long and double-width titles clip by display cells to their pane.
#[test]
fn long_titles_clip_by_display_width() {
    let content = railed(24, 6);
    let layout = lone_layout(content);
    for title in [
        "a-very-long-pane-title-indeed",
        "編集器編集器編集器編集器編集器",
    ] {
        let s = render(&layout, content, Some(title));
        assert!(rail_extent(&s, content) < 24, "{title}");
    }
    assert!(render(&layout, content, Some("a-very-long-pane-title-indeed")).contains(ELLIPSIS));
}

/// Badges: an asking pane badges in the attention tone; a satellite pane
/// badges its host and greys (dropping the focus colour) when down; the
/// agent glyph matches the sidebar vocabulary.
#[test]
fn pane_badges() {
    let content = railed(80, 24);
    let layout = split_layout(content);
    let th = theme();
    let s = render_label(
        &layout,
        content,
        Some(PaneLabel {
            attention: true,
            ..label("claude")
        }),
    );
    assert!(
        s.contains('●') && s.contains(&sgr_fg(th.attention)),
        "{s:?}"
    );

    let sat = |unreachable| {
        render_label(
            &layout,
            content,
            Some(PaneLabel {
                host: Some("devbox"),
                unreachable,
                ..label("shell")
            }),
        )
    };
    let up = sat(false);
    assert!(up.contains("devbox") && up.contains("shell"));
    assert!(up.contains(&sgr_fg(th.chord)) && up.contains(&sgr_fg(th.divider_focus)));
    let down = sat(true);
    assert!(down.contains("devbox") && !down.contains(&sgr_fg(th.divider_focus)));
    assert!(down.contains(&sgr_fg(th.divider)) && !down.contains(&sgr_fg(th.chord)));

    for state in [
        AgentMetaState::Working,
        AgentMetaState::Blocked,
        AgentMetaState::Done,
        AgentMetaState::Idle,
    ] {
        let badge = PaneLabel {
            agent: Some(state),
            ..label("claude")
        }
        .badge(&th)
        .unwrap();
        assert_eq!(badge, agent_badge(&th, state, false, true));
    }
}

/// The transient origin badge: `[ada]` sits ahead of the title.
/// The glyphs stay put when it dims; only the colour and weight change.
#[test]
fn input_origin_badge_dims_in_place() {
    let content = railed(48, 8);
    let layout = lone_layout(content);
    let paint = |dim| {
        render_label(
            &layout,
            content,
            Some(PaneLabel {
                origin: Some(OriginBadge { text: "ada", dim }),
                ..label("shell")
            }),
        )
    };
    let row = |s: &str| {
        let y = rail_row(content).unwrap();
        let mut cells: Vec<_> = painted_cells(s)
            .into_iter()
            .filter(|(_, cy, _)| *cy == y)
            .collect();
        cells.sort_by_key(|(x, _, _)| *x);
        let mut glyphs = vec![' '; usize::from(content.w)];
        for (x, _, sym) in cells {
            if let Some(ch) = sym.chars().next()
                && let Some(cell) = glyphs.get_mut(usize::from(x))
            {
                *cell = ch;
            }
        }
        let text: String = glyphs.into_iter().collect();
        text.trim_end().to_owned()
    };
    let bright = paint(false);
    let dimmed = paint(true);
    let bright_row = row(&bright);
    let dim_row = row(&dimmed);
    assert!(bright_row.contains("[ada] shell"), "{bright_row}");
    assert_eq!(bright_row, dim_row);
    assert!(bright.contains(&sgr_fg(theme().chord)) && bright.contains("\x1b[1m"));
    assert!(dimmed.contains(&sgr_fg(theme().dim)) && !dimmed.contains("\x1b[1m"));
}

/// A same-styled run costs one CUP; a wide glyph's trailing half is left
/// unpainted and the next cell re-anchors with its own CUP, as does the
/// cell after a badge. The badge reservation equals its written width.
#[test]
fn runs_reanchor_where_widths_could_drift() {
    let content = railed(80, 6);
    let s = render(&lone_layout(content), content, None);
    assert_eq!(extract_cups(&s).len(), 1, "an unbroken rail is one CUP");
    assert!(s.len() < 80 * 4, "{} bytes", s.len());

    let content = railed(40, 10);
    let layout = split_layout(content);
    let s = render(&layout, content, Some("日x"));
    let cells = painted_cells(&s);
    let wide = cells.iter().find(|(_, _, sym)| sym == "日").unwrap();
    let after = cells.iter().find(|(_, _, sym)| sym == "x").unwrap();
    assert_eq!(after.0, wide.0 + 2);
    assert!(
        !cells
            .iter()
            .any(|(x, y, _)| *x == wide.0 + 1 && *y == wide.1)
    );
    assert!(extract_cups(&s).contains(&(after.1 + 1, after.0 + 1)));

    let working = PaneLabel {
        agent: Some(AgentMetaState::Working),
        ..label("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    };
    let s = render_label(&layout, content, Some(working));
    let badge = painted_cells(&s)
        .into_iter()
        .find(|(_, _, sym)| sym == "◐")
        .unwrap();
    assert!(extract_cups(&s).contains(&(badge.1 + 1, badge.0 + 2)));
    assert_chrome_only(&layout, content, &s);
}

/// Zero-width marks ride with their base and cost no budget (breaking at
/// one once silently truncated titles).
#[test]
fn zero_width_marks_share_their_base_cell() {
    let content = railed(60, 10);
    let layout = split_layout(content);
    for (title, tail) in [
        ("cafe\u{301}/src", "/src"),
        ("\u{2705}\u{fe0f} build", "build"),
    ] {
        let s = render(&layout, content, Some(title));
        assert!(
            s.contains(tail) && !s.contains(ELLIPSIS),
            "{title:?}: {s:?}"
        );
    }
    let cells = painted_cells(&render(&layout, content, Some("e\u{301}x")));
    let base = cells
        .iter()
        .find(|(_, _, sym)| sym.starts_with('e'))
        .unwrap();
    assert_eq!(base.2, "e\u{301}");
    assert_eq!(
        cells.iter().find(|(_, _, sym)| sym == "x").unwrap().0,
        base.0 + 1
    );
}

/// A title is untrusted OSC-2 input on a VT path: no control byte or
/// bidi override may reach the wire (a pane could inject escapes or make
/// its label read as a neighbour's), and an override costs no budget.
#[test]
fn titles_cannot_inject_controls_or_bidi_overrides() {
    let content = railed(60, 10);
    let layout = split_layout(content);
    let s = render(&layout, content, Some("a\u{1b}]0;pwned\u{7}b\u{9b}c"));
    assert!(
        s.split('\u{1b}').skip(1).all(|seq| seq.starts_with('[')),
        "{s:?}"
    );
    assert!(s.chars().all(|c| c == '\u{1b}' || !c.is_control()), "{s:?}");
    assert!(s.contains("a]0;pwnedbc"));

    let s = render(
        &layout,
        content,
        Some("run\u{202e}gpj.exe\u{202c} \u{2066}x\u{2069}\u{200f}"),
    );
    for bad in [
        '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}', '\u{2066}', '\u{2067}',
        '\u{2068}', '\u{2069}', '\u{200e}', '\u{200f}', '\u{061c}',
    ] {
        assert!(!s.contains(bad), "U+{:04X}", bad as u32);
    }
    assert!(s.contains("gpj.exe"));
    assert_eq!(
        render(&layout, content, Some("abcdef")),
        render(&layout, content, Some("abc\u{202e}def"))
    );
}

/// A zero-height leaf shares its row with the leaf below; labelling it
/// would make the surviving title depend on `HashMap` order.
#[test]
fn a_zero_height_leaf_is_not_labelled() {
    let content = railed(40, 6);
    let mut layout = split_layout(content);
    let one = layout.rects[&t(1)];
    layout.rects.insert(t(2), Rect { h: 0, ..one });
    let a = render(&layout, content, Some("Pane"));
    assert_eq!(a, render(&layout, content, Some("Pane")));
    let titles = painted_cells(&a)
        .iter()
        .filter(|(_, y, sym)| *y == 0 && sym == "P")
        .count();
    assert_eq!(titles, 1);
}

/// A pane among a slot's tabs shows its place, `2/3`, ahead of its title.
#[test]
fn a_slot_tab_shows_its_place_ahead_of_the_title() {
    let content = railed(80, 24);
    let layout = split_layout(content);
    let s = render_label(
        &layout,
        content,
        Some(PaneLabel {
            slot_tab: Some((2, 3)),
            ..label("editor")
        }),
    );
    let tab = s.find("2/3").expect("the tab place");
    assert!(tab < s.find("editor").expect("the title"), "{s:?}");
    assert!(!render_label(&layout, content, Some(label("editor"))).contains("2/3"));
}
