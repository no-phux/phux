//! `session-name` widget — renders the current session name, optionally
//! with a prefix, length cap, and `{name}` format template.

use std::collections::BTreeMap;

use crate::widget::{
    StatusWidget, WidgetCells, WidgetContext, WidgetError, WidgetKindSpec, WidgetOptSpec,
    reject_unknown_opts,
};

/// Widget kind, used in error messages.
const KIND: &str = "session-name";
/// Default render format; `{name}` is the (possibly truncated) session
/// name.
const DEFAULT_FORMAT: &str = "{name}";

/// Doc spec — the factory validates against this same const, so the
/// documented option surface is the enforced one (phux-i0e8.11.3).
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

/// `session-name` widget.
///
/// Output is `prefix + format.replace("{name}", truncate(session_name,
/// max_len))`. The default `format` of `"{name}"` keeps the historical
/// behavior (`prefix + truncated name`) unchanged. If `max_len` is set
/// and the session name exceeds it, the name is truncated to `max_len`
/// characters (no ellipsis — keep cell math simple at this stage;
/// callers that want one can opt in via a future option).
#[derive(Debug, Clone)]
pub struct SessionNameWidget {
    /// Optional literal prefix prepended verbatim to the formatted name.
    pub prefix: Option<String>,
    /// Maximum displayed `char` count of the session name itself
    /// (prefix and format literals not counted). `None` ⇒ unbounded.
    pub max_len: Option<usize>,
    /// Render template; every `{name}` occurrence is replaced with the
    /// (truncated) session name. Default `"{name}"`.
    pub format: String,
}

impl SessionNameWidget {
    /// Construct a `SessionNameWidget` with the default `format`.
    #[must_use]
    pub fn new(prefix: Option<String>, max_len: Option<usize>) -> Self {
        Self {
            prefix,
            max_len,
            format: DEFAULT_FORMAT.to_owned(),
        }
    }
}

impl StatusWidget for SessionNameWidget {
    #[allow(
        clippy::literal_string_with_formatting_args,
        reason = "`{name}` is this widget's documented TOML placeholder, not a Rust format arg"
    )]
    fn render(&self, ctx: &WidgetContext<'_>) -> WidgetCells {
        let truncated: String = self.max_len.map_or_else(
            || ctx.session_name.to_owned(),
            |n| ctx.session_name.chars().take(n).collect(),
        );
        let formatted = self.format.replace("{name}", &truncated);
        let mut out =
            String::with_capacity(self.prefix.as_deref().map_or(0, str::len) + formatted.len());
        if let Some(p) = &self.prefix {
            out.push_str(p);
        }
        out.push_str(&formatted);
        WidgetCells::from_text(&out)
    }

    // No `poll_interval` — session-name repaints are event-driven
    // (status bar redraws when the active session changes).
}

/// Factory: builds a [`SessionNameWidget`] from a TOML `opts` map.
///
/// Accepted keys (per [`SPEC`], rendered into `docs/reference/widgets.md`):
/// - `format` (string, optional, default `"{name}"`) — render template;
///   `{name}` is replaced with the (truncated) session name.
/// - `prefix` (string, optional) — literal prefix prepended to the
///   formatted output.
/// - `max-len` (integer, optional, `> 0`) — truncate the name to this
///   many characters. The kebab-case spelling matches the rest of the
///   schema; `max_len` is also accepted for ergonomics.
///
/// # Errors
///
/// Returns [`WidgetError::InvalidOption`] on an unknown option, a value
/// of the wrong type, or a `max-len` that is `<= 0`.
pub(in crate::widget) fn factory(
    opts: &BTreeMap<String, toml::Value>,
) -> Result<Box<dyn StatusWidget>, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
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
    let prefix = match opts.get("prefix") {
        None => None,
        Some(toml::Value::String(s)) => Some(s.clone()),
        Some(other) => {
            return Err(WidgetError::InvalidOption {
                kind: KIND.to_owned(),
                message: format!("`prefix` must be a string, got {}", other.type_str()),
            });
        }
    };
    let raw_len = opts.get("max-len").or_else(|| opts.get("max_len"));
    let max_len = match raw_len {
        None => None,
        Some(toml::Value::Integer(n)) if *n > 0 => {
            Some(usize::try_from(*n).map_err(|_| WidgetError::InvalidOption {
                kind: KIND.to_owned(),
                message: format!("`max-len` does not fit in usize: {n}"),
            })?)
        }
        Some(toml::Value::Integer(n)) => {
            return Err(WidgetError::InvalidOption {
                kind: KIND.to_owned(),
                message: format!("`max-len` must be > 0, got {n}"),
            });
        }
        Some(other) => {
            return Err(WidgetError::InvalidOption {
                kind: KIND.to_owned(),
                message: format!("`max-len` must be an integer, got {}", other.type_str()),
            });
        }
    };
    Ok(Box::new(SessionNameWidget {
        prefix,
        max_len,
        format,
    }))
}
