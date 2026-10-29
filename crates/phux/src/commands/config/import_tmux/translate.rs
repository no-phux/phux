//! A deliberately finite translator, not a tmux interpreter. Extra arguments,
//! command chains, targets, formats and scripts must never disappear in a map.

use std::collections::BTreeMap;
use std::path::Path;

use phux_config::keybind::{parse_chord, parse_chord_sequence};
use serde::Serialize;
use toml::{Table, Value};

use super::snapshot::Snapshot;

#[derive(Debug, Serialize)]
pub(super) struct Entry {
    pub source: String,
    pub status: &'static str,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub(super) struct Plan {
    pub toml: String,
    pub entries: Vec<Entry>,
    pub notes: Vec<String>,
}

impl Plan {
    fn record(
        &mut self,
        source: impl Into<String>,
        status: &'static str,
        detail: impl Into<String>,
    ) {
        self.entries.push(Entry {
            source: source.into(),
            status,
            detail: detail.into(),
        });
    }
}

pub(super) fn translate(snapshot: &Snapshot) -> Result<Plan, String> {
    let options = parse_options(&snapshot.session)?;
    let window = parse_options(&snapshot.window)?;
    let prefix = options
        .get("prefix")
        .ok_or("tmux did not report a prefix")?;
    let prefix = chord(prefix)?;
    let mut plan = Plan {
        toml: String::new(), entries: Vec::new(),
        notes: vec![
            "Imports global tmux preferences and prefix/root bindings for the phux TUI. Per-session/window overrides and desktop shortcuts are separate.".into(),
            "Plugins, hooks, custom/copy-mode tables, status formats and live sessions/processes are not migrated. Shell commands are never executed.".into(),
            "Phux's shipped prefix bindings are disabled in this layer before importing tmux keys. Unsupported keys stay unbound; phux passes an unbound key through to the pane.".into(),
            "Phux tab targets are positions, not persistent tmux window numbers. Existing phux layers/settings take precedence when applying.".into(),
        ],
    };
    let mut root = Table::new();
    let mut kb = Table::new();
    kb.insert("prefix".into(), Value::String(prefix.clone()));
    plan.record(format!("prefix {prefix}"), "mapped", "keybindings.prefix");
    preferences(&options, &window, &mut root, &mut plan);
    let defaults = phux_config::parse_with_defaults("", Path::new("tmux-import.toml"))
        .map_err(|e| e.to_string())?;
    let mut prefix_table: Table = defaults
        .keybindings
        .prefix_table
        .keys()
        .map(|key| (key.clone(), Value::String("none".into())))
        .collect();
    let mut global = Table::new();
    let mut skipped = BTreeMap::new();
    let mut seen = BTreeMap::new();
    for line in snapshot.keys.lines().filter(|line| !line.trim().is_empty()) {
        let binding = binding(line)?;
        let table = match binding.table.as_str() {
            "prefix" => &mut prefix_table,
            "root" => &mut global,
            name => {
                *skipped.entry(name.to_owned()).or_insert(0_usize) += 1;
                continue;
            }
        };
        import_binding(&binding, &prefix, &options, table, &mut seen, &mut plan);
    }
    for (table, count) in skipped {
        plan.record(
            format!("table {table}"),
            "unsupported",
            format!("{count} bindings; custom and copy-mode tables need manual configuration"),
        );
    }
    kb.insert("prefix-table".into(), Value::Table(prefix_table));
    kb.insert("global".into(), Value::Table(global));
    root.insert("keybindings".into(), Value::Table(kb));
    plan.toml = format!(
        "# Imported from tmux by phux config import-tmux.\n# Native phux TOML; tmux is not needed after import.\n{}",
        toml::to_string_pretty(&root).map_err(|e| e.to_string())?
    );
    let check = phux_config::check::check(&plan.toml, Path::new("tmux-import.toml"))
        .map_err(|e| e.to_string())?;
    if !check.is_ok() {
        return Err(format!(
            "translated config failed validation: {:?}",
            check.findings
        ));
    }
    Ok(plan)
}

