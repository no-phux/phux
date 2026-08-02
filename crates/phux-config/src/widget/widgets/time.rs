//! `time` widget — strftime-formatted wall clock.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::format::{Item, StrftimeItems};
use chrono::{DateTime, Local};

use crate::widget::{StatusWidget, WidgetCells, WidgetContext, WidgetError, reject_unknown_opts};

/// Default strftime format if none is supplied.
const DEFAULT_FORMAT: &str = "%H:%M";
/// Widget kind, used in error messages.
const KIND: &str = "time";

/// `time` widget: renders [`WidgetContext::now`] formatted with
/// `strftime`-style directives.
///
/// Format is validated eagerly at build time (in the factory function);
/// render itself cannot fail and will not panic on the strftime spec.
#[derive(Debug, Clone)]
pub struct TimeWidget {
    /// strftime-style format string (validated at construction).
    pub format: String,
}

impl TimeWidget {
    /// Construct a `TimeWidget` with an explicit format string.
    ///
    /// # Errors
    ///
    /// Returns [`WidgetError::InvalidOption`] if `format` contains an
    /// invalid `strftime` directive.
    pub fn new(format: impl Into<String>) -> Result<Self, WidgetError> {
        let format = format.into();
        validate_strftime(&format)?;
        Ok(Self { format })
    }
}

impl StatusWidget for TimeWidget {
    fn render(&self, ctx: &WidgetContext<'_>) -> WidgetCells {
        // `now` is a `SystemTime`. Convert to `DateTime<Local>` for
        // strftime. We render in the *local* zone — the status bar is a
        // user-facing surface and 24:00 UTC in San Francisco is not
        // what a user wants on their bar.
        let dt: DateTime<Local> = ctx.now.into();
        // Format is pre-validated; this iterator yields no `Item::Error`
        // tokens, so `format_with_items` succeeds and we render its
        // `Display`. We still avoid `unwrap()` and fall back to an
        // empty strip on the impossible failure path.
        let items = StrftimeItems::new(&self.format).parse();
        let text = items
            .ok()
            .map(|items| dt.format_with_items(items.iter()).to_string())
            .unwrap_or_default();
        WidgetCells::from_text(&text)
    }

    fn poll_interval(&self) -> Option<Duration> {
        Some(Duration::from_secs(1))
    }
}

/// Factory: builds a [`TimeWidget`] from a TOML `opts` map.
///
/// Accepted keys:
/// - `format` (string, optional, default `"%H:%M"`) — strftime spec.
///
/// # Errors
///
/// Returns [`WidgetError::InvalidOption`] on an unknown option, a
/// wrong-typed `format`, or an invalid strftime directive.
pub(in crate::widget) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    reject_unknown_opts(KIND, opts, &["format"])?;
    let format = match opts.get("format") {
        None => DEFAULT_FORMAT.to_owned(),
        Some(toml::Value::String(s)) => s.clone(),
        Some(other) => {
            return Err(WidgetError::InvalidOption {
                kind: KIND.to_owned(),
                message: format!("`format` must be a string, got {}", other.type_str()),
            });
        }
    };
    let w = TimeWidget::new(format)?;
    Ok(Box::new(w))
}

/// Walk the strftime items and surface any parser-emitted `Error` token
/// as a [`WidgetError::InvalidOption`]. `StrftimeItems` is lazy and only
/// yields `Item::Error` for malformed directives (e.g. `%Q`).
fn validate_strftime(fmt: &str) -> Result<(), WidgetError> {
    for item in StrftimeItems::new(fmt) {
        if matches!(item, Item::Error) {
            return Err(WidgetError::InvalidOption {
                kind: KIND.to_owned(),
                message: format!("invalid strftime format: {fmt:?}"),
            });
        }
    }
    Ok(())
}
