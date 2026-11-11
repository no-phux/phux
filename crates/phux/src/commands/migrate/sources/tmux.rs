//! tmux: every line decided, bindings through the `config import-tmux`
//! translator.

use std::collections::BTreeMap;
use std::path::Path;

use phux_config::keybind::parse_chord;
use toml::Value;

use super::{
    Class, Ctx, Line, Source, Table, Verdict, against_default, expand, keep_redundant_unbinds,
    read_tree, skip, tilde,
};
use crate::commands::MigrateSource;
use crate::commands::config::import_tmux::translate;

pub(super) fn read(path: &Path, ctx: &Ctx<'_>) -> Source {
    let mut files = Vec::new();
    let mut rows = Vec::new();
    // tmux's own default until a `set prefix` line changes it.
    let mut prefix = "C-b".to_owned();
    let mut options = BTreeMap::new();
    let mut decide =
        |file: &Path, line: &str| tmux_line(file, line, &mut prefix, &mut options, ctx);
    read_tree(path, &mut files, &mut rows, &mut decide);
    let mut source = Source {
        kind: MigrateSource::Tmux,
        files,
        rows,
        note: None,
        prefix: Some(prefix),
    };
    keep_redundant_unbinds(&mut source, ctx);
    source
}

fn tmux_line(
    file: &Path,
    line: &str,
    prefix: &mut String,
    options: &mut BTreeMap<String, String>,
    ctx: &Ctx<'_>,
) -> Line {
    let Ok(words) = shell_words::split(line) else {
        return skip(Class::Other, "tmux could not parse this line either");
    };
    let Some(first) = words.first() else {
        return Line::Nothing;
    };
    match first.as_str() {
        "bind" | "bind-key" => tmux_bind(line, prefix, options, ctx),
        "unbind" | "unbind-key" => tmux_unbind(&words[1..], ctx),
        "set" | "set-option" | "setw" | "set-window-option" => {
            tmux_set(&words[1..], prefix, options, ctx)
        }
        "source" | "source-file" => {
            let Some(target) = words[1..].iter().find(|w| !w.starts_with('-')) else {
                return skip(Class::Other, "names no file");
            };
            let include = expand(
                target,
                file.parent().unwrap_or_else(|| Path::new("/")),
                ctx.home,
            );
            let reason = if include.is_file() {
                format!("loads {}; its lines follow", tilde(&include, ctx.home))
            } else {
                format!("loads {}, which does not exist", tilde(&include, ctx.home))
            };
            Line::Row(Class::Other, Verdict::Skip(reason), Some(include))
        }
        "run" | "run-shell" if line.contains("tpm") => skip(
            Class::Setting,
            "the tmux plugin manager; tmux plugins are not migrated",
        ),
        _ => skip(Class::Setting, "a tmux command with no phux counterpart"),
    }
}

fn tmux_bind(line: &str, prefix: &str, options: &BTreeMap<String, String>, ctx: &Ctx<'_>) -> Line {
    let binding = match translate::binding(line) {
        Ok(binding) => binding,
        Err(err) => return skip(Class::Binding, err),
    };
    let table = match binding.table.as_str() {
        "prefix" => Table::Prefix,
        "root" => Table::Global,
        "copy-mode" | "copy-mode-vi" => {
            return skip(
                Class::Binding,
                "a copy-mode binding; phux copy mode has its own keys",
            );
        }
        other => {
            return skip(
                Class::Binding,
                format!("a binding in the tmux key table {other}, which phux does not have"),
            );
        }
    };
    let Ok(key) = translate::chord(&binding.key) else {
        return skip(
            Class::Binding,
            format!("phux cannot spell the tmux key {}", binding.key),
        );
    };
    if table == Table::Global && parse_chord(&key).ok() == parse_chord(prefix).ok() {
        return skip(
            Class::Binding,
            "binds the prefix itself, which tmux never lets through",
        );
    }
    let mut command: Vec<_> = binding.command.iter().map(String::as_str).collect();
    // phux opens new panes in the focused pane's directory already
    // (`defaults.cwd-inheritance = "inherit-focused"`).
    if matches!(command.first(), Some(&("split-window" | "new-window")))
        && let Some(at) = command
            .windows(2)
            .position(|w| w == ["-c", "#{pane_current_path}"])
    {
        command.drain(at..at + 2);
    }
    let mapped = translate::map_command(&command, &key, prefix)
        .or_else(|| translate::window_target(&command, options));
    let Some((value, note)) = mapped else {
        return skip(
            Class::Binding,
            "no phux action does what this tmux command does",
        );
    };
    if value.as_str() == Some("none") {
        // `bind C-a send-prefix`: phux sends the prefix when pressed twice.
        return Line::Row(Class::Binding, Verdict::Default { table, key }, None);
    }
    let mut notes: Vec<String> = note.map(str::to_owned).into_iter().collect();
    if binding.repeat {
        notes.push("tmux repeats it without the prefix; phux needs the prefix each time".into());
    }
    let note = (!notes.is_empty()).then(|| notes.join("; "));
    Line::Row(
        Class::Binding,
        against_default(ctx, table, key, value, note),
        None,
    )
}

