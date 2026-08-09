//! Status-bar composer: turns a [`StatusCfg`] plus a [`WidgetRegistry`]
//! into a runtime [`StatusBar`], and lays widget output into a single
//! row of [`Cell`]s.
//!
//! Owned by `phux-nz4.5`. The composer is host-agnostic — it does not
//! emit VT, does not pick a screen row, and does not own a clock; it
//! just produces a `width`-wide cell strip on demand. The TUI client
//! (`phux-client::attach::status_bar`) takes that strip, paints it at
//! the bottom of the outer terminal, and decides the refresh cadence.
//!
//! Layout: three slots from [`StatusCfg`] — `left`, `center`, `right` —
//! each a list of widgets rendered with no implicit separator (per
//! `docs/consumers/tui.md` §8.4). Slots are concatenated independently, then
//! placed:
//!
//! - `left` flush at column 0,
//! - `right` flush against the last column (`width - 1`),
//! - `center` centered in whatever gap remains between the two.
//!
//! Truncation is left-biased: when the three slots together overflow
//! `width`, the right slot is preserved first, then the left, and the
//! center yields. Within a slot we drop trailing cells once the slot's
//! budget runs out. The result vector is always exactly `width` cells
//! long, padded with blank cells where slots don't reach.

use std::collections::BTreeMap;

use crate::plugin::{PluginManifest, PluginWidgetSlot};
use crate::schema::{StatusCfg, Widget, WidgetSpec};
use crate::widget::{
    Cell, ExecFeed, StatusWidget, WidgetCells, WidgetContext, WidgetError, WidgetRegistry,
};

/// One composed slot's worth of widgets.
struct Slot {
    widgets: Vec<Box<dyn StatusWidget>>,
}

impl Slot {
    fn build(specs: &[Widget], registry: &WidgetRegistry) -> Result<Self, WidgetError> {
        let mut widgets = Vec::with_capacity(specs.len());
        for entry in specs {
            let spec = match entry {
                Widget::Bare(kind) => WidgetSpec {
                    kind: kind.clone(),
                    opts: BTreeMap::new(),
                },
                Widget::Spec(s) => s.clone(),
            };
            widgets.push(registry.build(&spec)?);
        }
        Ok(Self { widgets })
    }

    fn render(&self, ctx: &WidgetContext<'_>) -> Vec<Cell> {
        let mut out: Vec<Cell> = Vec::new();
        for w in &self.widgets {
            let WidgetCells { cells } = w.render(ctx);
            out.extend(cells);
        }
        out
    }

    /// The width this slot wants if nothing constrains it.
    fn natural_width(&self, ctx: &WidgetContext<'_>) -> usize {
        self.widgets.iter().map(|w| w.render(ctx).len()).sum()
    }

    /// Render the slot into at most `budget` cells.
    ///
    /// Within a slot, **later widgets yield first**. Slots are written in
    /// reading order and that order is a statement of priority: in the
    /// shipped `right = ["session-name", { time }]`, losing the clock on
    /// a narrow terminal costs you nothing, while losing the session name
    /// costs you the answer to "which of my sessions am I looking at".
    /// Each widget then decides *how* to spend what it is given via
    /// [`StatusWidget::render_within`].
    fn render_within(&self, ctx: &WidgetContext<'_>, budget: usize) -> Vec<Cell> {
        if budget == 0 {
            return Vec::new();
        }
        let mut budgets: Vec<usize> = self.widgets.iter().map(|w| w.render(ctx).len()).collect();
        let natural: usize = budgets.iter().sum();
        // Charge the whole shortfall to the trailing widgets, in reverse
        // order, until it is paid off.
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
        // A widget is free to under-spend but never to overrun; clamp
        // anyway so a third-party widget cannot corrupt the row geometry.
        out.truncate(budget);
        out
    }
}

