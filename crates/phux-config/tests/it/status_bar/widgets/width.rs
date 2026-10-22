//! The width contract: cells are columns.

use super::*;

// --- Width contract: cells are COLUMNS (phux-l96p.8 fix pass II)

/// Walk a composed row exactly as the emitter does, returning the
/// columns it advances the terminal.
///
/// The emitter derives "claimed" from POSITION, not from the cell: after
/// writing a symbol it advances by that symbol's display width, so a
/// double-width character consumes the cell after it and a genuinely
/// blank cell is written as one space. `Err(index)` means the walk ran
/// off the end of the row — an ORPHAN BASE, a cell the row counts as one
/// column and the terminal advances two for.
fn emitted_columns(cells: &[phux_config::widget::Cell]) -> Result<usize, usize> {
    let mut i = 0usize;
    let mut columns = 0usize;
    while i < cells.len() {
        let w = phux_config::widget::cell_columns(&cells[i]).max(1);
        if i + w > cells.len() {
            return Err(i);
        }
        columns += w;
        i += w;
    }
    Ok(columns)
}

/// The separator is user-configurable TEXT, so it can be double-width.
/// `windowed_width` measured it with `chars().count()` while the cells
/// it was predicting came from the width-aware `from_styled`, so every
/// gap under-counted by a column: the widget handed back more cells than
/// its budget, tripping its own `debug_assert` in debug and test builds,
/// and in release leaving the slot clamp to cut the overrun — possibly
/// through the middle of a double-width character.
#[test]
fn a_wide_tab_separator_never_overruns_the_budget() {
    let windows = vec![win("a", true), win("b", false), win("c", false)];
    let opts = [(
        "separator",
        // U+FF5C FULLWIDTH VERTICAL LINE: one char, two columns.
        toml::Value::String("\u{ff5c}".to_owned()),
    )];
    let widget = build_spec("windows", &opts).expect("windows builds");
    let ctx = WidgetContext::new(fixed_time(), "", "C-a", &windows);
    // The natural strip is 3 tabs (3 columns each) and 2 separators (2
    // columns each) = 13 columns; the old char count said 11.
    let natural = widget.render(&ctx);
    assert_eq!(natural.len(), 13);
    assert_eq!(emitted_columns(&natural.cells), Ok(13));
    for budget in 0..20 {
        let out = widget.render_within(&ctx, budget);
        assert!(
            out.len() <= budget,
            "budget {budget}: widget returned {} cells",
            out.len()
        );
        // And `len()` really is the column count, which is the contract
        // every consumer of the strip relies on.
        assert_eq!(
            emitted_columns(&out.cells),
            Ok(out.len()),
            "budget {budget}: cells and emitted columns disagree"
        );
    }
}

#[test]
fn hidden_tab_arrows_navigate_even_beside_a_long_active_label() {
    use phux_config::widget::CellHit;
    let windows = [
        win("before", false),
        win("a-long-active-window", true),
        win("after", false),
    ];
    let widget = build_spec("windows", &[]).expect("windows builds");
    let ctx = WidgetContext::new(fixed_time(), "", "C-a", &windows);
    let cells = widget.render_within(&ctx, 12);
    assert_eq!(text_of(&cells), "‹1:a-long-…›");
    assert_eq!(
        cells.cells.first().expect("left arrow").hit,
        Some(CellHit::Window(0))
    );
    assert_eq!(
        cells.cells.last().expect("right arrow").hit,
        Some(CellHit::Window(2))
    );
    assert!(
        cells.cells[1..cells.len() - 1]
            .iter()
            .all(|c| c.hit == Some(CellHit::Window(1)))
    );
    assert_eq!(emitted_columns(&cells.cells), Ok(cells.len()));
}

