//! `session-name` widget — the current session name, optionally prefixed,
//! truncated, and templated.

use std::collections::BTreeMap;

use crate::widget::{
    StatusWidget, WidgetCells, WidgetContext, WidgetError, WidgetKindSpec, WidgetOptSpec,
    positive_opt, reject_unknown_opts, string_opt,
};

const KIND: &str = "session-name";

pub(in crate::widget) const SPEC: WidgetKindSpec = WidgetKindSpec {
    kind: KIND,
    summary: "The current session's name, optionally truncated, templated \
              via `format`, and prefixed.",
    options: &[
        WidgetOptSpec {
            name: "format",
            aliases: &[],
            doc: "string, default `\"{name}\"` — render template; every \
                  `{name}` occurrence is replaced with the (truncated) \
                  session name.",
        },
        WidgetOptSpec {
            name: "prefix",
            aliases: &[],
            doc: "string, optional — literal text prepended verbatim to \
                  the formatted output.",
        },
        WidgetOptSpec {
            name: "max-len",
            aliases: &["max_len"],
            doc: "integer `> 0`, optional — truncate the session name \
                  itself to this many characters (prefix and format \
                  literals not counted); no ellipsis.",
        },
    ],
};

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

impl SessionNameWidget {
    /// Construct a `SessionNameWidget` with the default `{name}` format.
    #[must_use]
    pub fn new(prefix: Option<String>, max_len: Option<usize>) -> Self {
        Self {
            prefix,
            max_len,
            format: "{name}".to_owned(),
        }
    }
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

pub(in crate::widget) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
    let mut widget = SessionNameWidget::new(
        string_opt(KIND, opts, "prefix")?,
        positive_opt(KIND, opts, "max-len", Some("max_len"))?,
    );
    if let Some(format) = string_opt(KIND, opts, "format")? {
        widget.format = format;
    }
    Ok(Box::new(widget))
}