/// The composed status bar.
///
/// Built once from a parsed [`StatusCfg`] and a populated
/// [`WidgetRegistry`]; rendered per-tick into a [`Vec<Cell>`] of caller-
/// supplied width.
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
    /// Build a [`StatusBar`] from parsed config + a populated widget
    /// registry.
    ///
    /// # Errors
    ///
    /// Forwards any [`WidgetError`] from the registry — most commonly
    /// `UnknownKind` (a widget kind in config that the registry has no
    /// factory for) or `InvalidOption` (a factory rejected its TOML
    /// options).
    pub fn build(cfg: &StatusCfg, registry: &WidgetRegistry) -> Result<Self, WidgetError> {
        Ok(Self {
            left: Slot::build(&cfg.left, registry)?,
            center: Slot::build(&cfg.center, registry)?,
            right: Slot::build(&cfg.right, registry)?,
        })
    }

    /// An empty bar: no widgets in any slot.
    ///
    /// phux-9vf: the TUI's error-line painter wraps an empty bar so the
    /// widget pipeline produces no output — the painter substitutes a
    /// fixed diagnostic row instead. Cheaper and clearer than threading
    /// an `Option<StatusBar>` through the painter.
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

    /// phux-r82.6: the asynchronous data feeds behind this bar's `exec`
    /// widgets, in slot order (left, center, right). The host runs each
    /// feed's command on its interval and pushes output through
    /// [`ExecFeed::apply_output`]; a bar with no `exec` widgets returns
    /// an empty vec and the host spawns nothing.
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

    /// True if no slot carries any widgets — caller may then skip
    /// reserving a status row entirely.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.left.widgets.is_empty()
            && self.center.widgets.is_empty()
            && self.right.widgets.is_empty()
    }

    /// Render the bar at the supplied display width. Returns exactly
    /// `width` cells, padded with blanks where slots don't reach.
    ///
    /// ## How the row narrows
    ///
    /// When everything fits, the three slots render at their natural
    /// width and nothing below applies. When they don't, the bar is
    /// resolved in priority order:
    ///
    /// 1. **Right** takes what it needs, up to half the row. The cap is
    ///    there so a long session name cannot push the window tabs off a
    ///    narrow bar; half rather than something tighter because on a
    ///    genuinely small terminal the right slot is where the shipped
    ///    lineup puts the `switch` affordance, and an affordance that
    ///    disappears at the width that made it necessary is worse than no
    ///    affordance at all. Inside the cap the slot shrinks itself.
    /// 2. **Left** gets everything the right slot did not take. It holds
    ///    the tab bar — the chrome you navigate by — so it is the last
    ///    thing to lose room.
    /// 3. **Center** gets whatever gap survives, minus one blank column
    ///    of breathing room on each side so it never abuts its
    ///    neighbours. Below `CENTER_SLOT_MIN` (8) the gap is not worth
    ///    filling and the center slot renders nothing.
    ///
    /// Slots shrink through [`StatusWidget::render_within`], so widgets
    /// degrade on their own terms — the composer never cuts cells it does
    /// not understand.
    ///
    /// [`StatusWidget::render_within`]: crate::widget::StatusWidget::render_within
    #[must_use]
    pub fn render(&self, ctx: &WidgetContext<'_>, width: u16) -> Vec<Cell> {
        // Widgets answer "is this a cramped terminal?" from `ctx.cols`,
        // so the composer stamps it here — the one place that knows the
        // real row width — rather than trusting every caller to set it.
        let ctx = &WidgetContext {
            cols: width,
            ..*ctx
        };
        let width = usize::from(width);
        if width == 0 {
            return Vec::new();
        }

        let (left, center, right) = self.resolve_slots(ctx, width);

        // Compose into a fixed-width row.
        let mut row: Vec<Cell> = vec![Cell::default(); width];

        // Left: flush at column 0.
        let left_take = left.len();
        for (i, c) in left.into_iter().enumerate() {
            row[i] = c;
        }

        // Right: flush at the last column.
        let right_start = width - right.len();
        for (i, c) in right.into_iter().enumerate() {
            row[right_start + i] = c;
        }

        // Center: centered within the gap between left and right.
        let gap_width = right_start.saturating_sub(left_take);
        let center_offset = left_take + gap_width.saturating_sub(center.len()) / 2;
        for (i, c) in center.into_iter().enumerate() {
            row[center_offset + i] = c;
        }

        row
    }

    /// Resolve the three slots' cells for a row of `width`. Split out of
    /// [`Self::render`] so the placement arithmetic above reads as pure
    /// geometry and the priority policy is testable on its own.
    fn resolve_slots(
        &self,
        ctx: &WidgetContext<'_>,
        width: usize,
    ) -> (Vec<Cell>, Vec<Cell>, Vec<Cell>) {
        let (ln, cn, rn) = (
            self.left.natural_width(ctx),
            self.center.natural_width(ctx),
            self.right.natural_width(ctx),
        );

        // Everything fits with a blank column either side of the center:
        // no policy needed.
        if ln + cn + rn + center_gutters(cn) <= width {
            return (
                self.left.render(ctx),
                self.center.render(ctx),
                self.right.render(ctx),
            );
        }

        let right = self.right.render_within(ctx, rn.min(width / 2));
        let left = self
            .left
            .render_within(ctx, width.saturating_sub(right.len()));

        // Whatever is genuinely left over, less the breathing room.
        let gap = width
            .saturating_sub(left.len() + right.len())
            .saturating_sub(CENTER_GUTTER * 2);
        let center = if gap >= CENTER_SLOT_MIN {
            self.center.render_within(ctx, gap)
        } else {
            Vec::new()
        };

        (left, center, right)
    }
}

