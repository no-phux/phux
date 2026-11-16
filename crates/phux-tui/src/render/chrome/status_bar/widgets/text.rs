//! `text` widget — a literal string, for separators, labels, and decoration.

use std::collections::BTreeMap;

use phux_config::widget::kinds::text::{TextOpts, parse};

use super::{StatusWidget, WidgetCells, WidgetContext, WidgetError};

/// `text` widget: renders [`Self::value`] verbatim.
#[derive(Debug, Clone)]
pub struct TextWidget {
    /// The literal text.
    pub value: String,
}

impl StatusWidget for TextWidget {
    fn render(&self, _ctx: &WidgetContext<'_>) -> WidgetCells {
        WidgetCells::from_text(&self.value)
    }
}

pub(super) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    let TextOpts { value } = parse(opts)?;
    Ok(Box::new(TextWidget { value }))
}
