use super::*;
use crate::widget::{CellHit, WidgetCells, WindowInfo};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::UNIX_EPOCH;

fn ctx_with(session: &str) -> WidgetContext<'_> {
    WidgetContext::new(UNIX_EPOCH, session, "C-a", &[])
}

fn spec(kind: &str, opts: &[(&str, toml::Value)]) -> Widget {
    Widget::Spec(WidgetSpec {
        kind: kind.to_owned(),
        opts: opts
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect(),
    })
}

fn text(value: &str) -> Widget {
    spec("text", &[("value", value.into())])
}

fn session(prefix: &str) -> Widget {
    spec("session-name", &[("prefix", prefix.into())])
}

fn spacer() -> Widget {
    spec("spacer", &[])
}

fn bar(left: Vec<Widget>, center: Vec<Widget>, right: Vec<Widget>) -> StatusBar {
    let cfg = StatusCfg {
        left,
        center,
        right,
        ..Default::default()
    };
    StatusBar::build(&cfg, &WidgetRegistry::with_builtins()).unwrap()
}

fn win(name: &str, active: bool) -> WindowInfo {
    WindowInfo {
        name: name.to_owned(),
        active,
        ..WindowInfo::default()
    }
}

fn window_hits(row: &[Cell]) -> Vec<Option<usize>> {
    row.iter()
        .map(|c| match c.hit {
            Some(CellHit::Window(i)) => Some(i),
            _ => None,
        })
        .collect()
}

/// Placement, narrowing, and elastic-space policy as rendered rows.
#[test]
#[allow(clippy::too_many_lines, reason = "one case table")]
fn rows_compose_per_the_layout_policy() {
    let cases: Vec<(&str, StatusBar, &str, u16, &str)> = vec![
        ("empty bar pads", bar(vec![], vec![], vec![]), "", 4, "    "),
        (
            "zero width",
            bar(vec![text("A")], vec![], vec![]),
            "",
            0,
            "",
        ),
        (
            "left flush",
            bar(vec![Widget::Bare("session-name".into())], vec![], vec![]),
            "alpha",
            8,
            "alpha   ",
        ),
        (
            "right flush",
            bar(vec![], vec![], vec![Widget::Bare("session-name".into())]),
            "beta",
            10,
            "      beta",
        ),
        (
            "three slots, center centered in the gap",
            bar(
                vec![session("L:")],
                vec![session("C:")],
                vec![session("R:")],
            ),
            "x",
            20,
            "L:x     C:x      R:x",
        ),
        (
            "everything fits",
            bar(
                vec![text("LEFT")],
                vec![text("CENTER")],
                vec![text("RIGHT")],
            ),
            "",
            22,
            "LEFT   CENTER    RIGHT",
        ),
        (
            "crowded: the center yields whole, not as a fragment",
            bar(
                vec![text("LEFT")],
                vec![text("CENTER")],
                vec![text("RIGHT")],
            ),
            "",
            10,
            "LEFT RIGHT",
        ),
        (
            "right slot capped at half the row, cut marked",
            bar(
                vec![text("LEFT")],
                vec![text("CENTER")],
                vec![text("RIGHT")],
            ),
            "",
            8,
            "LEFTRIG…",
        ),
        (
            "a slot shrinks from its trailing widget",
            bar(
                vec![session("LEFTLEFTLEFT")],
                vec![],
                vec![Widget::Bare("session-name".into()), text("CLOCK")],
            ),
            "main",
            15,
            "LEFTLEF…mainCL…",
        ),
        (
            "a spacer pushes its neighbours to the ends",
            bar(vec![text("AA"), spacer(), text("ZZ")], vec![], vec![]),
            "",
            10,
            "AA      ZZ",
        ),
        (
            "slack is row-wide, not per slot",
            bar(
                vec![text("AA"), spacer(), text("BB")],
                vec![],
                vec![text("ZZ")],
            ),
            "",
            12,
            "AA      BBZZ",
        ),
        (
            "two spacers: the odd column goes first",
            bar(
                vec![text("A"), spacer(), text("B"), spacer(), text("C")],
                vec![],
                vec![],
            ),
            "",
            10,
            "A    B   C",
        ),
        (
            "a spacer gated out by min-cols claims no slack",
            bar(
                vec![
                    text("A"),
                    spec("spacer", &[("min-cols", 20.into())]),
                    text("B"),
                    spacer(),
                    text("C"),
                ],
                vec![],
                vec![],
            ),
            "",
            10,
            "AB       C",
        ),
        (
            "gated spacer live at a wide row",
            bar(
                vec![
                    text("A"),
                    spec("spacer", &[("min-cols", 20.into())]),
                    text("B"),
                    spacer(),
                    text("C"),
                ],
                vec![],
                vec![],
            ),
            "",
            20,
            "A         B        C",
        ),
    ];
    for (what, bar, session, width, want) in cases {
        assert_eq!(
            row_to_string(&bar.render(&ctx_with(session), width)),
            want,
            "{what}"
        );
    }
}

