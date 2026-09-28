//! Status-bar composer: builds a [`StatusBar`] from a [`StatusCfg`] and lays
//! widget output into one `width`-wide row of [`Cell`]s.
//!
//! Host-agnostic: it emits no VT, picks no screen row, and owns no clock.
//! Three slots (`docs/consumers/tui.md` §8.4) are placed left-flush,
//! right-flush, and centered in the gap between.

use crate::plugin::{PluginManifest, PluginWidgetSlot};
use crate::schema::{StatusCfg, Widget, WidgetSpec};
use crate::widget::{Cell, ExecFeed, StatusWidget, WidgetContext, WidgetError, WidgetRegistry};

/// One composed slot's worth of widgets.
struct Slot {
    widgets: Vec<Box<dyn StatusWidget>>,
}

impl Slot {
    fn build(specs: &[Widget], registry: &WidgetRegistry) -> Result<Self, WidgetError> {
        let widgets = specs
            .iter()
            .map(|entry| registry.build(&entry.to_spec()))
            .collect::<Result<_, _>>()?;
        Ok(Self { widgets })
    }

    /// Measure once, keeping the rendered cells so measuring and painting
    /// see the same clock and feed state.
    fn measure<'a>(&'a self, ctx: &WidgetContext<'_>) -> MeasuredSlot<'a> {
        MeasuredSlot {
            widgets: &self.widgets,
            cells: self.widgets.iter().map(|w| w.render(ctx).cells).collect(),
        }
    }
}

/// One frame's natural widget output.
struct MeasuredSlot<'a> {
    widgets: &'a [Box<dyn StatusWidget>],
    cells: Vec<Vec<Cell>>,
}

impl MeasuredSlot<'_> {
    /// Render at natural width, paying each elastic widget out of `slack`.
    fn render(self, ctx: &WidgetContext<'_>, slack: &mut Slack) -> Vec<Cell> {
        let mut out: Vec<Cell> = Vec::new();
        for (w, natural) in self.widgets.iter().zip(self.cells) {
            let cells = if w.elastic(ctx) {
                w.render_within(ctx, slack.take()).cells
            } else {
                natural
            };
            out.extend(cells);
        }
        out
    }

    /// Natural width; an elastic widget renders nothing here, so the row is
    /// measured before any spacer is paid.
    fn natural_width(&self) -> usize {
        self.cells.iter().map(Vec::len).sum()
    }

    fn elastic_count(&self, ctx: &WidgetContext<'_>) -> usize {
        self.widgets.iter().filter(|w| w.elastic(ctx)).count()
    }

    /// Render into at most `budget` cells. Later widgets yield first:
    /// reading order is priority, so the shipped right slot loses its clock
    /// before the session name.
    fn render_within(self, ctx: &WidgetContext<'_>, budget: usize) -> Vec<Cell> {
        if budget == 0 {
            return Vec::new();
        }
        let mut budgets: Vec<usize> = self.cells.iter().map(Vec::len).collect();
        let natural: usize = budgets.iter().sum();
        let mut deficit = natural.saturating_sub(budget);
        for b in budgets.iter_mut().rev() {
            if deficit == 0 {
                break;
            }
            let cut = deficit.min(*b);
            *b -= cut;
            deficit -= cut;
        }

        let mut out: Vec<Cell> = Vec::new();
        for (w, b) in self.widgets.iter().zip(budgets) {
            out.extend(w.render_within(ctx, b).cells);
        }
        // A widget may under-spend but never overrun; clamp anyway, without
        // leaving half a double-width character to wrap the row.
        out.truncate(budget);
        crate::widget::drop_orphan_base(&mut out);
        out
    }
}

/// The row's leftover columns, paid out evenly to elastic widgets in reading
/// order; the first `remainder` claimants get one column more.
#[derive(Debug, Clone, Copy)]
struct Slack {
    per: usize,
    remainder: usize,
}

impl Slack {
    const fn split(total: usize, claimants: usize) -> Self {
        if claimants == 0 {
            return Self {
                per: 0,
                remainder: 0,
            };
        }
        Self {
            per: total / claimants,
            remainder: total % claimants,
        }
    }

    const fn take(&mut self) -> usize {
        if self.remainder > 0 {
            self.remainder -= 1;
            self.per + 1
        } else {
            self.per
        }
    }
}

/// The composed status bar, rendered per tick at a caller-supplied width.
pub struct StatusBar {
    left: Slot,
    center: Slot,
    right: Slot,
}

impl std::fmt::Debug for StatusBar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatusBar")
            .field("left.len", &self.left.widgets.len())
            .field("center.len", &self.center.widgets.len())
            .field("right.len", &self.right.widgets.len())
            .finish()
    }
}

impl StatusBar {
    /// Build a bar from parsed config and a widget registry.
    ///
    /// # Errors
    ///
    /// Forwards any [`WidgetError`] from the registry.
    pub fn build(cfg: &StatusCfg, registry: &WidgetRegistry) -> Result<Self, WidgetError> {
        Ok(Self {
            left: Slot::build(&cfg.left, registry)?,
            center: Slot::build(&cfg.center, registry)?,
            right: Slot::build(&cfg.right, registry)?,
        })
    }