fn parse_options(input: &str) -> Result<BTreeMap<String, String>, String> {
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

fn preferences(
    options: &BTreeMap<String, String>,
    window: &BTreeMap<String, String>,
    root: &mut Table,
    plan: &mut Plan,
) {
    let mut defaults = Table::new();
    for (source, target) in [
        ("mouse", "mouse"),
        ("default-shell", "shell"),
        ("history-limit", "history-limit"),
    ] {
        let Some(value) = options.get(source) else {
            continue;
        };
        let converted = default_preference(source, value);
        if let Some(value) = converted {
            defaults.insert(target.into(), value);
            plan.record(source, "mapped", format!("defaults.{target}"));
        } else {
            plan.record(source, "unsupported", "value has no direct phux equivalent");
        }
    }
    root.insert("defaults".into(), Value::Table(defaults));
    if let Some(position) = options
        .get("status-position")
        .filter(|v| matches!(v.as_str(), "top" | "bottom"))
    {
        root.insert(
            "status".into(),
            Value::Table(Table::from_iter([(
                "position".into(),
                Value::String(position.clone()),
            )])),
        );
        plan.record("status-position", "mapped", "status.position");
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

#[derive(Debug)]
struct Binding {
    table: String,
    key: String,
    command: Vec<String>,
    repeat: bool,
    source: String,
}

fn binding(line: &str) -> Result<Binding, String> {
    let words = shell_words::split(line).map_err(|e| format!("cannot decode tmux binding: {e}"))?;
    let mut rest = words.as_slice();
    if !matches!(rest.first().map(String::as_str), Some("bind-key" | "bind")) {
        return Err("unexpected tmux list-keys output".into());
    }
    rest = &rest[1..];
    let mut table = "prefix".to_owned();
    let mut repeat = false;
    while let [flag, tail @ ..] = rest {
        match flag.as_str() {
            "-r" => {
                repeat = true;
                rest = tail;
            }
            "-T" | "-N" => {
                let [value, tail @ ..] = tail else {
                    return Err("incomplete tmux binding flag".into());
                };
                if flag == "-T" {
                    table.clone_from(value);
                }
                rest = tail;
            }
            "--" => {
                rest = tail;
                break;
            }
            _ => break,
        }
    }
    let [key, command @ ..] = rest else {
        return Err("tmux binding has no key".into());
    };
    if command.is_empty() {
        return Err("tmux binding has no command".into());
    }
    Ok(Binding {
        table,
        key: key.clone(),
        command: command.to_vec(),
        repeat,
        source: line.to_owned(),
    })
}

fn chord(key: &str) -> Result<String, String> {
    let key = match key {
        "BSpace" => "Backspace",
        "BTab" => "BackTab",
        "PPage" => "PageUp",
        "NPage" => "PageDown",
        _ => key,
    };
    // A tmux key is exactly one chord, never a phux space-separated sequence.
    parse_chord(key).map_err(|e| e.to_string())?;
    Ok(key.to_owned())
}

fn import_binding(
    binding: &Binding,
    prefix: &str,
    options: &BTreeMap<String, String>,
    table: &mut Table,
    seen: &mut BTreeMap<(String, Vec<phux_config::keybind::KeyChord>), usize>,
    plan: &mut Plan,
) {
    let Ok(key) = chord(&binding.key) else {
        plan.record(
            &binding.source,
            "unsupported",
            "key/mouse event cannot be represented by a phux chord",
        );
        return;
    };
    let Ok(sequence) = parse_chord_sequence(&key) else {
        return;
    };
    if binding.table == "root" && parse_chord(&key).ok() == parse_chord(prefix).ok() {
        plan.record(
            &binding.source,
            "unsupported",
            "the prefix takes precedence over a global binding",
        );
        return;
    }
    if let Some(previous) = seen.insert(
        (binding.table.clone(), sequence.clone()),
        plan.entries.len(),
    ) {
        // tmux distinguishes some keys that phux canonicalizes to one chord.
        let detail = "collides with another tmux key after phux key normalization; disabled";
        plan.entries[previous].status = "unsupported";
        detail.clone_into(&mut plan.entries[previous].detail);
        table.retain(|other, _| parse_chord_sequence(other).ok().as_ref() != Some(&sequence));
        table.insert(key, Value::String("none".into()));
        plan.record(&binding.source, "unsupported", detail);
        return;
    }
    // Drop the shipped spelling too (e.g. S-x vs X), so the generated layer
    // never contains two spellings of the same physical key.
    table.retain(|other, _| parse_chord_sequence(other).ok().as_ref() != Some(&sequence));
    let command: Vec<_> = binding.command.iter().map(String::as_str).collect();
    let mapped = map_command(&command, &key, prefix).or_else(|| window_target(&command, options));
    let unsupported = mapped.is_none();
    let (value, note) = mapped.unwrap_or_else(|| {
        (
            Value::String("none".into()),
            Some("unsupported command/arguments; left unbound"),
        )
    });
    let detail = note.map_or_else(|| format!("{} -> {value}", binding.key), str::to_owned);
    let status = if unsupported {
        "unsupported"
    } else if note.is_some() || binding.repeat {
        "approximate"
    } else {
        "mapped"
    };
    let detail = if binding.repeat {
        format!("{detail}; tmux repeat timing is not imported")
    } else {
        detail
    };
    table.insert(key, value);
    plan.record(&binding.source, status, detail);
}

fn action(name: &str, args: &[(&str, Value)]) -> Value {
    if args.is_empty() {
        return Value::String(name.into());
    }
    let mut table = Table::from_iter([("action".into(), Value::String(name.into()))]);
    table.extend(args.iter().map(|(k, v)| ((*k).into(), v.clone())));
    Value::Table(table)
}

fn map_command(command: &[&str], key: &str, prefix: &str) -> Option<(Value, Option<&'static str>)> {
    if let Some(mapped) = fixed_command(command) {
        return Some(mapped);
    }
    let value = match command {
        ["split-window"] | ["split-window", "-v"] => {
            action("split-pane", &[("direction", "horizontal".into())])
        }
        ["split-window", "-h"] => action("split-pane", &[("direction", "vertical".into())]),
        ["select-pane", direction] => direction_action("focus-direction", direction, None)?,
        ["resize-pane", direction] => direction_action("resize-pane", direction, Some(1))?,
        ["resize-pane", direction, amount] => direction_action(
            "resize-pane",
            direction,
            Some(amount.parse::<u16>().ok().filter(|n| *n > 0)?),
        )?,
        ["send-prefix"] if parse_chord(key).ok() == parse_chord(prefix).ok() => {
            return Some((
                Value::String("none".into()),
                Some("pressing the prefix twice sends it through in phux"),
            ));
        }
        _ => return adapted_prompt(command),
    };
    Some((value, None))
}

fn fixed_command(command: &[&str]) -> Option<(Value, Option<&'static str>)> {
    // Whole argv matches: adding any argument makes a row stop matching.
    let mappings: &[(&[&str], &str, Option<&str>)] = &[
        (&["detach-client"], "detach", None),
        (&["last-pane"], "last-pane", None),
        (&["next-window"], "next-window", None),
        (&["previous-window"], "previous-window", None),
        (&["new-window"], "new-window", None),
        (&["kill-pane"], "kill-pane", None),
        (&["rename-window"], "rename-window", None),
        (&["rename-session"], "rename-session", None),
        (&["resize-pane", "-Z"], "toggle-zoom", None),
        (&["select-pane", "-t", ".+"], "next-pane", None),
        (&["select-pane", "-t", ".-"], "previous-pane", None),
        (&["switch-client", "-n"], "next-session", None),
        (&["switch-client", "-p"], "previous-session", None),
        (&["switch-client", "-l"], "last-session", None),
        (
            &["copy-mode"],
            "copy-mode",
            Some("enters phux copy mode; navigation/selection keys differ"),
        ),
        (&["list-keys"], "show-help", Some("opens phux help")),
        (
            &["command-prompt"],
            "command-palette",
            Some("opens phux's action palette; tmux command syntax is not supported"),
        ),
        (
            &["choose-tree", "-Zw"],
            "window-picker",
            Some("opens phux's tab picker"),
        ),
        (
            &["choose-window"],
            "window-picker",
            Some("opens phux's tab picker"),
        ),
        (
            &["choose-tree", "-Zs"],
            "session-picker",
            Some("opens phux's session picker"),
        ),
        (
            &["choose-session"],
            "session-picker",
            Some("opens phux's session picker"),
        ),
    ];
    mappings
        .iter()
        .find(|(words, _, _)| *words == command)
        .map(|(_, name, note)| (action(name, &[]), *note))
}

fn adapted_prompt(command: &[&str]) -> Option<(Value, Option<&'static str>)> {
    let (name, note) = match command {
        ["confirm-before", "-p", _, "kill-window"] => (
            "kill-window",
            "phux confirms closing the active tab using its own prompt",
        ),
        [
            "command-prompt",
            "-I",
            "#W",
            "{",
            "rename-window",
            "%%",
            "}",
        ]
        | ["command-prompt", "-I", "#W", "rename-window %%"] => {
            ("rename-window", "opens phux's rename-tab prompt")
        }
        [
            "command-prompt",
            "-I",
            "#S",
            "{",
            "rename-session",
            "%%",
            "}",
        ]
        | ["command-prompt", "-I", "#S", "rename-session %%"] => {
            ("rename-session", "opens phux's rename-session prompt")
        }
        ["source-file", path] if !path.starts_with('-') => {
            ("reload-config", "reloads phux config, not the tmux file")
        }
        _ => return None,
    };
    Some((action(name, &[]), Some(note)))
}

fn direction_action(name: &str, flag: &str, amount: Option<u16>) -> Option<Value> {
    let direction = match flag {
        "-L" => "left",
        "-R" => "right",
        "-U" => "up",
        "-D" => "down",
        _ => return None,
    };
    let mut args = vec![("direction", Value::String(direction.into()))];
    if let Some(amount) = amount {
        args.push(("amount", Value::Integer(i64::from(amount))));
    }
    Some(action(name, &args))
}

fn window_target(
    command: &[&str],
    options: &BTreeMap<String, String>,
) -> Option<(Value, Option<&'static str>)> {
    let ["select-window", "-t", target] = command else {
        return None;
    };
    let target = target.strip_prefix(':')?;
    let target = target.strip_prefix('=').unwrap_or(target);
    // Only numeric current-session targets, never names, relative offsets,
    // formats, or a session-qualified target.
    if target.is_empty() || !target.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let base = options
        .get("base-index")
        .map_or(Some(0), |v| v.parse::<u32>().ok())?;
    let index = target.parse::<u32>().ok()?.checked_sub(base)?;
    Some((
        action(
            "select-window",
            &[("index", Value::Integer(i64::from(index)))],
        ),
        Some(
            "uses tab position after subtracting tmux base-index; gaps in tmux window numbering cannot be preserved",
        ),
    ))
}

#[cfg(test)]
mod tests;