fn tmux_unbind(args: &[String], ctx: &Ctx<'_>) -> Line {
    let mut table = Table::Prefix;
    let mut words = args.iter();
    let mut key = None;
    while let Some(word) = words.next() {
        match word.as_str() {
            "-a" => {
                return skip(
                    Class::Unbinding,
                    "removes every tmux binding; phux keeps its own defaults",
                );
            }
            "-n" => table = Table::Global,
            "-T" => match words.next().map(String::as_str) {
                Some("root") => table = Table::Global,
                Some("prefix") => {}
                _ => {
                    return skip(
                        Class::Unbinding,
                        "removes a binding from a tmux key table phux does not have",
                    );
                }
            },
            flag if flag.len() > 1 && flag.starts_with('-') => {}
            _ => {
                key = Some(word);
                break;
            }
        }
    }
    let Some(key) = key.and_then(|k| translate::chord(k).ok()) else {
        return skip(Class::Unbinding, "phux cannot spell this key");
    };
    let verdict = against_default(ctx, table, key, Value::String("none".into()), None);
    Line::Row(Class::Unbinding, verdict, None)
}

fn tmux_set(
    args: &[String],
    prefix: &mut String,
    options: &mut BTreeMap<String, String>,
    ctx: &Ctx<'_>,
) -> Line {
    let mut words = args.iter();
    let mut name = None;
    while let Some(word) = words.next() {
        if word == "-t" {
            words.next();
        } else if !word.starts_with('-') {
            name = Some(word);
            break;
        }
    }
    let Some(name) = name else {
        return skip(Class::Setting, "names no option");
    };
    let value = words.next().cloned().unwrap_or_default();
    options.insert(name.clone(), value.clone());
    if name == "prefix" {
        let Ok(key) = translate::chord(&value) else {
            return skip(Class::Setting, format!("phux cannot spell the key {value}"));
        };
        prefix.clone_from(&key);
        let verdict = against_default(
            ctx,
            Table::Keybindings,
            "prefix".into(),
            Value::String(key),
            None,
        );
        return Line::Row(Class::Setting, verdict, None);
    }
    tmux_option(name, &value, ctx)
}

/// A tmux option other than `prefix`: a phux setting where one does the same
/// job, else whether the user loses anything.
fn tmux_option(name: &str, value: &str, ctx: &Ctx<'_>) -> Line {
    let write = |table, key: &str, value: Value, note: Option<&str>| {
        let verdict = against_default(ctx, table, key.into(), value, note.map(str::to_owned));
        Line::Row(Class::Setting, verdict, None)
    };
    let same = |reason: &str| Line::Row(Class::Setting, Verdict::Same(reason.into()), None);
    let on = match value {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    };
    match (name, on) {
        ("mouse", Some(on)) => write(Table::Defaults, "mouse", on.into(), None),
        ("history-limit", _) if value.parse::<u32>().is_ok() => write(
            Table::Defaults,
            "history-limit",
            Value::Integer(value.parse::<u32>().map(i64::from).unwrap_or_default()),
            Some("phux also caps each pane's scrollback at `defaults.history-bytes`"),
        ),
        ("default-shell", _) if !value.is_empty() => {
            write(Table::Defaults, "shell", value.into(), None)
        }
        ("status-position", _) if matches!(value, "top" | "bottom") => {
            write(Table::Status, "position", value.into(), None)
        }
        ("set-clipboard", _) if matches!(value, "on" | "external") => {
            write(Table::Defaults, "clipboard-write", "allow".into(), None)
        }
        ("set-clipboard", Some(false)) => {
            write(Table::Defaults, "clipboard-write", "deny".into(), None)
        }
        ("mode-keys", _) if value == "vi" => same("phux copy mode already uses vi keys"),
        ("mode-keys", _) => skip(
            Class::Setting,
            "phux copy mode uses vi keys and the arrows; it has no emacs keys",
        ),
        ("renumber-windows", Some(true)) => {
            same("phux numbers tabs by position, so there are never gaps")
        }
        ("renumber-windows", _) => skip(
            Class::Setting,
            "phux numbers tabs by position and always closes gaps",
        ),
        ("base-index", _) if value == "0" => same("phux numbers tabs from 0 too"),
        ("base-index", _) => skip(
            Class::Setting,
            "phux numbers tabs from 0, and no setting starts them at 1",
        ),
        ("pane-base-index", _) => skip(
            Class::Setting,
            "phux has no setting for where pane numbers start",
        ),
        ("escape-time", _) => {
            same("phux waits a fixed 10 ms after Escape; there is nothing to set")
        }
        ("focus-events", Some(true)) => {
            same("phux always reports focus to programs that ask for it")
        }
        ("default-terminal", _) => same(
            "it names tmux to the programs inside it; phux sets TERM for its own panes \
             (`defaults.term`)",
        ),
        ("terminal-overrides" | "terminal-features", _) => {
            same("it tells tmux what your terminal can do; phux needs no override")
        }
        (
            "status-style"
            | "status-left"
            | "status-right"
            | "status-justify"
            | "status-format"
            | "status-interval"
            | "status-bg"
            | "status-fg"
            | "status-left-length"
            | "status-right-length"
            | "status-left-style"
            | "status-right-style",
            _,
        ) => skip(
            Class::Setting,
            "phux builds its status bar from widgets (`[status]`), not tmux formats; its \
             default bar already shows the session name and the time",
        ),
        ("prefix2", _) => skip(Class::Setting, "phux has one prefix"),
        _ if name.starts_with('@') => skip(
            Class::Setting,
            "a tmux plugin option; tmux plugins are not migrated",
        ),
        _ => skip(
            Class::Setting,
            format!("the tmux option {name} has no phux setting"),
        ),
    }
}
