//! `text` widget — a literal string, for separators, labels, and decoration.

use std::collections::BTreeMap;

use crate::widget::{StatusWidget, WidgetCells, WidgetContext, WidgetError, kinds};

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

pub(in crate::widget) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    let kinds::text::TextOpts { value } = kinds::text::parse(opts)?;
    Ok(Box::new(TextWidget { value }))
}