/// Blank columns held either side of the center slot so a centered widget
/// never touches the slot beside it. Purely visual, and the reason a
/// center widget can be dropped while a column or two still appears free.
const CENTER_GUTTER: usize = 1;

/// Narrowest gap worth handing to the center slot. Under this, a centered
/// widget is a fragment wedged between two neighbours rather than a
/// legible hint, so the slot yields the space to the row's blank fill.
const CENTER_SLOT_MIN: usize = 8;

/// The gutters a center slot of `n` cells actually costs (none when the
/// center slot is empty).
const fn center_gutters(n: usize) -> usize {
    if n == 0 { 0 } else { CENTER_GUTTER * 2 }
}

/// Convenience: collect the printable text of a rendered row into a
/// `String`. Blank cells become spaces. Useful for tests and for the
/// minimal "render to bytes" path the TUI client uses.
#[must_use]
pub fn row_to_string(row: &[Cell]) -> String {
    let mut s = String::with_capacity(row.len());
    for cell in row {
        match cell.text.first() {
            Some(ch) => s.push(*ch),
            None => s.push(' '),
        }
    }
    s
}

/// Fold enabled plugins' `[[widgets]]` contributions into a `[status]`
/// config (phux-r82.6), appending each contributed spec after the user's
/// own widgets in its declared slot.
///
/// Contributions are validated against `registry` first; a spec that does
/// not build (unknown kind, bad option) is dropped with a `tracing::warn!`
/// so one broken plugin cannot degrade the whole bar into the error strip.
///
/// Lives with the status-bar composer rather than in [`crate::plugin`]
/// because it is the one place that speaks all three vocabularies —
/// manifest contributions, `[status]` schema, and the widget registry that
/// validates them. Hanging it off `plugin` made `plugin` and `schema`
/// import each other for no other reason (phux-4fbs.5).
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
            match registry.build(&spec) {
                Ok(_) => {
                    let slot = match widget.slot {
                        PluginWidgetSlot::Left => &mut status.left,
                        PluginWidgetSlot::Center => &mut status.center,
                        PluginWidgetSlot::Right => &mut status.right,
                    };
                    slot.push(Widget::Spec(spec));
                }
                Err(err) => {
                    tracing::warn!(
                        plugin = %manifest.id,
                        widget = %widget.id,
                        error = %err,
                        "dropping plugin status-widget contribution that failed validation",
                    );
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::schema::{StatusCfg, StatusPosition, Widget, WidgetSpec};
    use std::time::{Duration, UNIX_EPOCH};

    fn ctx_with(session: &str) -> WidgetContext<'_> {
        WidgetContext::new(UNIX_EPOCH + Duration::from_secs(0), session, "C-a", &[])
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

    #[test]
    fn empty_config_is_empty() {
        let cfg = StatusCfg::default();
        let reg = WidgetRegistry::with_builtins();
        let bar = StatusBar::build(&cfg, &reg).unwrap();
        assert!(bar.is_empty());
        let row = bar.render(&ctx_with(""), 10);
        assert_eq!(row.len(), 10);
        assert!(row.iter().all(|c| c.text.is_empty()));
    }

    #[test]
    fn left_slot_flushes_left() {
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            ..Default::default()
        };
        let reg = WidgetRegistry::with_builtins();
        let bar = StatusBar::build(&cfg, &reg).unwrap();
        let row = bar.render(&ctx_with("alpha"), 20);
        let s = row_to_string(&row);
        assert_eq!(s, "alpha               ");
    }

    #[test]
    fn right_slot_flushes_right() {
        let cfg = StatusCfg {
            right: vec![Widget::Bare("session-name".into())],
            ..Default::default()
        };
        let reg = WidgetRegistry::with_builtins();
        let bar = StatusBar::build(&cfg, &reg).unwrap();
        let row = bar.render(&ctx_with("beta"), 10);
        let s = row_to_string(&row);
        assert_eq!(s, "      beta");
    }

    #[test]
    fn three_slots_compose() {
        // left=session, center=session, right=session — distinct names
        // are hard without a second widget; reuse the same widget with
        // different prefixes via spec form.
        let cfg = StatusCfg {
            left: vec![spec(
                "session-name",
                &[("prefix", toml::Value::String("L:".into()))],
            )],
            center: vec![spec(
                "session-name",
                &[("prefix", toml::Value::String("C:".into()))],
            )],
            right: vec![spec(
                "session-name",
                &[("prefix", toml::Value::String("R:".into()))],
            )],
            position: StatusPosition::default(),
        };
        let reg = WidgetRegistry::with_builtins();
        let bar = StatusBar::build(&cfg, &reg).unwrap();
        let row = bar.render(&ctx_with("x"), 20);
        let s = row_to_string(&row);
        // 3 cells per slot: L:x (3) … C:x (3) centered … R:x (3) flush right
        // Gap = 20 - 3 - 3 = 14; center starts at 3 + (14-3)/2 = 3+5 = 8.
        assert_eq!(s, "L:x     C:x      R:x");
        assert_eq!(s.len(), 20);
    }

    /// The narrowing policy end to end: the center slot is dropped whole
    /// rather than wedged in as a fragment, the right slot is held to its
    /// share of the row, and whatever survives a cut says so with an
    /// ellipsis instead of pretending to be complete.
    #[test]
    fn overflow_caps_the_right_slot_and_drops_the_center_whole() {
        let cfg = StatusCfg {
            left: vec![spec(
                "session-name",
                &[("prefix", toml::Value::String("LEFT".into()))],
            )],
            center: vec![spec(
                "session-name",
                &[("prefix", toml::Value::String("CENTER".into()))],
            )],
            right: vec![spec(
                "session-name",
                &[("prefix", toml::Value::String("RIGHT".into()))],
            )],
            position: StatusPosition::default(),
        };
        let reg = WidgetRegistry::with_builtins();
        let bar = StatusBar::build(&cfg, &reg).unwrap();

        // Natural: LEFT(4) + CENTER(6) + RIGHT(5) = 15, plus the two
        // gutter columns. At 22 everything fits, center centered in the gap.
        assert_eq!(
            row_to_string(&bar.render(&ctx_with(""), 22)),
            "LEFT   CENTER    RIGHT"
        );

        // At 10 the row is crowded. Right needs 5 and the cap is half of
        // 10, so it survives whole; left keeps its 4. The 1 column left
        // over is far under CENTER_SLOT_MIN, so the center yields
        // entirely rather than rendering "C".
        let s = row_to_string(&bar.render(&ctx_with(""), 10));
        assert_eq!(s, "LEFT RIGHT");
        assert_eq!(s.chars().count(), 10);

        // At 8 the right slot hits the half-row ceiling and clips to 4,
        // marking the cut: the bar admits it shortened something rather
        // than showing a "RIGH" that reads as a complete word.
        assert_eq!(row_to_string(&bar.render(&ctx_with(""), 8)), "LEFTRIG…");
    }

    /// A slot spends its budget front-to-back: the trailing widget is the
    /// one that yields. The shipped `right = ["session-name", time]`
    /// therefore loses its clock before it loses the session name.
    #[test]
    fn a_slot_shrinks_from_its_trailing_widget() {
        let cfg = StatusCfg {
            // A left slot wide enough to actually crowd the row — the
            // policy only engages once the three slots overflow.
            left: vec![spec(
                "session-name",
                &[("prefix", toml::Value::String("LEFTLEFTLEFT".into()))],
            )],
            right: vec![
                Widget::Bare("session-name".into()),
                spec("time", &[("format", toml::Value::String("CLOCK".into()))]),
            ],
            ..Default::default()
        };
        let reg = WidgetRegistry::with_builtins();
        let bar = StatusBar::build(&cfg, &reg).unwrap();

        // `session-name` renders prefix + name, so left is 16 cells and
        // right 9. At width 30 everything survives whole.
        assert_eq!(
            row_to_string(&bar.render(&ctx_with("main"), 30)),
            "LEFTLEFTLEFTmain     mainCLOCK"
        );

        // At width 15 the row overflows. The right slot is capped at half
        // the row (7 cells): the whole session name plus two of the
        // clock's, so the clock absorbs the entire cut and the identity
        // survives intact. Left then clips into the 8 columns it is left
        // with.
        let s = row_to_string(&bar.render(&ctx_with("main"), 15));
        assert_eq!(s, "LEFTLEF…mainCL…");
        assert_eq!(s.chars().count(), 15);
    }

    /// phux-foz.12: window-tab hit targets survive slot placement — a
    /// left-slot tab strip keeps its per-column `CellHit::Window` stamps at
    /// the columns the tabs occupy, other columns stay inert, and the same
    /// holds when the strip rides the right slot (offset by the flush).
    #[test]
    fn window_tab_hits_survive_slot_placement() {
        use crate::widget::{CellHit, WindowInfo};
        let windows = [
            WindowInfo {
                name: "a".to_owned(),
                active: true,
                zoomed: false,
                attention: false,
                branch: None,
            },
            WindowInfo {
                name: "b".to_owned(),
                active: false,
                zoomed: false,
                attention: false,
                branch: None,
            },
        ];
        let ctx = WidgetContext::new(UNIX_EPOCH, "", "C-a", &windows);
        let reg = WidgetRegistry::with_builtins();
        let hits_of = |cfg: &StatusCfg, width: u16| -> Vec<Option<usize>> {
            let bar = StatusBar::build(cfg, &reg).unwrap();
            bar.render(&ctx, width)
                .iter()
                .map(|c| {
                    c.hit.and_then(|h| match h {
                        CellHit::Window(i) => Some(i),
                        CellHit::Switch => None,
                    })
                })
                .collect()
        };
        // Left slot: "0:a 1:b" flush at column 0 in a 10-wide row.
        let left = StatusCfg {
            left: vec![Widget::Bare("windows".into())],
            ..Default::default()
        };
        assert_eq!(
            hits_of(&left, 10),
            vec![
                Some(0),
                Some(0),
                Some(0),
                None,
                Some(1),
                Some(1),
                Some(1),
                None,
                None,
                None
            ]
        );
        // Right slot: same strip flush against the last column.
        let right = StatusCfg {
            right: vec![Widget::Bare("windows".into())],
            ..Default::default()
        };
        assert_eq!(
            hits_of(&right, 10),
            vec![
                None,
                None,
                None,
                Some(0),
                Some(0),
                Some(0),
                None,
                Some(1),
                Some(1),
                Some(1)
            ]
        );
    }

    /// A narrowed tab bar drops whole tabs, never parts of one, and marks
    /// what it hid.
    ///
    /// The old composer clipped the strip by raw cell count and produced
    /// `0:alpha 1:` at width 10 — which reads as a second window named
    /// `1:`, conceals that a third exists, and leaves the fragment
    /// clickable. Every one of those is a lie about the session. The bar
    /// now shows the whole tabs that fit plus a `›` for the rest.
    #[test]
    fn a_narrow_tab_bar_drops_whole_tabs_and_marks_the_hidden_ones() {
        use crate::widget::{CellHit, WindowInfo};
        let mk = |name: &str, active: bool| WindowInfo {
            name: name.to_owned(),
            active,
            zoomed: false,
            attention: false,
            branch: None,
        };
        let windows = [mk("alpha", true), mk("beta", false), mk("gamma", false)];
        let ctx = WidgetContext::new(UNIX_EPOCH, "", "C-a", &windows);
        let reg = WidgetRegistry::with_builtins();
        let cfg = StatusCfg {
            left: vec![Widget::Bare("windows".into())],
            ..Default::default()
        };
        let bar = StatusBar::build(&cfg, &reg).unwrap();

        // Full strip "0:alpha 1:beta 2:gamma" is 22 cells and fits at 22.
        assert_eq!(
            row_to_string(&bar.render(&ctx, 22)),
            "0:alpha 1:beta 2:gamma"
        );

        // At 10 only the active tab fits; the rest become one `›`.
        let row = bar.render(&ctx, 10);
        assert_eq!(row_to_string(&row), "0:alpha\u{203a}  ");
        let hits: Vec<Option<usize>> = row
            .iter()
            .map(|c| {
                c.hit.and_then(|h| match h {
                    CellHit::Window(i) => Some(i),
                    CellHit::Switch => None,
                })
            })
            .collect();
        assert_eq!(
            hits,
            vec![
                Some(0),
                Some(0),
                Some(0),
                Some(0),
                Some(0),
                Some(0),
                Some(0),
                // The overflow mark is chrome, not a window: inert.
                None,
                None,
                None
            ]
        );

        // At 16 the neighbour comes back, and the mark moves to cover
        // only what is still hidden.
        assert_eq!(
            row_to_string(&bar.render(&ctx, 16)),
            "0:alpha 1:beta\u{203a} "
        );
    }

    /// The visible run is anchored on the active tab, not on window 0, so
    /// narrowing the terminal never hides where you actually are.
    #[test]
    fn a_narrow_tab_bar_keeps_the_active_tab_visible() {
        use crate::widget::WindowInfo;
        let mk = |name: &str, active: bool| WindowInfo {
            name: name.to_owned(),
            active,
            zoomed: false,
            attention: false,
            branch: None,
        };
        let windows = [
            mk("alpha", false),
            mk("beta", false),
            mk("gamma", true),
            mk("delta", false),
        ];
        let ctx = WidgetContext::new(UNIX_EPOCH, "", "C-a", &windows);
        let reg = WidgetRegistry::with_builtins();
        let cfg = StatusCfg {
            left: vec![Widget::Bare("windows".into())],
            ..Default::default()
        };
        let bar = StatusBar::build(&cfg, &reg).unwrap();

        // Marks on both sides: windows 0-1 are hidden left, 3 hidden right.
        let s = row_to_string(&bar.render(&ctx, 11));
        assert_eq!(s, "\u{2039}2:gamma\u{203a}  ");

        // Even at an absurd width the active index survives longest,
        // because that is the character you need to navigate by.
        assert_eq!(row_to_string(&bar.render(&ctx, 3)), "2:\u{2026}");
    }

    #[test]
    fn zero_width_returns_empty() {
        let cfg = StatusCfg::default();
        let reg = WidgetRegistry::with_builtins();
        let bar = StatusBar::build(&cfg, &reg).unwrap();
        let row = bar.render(&ctx_with(""), 0);
        assert!(row.is_empty());
    }

    #[test]
    fn unknown_widget_kind_propagates_error() {
        let cfg = StatusCfg {
            left: vec![Widget::Bare("not-a-real-widget".into())],
            ..Default::default()
        };
        let reg = WidgetRegistry::with_builtins();
        match StatusBar::build(&cfg, &reg) {
            Err(WidgetError::UnknownKind(k)) => assert_eq!(k, "not-a-real-widget"),
            other => panic!("expected UnknownKind, got {other:?}"),
        }
    }

    #[test]
    fn time_and_session_render_together() {
        // Mirrors the integration target: bar with both built-in widgets.
        let cfg = StatusCfg {
            left: vec![Widget::Bare("session-name".into())],
            right: vec![spec(
                "time",
                &[("format", toml::Value::String("YEAR".into()))],
            )],
            ..Default::default()
        };
        let reg = WidgetRegistry::with_builtins();
        let bar = StatusBar::build(&cfg, &reg).unwrap();
        // "YEAR" is a literal (no `%` escapes) so the time widget renders
        // it verbatim regardless of clock — deterministic snapshot.
        let row = bar.render(&ctx_with("main"), 20);
        let s = row_to_string(&row);
        assert_eq!(s, "main            YEAR");
    }
}
