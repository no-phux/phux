//! `spacer` widget — elastic blank space that pushes its neighbours apart.
//!
//! Zero natural width; on a row that fits it is paid an even share of the
//! slack (see [`crate::widget::StatusBar::render`]), and on a full row it
//! renders nothing.

use std::collections::BTreeMap;

use crate::widget::{
    Cell, StatusWidget, WidgetCells, WidgetContext, WidgetError, WidgetKindSpec,
    reject_unknown_opts,
};

pub(in crate::widget) const SPEC: WidgetKindSpec = WidgetKindSpec {
    kind: "spacer",
    summary: "Elastic blank space. Takes no width of its own and then \
              absorbs an even share of whatever columns the row has left \
              over, so the widgets on either side of it are pushed apart. \
              Renders nothing on a row with no room to spare, which makes \
              it the first thing to yield on a narrow terminal rather than \
              something that has to be configured away. Style it (`style = \
              { bg = ... }`) to paint the gap rather than leave it blank.",
    options: &[],
};

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

pub(in crate::widget) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
    Ok(Box::new(SpacerWidget))
}
