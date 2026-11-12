//! Ghostty: `keybind` lines whose action is clear become global bindings;
//! the rest go to the agent. Cmd is never bound.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use toml::Value;

use super::{
    Class, Ctx, Line, MACOS_KEYS, Source, Table, Verdict, against_default, expand, read_tree, skip,
    theme, tilde,
};
use crate::commands::MigrateSource;
use crate::commands::config::import_tmux::translate;

pub(super) fn read(paths: &[PathBuf], ctx: &Ctx<'_>) -> Source {
    let mut files = Vec::new();
    let mut rows = Vec::new();
    let mut decide = |file: &Path, line: &str| ghostty_line(file, line, ctx);
    for path in paths {
        read_tree(path, &mut files, &mut rows, &mut decide);
    }
    Source {
        kind: MigrateSource::Ghostty,
        files,
        rows,
        note: None,
        prefix: None,
    }
}

fn ghostty_line(file: &Path, line: &str, ctx: &Ctx<'_>) -> Line {
    if line.is_empty() || line.starts_with('#') {
        return Line::Nothing;
    }
    let Some((name, value)) = line.split_once('=') else {
        return skip(Class::Setting, "not a Ghostty `key = value` line");
    };
    let (name, value) = (name.trim(), value.trim().trim_matches('"'));
    match name {
        "keybind" => ghostty_keybind(value, ctx),
        "config-file" => {
            let raw = value.strip_prefix('?').unwrap_or(value);
            let include = expand(
                raw,
                file.parent().unwrap_or_else(|| Path::new("/")),
                ctx.home,
            );
            let reason = format!("loads {}; its lines follow", tilde(&include, ctx.home));
            Line::Row(Class::Other, Verdict::Skip(reason), Some(include))
        }
        "theme" => theme("Ghostty", value, ctx.theme),
        _ => super::covered(
            Class::Setting,
            format!("the Ghostty setting {name} keeps working while phux runs inside Ghostty"),
        ),
    }
}

fn ghostty_keybind(value: &str, ctx: &Ctx<'_>) -> Line {
    if value == "clear" {
        return super::covered(
            Class::Unbinding,
            "clears Ghostty's own defaults, which keeps working in Ghostty; phux keeps its own",
        );
    }
    // The first `=` that is not itself the key (`ctrl+==...`).
    let bytes = value.as_bytes();
    let Some(split) = (1..bytes.len()).find(|&i| bytes[i] == b'=' && bytes[i - 1] != b'+') else {
        return Line::Row(Class::Binding, Verdict::Raw, None);
    };
    let (mut trigger, action) = (&value[..split], &value[split + 1..]);
    while let Some((flag, rest)) = trigger.split_once(':')
        && matches!(flag, "global" | "all" | "unconsumed" | "performable")
    {
        trigger = rest;
    }
    if action == "unbind" {
        return super::covered(
            Class::Unbinding,
            "removes a Ghostty default, which keeps working in Ghostty; phux has nothing to remove",
        );
    }
    let chord = match ghostty_chord(trigger) {
        None => return Line::Row(Class::Binding, Verdict::Raw, None),
        Some(Err(reason)) => return skip(Class::Binding, reason),
        Some(Ok(chord)) => chord,
    };
    let Some(value) = ghostty_action(action) else {
        return Line::Row(Class::Binding, Verdict::Raw, None);
    };
    Line::Row(
        Class::Binding,
        against_default(ctx, Table::Global, chord, value, None),
        None,
    )
}

/// A Ghostty trigger as a phux key: `None` when it needs the agent (a
/// sequence, an unknown key name), `Err` when phux cannot bind it at all.
fn ghostty_chord(trigger: &str) -> Option<Result<String, String>> {
    if trigger.contains('>') {
        return None;
    }
    let (mods, key) = trigger.strip_suffix("++").map_or_else(
        || trigger.rsplit_once('+').unwrap_or(("", trigger)),
        |mods| (mods, "+"),
    );
    // Sorted, so `shift+cmd+3` and `cmd+shift+3` compare equal.
    let mut set = BTreeSet::new();
    for word in mods.split('+').filter(|w| !w.is_empty()) {
        set.insert(match word {
            "ctrl" | "control" => "ctrl",
            "alt" | "opt" | "option" => "alt",
            "shift" => "shift",
            "super" | "cmd" | "command" => "cmd",
            _ => return None,
        });
    }
    let key = ghostty_key(key)?;
    if set.contains("cmd") {
        let mut spelled: Vec<_> = set.iter().map(|m| (*m).to_owned()).collect();
        spelled.push(key.to_lowercase());
        let reason = if MACOS_KEYS.contains(&spelled.join("+").as_str()) {
            "macOS uses this key itself"
        } else {
            "phux key bindings have no Cmd key. It keeps working in Ghostty, but on \
             Ghostty's own tabs and splits, not phux's"
        };
        return Some(Err(reason.into()));
    }
    let mut phux = String::new();
    for (word, letter) in [("ctrl", "C-"), ("alt", "M-"), ("shift", "S-")] {
        if set.contains(word) {
            phux.push_str(letter);
        }
    }
    phux.push_str(&key);
    Some(translate::chord(&phux).map_err(|_| format!("phux cannot spell the key {trigger}")))
}

