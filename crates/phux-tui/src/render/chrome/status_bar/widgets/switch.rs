//! `switch` widget — a clickable chip that opens the fleet switcher: the
//! visible signpost for `prefix A` on terminals too narrow for the sidebar.

use std::collections::BTreeMap;

use phux_config::widget::kinds::switch::{SwitchOpts, parse};

use super::{CellHit, CellStyle, StatusWidget, WidgetCells, WidgetContext, WidgetError};

/// `switch` widget.
#[derive(Debug, Clone)]
pub struct SwitchWidget {
    /// The chip's text, padded by one space each side.
    pub label: String,
    /// Style of every chip cell.
    pub chip: CellStyle,
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

pub(super) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    let SwitchOpts { label, chip } = parse(opts)?;
    Ok(Box::new(SwitchWidget { label, chip }))
}
