//! Decode snapshot options and translate preferences shared with `phux migrate`.

use std::collections::BTreeMap;

use toml::{Table, Value};

use super::Plan;

pub(super) fn parse_options(input: &str) -> Result<BTreeMap<String, String>, String> {
    input
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let words =
                shell_words::split(line).map_err(|e| format!("cannot decode tmux option: {e}"))?;
            match words.as_slice() {
                [key, value] => Ok((key.clone(), value.clone())),
                // tmux represents an empty array (e.g. pane-colours) by its
                // name alone. An explicit empty string is quoted instead.
                [key] => Ok((key.clone(), String::new())),
                _ => Err(format!(
                    "cannot decode tmux option: {}",
                    line.escape_debug()
                )),
            }
        })
        .collect()
}

/// tmux options with a phux setting: `(option, table, key)`.
const SETTINGS: &[(&str, &str, &str)] = &[
    ("mouse", "defaults", "mouse"),
    ("default-shell", "defaults", "shell"),
    ("history-limit", "defaults", "history-limit"),
    ("status-position", "status", "position"),
];

/// The phux setting a tmux option maps to, as `(table, key, value)`, with
/// no value when this one has no phux equivalent. `None` when phux has no
/// setting for the option at all.
pub(crate) fn setting(
    option: &str,
    value: &str,
) -> Option<(&'static str, &'static str, Option<Value>)> {
    let (_, table, key) = SETTINGS.iter().find(|(name, ..)| *name == option)?;
    Some((table, key, default_preference(option, value)))
}

pub(super) fn preferences(
    options: &BTreeMap<String, String>,
    window: &BTreeMap<String, String>,
    root: &mut Table,
    plan: &mut Plan,
) {
    let mut defaults = Table::new();
    let mut status = Table::new();
    for (option, ..) in SETTINGS {
        let Some(value) = options.get(*option) else {
            continue;
        };
        if let Some((table, key, Some(value))) = setting(option, value) {
            let target = if table == "status" {
                &mut status
            } else {
                &mut defaults
            };
            target.insert(key.into(), value);
            plan.record(*option, "mapped", format!("{table}.{key}"));
        } else {
            plan.record(
                *option,
                "unsupported",
                "value has no direct phux equivalent",
            );
        }
    }
    root.insert("defaults".into(), Value::Table(defaults));
    if !status.is_empty() {
        root.insert("status".into(), Value::Table(status));
    }
    unsupported_preferences(options, window, plan);
}

fn default_preference(source: &str, value: &str) -> Option<Value> {
    match source {
        "mouse" => match value {
            "on" => Some(Value::Boolean(true)),
            "off" => Some(Value::Boolean(false)),
            _ => None,
        },
        "history-limit" => value
            .parse::<u32>()
            .ok()
            .map(|n| Value::Integer(i64::from(n))),
        "status-position" => {
            matches!(value, "top" | "bottom").then(|| Value::String(value.to_owned()))
        }
        _ => (!value.is_empty()).then(|| Value::String(value.to_owned())),
    }
}

fn unsupported_preferences(
    options: &BTreeMap<String, String>,
    window: &BTreeMap<String, String>,
    plan: &mut Plan,
) {
    for key in [
        "prefix2",
        "base-index",
        "default-command",
        "status-left",
        "status-right",
        "status",
        "status-style",
    ] {
        if let Some(value) = options.get(key)
            && !value.is_empty()
            && !value.eq_ignore_ascii_case("none")
        {
            plan.record(
                format!("{key} {value}"),
                "unsupported",
                "kept phux behavior; no automatic equivalent",
            );
        }
    }
    for (key, value) in options.iter().filter(|(key, _)| key.starts_with('@')) {
        plan.record(
            format!("{key} {value}"),
            "unsupported",
            "tmux plugin/user option; plugins are not ported",
        );
    }
    for key in ["mode-keys", "pane-base-index"] {
        if let Some(value) = window.get(key) {
            plan.record(
                format!("{key} {value}"),
                "unsupported",
                "copy-mode keys and pane numbering differ in phux",
            );
        }
    }
    plan.notes.push("Other tmux options retain phux defaults. Mouse behavior, scrollback byte limits, new-pane working directories, prompts, and copy-mode navigation differ; this is not a compatibility runtime.".into());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_settings_distinguish_invalid_values_from_unknown_options() {
        assert_eq!(
            setting("status-position", "bottom"),
            Some(("status", "position", Some(Value::String("bottom".into()))))
        );
        assert_eq!(
            setting("status-position", "middle"),
            Some(("status", "position", None))
        );
        assert_eq!(
            setting("mouse", "off"),
            Some(("defaults", "mouse", Some(Value::Boolean(false))))
        );
        assert_eq!(
            setting("history-limit", "100000"),
            Some(("defaults", "history-limit", Some(Value::Integer(100_000))))
        );
        assert_eq!(
            setting("history-limit", "-1"),
            Some(("defaults", "history-limit", None))
        );
        assert_eq!(
            setting("default-shell", ""),
            Some(("defaults", "shell", None))
        );
        assert_eq!(setting("status-left", "custom"), None);
    }

    #[test]
    fn invalid_status_position_is_reported_instead_of_silently_dropped() {
        let snapshot = super::super::Snapshot {
            session: "prefix C-b\nstatus-position middle\n".into(),
            window: String::new(),
            keys: String::new(),
        };
        let plan = super::super::translate(&snapshot).expect("translation");
        assert!(
            plan.entries.iter().any(|entry| {
                entry.source == "status-position" && entry.status == "unsupported"
            })
        );
        let table: Table = toml::from_str(&plan.toml).expect("TOML");
        assert!(!table.contains_key("status"));
    }
}