#[test]
fn tab_navigation_stays_in_bounds_at_every_width_and_active_position() {
    use phux_config::widget::CellHit;
    let widget = build_spec("windows", &[]).expect("windows builds");
    for active in 0..3 {
        let windows: Vec<_> = ["漢字-long-name", "short", "another-long-name"]
            .iter()
            .enumerate()
            .map(|(i, name)| win(name, i == active))
            .collect();
        let ctx = WidgetContext::new(fixed_time(), "", "C-a", &windows);
        for budget in 0..80 {
            let rendered = widget.render_within(&ctx, budget);
            assert!(rendered.len() <= budget);
            assert_eq!(emitted_columns(&rendered.cells), Ok(rendered.len()));
            for cell in &rendered.cells {
                if let Some(CellHit::Window(index)) = cell.hit {
                    assert!(index < windows.len());
                }
            }
            if budget > 0 {
                assert!(
                    rendered
                        .cells
                        .iter()
                        .any(|c| c.hit == Some(CellHit::Window(active)))
                );
            }
        }
    }
}

/// A composed row must advance the terminal exactly as many columns as
/// it claims. An ORPHAN BASE — a double-width character whose claimed
/// cell was cut away — is one cell the row counts and two the terminal
/// advances; in the right slot it lands on the last column, so the row
/// runs past the end and wraps into the pane grid below (ADR-0020).
///
/// Swept across widths so the boundary is hit exactly, and with a
/// `spacer` in the bar so the ADR-0087 slack path is exercised at those
/// boundaries too.
#[test]
fn a_composed_row_advances_exactly_its_own_width() {
    use phux_config::widget::{StatusBar, WidgetRegistry};
    use phux_config::{StatusCfg, Widget, WidgetSpec};

    let windows = vec![win("\u{65e5}\u{672c}", true), win("b", false)];
    let cfg = StatusCfg {
        left: vec![Widget::Spec(WidgetSpec {
            kind: "windows".to_owned(),
            opts: opts_with(&[("separator", toml::Value::String("\u{ff5c}".to_owned()))]),
        })],
        center: vec![Widget::Bare("spacer".to_owned())],
        right: vec![Widget::Bare("session-name".to_owned())],
        ..Default::default()
    };
    let bar = StatusBar::build(&cfg, &WidgetRegistry::with_builtins()).expect("bar builds");
    for width in 1usize..48 {
        let ctx = WidgetContext::new(
            fixed_time(),
            "\u{65e5}\u{672c}\u{8a9e}session",
            "C-a",
            &windows,
        );
        let row = bar.render(&ctx, u16::try_from(width).unwrap());
        assert_eq!(row.len(), width, "width {width}: wrong cell count");
        assert_eq!(
            emitted_columns(&row),
            Ok(width),
            "width {width}: the row does not advance exactly its own width"
        );
    }
}

/// `clip` stamps its ellipsis on a BASE cell, never on a claimed one.
/// A claimed cell is skipped at emit — the terminal advanced past it
/// when it drew the base — so an ellipsis parked there is never drawn,
/// and a clipped strip silently loses the mark that says it was clipped.
#[test]
fn a_clip_marks_the_cut_even_through_a_wide_character() {
    // [a][\u{65e5}][claimed][b] — four cells, four columns.
    let full = WidgetCells::from_text("a\u{65e5}b");
    assert_eq!(full.len(), 4);
    for budget in 1..=4 {
        let out = full.clone().clipped(budget);
        assert!(out.len() <= budget, "budget {budget} overrun");
        assert_eq!(
            emitted_columns(&out.cells),
            Ok(out.len()),
            "budget {budget}: a clip left a base whose claimed cell was cut away"
        );
        if budget < 4 {
            assert!(
                out.cells
                    .iter()
                    .any(|c| c.text.first() == Some(&phux_config::widget::ELLIPSIS)),
                "budget {budget}: a clipped strip must carry its cut marker"
            );
        }
    }
    // The cut that lands between the wide base and its claimed cell
    // takes the whole character and marks the cut on the cell before it.
    assert_eq!(text_of(&full.clone().clipped(3)), "a\u{2026}");
    // A strip that fits is untouched, ellipsis included.
    assert_eq!(text_of(&full.clipped(4)), "a\u{65e5}b");
}

