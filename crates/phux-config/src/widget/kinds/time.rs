//! `time` options — strftime-formatted wall clock.

use std::collections::BTreeMap;

use chrono::format::{Item, StrftimeItems};

use crate::widget::{
    WidgetError, WidgetKindSpec, WidgetOptSpec, invalid, reject_unknown_opts, string_opt,
};

const KIND: &str = "time";

/// Doc and validation spec of the `time` kind.
pub const SPEC: WidgetKindSpec = WidgetKindSpec {
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
    validate: |opts| parse(opts).map(drop),
};

/// Resolved `time` options.
#[derive(Debug, Clone)]
pub struct TimeOpts {
    /// strftime-style format string, known to hold only valid directives.
    pub format: String,
}

/// Validate and resolve `time` options.
///
/// # Errors
///
/// [`WidgetError::InvalidOption`] for an unknown or mistyped option, or an
/// invalid `strftime` directive.
pub fn parse(opts: &BTreeMap<String, toml::Value>) -> Result<TimeOpts, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
    let format = string_opt(KIND, opts, "format")?.unwrap_or_else(|| "%H:%M".to_owned());
    check_format(&format)?;
    Ok(TimeOpts { format })
}

/// Refuse a strftime spec carrying an invalid directive.
fn check_format(format: &str) -> Result<(), WidgetError> {
    if StrftimeItems::new(format).any(|item| matches!(item, Item::Error)) {
        return Err(invalid(
            KIND,
            format!("invalid strftime format: {format:?}"),
        ));
    }
    Ok(())
}
