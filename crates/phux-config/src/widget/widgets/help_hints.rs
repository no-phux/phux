use std::collections::BTreeMap;

use crate::widget::{
    CellStyle, StatusWidget, WidgetCells, WidgetContext, WidgetError, reject_unknown_opts,
};

const KIND: &str = "help-hints";

/// `help-hints` widget.
#[derive(Debug, Clone, Copy, Default)]
pub struct HelpHintsWidget;

impl StatusWidget for HelpHintsWidget {
    fn render(&self, ctx: &WidgetContext<'_>) -> WidgetCells {
        let mut text = String::with_capacity(ctx.prefix.len().saturating_mul(3) + 32);
        text.push_str(ctx.prefix);
        text.push_str(" ? help | ");
        text.push_str(ctx.prefix);
        text.push_str(" : palette | ");
        text.push_str(ctx.prefix);
        text.push_str(" [ copy");
        WidgetCells::from_styled(
            &text,
            Some(CellStyle {
                dim: true,
                ..CellStyle::default()
            }),
        )
    }
}

pub(in crate::widget) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    reject_unknown_opts(KIND, opts, &[])?;
    Ok(Box::new(HelpHintsWidget))
}
