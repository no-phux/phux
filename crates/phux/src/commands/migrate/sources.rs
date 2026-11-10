//! Find the configs `phux migrate` reads and decide every line of them: the
//! exact phux line to write, why nothing is written, or "translate this
//! yourself" for the agent. Reads files only; never runs a program.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use phux_config::keybind::{KeyChord, parse_chord, parse_chord_sequence};
use phux_config::{Action, KeybindingsCfg};
use toml::Value;

use crate::commands::MigrateSource;
use crate::commands::config::import_tmux::translate;

/// Keys macOS keeps for itself, as Ghostty spells them (sorted modifiers).
const MACOS_KEYS: &[&str] = &[
    "cmd+q",
    "cmd+h",
    "cmd+m",
    "cmd+tab",
    "cmd+space",
    "cmd+shift+3",
    "cmd+shift+4",
    "cmd+shift+5",
    "cmd+ctrl+q",
];

/// How many files one source may pull in through `source`/`config-file`.
const MAX_FILES: usize = 32;

/// One program's config: every file it loads, and every line decided.
#[derive(Debug)]
pub(super) struct Source {
    pub kind: MigrateSource,
    pub files: Vec<PathBuf>,
    pub rows: Vec<Row>,
    /// Something the agent must know about the whole source.
    pub note: Option<String>,
}

/// What every decider needs: `$HOME` (for `~`) and the shipped bindings.
struct Ctx<'a> {
    home: &'a Path,
    defaults: &'a KeybindingsCfg,
}

/// One line of a source file.
#[derive(Debug)]
pub(super) struct Row {
    pub file: PathBuf,
    pub line: usize,
    pub text: String,
    pub class: Class,
    pub verdict: Verdict,
}

/// What kind of line it is, for the counts the JSON reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Class {
    Binding,
    Unbinding,
    Setting,
    /// Loads another file, or only gives structure to the lines around it.
    Other,
}

/// The phux table a line writes into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Table {
    /// `[keybindings]` itself: the `prefix` key.
    Keybindings,
    Prefix,
    Global,
}