    /// An empty bar: no widgets in any slot.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            left: Slot {
                widgets: Vec::new(),
            },
            center: Slot {
                widgets: Vec::new(),
            },
            right: Slot {
                widgets: Vec::new(),
            },
        }
    }

    /// The async feeds behind this bar's `exec` widgets, in slot order.
    #[must_use]
    pub fn exec_feeds(&self) -> Vec<ExecFeed> {
        self.left
            .widgets
            .iter()
            .chain(&self.center.widgets)
            .chain(&self.right.widgets)
            .filter_map(|w| w.exec_feed())
            .collect()
    }

    /// True if no slot carries any widgets.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.left.widgets.is_empty()
            && self.center.widgets.is_empty()
            && self.right.widgets.is_empty()
    }

    /// Render exactly `width` cells, padded with blanks.
    ///
    /// When the slots overflow, they narrow in priority order through
    /// [`StatusWidget::render_within`]:
    ///
    /// 1. **Right** takes what it needs, up to half the row (so a long
    ///    session name cannot push the tabs off, while the `switch`
    ///    affordance survives on small terminals).
    /// 2. **Left** (the tab bar you navigate by) gets the rest.
    /// 3. **Center** gets the surviving gap less a one-column gutter each
    ///    side, or nothing below eight columns.
    ///
    /// On a row that fits, the leftover width is split across every `spacer`
    /// in the bar (row-wide, not per slot). Spacers have no natural width, so
    /// on a full row they render nothing and never push content off.
    #[must_use]
    pub fn render(&self, ctx: &WidgetContext<'_>, width: u16) -> Vec<Cell> {
        let ctx = &WidgetContext {
            cols: width,
            ..*ctx
        };
        let width = usize::from(width);
        if width == 0 {
            return Vec::new();
        }

        let (left, center, right) = self.resolve_slots(ctx, width);
        let mut row: Vec<Cell> = vec![Cell::default(); width];

        let left_take = left.len();
        for (i, c) in left.into_iter().enumerate() {
            row[i] = c;
        }
        let right_start = width - right.len();
        for (i, c) in right.into_iter().enumerate() {
            row[right_start + i] = c;
        }
        let gap_width = right_start.saturating_sub(left_take);
        let center_offset = left_take + gap_width.saturating_sub(center.len()) / 2;
        for (i, c) in center.into_iter().enumerate() {
            row[center_offset + i] = c;
        }
        row
    }

    fn resolve_slots(
        &self,
        ctx: &WidgetContext<'_>,
        width: usize,
    ) -> (Vec<Cell>, Vec<Cell>, Vec<Cell>) {
        let left = self.left.measure(ctx);
        let center = self.center.measure(ctx);
        let right = self.right.measure(ctx);
        let (ln, cn, rn) = (
            left.natural_width(),
            center.natural_width(),
            right.natural_width(),
        );

        let claimed = ln + cn + rn + if cn == 0 { 0 } else { CENTER_GUTTER * 2 };
        if claimed <= width {
            let mut slack = Slack::split(
                width - claimed,
                left.elastic_count(ctx) + center.elastic_count(ctx) + right.elastic_count(ctx),
            );
            return (
                left.render(ctx, &mut slack),
                center.render(ctx, &mut slack),
                right.render(ctx, &mut slack),
            );
        }

        let right = right.render_within(ctx, rn.min(width / 2));
        let left = left.render_within(ctx, width.saturating_sub(right.len()));
        let gap = width
            .saturating_sub(left.len() + right.len())
            .saturating_sub(CENTER_GUTTER * 2);
        let center = if gap >= CENTER_SLOT_MIN {
            center.render_within(ctx, gap)
        } else {
            Vec::new()
        };
        (left, center, right)
    }
}

/// Blank columns held either side of a non-empty center slot.
const CENTER_GUTTER: usize = 1;

/// Narrowest gap worth handing to the center slot; under it a centered
/// widget would be an illegible fragment.
const CENTER_SLOT_MIN: usize = 8;

/// The printable text of a rendered row; blank cells become spaces.
#[must_use]
pub fn row_to_string(row: &[Cell]) -> String {
    row.iter()
        .map(|cell| cell.text.first().copied().unwrap_or(' '))
        .collect()
}

/// Append enabled plugins' `[[widgets]]` contributions to their slots.
///
/// A contribution that does not build is dropped with a warning, so one
/// broken plugin cannot break the whole bar.
pub fn merge_widget_contributions(
    status: &mut StatusCfg,
    manifests: &[PluginManifest],
    registry: &WidgetRegistry,
) {
    for manifest in manifests {
        for widget in &manifest.widgets {
            let spec = WidgetSpec {
                kind: widget.kind.clone(),
                opts: widget.opts.clone(),
            };
            if let Err(err) = registry.build(&spec) {
                tracing::warn!(
                    plugin = %manifest.id,
                    widget = %widget.id,
                    error = %err,
                    "dropping plugin status-widget contribution that failed validation",
                );
                continue;
            }
            let slot = match widget.slot {
                PluginWidgetSlot::Left => &mut status.left,
                PluginWidgetSlot::Center => &mut status.center,
                PluginWidgetSlot::Right => &mut status.right,
            };
            slot.push(Widget::Spec(spec));
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
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
}
