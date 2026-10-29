//! Status-bar composer: builds a [`StatusBar`] from a [`StatusCfg`] and lays
//! widget output into one `width`-wide row of [`Cell`]s.
//!
//! Host-agnostic: it emits no VT, picks no screen row, and owns no clock.
//! Three slots (`docs/status-bar.md#the-status-bar`) are placed left-flush,
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
mod tests;
