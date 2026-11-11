//! Find the configs `phux migrate` reads and decide every line of them: the
//! exact phux line to write, why nothing is written, or "translate this
//! yourself" for the agent. Reads files only; never runs a program.

mod ghostty;
mod raw;
mod tmux;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use phux_config::keybind::{KeyChord, parse_chord, parse_chord_sequence};
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
    /// The prefix the program used (tmux only): its `set prefix`, else C-b.
    pub prefix: Option<String>,
}

/// How closely the migration follows the source.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Mode {
    /// Keep a phux default wherever it already does what the user wanted.
    #[default]
    Beginner,
    /// Mirror the source as closely as phux allows.
    Advanced,
}

/// What every decider needs: `$HOME` (for `~`), the shipped bindings and
/// settings, and the mode.
pub(super) struct Ctx<'a> {
    pub home: &'a Path,
    pub defaults: &'a KeybindingsCfg,
    /// The shipped config as TOML, to tell a default setting from a change.
    pub shipped: &'a Value,
    pub mode: Mode,
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
    /// `[defaults]`: a setting, not a key.
    Defaults,
    /// `[status]`: a setting, not a key.
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

    /// The TOML table name, for looking up a shipped setting.
    const fn name(self) -> &'static str {
        match self {
            Self::Keybindings | Self::Prefix | Self::Global => "keybindings",
            Self::Defaults => "defaults",
            Self::Status => "status",
        }
    }

    /// Its keys are setting names, written bare, not key chords.
    pub(super) const fn is_setting(self) -> bool {
        matches!(self, Self::Keybindings | Self::Defaults | Self::Status)
    }
}