/// The slot clamp is the row's last line of defence — its stated job is
/// that "a third-party widget cannot corrupt the row geometry" — so it
/// has to survive a widget that both overruns AND ends on a double-width
/// character. Cutting between that character's base and the cell it
/// claimed leaves an ORPHAN BASE: one cell the row counts and two
/// columns the terminal advances. In the RIGHT slot the orphan lands on
/// the last column, the terminal advances past the end of the row, and
/// the bar wraps into the pane grid below (ADR-0020).
///
/// Swept so the cut lands on the boundary exactly, and with a `spacer`
/// so the ADR-0087 slack path is exercised at those boundaries too.
#[test]
fn the_slot_clamp_never_leaves_an_orphan_base() {
    use phux_config::widget::{StatusBar, WidgetRegistry};
    use phux_config::{StatusCfg, Widget};

    /// Renders "aa日本語..." and IGNORES its budget entirely.
    #[derive(Debug)]
    struct Greedy;
    impl StatusWidget for Greedy {
        fn render(&self, _ctx: &WidgetContext<'_>) -> WidgetCells {
            WidgetCells::from_text("aa\u{65e5}\u{672c}\u{8a9e}\u{306e}\u{5e45}")
        }
        fn render_within(&self, ctx: &WidgetContext<'_>, _budget: usize) -> WidgetCells {
            self.render(ctx)
        }
    }
    #[allow(clippy::unnecessary_wraps)]
    fn greedy(_opts: &BTreeMap<String, toml::Value>) -> Result<Box<dyn StatusWidget>, WidgetError> {
        Ok(Box::new(Greedy))
    }

    let mut reg = WidgetRegistry::with_builtins();
    reg.register("greedy", greedy);
    let cfg = StatusCfg {
        left: vec![Widget::Bare("session-name".to_owned())],
        center: vec![Widget::Bare("spacer".to_owned())],
        right: vec![Widget::Bare("greedy".to_owned())],
        ..Default::default()
    };
    let bar = StatusBar::build(&cfg, &reg).expect("bar builds");
    for width in 1u16..24 {
        let ctx = WidgetContext::new(fixed_time(), "s", "C-a", &[]);
        let row = bar.render(&ctx, width);
        assert_eq!(row.len(), usize::from(width), "width {width}");
        assert_eq!(
            emitted_columns(&row),
            Ok(usize::from(width)),
            "width {width}: the row does not advance exactly its own width"
        );
    }
}

/// A window name is a pane's OSC-2 title, and the bar is emitted as raw
/// VT. Explicit bidi overrides are zero-width, so they cost no budget
/// and no width check notices them, but they reorder everything drawn
/// after them — a pane could make its own tab read as another window's.
#[test]
fn a_window_name_cannot_carry_a_bidi_override() {
    let cells = render_windows(&[], &[win("run\u{202e}gpj.exe", true)]);
    let text = text_of(&cells);
    assert!(
        !text.contains('\u{202e}'),
        "a bidi override reached the tab bar: {text:?}"
    );
    assert_eq!(text, "0:rungpj.exe");
    // Zero-width, so it changes no geometry either.
    assert_eq!(
        cells.len(),
        render_windows(&[], &[win("rungpj.exe", true)]).len()
    );
}

/// A budget too narrow for the strip's first character still says the
/// strip was cut. Dropping the whole character and then having nothing
/// to stamp the ellipsis on would leave a one-column strip rendering
/// blank — indistinguishable from a widget that had nothing to say.
#[test]
fn a_clip_narrower_than_the_first_character_still_marks_the_cut() {
    let wide = WidgetCells::from_text("\u{65e5}\u{672c}");
    assert_eq!(wide.len(), 4);
    let out = wide.clipped(1);
    assert_eq!(text_of(&out), "\u{2026}");
    assert_eq!(out.len(), 1);
    assert_eq!(emitted_columns(&out.cells), Ok(1));
    // Zero budget really is empty, though: there is no column to mark.
    assert!(WidgetCells::from_text("\u{65e5}").clipped(0).is_empty());
}
