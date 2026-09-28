//! `switch` widget — a clickable chip that opens the fleet switcher: the
//! visible signpost for `prefix A` on terminals too narrow for the sidebar.

use std::collections::BTreeMap;

use crate::widget::{
    CellHit, CellStyle, StatusWidget, WidgetCells, WidgetContext, WidgetError, WidgetKindSpec,
    WidgetOptSpec, reject_unknown_opts, string_opt, style_opt,
};

const KIND: &str = "switch";

pub(in crate::widget) const SPEC: WidgetKindSpec = WidgetKindSpec {
    kind: KIND,
    summary: "A clickable chip that opens the agent-fleet switcher (the \
              same overlay `prefix A` opens). Every cell of the chip, \
              padding included, is a click target. Pair it with \
              `max-cols` to surface it only on terminals too narrow for \
              the sidebar and the full tab strip.",
    options: &[
        WidgetOptSpec {
            name: "label",
            aliases: &[],
            doc: "string, default `\"switch\"` — the chip's text. Rendered \
                  with one space of padding on each side.",
        },
        WidgetOptSpec {
            name: "chip",
            aliases: &[],
            doc: "style table, default bold reverse-video — the chip's \
                  style. Reverse video by default so the affordance reads \
                  as a button on any palette.",
        },
    ],
};

/// `switch` widget.
#[derive(Debug, Clone)]
pub struct SwitchWidget {
    /// The chip's text, padded by one space each side.
    pub label: String,
    /// Style of every chip cell.
    pub chip: CellStyle,
}

impl Default for SwitchWidget {
    fn default() -> Self {
        Self {
            label: "switch".to_owned(),
            chip: CellStyle {
                bold: true,
                reverse: true,
                ..CellStyle::default()
            },
        }
    }
}

impl StatusWidget for SwitchWidget {
    /// The padded label, every cell (padding included) a click target.
    fn render(&self, _ctx: &WidgetContext<'_>) -> WidgetCells {
        let style = (!self.chip.is_plain()).then(|| self.chip.clone());
        let mut cells = WidgetCells::from_styled(&format!(" {} ", self.label), style);
        for cell in &mut cells.cells {
            cell.hit = Some(CellHit::Switch);
        }
        cells
    }

    /// Whole or not at all: a clipped chip is a smaller target claiming the
    /// same columns.
    fn render_within(&self, ctx: &WidgetContext<'_>, budget: usize) -> WidgetCells {
        let cells = self.render(ctx);
        if cells.len() <= budget {
            cells
        } else {
            WidgetCells { cells: Vec::new() }
        }
    }
}

pub(in crate::widget) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
    let defaults = SwitchWidget::default();
    Ok(Box::new(SwitchWidget {
        label: string_opt(KIND, opts, "label")?.unwrap_or(defaults.label),
        chip: style_opt(KIND, opts, "chip")?.unwrap_or(defaults.chip),
    }))
}
