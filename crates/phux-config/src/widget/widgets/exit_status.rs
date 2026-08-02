//! `exit` widget — the focused pane's last command exit code (phux-foz.4).
//!
//! Backed by [`WidgetContext::last_exit`], which the TUI feeds from the
//! server's `command_finished` agent events (OSC-133 `D`-mark exit codes,
//! per `docs/consumers/tui.md` §8.3). Renders nothing while no exit code
//! is known — before the first command finishes, and for shells whose
//! integration emits `OSC 133 ; D` without a code.

use std::collections::BTreeMap;

use crate::widget::{StatusWidget, WidgetCells, WidgetContext, WidgetError, reject_unknown_opts};

/// Widget kind, used in error messages.
const KIND: &str = "exit";
/// Default render format; `{code}` is the decimal exit code.
const DEFAULT_FORMAT: &str = "{code}";

/// `exit` widget.
#[derive(Debug, Clone)]
pub struct ExitWidget {
    /// Render format; every `{code}` occurrence is replaced with the
    /// decimal exit code.
    pub format: String,
}

impl ExitWidget {
    /// Construct an `ExitWidget` with an explicit format string.
    #[must_use]
    pub const fn new(format: String) -> Self {
        Self { format }
    }
}

impl StatusWidget for ExitWidget {
    #[allow(
        clippy::literal_string_with_formatting_args,
        reason = "`{code}` is this widget's documented TOML placeholder, not a Rust format arg"
    )]
    fn render(&self, ctx: &WidgetContext<'_>) -> WidgetCells {
        ctx.last_exit.map_or_else(
            || WidgetCells::from_text(""),
            |code| {
                let text = self.format.replace("{code}", &code.to_string());
                WidgetCells::from_text(&text)
            },
        )
    }

    // No `poll_interval` — exit repaints are event-driven (the bar redraws
    // when a `command_finished` event lands or focus moves).
}

/// Factory: builds an [`ExitWidget`] from a TOML `opts` map.
///
/// Accepted keys (per `docs/consumers/tui.md` §8.3):
/// - `format` (string, optional, default `"{code}"`).
///
/// # Errors
///
/// Returns [`WidgetError::InvalidOption`] on an unknown option or if
/// `format` is not a string.
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
    Ok(Box::new(ExitWidget::new(format)))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn render(widget: &ExitWidget, last_exit: Option<i32>) -> String {
        let ctx = WidgetContext {
            last_exit,
            ..WidgetContext::new(UNIX_EPOCH, "", "C-a", &[])
        };
        widget
            .render(&ctx)
            .cells
            .iter()
            .filter_map(|c| c.text.first())
            .collect()
    }

    #[test]
    fn renders_known_exit_code() {
        let w = ExitWidget::new("{code}".to_owned());
        assert_eq!(render(&w, Some(0)), "0");
        assert_eq!(render(&w, Some(127)), "127");
    }

    #[test]
    fn unknown_exit_renders_nothing() {
        let w = ExitWidget::new("{code}".to_owned());
        assert_eq!(render(&w, None), "");
    }

    #[test]
    fn format_substitutes_code_placeholder() {
        let w = ExitWidget::new("rc={code}".to_owned());
        assert_eq!(render(&w, Some(1)), "rc=1");
    }

    #[test]
    fn factory_rejects_non_string_format() {
        use crate::schema::WidgetSpec;
        use crate::widget::WidgetRegistry;
        let spec = WidgetSpec {
            kind: "exit".to_owned(),
            opts: std::iter::once(("format".to_owned(), toml::Value::Integer(1))).collect(),
        };
        assert!(matches!(
            WidgetRegistry::with_builtins().build(&spec),
            Err(WidgetError::InvalidOption { .. })
        ));
    }
}
