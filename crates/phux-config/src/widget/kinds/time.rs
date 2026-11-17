//! `time` options — strftime-formatted wall clock.

use std::collections::BTreeMap;

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
    let sample = jiff::Timestamp::UNIX_EPOCH.to_zoned(jiff::tz::TimeZone::UTC);
    if jiff::fmt::strtime::format(format, &sample).is_err() {
        return Err(invalid(
            KIND,
            format!("invalid strftime format: {format:?}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// The kind's default and the shipped bar's clock render as they did
    /// under chrono: 2027-01-15 08:05 UTC is a Friday.
    #[test]
    fn shipped_clock_formats_render_unchanged() {
        let at = jiff::Timestamp::from_second(1_800_000_300)
            .unwrap()
            .to_zoned(jiff::tz::TimeZone::UTC);
        for (format, shown) in [("%H:%M", "08:05"), (" %a %H:%M ", " Fri 08:05 ")] {
            super::check_format(format).unwrap();
            assert_eq!(jiff::fmt::strtime::format(format, &at).unwrap(), shown);
        }
        assert!(super::check_format("%J").is_err());
    }
}
