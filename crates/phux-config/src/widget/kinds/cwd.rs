//! `cwd` options — the focused pane's live working directory.

use std::collections::BTreeMap;

use crate::widget::{
    WidgetError, WidgetKindSpec, WidgetOptSpec, positive_opt, reject_unknown_opts, string_opt,
};

const KIND: &str = "cwd";

/// Doc and validation spec of the `cwd` kind.
pub const SPEC: WidgetKindSpec = WidgetKindSpec {
    kind: KIND,
    summary: "The focused pane's live working directory, fed by the \
              server's `cwd_changed` agent events (kernel-queried PTY-child \
              cwd). A `$HOME` prefix collapses to `~`; renders nothing \
              until the cwd is known.",
    options: &[
        WidgetOptSpec {
            name: "format",
            aliases: &[],
            doc: "string, default `\"{cwd}\"` — render template; every \
                  `{cwd}` occurrence is replaced with the (home-collapsed, \
                  truncated) directory.",
        },
        WidgetOptSpec {
            name: "truncate",
            aliases: &[],
            doc: "integer `> 0`, optional — maximum displayed characters \
                  of the directory itself (format literals not counted); \
                  truncation keeps the path's trailing end.",
        },
    ],
    validate: |opts| parse(opts).map(drop),
};

/// Resolved `cwd` options.
#[derive(Debug, Clone)]
pub struct CwdOpts {
    /// Render format (`{cwd}`).
    pub format: String,
    /// Maximum displayed `char` count of the directory; `None` is unbounded.
    pub truncate: Option<usize>,
}

/// Validate and resolve `cwd` options.
///
/// # Errors
///
/// [`WidgetError::InvalidOption`] for an unknown or mistyped option.
pub fn parse(opts: &BTreeMap<String, toml::Value>) -> Result<CwdOpts, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
    Ok(CwdOpts {
        format: string_opt(KIND, opts, "format")?.unwrap_or_else(|| "{cwd}".to_owned()),
        truncate: positive_opt(KIND, opts, "truncate", None)?,
    })
}
