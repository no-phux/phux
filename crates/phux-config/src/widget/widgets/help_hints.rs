use std::collections::BTreeMap;

use crate::widget::{
    CellStyle, StatusWidget, WidgetCells, WidgetContext, WidgetError, WidgetKindSpec,
    reject_unknown_opts,
};

const KIND: &str = "help-hints";

/// Doc spec — the factory validates against this same const, so the
/// documented option surface is the enforced one (phux-i0e8.11.3).
pub(in crate::widget) const SPEC: WidgetKindSpec = WidgetKindSpec {
    kind: KIND,
    summary: "Dim, prefix-aware affordance hints (`<prefix>  Space \
              palette · ? help · [ copy`), rendered with the configured \
              prefix chord. Drops hints from the right as the bar \
              narrows, and disappears entirely rather than showing a \
              fragment.",
    options: &[],
};

/// Separator between two hints. A middot rather than a pipe: it reads as
/// punctuation between peers instead of as a table rule.
const SEP: &str = " · ";

/// The hints, most useful first. The order is the drop order in reverse:
/// the palette is the one chord worth knowing if you only learn one, so
/// it is the last to go.
const HINTS: [&str; 3] = ["Space palette", "? help", "[ copy"];

/// `help-hints` widget.
#[derive(Debug, Clone, Copy, Default)]
pub struct HelpHintsWidget;

impl HelpHintsWidget {
    /// The hint line carrying the first `n` hints, or `None` when `n` is
    /// 0 (no hints means no line at all — a bare prefix chord floating in
    /// the middle of the bar teaches nothing).
    ///
    /// The prefix is printed once, followed by two spaces, and the hints
    /// are its continuations: `C-a  Space palette · ? help`. Repeating
    /// the prefix per hint (the old shape) tripled the cost of the widget
    /// in columns to say the same thing three times.
    fn line(ctx: &WidgetContext<'_>, n: usize) -> Option<String> {
        if n == 0 {
            return None;
        }
        let mut text = String::with_capacity(ctx.prefix.len() + 40);
        text.push_str(ctx.prefix);
        text.push_str("  ");
        for (i, hint) in HINTS.iter().take(n).enumerate() {
            if i > 0 {
                text.push_str(SEP);
            }
            text.push_str(hint);
        }
        Some(text)
    }

    fn cells(text: &str) -> WidgetCells {
        WidgetCells::from_styled(
            text,
            Some(CellStyle {
                dim: true,
                ..CellStyle::default()
            }),
        )
    }
}

impl StatusWidget for HelpHintsWidget {
    fn render(&self, ctx: &WidgetContext<'_>) -> WidgetCells {
        Self::line(ctx, HINTS.len()).map_or_else(
            || WidgetCells { cells: Vec::new() },
            |text| Self::cells(&text),
        )
    }

    /// Drop whole hints, never half of one.
    ///
    /// These hints exist to be *read* by someone who does not yet know
    /// the keys. `C-a  Space palette · ? he…` fails at that job in a way
    /// that showing one fewer hint does not, so the ladder walks down
    /// whole entries and then stops rendering rather than clipping.
    fn render_within(&self, ctx: &WidgetContext<'_>, budget: usize) -> WidgetCells {
        for n in (1..=HINTS.len()).rev() {
            if let Some(text) = Self::line(ctx, n)
                && text.chars().count() <= budget
            {
                return Self::cells(&text);
            }
        }
        WidgetCells { cells: Vec::new() }
    }
}

pub(in crate::widget) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
    Ok(Box::new(HelpHintsWidget))
}