/// What a written line claims: a key, or a setting by name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Ident {
    Keys(Vec<KeyChord>),
    Name(String),
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
    /// Nothing is written and the user loses nothing: it keeps working
    /// where it is, or phux already behaves this way. The reason says which.
    Same(String),
    /// Nothing is written, for this reason: the user loses it.
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
pub(super) fn discover(
    home: &Path,
    xdg: Option<&Path>,
    only: &[MigrateSource],
    mode: Mode,
) -> Vec<Source> {
    let config = xdg.map_or_else(|| home.join(".config"), Path::to_path_buf);
    let defaults = shipped_bindings();
    let shipped = shipped_settings();
    let ctx = Ctx {
        home,
        defaults: &defaults,
        shipped: &shipped,
        mode,
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
        out.push(raw::zellij(&path));
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
        out.push(raw::herdr(&path));
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

/// The whole shipped config as TOML, to tell a default setting from a change.
fn shipped_settings() -> Value {
    phux_config::parse_with_defaults("", Path::new("config.toml"))
        .ok()
        .and_then(|cfg| Value::try_from(cfg).ok())
        .unwrap_or_else(|| Value::Table(toml::Table::new()))
}

/// Settle the prefix: `chosen` is the one the user picked at setup. tmux's
/// own prefix line (or, with none, a line standing for tmux's built-in C-b)
/// becomes that choice, and the source's note says what was decided.
pub(super) fn apply_prefix(sources: &mut [Source], chosen: &str, shipped: &str) {
    let same = |a: &str, b: &str| parse_chord(a).ok() == parse_chord(b).ok();
    for source in sources.iter_mut() {
        let Some(tmux_prefix) = source.prefix.clone() else {
            continue;
        };
        let keep = same(chosen, shipped);
        let verdict = if keep && same(&tmux_prefix, shipped) {
            Verdict::Default {
                table: Table::Keybindings,
                key: "prefix".into(),
            }
        } else if keep {
            Verdict::Same(format!("I chose to keep phux's prefix, {shipped}"))
        } else {
            let note = (!same(chosen, &tmux_prefix))
                .then(|| format!("I chose {chosen}; tmux used {tmux_prefix}"));
            Verdict::Write {
                table: Table::Keybindings,
                key: "prefix".into(),
                value: Value::String(chosen.to_owned()),
                note,
                replaces: Some(format!("the prefix {shipped}")),
            }
        };
        let line = source.rows.iter().rposition(|row| {
            matches!(
                &row.verdict,
                Verdict::Write {
                    table: Table::Keybindings,
                    ..
                } | Verdict::Default {
                    table: Table::Keybindings,
                    ..
                }
            )
        });
        match line {
            Some(index) => source.rows[index].verdict = verdict,
            None if !keep => {
                let file = source.files.first().cloned().unwrap_or_default();
                source.rows.push(Row {
                    file,
                    line: 0,
                    text: format!("set -g prefix {tmux_prefix}"),
                    class: Class::Other,
                    verdict,
                });
            }
            None => {}
        }
        source.note = Some(if keep {
            format!(
                "tmux's prefix was {tmux_prefix}. I chose to keep phux's prefix, {shipped}: every \
                 prefix-table line below fires after it. Do not change `prefix`."
            )
        } else {
            format!(
                "I chose {chosen} as my phux prefix: write the prefix line below, and every \
                 prefix-table line fires after it."
            )
        });
    }
}

/// Beginner mode: an unbind of a shipped prefix key is not written while
/// phux still does the same thing on another key, so the user keeps a key
/// and loses nothing (`unbind '"'` beside phux's own `-` split).
pub(super) fn keep_redundant_unbinds(source: &mut Source, ctx: &Ctx<'_>) {
    if ctx.mode == Mode::Advanced {
        return;
    }
    let shipped = &ctx.defaults.prefix_table;
    // Prefix keys this source writes to something else.
    let rewritten: Vec<_> = source
        .rows
        .iter()
        .filter(|row| matches!(row.verdict, Verdict::Write { .. }))
        .filter_map(identity)
        .filter(|(table, _)| *table == Table::Prefix)
        .map(|(_, id)| id)
        .collect();
    for index in 0..source.rows.len() {
        let row = &source.rows[index];
        let Verdict::Write {
            table: Table::Prefix,
            key,
            value,
            ..
        } = &row.verdict
        else {
            continue;
        };
        if row.class != Class::Unbinding || value.as_str() != Some("none") {
            continue;
        }
        let Ok(sequence) = parse_chord_sequence(key) else {
            continue;
        };
        let Some((_, action)) = shipped
            .iter()
            .find(|(spelling, _)| parse_chord_sequence(spelling).ok().as_ref() == Some(&sequence))
        else {
            continue;
        };
        // Another shipped key with the same action, left alone by this source.
        let other = shipped.iter().find(|(spelling, other)| {
            let Ok(keys) = parse_chord_sequence(spelling) else {
                return false;
            };
            *other == action && keys != sequence && !rewritten.contains(&Ident::Keys(keys))
        });
        if let Some((spelling, _)) = other {
            source.rows[index].verdict = Verdict::Same(format!(
                "phux also does this on `{spelling}`, so beginner mode keeps `{key}` too; \
                 `phux migrate --advanced` removes it as tmux did"
            ));
        }
    }
}

/// Inside one source, the line the program loads last wins: earlier
/// lines on the same key are not written.
fn last_line_wins(source: &mut Source, home: &Path) {
    let mut last: BTreeMap<(Table, Ident), usize> = BTreeMap::new();
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

/// The key or setting a written (or already-default) row claims.
pub(super) fn identity(row: &Row) -> Option<(Table, Ident)> {
    let (Verdict::Write { table, key, .. } | Verdict::Default { table, key }) = &row.verdict else {
        return None;
    };
    if table.is_setting() {
        return Some((*table, Ident::Name(key.clone())));
    }
    Some((*table, Ident::Keys(parse_chord_sequence(key).ok()?)))
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
        Table::Defaults | Table::Status => {
            let current = ctx.shipped.get(table.name()).and_then(|t| t.get(&key));
            return if current == Some(&value) {
                Verdict::Default { table, key }
            } else {
                let replaces = current.map(|current| format!("{key} = {current}"));
                Verdict::Write {
                    table,
                    key,
                    value,
                    note,
                    replaces,
                }
            };
        }
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
