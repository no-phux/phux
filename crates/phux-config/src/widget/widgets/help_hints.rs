use std::collections::BTreeMap;

use crate::widget::{
    Cell, CellHit, CellStyle, StatusWidget, WidgetCells, WidgetContext, WidgetError,
    WidgetKindSpec, reject_unknown_opts,
};

const KIND: &str = "help-hints";

/// Doc spec — the factory validates against this same const, so the
/// documented option surface is the enforced one (phux-i0e8.11.3).
pub(in crate::widget) const SPEC: WidgetKindSpec = WidgetKindSpec {
    kind: KIND,
    summary: "Clickable, prefix-aware navigation (`<prefix>  s Sessions · \
              Space Commands · S Settings · ? Help · [ Copy`), rendered \
              with the configured prefix chord. Drops complete destinations \
              from the right as the bar narrows, and disappears entirely \
              rather than showing a fragment.",
    options: &[],
};

/// Separator between two hints. A middot rather than a pipe: it reads as
/// punctuation between peers instead of as a table rule.
const SEP: &str = " · ";

/// The hints, most useful first. The order is the drop order in reverse:
/// cross-host session navigation is the route that remains when only one
/// destination fits.
#[derive(Debug, Clone, Copy)]
struct Hint {
    label: &'static str,
    action: &'static str,
}

const HINTS: [Hint; 5] = [
    Hint {
        label: "s Sessions",
        action: "session-picker",
    },
    Hint {
        label: "Space Commands",
        action: "command-palette",
    },
    Hint {
        label: "S Settings",
        action: "settings",
    },
    Hint {
        label: "? Help",
        action: "show-help",
    },
    Hint {
        label: "[ Copy",
        action: "copy-mode",
    },
];

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
            text.push_str(hint.label);
        }
        Some(text)
    }

    fn cells(ctx: &WidgetContext<'_>, n: usize) -> WidgetCells {
        let style = Some(CellStyle {
            dim: true,
            ..CellStyle::default()
        });
        let mut cells = WidgetCells::from_styled(ctx.prefix, style.clone()).cells;
        cells.extend(WidgetCells::from_styled("  ", style.clone()).cells);
        for (i, hint) in HINTS.iter().take(n).enumerate() {
            if i > 0 {
                cells.extend(WidgetCells::from_styled(SEP, style.clone()).cells);
            }
            let mut target = WidgetCells::from_styled(hint.label, style.clone()).cells;
            stamp_action(&mut target, hint.action);
            cells.extend(target);
        }
        WidgetCells { cells }
    }
}

fn stamp_action(cells: &mut [Cell], action: &'static str) {
    for cell in cells {
        cell.hit = Some(CellHit::Action(action));
    }
}

impl StatusWidget for HelpHintsWidget {
    fn render(&self, ctx: &WidgetContext<'_>) -> WidgetCells {
        Self::line(ctx, HINTS.len()).map_or_else(
            || WidgetCells { cells: Vec::new() },
            |_| Self::cells(ctx, HINTS.len()),
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
                return Self::cells(ctx, n);
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
