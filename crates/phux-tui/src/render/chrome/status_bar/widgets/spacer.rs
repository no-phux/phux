//! `spacer` widget — elastic blank space that pushes its neighbours apart.
//!
//! Zero natural width; on a row that fits it is paid an even share of the
//! slack (see [`super::StatusBar::render`]), and on a full row it
//! renders nothing.

use std::collections::BTreeMap;

use phux_config::widget::kinds::spacer::parse;

use super::{Cell, StatusWidget, WidgetCells, WidgetContext, WidgetError};

/// `spacer` widget.
#[derive(Debug, Clone, Copy, Default)]
pub struct SpacerWidget;

impl StatusWidget for SpacerWidget {
    fn render(&self, _ctx: &WidgetContext<'_>) -> WidgetCells {
        WidgetCells { cells: Vec::new() }
    }

    /// Exactly `budget` unstyled blanks, so a widget-level `style` fills the
    /// gap.
    fn render_within(&self, _ctx: &WidgetContext<'_>, budget: usize) -> WidgetCells {
        WidgetCells {
            cells: vec![Cell::default(); budget],
        }
    }

    fn elastic(&self, _ctx: &WidgetContext<'_>) -> bool {
        true
    }
}

pub(super) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    parse(opts)?;
    Ok(Box::new(SpacerWidget))
}
