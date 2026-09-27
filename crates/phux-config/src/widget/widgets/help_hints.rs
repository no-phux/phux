//! `help-hints` widget — clickable, prefix-aware navigation hints.

use std::collections::BTreeMap;

use crate::widget::{
    CellHit, CellStyle, StatusWidget, WidgetCells, WidgetContext, WidgetError, WidgetKindSpec,
    reject_unknown_opts,
};

pub(in crate::widget) const SPEC: WidgetKindSpec = WidgetKindSpec {
    kind: "help-hints",
    summary: "Clickable, prefix-aware navigation (`<prefix>  s Sessions · \
              Space Commands · S Settings · ? Help · [ Copy`), rendered \
              with the configured prefix chord. Drops complete destinations \
              from the right as the bar narrows, and disappears entirely \
              rather than showing a fragment.",
    options: &[],
};

const SEP: &str = " · ";

/// `(label, action)`, most useful first; the drop order is the reverse.
const HINTS: [(&str, &str); 5] = [
    ("s Sessions", "session-picker"),
    ("Space Commands", "command-palette"),
    ("S Settings", "settings"),
    ("? Help", "show-help"),
    ("[ Copy", "copy-mode"),
];

/// `help-hints` widget.
#[derive(Debug, Clone, Copy, Default)]
pub struct HelpHintsWidget;

impl HelpHintsWidget {
    /// The prefix once, then the first `n` hints as its continuations, each
    /// hint stamped with its action.
    fn cells(ctx: &WidgetContext<'_>, n: usize) -> WidgetCells {
        let style = Some(CellStyle {
            dim: true,
            ..CellStyle::default()
        });
        let styled = |s: &str| WidgetCells::from_styled(s, style.clone()).cells;
        let mut cells = styled(ctx.prefix);
        cells.extend(styled("  "));
        for (i, (label, action)) in HINTS.iter().take(n).enumerate() {
            if i > 0 {
                cells.extend(styled(SEP));
            }
            let mut target = styled(label);
            for cell in &mut target {
                cell.hit = Some(CellHit::Action(action));
            }
            cells.extend(target);
        }
        WidgetCells { cells }
    }
}

impl StatusWidget for HelpHintsWidget {
    fn render(&self, ctx: &WidgetContext<'_>) -> WidgetCells {
        Self::cells(ctx, HINTS.len())
    }

    /// Drop whole hints, then everything: a clipped hint teaches nothing.
    fn render_within(&self, ctx: &WidgetContext<'_>, budget: usize) -> WidgetCells {
        (1..=HINTS.len())
            .rev()
            .map(|n| Self::cells(ctx, n))
            .find(|cells| cells.len() <= budget)
            .unwrap_or(WidgetCells { cells: Vec::new() })
    }
}

pub(in crate::widget) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
    Ok(Box::new(HelpHintsWidget))
}
