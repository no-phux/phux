//! `time` widget — strftime-formatted wall clock.

use std::collections::BTreeMap;
use std::time::Duration;

use phux_config::widget::kinds::time::{TimeOpts, parse};

use super::{StatusWidget, WidgetCells, WidgetContext, WidgetError};

/// `time` widget: [`WidgetContext::now`] in the local zone, formatted with a
/// strftime spec validated when the options were parsed.
#[derive(Debug, Clone)]
pub struct TimeWidget {
    /// strftime-style format string.
    pub format: String,
}

impl StatusWidget for TimeWidget {
    fn render(&self, ctx: &WidgetContext<'_>) -> WidgetCells {
        let text = jiff::Zoned::try_from(ctx.now)
            .and_then(|now| jiff::fmt::strtime::format(&self.format, &now))
            .unwrap_or_default();
        WidgetCells::from_text(&text)
    }

    /// Whole or not at all: a clipped clock (`Wed 18:…`) tells no time.
    fn render_within(&self, ctx: &WidgetContext<'_>, budget: usize) -> WidgetCells {
        let cells = self.render(ctx);
        if cells.cells.len() <= budget {
            cells
        } else {
            WidgetCells { cells: Vec::new() }
        }
    }

    fn poll_interval(&self) -> Option<Duration> {
        Some(Duration::from_secs(1))
    }
}

pub(super) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    let TimeOpts { format } = parse(opts)?;
    Ok(Box::new(TimeWidget { format }))
}
