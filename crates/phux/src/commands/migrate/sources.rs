//! Find the configs `phux migrate` reads and decide every line of them: the
//! exact phux line to write, why nothing is written, or "translate this
//! yourself" for the agent. Reads files only; never runs a program.

mod ghostty;
mod raw;
mod tmux;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use phux_config::keybind::{KeyChord, parse_chord_sequence};
use phux_config::{Action, KeybindingsCfg};
use toml::Value;

use crate::commands::MigrateSource;

/// Keys macOS keeps for itself, as Ghostty spells them (sorted modifiers).
pub(super) const MACOS_KEYS: &[&str] = &[
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
    /// The prefix the program used, for a program that has one (tmux).
    pub prefix: Option<String>,
}

/// What `phux migrate` asked the user, or took from its flags.
#[derive(Debug, Default)]
pub(super) struct Setup {
    /// The phux prefix chosen; `None` keeps phux's own.
    pub prefix: Option<String>,
    /// Mirror tmux as closely as phux allows, instead of keeping phux's
    /// defaults wherever they already do what the user wanted.
    pub advanced: bool,
}

impl Setup {
    /// The phux prefix the migrated config ends up with.
    pub(super) fn chosen_prefix(&self) -> String {
        self.prefix
            .clone()
            .unwrap_or_else(|| shipped_bindings().prefix)
    }
}

/// What every decider needs: `$HOME` (for `~`), the shipped bindings and
/// the phux theme already selected, if any, and what the user chose.
pub(super) struct Ctx<'a> {
    pub home: &'a Path,
    pub defaults: &'a KeybindingsCfg,
    pub theme: Option<&'a str>,
    pub setup: &'a Setup,
}

impl Ctx<'_> {
    /// The phux prefix the migrated config ends up with.
    pub(super) fn prefix(&self) -> &str {
        self.setup
            .prefix
            .as_deref()
            .unwrap_or(&self.defaults.prefix)
    }
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
    /// Settings, not bindings.
    Defaults,
    Status,
}

impl Table {
    pub(super) const fn header(self) -> &'static str {
        match self {
            Self::Keybindings => "[keybindings]",
            Self::Prefix => "[keybindings.prefix-table]",
            Self::Global => "[keybindings.global]",
            Self::Defaults => "[defaults]",
            Self::Status => "[status]",
        }
    }

    /// Whether its keys are key chords, not setting names.
    pub(super) const fn binds_keys(self) -> bool {
        matches!(self, Self::Prefix | Self::Global)
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
    /// Nothing is written and something is lost, for this reason: the user
    /// has to know.
    Skip(String),
    /// Nothing is written and nothing is lost, for this reason: the source
    /// line keeps working, or phux already behaves that way.
    Covered(String),
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
/// narrows it; empty means all. `theme` is the phux theme already selected.
pub(super) fn discover(
    home: &Path,
    xdg: Option<&Path>,
    only: &[MigrateSource],
    theme: Option<&str>,
    setup: &Setup,
) -> Vec<Source> {
    let config = xdg.map_or_else(|| home.join(".config"), Path::to_path_buf);
    let defaults = shipped_bindings();
    let ctx = Ctx {
        home,
        defaults: &defaults,
        theme,
        setup,
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
        out.push(tmux::read(&path, &ctx));
    }
    if wanted(MigrateSource::Zellij)
        && let Some(path) = first(vec![config.join("zellij/config.kdl")])
    {
        out.push(raw::zellij(&path, theme));
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
            out.push(ghostty::read(&files, &ctx));
        }
    }
    if wanted(MigrateSource::Herdr)
        && let Some(path) = first(vec![config.join("herdr/config.toml")])
    {
        out.push(raw::herdr(&path, theme));
    }
    if wanted(MigrateSource::Rex)
        && let Some(path) = first(vec![config.join("rex/init.lua")])
    {
        out.push(raw::rex(&path));
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
    let mut last: BTreeMap<(Table, String, Vec<KeyChord>), usize> = BTreeMap::new();
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
            source.rows[index].verdict = Verdict::Covered(reason);
        }
    }
}

/// The key a written (or already-default) row claims: a setting by name,
/// a binding by the keys it means.
pub(super) fn identity(row: &Row) -> Option<(Table, String, Vec<KeyChord>)> {
    let (Verdict::Write { table, key, .. } | Verdict::Default { table, key }) = &row.verdict else {
        return None;
    };
    if !table.binds_keys() {
        return Some((*table, key.clone(), Vec::new()));
    }
    Some((*table, String::new(), parse_chord_sequence(key).ok()?))
}

/// `~/...` when under `$HOME`, for the prompt.
pub(super) fn tilde(path: &Path, home: &Path) -> String {
    path.strip_prefix(home).map_or_else(
        |_| path.display().to_string(),
        |rest| format!("~/{}", rest.display()),
    )
}

pub(super) fn expand(raw: &str, base: &Path, home: &Path) -> PathBuf {
    raw.strip_prefix("~/")
        .map_or_else(|| base.join(raw), |rest| home.join(rest))
}

/// Read `path` and the files it includes, depth-first, each once.
pub(super) fn read_tree(
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
pub(super) enum Line {
    /// Blank or a comment.
    Nothing,
    /// A row, and the file it includes, read right after it.
    Row(Class, Verdict, Option<PathBuf>),
}

pub(super) fn skip(class: Class, reason: impl Into<String>) -> Line {
    Line::Row(class, Verdict::Skip(reason.into()), None)
}

/// A source's theme line. Never migrated: a phux theme already selected
/// wins, and with none selected the user picks one.
pub(super) fn theme(program: &str, value: &str, selected: Option<&str>) -> Line {
    let reason = selected.map_or_else(
        || {
            "the theme is not migrated; pick one with `phux theme list` and `phux theme set`"
                .to_owned()
        },
        |name| format!("kept your phux theme `{name}`; {program}'s `{value}` is not applied"),
    );
    skip(Class::Setting, reason)
}

pub(super) fn covered(class: Class, reason: impl Into<String>) -> Line {
    Line::Row(class, Verdict::Covered(reason.into()), None)
}

/// A phux setting from a source line, against the shipped config: the
/// value phux already ships is `Default`, any other is written.
fn setting(table: Table, key: &str, value: Value) -> Verdict {
    let name = match table {
        Table::Status => "status",
        _ => "defaults",
    };
    let shipped = phux_config::parse_with_defaults("", Path::new("config.toml"))
        .ok()
        .and_then(|cfg| Value::try_from(&cfg).ok())
        .and_then(|cfg| cfg.get(name)?.get(key).cloned());
    let key = key.to_owned();
    if shipped.as_ref() == Some(&value) {
        return Verdict::Default { table, key };
    }
    Verdict::Write {
        table,
        key,
        value,
        note: None,
        replaces: shipped.map(|v| v.to_string()),
    }
}

/// Turn a translated key and value into a verdict against the shipped
/// table: the same binding is `Default`, a different one replaces it.
pub(super) fn against_default(
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
        Table::Defaults | Table::Status => return setting(table, &key, value),
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
        None if value.as_str() == Some("none") => Verdict::Covered(
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