fn ghostty_key(key: &str) -> Option<String> {
    let named = match key {
        "enter" | "return" => "Enter",
        "escape" => "Esc",
        "tab" => "Tab",
        "space" => "Space",
        "backspace" => "Backspace",
        "delete" => "Delete",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "page_up" => "PageUp",
        "page_down" => "PageDown",
        "arrow_up" | "up" => "Up",
        "arrow_down" | "down" => "Down",
        "arrow_left" | "left" => "Left",
        "arrow_right" | "right" => "Right",
        "minus" => "-",
        "equal" => "=",
        "plus" => "+",
        "comma" => ",",
        "period" => ".",
        "slash" => "/",
        "backslash" => "\\",
        "semicolon" => ";",
        "apostrophe" | "quote" => "'",
        "grave_accent" | "backquote" => "`",
        "bracket_left" => "[",
        "bracket_right" => "]",
        "zero" | "digit_0" => "0",
        "one" | "digit_1" => "1",
        "two" | "digit_2" => "2",
        "three" | "digit_3" => "3",
        "four" | "digit_4" => "4",
        "five" | "digit_5" => "5",
        "six" | "digit_6" => "6",
        "seven" | "digit_7" => "7",
        "eight" | "digit_8" => "8",
        "nine" | "digit_9" => "9",
        _ => {
            if let Some(n) = key.strip_prefix('f').and_then(|n| n.parse::<u8>().ok())
                && (1..=24).contains(&n)
            {
                return Some(format!("F{n}"));
            }
            let mut chars = key.chars();
            return match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_graphic() => Some(c.to_ascii_lowercase().to_string()),
                _ => None,
            };
        }
    };
    Some(named.to_owned())
}

/// The phux action for a Ghostty action whose meaning is unambiguous.
fn ghostty_action(action: &str) -> Option<Value> {
    let (name, param) = action.split_once(':').unwrap_or((action, ""));
    let bare = |name: &str| Value::String(name.into());
    let with = |name: &str, args: Vec<(&str, Value)>| {
        let mut table = toml::Table::new();
        table.insert("action".into(), bare(name));
        table.extend(args.into_iter().map(|(k, v)| (k.to_owned(), v)));
        Value::Table(table)
    };
    let direction = |d: &str| {
        let d = match d {
            "left" => "left",
            "right" => "right",
            "up" | "top" => "up",
            "down" | "bottom" => "down",
            _ => return None,
        };
        Some(bare(d))
    };
    Some(match (name, param) {
        ("new_split", "right") => with("split-pane", vec![("direction", bare("vertical"))]),
        ("new_split", "down") => with("split-pane", vec![("direction", bare("horizontal"))]),
        ("goto_split", "previous") => bare("previous-pane"),
        ("goto_split", "next") => bare("next-pane"),
        ("goto_split", d) => with("focus-direction", vec![("direction", direction(d)?)]),
        ("resize_split", spec) => {
            let (d, amount) = spec.split_once(',')?;
            let amount: i64 = amount.trim().parse().ok().filter(|n| *n > 0)?;
            with(
                "resize-pane",
                vec![("direction", direction(d)?), ("amount", amount.into())],
            )
        }
        ("toggle_split_zoom", "") => bare("toggle-zoom"),
        ("new_tab", "") => bare("new-window"),
        ("next_tab", "") => bare("next-window"),
        ("previous_tab", "") => bare("previous-window"),
        ("goto_tab", n) => {
            let index = n.parse::<i64>().ok().filter(|n| *n > 0)? - 1;
            with("select-window", vec![("index", index.into())])
        }
        ("reload_config", "") => bare("reload-config"),
        ("toggle_command_palette", "") => bare("command-palette"),
        _ => return None,
    })
}