/// Spacers yield first: on a full or overflowing row a bar with a spacer
/// renders exactly like one without.
#[test]
fn a_spacer_renders_nothing_on_a_full_row() {
    let with = bar(vec![text("AAAA"), spacer(), text("ZZZZ")], vec![], vec![]);
    let without = bar(vec![text("AAAA"), text("ZZZZ")], vec![], vec![]);
    for width in [8, 6] {
        assert_eq!(
            row_to_string(&with.render(&ctx_with(""), width)),
            row_to_string(&without.render(&ctx_with(""), width)),
        );
    }
}

/// A styled spacer paints its gap: the registry's `style` decorator
/// fills in the blanks.
#[test]
fn a_styled_spacer_carries_its_style_into_the_gap() {
    let mut style = toml::map::Map::new();
    style.insert("bg".to_owned(), "#123456".into());
    let bar = bar(
        vec![
            text("A"),
            spec("spacer", &[("style", toml::Value::Table(style))]),
            text("B"),
        ],
        vec![],
        vec![],
    );
    let row = bar.render(&ctx_with(""), 6);
    assert!(row[1..5].iter().all(|c| {
        c.style
            .as_ref()
            .is_some_and(|s| s.bg.as_deref() == Some("#123456"))
    }));
}

/// Window-tab hit targets survive slot placement in either slot.
#[test]
fn window_tab_hits_survive_slot_placement() {
    let windows = [win("a", true), win("b", false)];
    let ctx = WidgetContext::new(UNIX_EPOCH, "", "C-a", &windows);
    let tabs = || vec![Widget::Bare("windows".into())];
    let (z, o) = (Some(0), Some(1));
    assert_eq!(
        window_hits(&bar(tabs(), vec![], vec![]).render(&ctx, 10)),
        vec![z, z, z, None, o, o, o, None, None, None]
    );
    assert_eq!(
        window_hits(&bar(vec![], vec![], tabs()).render(&ctx, 10)),
        vec![None, None, None, z, z, z, None, o, o, o]
    );
}

/// A narrowed tab bar drops whole tabs around the active one and marks
/// the hidden ones; a clipped `0:alpha 1:` would read as a window named
/// `1:` and hide that a third exists.
#[test]
fn a_narrow_tab_bar_drops_whole_tabs_and_marks_the_hidden_ones() {
    let windows = [win("alpha", true), win("beta", false), win("gamma", false)];
    let ctx = WidgetContext::new(UNIX_EPOCH, "", "C-a", &windows);
    let bar = bar(vec![Widget::Bare("windows".into())], vec![], vec![]);

    assert_eq!(
        row_to_string(&bar.render(&ctx, 22)),
        "0:alpha 1:beta 2:gamma"
    );
    let row = bar.render(&ctx, 10);
    assert_eq!(row_to_string(&row), "0:alpha\u{203a}  ");
    let z = Some(0);
    // The arrow selects the nearest hidden window.
    assert_eq!(
        window_hits(&row),
        vec![z, z, z, z, z, z, z, Some(1), None, None]
    );
    assert_eq!(
        row_to_string(&bar.render(&ctx, 16)),
        "0:alpha 1:beta\u{203a} "
    );

    // The visible run is anchored on the active tab, not window 0.
    let windows = [
        win("alpha", false),
        win("beta", false),
        win("gamma", true),
        win("delta", false),
    ];
    let ctx = WidgetContext::new(UNIX_EPOCH, "", "C-a", &windows);
    assert_eq!(
        row_to_string(&bar.render(&ctx, 11)),
        "\u{2039}2:gamma\u{203a}  "
    );
    assert_eq!(row_to_string(&bar.render(&ctx, 3)), "2:\u{2026}");
}

#[derive(Debug)]
struct CountingWidget(Arc<AtomicUsize>);

impl StatusWidget for CountingWidget {
    fn render(&self, _ctx: &WidgetContext<'_>) -> WidgetCells {
        self.0.fetch_add(1, Ordering::Relaxed);
        WidgetCells::from_text("context")
    }
}

/// A fitting frame paints the cells it measured rather than sampling the
/// feed again; a narrow frame adds one constrained render.
#[test]
fn fitting_widgets_are_sampled_once_per_frame() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut bar = StatusBar::empty();
    bar.left
        .widgets
        .push(Box::new(CountingWidget(calls.clone())));
    let ctx = ctx_with("work");
    assert_eq!(row_to_string(&bar.render(&ctx, 20)), "context             ");
    assert_eq!(calls.swap(0, Ordering::Relaxed), 1);
    assert_eq!(row_to_string(&bar.render(&ctx, 4)), "con…");
    assert_eq!(calls.load(Ordering::Relaxed), 2);
}

#[test]
fn unknown_widget_kind_propagates_error() {
    let cfg = StatusCfg {
        left: vec![Widget::Bare("not-a-real-widget".into())],
        ..Default::default()
    };
    match StatusBar::build(&cfg, &WidgetRegistry::with_builtins()) {
        Err(WidgetError::UnknownKind(k)) => assert_eq!(k, "not-a-real-widget"),
        other => panic!("expected UnknownKind, got {other:?}"),
    }
}
