//! `exec` options — the first output line of a user-supplied program
//! (`docs/status-bar.md#widget-kinds`).

use std::collections::BTreeMap;
use std::time::Duration;

use crate::widget::{WidgetError, WidgetKindSpec, WidgetOptSpec, reject_unknown_opts};

const KIND: &str = "exec";

/// Doc and validation spec of the `exec` kind.
pub const SPEC: WidgetKindSpec = WidgetKindSpec {
    kind: KIND,
    summary: "The first output line of a user-supplied command, run by the \
              client on an interval as a bounded child process (10s hard \
              cap, `kill_on_drop`) — render never blocks on it; the bar \
              shows the last completed run, and a failed or timed-out run \
              keeps the last good output.",
    options: &[
        WidgetOptSpec {
            name: "command",
            aliases: &[],
            doc: "string or non-empty string array, required — a string \
                  runs via `/bin/sh -c` (so `~` and `$VAR` expand); an \
                  array is an argv run directly.",
        },
        WidgetOptSpec {
            name: "interval",
            aliases: &[],
            doc: "duration string (`\"500ms\"`, `\"30s\"`, `\"2m\"`, \
                  `\"1h\"`) or positive integer seconds, default `\"5s\"` \
                  — run cadence, floored to 1s.",
        },
        WidgetOptSpec {
            name: "parse-ansi",
            aliases: &["parse_ansi"],
            doc: "bool, default `true` — interpret SGR escape sequences in \
                  the output into per-cell styles; when `false` (and for \
                  every non-SGR escape either way) escapes are stripped.",
        },
    ],
    validate: |opts| parse(opts).map(drop),
};
const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);

/// Resolved `exec` options.
#[derive(Debug, Clone)]
pub struct ExecOpts {
    /// Command argv; a TOML string command resolves to `/bin/sh -c`.
    pub argv: Vec<String>,
    /// Run cadence as configured (the renderer applies its floor).
    pub interval: Duration,
    /// Interpret SGR escapes into cell styles, or strip them.
    pub parse_ansi: bool,
}

/// Validate and resolve `exec` options.
///
/// # Errors
///
/// [`WidgetError::InvalidOption`] for an unknown option, a missing or
/// malformed `command`, a bad `interval`, or a non-boolean `parse-ansi`.
pub fn parse(opts: &BTreeMap<String, toml::Value>) -> Result<ExecOpts, WidgetError> {
    reject_unknown_opts(&SPEC, opts)?;
    Ok(ExecOpts {
        argv: command_argv(opts.get("command"))?,
        interval: interval(opts.get("interval"))?,
        parse_ansi: parse_ansi(opts.get("parse-ansi").or_else(|| opts.get("parse_ansi")))?,
    })
}

/// The argv `command` names: a string runs via `/bin/sh -c`, an array is
/// the argv itself.
fn command_argv(command: Option<&toml::Value>) -> Result<Vec<String>, WidgetError> {
    match command {
        Some(toml::Value::String(s)) if !s.trim().is_empty() => {
            Ok(vec!["/bin/sh".to_owned(), "-c".to_owned(), s.clone()])
        }
        Some(toml::Value::Array(items)) if !items.is_empty() => array_argv(items),
        Some(toml::Value::String(_) | toml::Value::Array(_)) => {
            Err(invalid("`command` must not be empty".to_owned()))
        }
        Some(other) => Err(invalid(format!(
            "`command` must be a string or an array of strings, got {}",
            other.type_str()
        ))),
        None => Err(invalid("`command` is required".to_owned())),
    }
}

fn array_argv(items: &[toml::Value]) -> Result<Vec<String>, WidgetError> {
    let mut argv = Vec::with_capacity(items.len());
    for item in items {
        match item {
            toml::Value::String(s) if !s.is_empty() => argv.push(s.clone()),
            other => {
                return Err(invalid(format!(
                    "`command` array entries must be non-empty strings, got {}",
                    other.type_str()
                )));
            }
        }
    }
    Ok(argv)
}

fn interval(value: Option<&toml::Value>) -> Result<Duration, WidgetError> {
    match value {
        None => Ok(DEFAULT_INTERVAL),
        Some(toml::Value::String(s)) => parse_duration(s),
        Some(toml::Value::Integer(n)) if *n > 0 => {
            Ok(Duration::from_secs(u64::try_from(*n).unwrap_or(u64::MAX)))
        }
        Some(other) => Err(invalid(format!(
            "`interval` must be a duration string (e.g. \"30s\") or a positive \
             integer second count, got {}",
            other.type_str()
        ))),
    }
}

fn parse_ansi(value: Option<&toml::Value>) -> Result<bool, WidgetError> {
    match value {
        None => Ok(true),
        Some(toml::Value::Boolean(b)) => Ok(*b),
        Some(other) => Err(invalid(format!(
            "`parse-ansi` must be a boolean, got {}",
            other.type_str()
        ))),
    }
}

fn invalid(message: String) -> WidgetError {
    crate::widget::invalid(KIND, message)
}

/// Parse a positive integer followed by `ms`, `s`, `m`, or `h` (bare means
/// seconds).
fn parse_duration(s: &str) -> Result<Duration, WidgetError> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (digits, unit) = s.split_at(split);
    let value: u64 = digits
        .parse()
        .map_err(|_| invalid(format!("invalid `interval` duration: {s:?}")))?;
    if value == 0 {
        return Err(invalid("`interval` must be > 0".to_owned()));
    }
    let duration = match unit {
        "ms" => Duration::from_millis(value),
        "" | "s" => Duration::from_secs(value),
        "m" => Duration::from_secs(value.saturating_mul(60)),
        "h" => Duration::from_secs(value.saturating_mul(3600)),
        _ => return Err(invalid(format!("invalid `interval` unit in {s:?}"))),
    };
    Ok(duration)
}
