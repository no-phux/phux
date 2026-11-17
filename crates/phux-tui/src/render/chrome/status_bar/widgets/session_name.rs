//! `session-name` widget — the current session name, optionally prefixed,
//! truncated, and templated.

use std::collections::BTreeMap;

use phux_config::widget::kinds::session_name::{SessionNameOpts, parse};

use super::{StatusWidget, WidgetCells, WidgetContext, WidgetError};

/// `session-name` widget: `prefix + format` with `{name}` replaced by the
/// session name truncated to `max_len` characters.
#[derive(Debug, Clone)]
pub struct SessionNameWidget {
    /// Literal prefix prepended to the formatted name.
    pub prefix: Option<String>,
    /// Maximum `char` count of the name itself; `None` is unbounded.
    pub max_len: Option<usize>,
    /// Render template (`{name}`).
    pub format: String,
}

impl StatusWidget for SessionNameWidget {
    #[allow(
        clippy::literal_string_with_formatting_args,
        reason = "`{name}` is this widget's TOML placeholder"
    )]
    fn render(&self, ctx: &WidgetContext<'_>) -> WidgetCells {
        let name: String = self.max_len.map_or_else(
            || ctx.session_name.to_owned(),
            |n| ctx.session_name.chars().take(n).collect(),
        );
        let prefix = self.prefix.as_deref().unwrap_or("");
        WidgetCells::from_text(&format!("{prefix}{}", self.format.replace("{name}", &name)))
    }
}

pub(super) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    let SessionNameOpts {
        prefix,
        max_len,
        format,
    } = parse(opts)?;
    Ok(Box::new(SessionNameWidget {
        prefix,
        max_len,
        format,
    }))
}
