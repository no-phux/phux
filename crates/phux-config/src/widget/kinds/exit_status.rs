//! `exit` options — the focused pane's last command exit code.

use std::collections::BTreeMap;

use crate::widget::{WidgetError, WidgetKindSpec, WidgetOptSpec, reject_unknown_opts, string_opt};

const KIND: &str = "exit";

/// Doc and validation spec of the `exit` kind.
pub const SPEC: WidgetKindSpec = WidgetKindSpec {
    kind: KIND,
    summary: "The focused pane's last command exit code, fed by the OSC-133 \
              `D`-mark (`command_finished.exit_code`), so it requires shell \
              integration. Renders nothing until a command finishes with a \
              reported code.",
    options: &[WidgetOptSpec {
        name: "format",
        aliases: &[],
        doc: "string, default `\"{code}\"` — render template; every \
              `{code}` occurrence is replaced with the decimal exit code.",
    }],
    validate: |opts| parse(opts).map(drop),
};

/// Resolved `exit` options.
#[derive(Debug, Clone)]
pub struct ExitOpts {
    /// Render format (`{code}`).
    pub format: String,
}

/// Validate and resolve `exit` options.
///
/// # Errors
///
/// [`WidgetError::InvalidOption`] for an unknown or mistyped option.
pub fn parse(opts: &BTreeMap<String, toml::Value>) -> Result<ExitOpts, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
    Ok(ExitOpts {
        format: string_opt(KIND, opts, "format")?.unwrap_or_else(|| "{code}".to_owned()),
    })
}
