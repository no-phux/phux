//! `exit` widget — the focused pane's last command exit code (OSC 133 `D`).
//! Renders nothing until a code is known.

use std::collections::BTreeMap;

use phux_config::widget::kinds::exit_status::{ExitOpts, parse};

use super::{StatusWidget, WidgetCells, WidgetContext, WidgetError};

/// `exit` widget.
#[derive(Debug, Clone)]
pub struct ExitWidget {
    /// Render format (`{code}`).
    pub format: String,
}

impl StatusWidget for ExitWidget {
    #[allow(
        clippy::literal_string_with_formatting_args,
        reason = "`{code}` is this widget's TOML placeholder"
    )]
    fn render(&self, ctx: &WidgetContext<'_>) -> WidgetCells {
        ctx.last_exit.map_or_else(
            || WidgetCells { cells: Vec::new() },
            |code| WidgetCells::from_text(&self.format.replace("{code}", &code.to_string())),
        )
    }
}

pub(super) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    let ExitOpts { format } = parse(opts)?;
    Ok(Box::new(ExitWidget { format }))
}
