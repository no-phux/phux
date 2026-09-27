//! `time` widget — strftime-formatted wall clock.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::format::{Item, StrftimeItems};
use chrono::{DateTime, Local};

use crate::widget::{
    StatusWidget, WidgetCells, WidgetContext, WidgetError, WidgetKindSpec, WidgetOptSpec, invalid,
    reject_unknown_opts, string_opt,
};

const KIND: &str = "time";

pub(in crate::widget) const SPEC: WidgetKindSpec = WidgetKindSpec {
    kind: KIND,
    summary: "The wall clock, strftime-formatted, rendered in the local \
              time zone and repainted every second.",
    options: &[WidgetOptSpec {
        name: "format",
        aliases: &[],
        doc: "string, default `\"%H:%M\"` — strftime spec, validated at \
              build time (an invalid directive fails `phux config check` \
              and the bar build).",
    }],
};

/// `time` widget: [`WidgetContext::now`] in the local zone, formatted with a
/// strftime spec validated at construction.
#[derive(Debug, Clone)]
pub struct TimeWidget {
    /// strftime-style format string.
    pub format: String,
}

impl TimeWidget {
    /// Construct a `TimeWidget` with an explicit format string.
    ///
    /// # Errors
    ///
    /// [`WidgetError::InvalidOption`] for an invalid `strftime` directive.
    pub fn new(format: impl Into<String>) -> Result<Self, WidgetError> {
        let format = format.into();
        if StrftimeItems::new(&format).any(|item| matches!(item, Item::Error)) {
            return Err(invalid(
                KIND,
                format!("invalid strftime format: {format:?}"),
            ));
        }
        Ok(Self { format })
    }
}

impl StatusWidget for TimeWidget {
    fn render(&self, ctx: &WidgetContext<'_>) -> WidgetCells {
        let dt: DateTime<Local> = ctx.now.into();
        let text = StrftimeItems::new(&self.format)
            .parse()
            .map(|items| dt.format_with_items(items.iter()).to_string())
            .unwrap_or_default();
        WidgetCells::from_text(&text)
    }

    fn poll_interval(&self) -> Option<Duration> {
        Some(Duration::from_secs(1))
    }
}

pub(in crate::widget) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
    let format = string_opt(KIND, opts, "format")?.unwrap_or_else(|| "%H:%M".to_owned());
    Ok(Box::new(TimeWidget::new(format)?))
}
