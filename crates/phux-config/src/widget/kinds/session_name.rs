//! `session-name` options — the current session name, optionally prefixed,
//! truncated, and templated.

use std::collections::BTreeMap;

use crate::widget::{
    WidgetError, WidgetKindSpec, WidgetOptSpec, positive_opt, reject_unknown_opts, string_opt,
};

const KIND: &str = "session-name";

/// Doc and validation spec of the `session-name` kind.
pub const SPEC: WidgetKindSpec = WidgetKindSpec {
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
    validate: |opts| parse(opts).map(drop),
};

/// Resolved `session-name` options.
#[derive(Debug, Clone)]
pub struct SessionNameOpts {
    /// Literal prefix prepended to the formatted name.
    pub prefix: Option<String>,
    /// Maximum `char` count of the name itself; `None` is unbounded.
    pub max_len: Option<usize>,
    /// Render template (`{name}`).
    pub format: String,
}

/// Validate and resolve `session-name` options.
///
/// # Errors
///
/// [`WidgetError::InvalidOption`] for an unknown or mistyped option.
pub fn parse(opts: &BTreeMap<String, toml::Value>) -> Result<SessionNameOpts, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
    let prefix = string_opt(KIND, opts, "prefix")?;
    let max_len = positive_opt(KIND, opts, "max-len", Some("max_len"))?;
    let format = string_opt(KIND, opts, "format")?.unwrap_or_else(|| "{name}".to_owned());
    Ok(SessionNameOpts {
        prefix,
        max_len,
        format,
    })
}