impl Table {
    pub(super) const fn header(self) -> &'static str {
        match self {
            Self::Keybindings => "[keybindings]",
            Self::Prefix => "[keybindings.prefix-table]",
            Self::Global => "[keybindings.global]",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Verdict {
    /// Write `key = value` under `table`. `replaces` names the shipped
    /// binding it replaces; `note` says how it differs from the source.
    Write {
        table: Table,
        key: String,
        value: Value,
        note: Option<String>,
        replaces: Option<String>,
    },
    /// phux already binds this key this way; nothing to write.
    Default { table: Table, key: String },
    /// Nothing is written, for this reason.
    Skip(String),
    /// The agent translates it.
    Raw,
}

impl Source {
    /// `bindings`, `unbindings`, `settings` line counts.
    pub(super) fn found(&self) -> (usize, usize, usize) {
        let count = |class| self.rows.iter().filter(|r| r.class == class).count();
        (
            count(Class::Binding),
            count(Class::Unbinding),
            count(Class::Setting),
        )
    }
}

/// Every source found under `home`, in `MigrateSource` order. `only`
/// narrows it; empty means all.
pub(super) fn discover(home: &Path, xdg: Option<&Path>, only: &[MigrateSource]) -> Vec<Source> {
    let config = xdg.map_or_else(|| home.join(".config"), Path::to_path_buf);
    let defaults = shipped_bindings();
    let ctx = Ctx {
        home,
        defaults: &defaults,
    };
    let wanted = |kind| only.is_empty() || only.contains(&kind);
    let first = |paths: Vec<PathBuf>| paths.into_iter().find(|p| p.is_file());
    let mut out = Vec::new();
    if wanted(MigrateSource::Tmux)
        && let Some(path) = first(vec![
            home.join(".tmux.conf"),
            config.join("tmux/tmux.conf"),
            home.join(".config/tmux/tmux.conf"),
        ])
    {
        out.push(tmux(&path, &ctx));
    }
    if wanted(MigrateSource::Zellij)
        && let Some(path) = first(vec![config.join("zellij/config.kdl")])
    {
        out.push(zellij(&path));
    }
    if wanted(MigrateSource::Ghostty) {
        // Ghostty loads the XDG file, then the macOS one on top.
        let files: Vec<_> = [
            config.join("ghostty/config"),
            config.join("ghostty/config.ghostty"),
            home.join("Library/Application Support/com.mitchellh.ghostty/config"),
            home.join("Library/Application Support/com.mitchellh.ghostty/config.ghostty"),
        ]
        .into_iter()
        .filter(|p| p.is_file())
        .collect();
        if !files.is_empty() {
            out.push(ghostty(&files, &ctx));
        }
    }
    if wanted(MigrateSource::Herdr)
        && let Some(path) = first(vec![config.join("herdr/config.toml")])
    {
        out.push(herdr(&path));
    }
    for source in &mut out {
        last_line_wins(source, home);
    }
    out
}

/// The shipped prefix and global tables, to tell a default from a change.
pub(super) fn shipped_bindings() -> KeybindingsCfg {
    phux_config::parse_with_defaults("", Path::new("config.toml"))
        .map(|cfg| cfg.keybindings)
        .unwrap_or_default()
}

/// Inside one source, the line the program loads last wins: earlier
/// lines on the same key are not written.
fn last_line_wins(source: &mut Source, home: &Path) {
    let mut last: BTreeMap<(Table, Vec<KeyChord>), usize> = BTreeMap::new();
    for (index, row) in source.rows.iter().enumerate() {
        if let Some(id) = identity(row) {
            last.insert(id, index);
        }
    }
    for index in 0..source.rows.len() {
        let Some(id) = identity(&source.rows[index]) else {
            continue;
        };
        let winner = last[&id];
        if winner != index {
            let by = &source.rows[winner];
            let reason = format!(
                "{}:{} binds this key again later, and that line wins",
                tilde(&by.file, home),
                by.line
            );
            source.rows[index].verdict = Verdict::Skip(reason);
        }
    }
}

/// The key a written (or already-default) row claims.
pub(super) fn identity(row: &Row) -> Option<(Table, Vec<KeyChord>)> {
    let (Verdict::Write { table, key, .. } | Verdict::Default { table, key }) = &row.verdict else {
        return None;
    };
    if *table == Table::Keybindings {
        return Some((*table, Vec::new()));
    }
    Some((*table, parse_chord_sequence(key).ok()?))
}

/// `~/...` when under `$HOME`, for the prompt.
pub(super) fn tilde(path: &Path, home: &Path) -> String {
    path.strip_prefix(home).map_or_else(
        |_| path.display().to_string(),
        |rest| format!("~/{}", rest.display()),
    )
}

fn expand(raw: &str, base: &Path, home: &Path) -> PathBuf {
    raw.strip_prefix("~/")
        .map_or_else(|| base.join(raw), |rest| home.join(rest))
}

/// Read `path` and the files it includes, depth-first, each once.
fn read_tree(
    path: &Path,
    files: &mut Vec<PathBuf>,
    rows: &mut Vec<Row>,
    decide: &mut dyn FnMut(&Path, &str) -> Line,
) {
    if files.iter().any(|seen| seen == path) || files.len() >= MAX_FILES {
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    files.push(path.to_path_buf());
    for (index, raw) in text.lines().enumerate() {
        let trimmed = raw.trim();
        let decided = decide(path, trimmed);
        let Line::Row(class, verdict, include) = decided else {
            continue;
        };
        rows.push(Row {
            file: path.to_path_buf(),
            line: index + 1,
            text: trimmed.to_owned(),
            class,
            verdict,
        });
        if let Some(include) = include {
            read_tree(&include, files, rows, decide);
        }
    }
}

/// What a decider makes of one line.
enum Line {
    /// Blank or a comment.
    Nothing,
    /// A row, and the file it includes, read right after it.
    Row(Class, Verdict, Option<PathBuf>),
}

fn skip(class: Class, reason: impl Into<String>) -> Line {
    Line::Row(class, Verdict::Skip(reason.into()), None)
}

/// Turn a translated key and value into a verdict against the shipped
/// table: the same binding is `Default`, a different one replaces it.
fn against_default(
    ctx: &Ctx<'_>,
    table: Table,
    key: String,
    value: Value,
    note: Option<String>,
) -> Verdict {
    let defaults = ctx.defaults;
    let shipped = match table {
        Table::Prefix => &defaults.prefix_table,
        Table::Global => &defaults.global,
        Table::Keybindings => {
            return if value.as_str() == Some(defaults.prefix.as_str()) {
                Verdict::Default { table, key }
            } else {
                Verdict::Write {
                    table,
                    key,
                    value,
                    note,
                    replaces: Some(format!("the prefix {}", defaults.prefix)),
                }
            };
        }
    };
    let sequence = parse_chord_sequence(&key).ok();
    let existing = shipped
        .iter()
        .find(|(spelling, _)| parse_chord_sequence(spelling).ok() == sequence);
    let wanted: Option<Action> = value.clone().try_into().ok();
    match existing {
        Some((_, action)) if Some(action) == wanted.as_ref() => Verdict::Default { table, key },
        Some((_, action)) => Verdict::Write {
            table,
            key,
            value,
            note,
            replaces: Some(describe(action)),
        },
        None if value.as_str() == Some("none") => Verdict::Skip(
            "removes a binding; phux binds nothing on this key, so nothing is written".into(),
        ),
        None => Verdict::Write {
            table,
            key,
            value,
            note,
            replaces: None,
        },
    }
}

fn describe(action: &Action) -> String {
    Value::try_from(action).map_or_else(|_| format!("{action:?}"), |value| value.to_string())
}

// ---- tmux -------------------------------------------------------------------

fn tmux(path: &Path, ctx: &Ctx<'_>) -> Source {
    let mut files = Vec::new();
    let mut rows = Vec::new();
    // tmux's own default until a `set prefix` line changes it.
    let mut prefix = "C-b".to_owned();
    let mut options = BTreeMap::new();
    let mut decide =
        |file: &Path, line: &str| tmux_line(file, line, &mut prefix, &mut options, ctx);
    read_tree(path, &mut files, &mut rows, &mut decide);
    let note = (!options.contains_key("prefix")).then(|| {
        format!(
            "These files never set a prefix, so tmux used C-b. phux keeps its own prefix, {}: \
             every prefix-table line below fires after it. Do not change `prefix` unless I ask.",
            ctx.defaults.prefix
        )
    });
    Source {
        kind: MigrateSource::Tmux,
        files,
        rows,
        note,
    }
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
    let reason = if name.starts_with('@') {
        "a tmux plugin option; tmux plugins are not migrated".to_owned()
    } else {
        format!("the tmux option {name} is a setting; this migration moves key bindings only")
    };
    skip(Class::Setting, reason)
}

// ---- Ghostty ----------------------------------------------------------------

fn ghostty(paths: &[PathBuf], ctx: &Ctx<'_>) -> Source {
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
        "theme" => skip(
            Class::Setting,
            "the theme is not migrated; pick one with `phux theme list` and `phux theme set`",
        ),
        _ => skip(
            Class::Setting,
            format!("the Ghostty setting {name}; this migration moves key bindings only"),
        ),
    }
}

fn ghostty_keybind(value: &str, ctx: &Ctx<'_>) -> Line {
    if value == "clear" {
        return skip(
            Class::Unbinding,
            "clears Ghostty's own defaults; phux keeps its own",
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
        return skip(
            Class::Unbinding,
            "removes a Ghostty default; phux has nothing to remove",
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
            "phux key bindings have no Cmd key; Rune's Cmd shortcuts are built in"
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

// ---- zellij and herdr: the agent translates ----------------------------------

fn zellij(path: &Path) -> Source {
    let mut files = Vec::new();
    let mut rows = Vec::new();
    // Brace depth inside `keybinds { ... }`; 0 outside it.
    let mut depth = 0_usize;
    let mut decide = |_: &Path, line: &str| {
        if line.is_empty() || line.starts_with("//") {
            return Line::Nothing;
        }
        if depth == 0 && !line.starts_with("keybinds") {
            if line
                .trim_matches(|c| c == '{' || c == '}' || c == ' ')
                .is_empty()
            {
                return Line::Nothing;
            }
            return skip(
                Class::Setting,
                "a zellij setting; this migration moves key bindings only",
            );
        }
        // ponytail: counts braces without parsing KDL strings; a `{` inside
        // a quoted key would end the block early.
        depth = (depth + line.matches('{').count()).saturating_sub(line.matches('}').count());
        let class = if line.starts_with("unbind") {
            Class::Unbinding
        } else if line.starts_with("bind ") {
            Class::Binding
        } else {
            Class::Other
        };
        Line::Row(class, Verdict::Raw, None)
    };
    read_tree(path, &mut files, &mut rows, &mut decide);
    Source {
        kind: MigrateSource::Zellij,
        files,
        rows,
        note: None,
    }
}

fn herdr(path: &Path) -> Source {
    let mut files = Vec::new();
    let mut rows = Vec::new();
    let mut keys = false;
    let mut decide = |_: &Path, line: &str| {
        if line.is_empty() || line.starts_with('#') {
            return Line::Nothing;
        }
        if line.starts_with('[') {
            keys = line
                .trim_matches(|c| c == '[' || c == ']')
                .starts_with("keys");
            return if keys {
                Line::Row(Class::Other, Verdict::Raw, None)
            } else {
                Line::Nothing
            };
        }
        if keys {
            return Line::Row(Class::Binding, Verdict::Raw, None);
        }
        skip(
            Class::Setting,
            "a herdr setting; this migration moves key bindings only",
        )
    };
    read_tree(path, &mut files, &mut rows, &mut decide);
    Source {
        kind: MigrateSource::Herdr,
        files,
        rows,
        note: None,
    }
}
