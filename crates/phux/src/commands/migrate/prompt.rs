//! Render the migration prompt from the decided sources.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use toml::Value;

use super::sources::{Row, Source, Verdict, identity, tilde};
use crate::commands::MigrateSource;

const TEMPLATE: &str = include_str!("prompt.txt");

/// Everything the prompt names besides the sources.
pub(super) struct Context<'a> {
    pub home: &'a Path,
    /// The phux binary the agent runs, as typed in a shell.
    pub phux: &'a str,
    pub config: &'a Path,
    /// The backup's date, `YYYY-MM-DD`.
    pub date: &'a str,
    pub prefix: &'a str,
    /// The "Actions you may bind" list.
    pub reference: &'a str,
}

pub(super) const fn name(kind: MigrateSource) -> &'static str {
    match kind {
        MigrateSource::Tmux => "tmux",
        MigrateSource::Zellij => "zellij",
        MigrateSource::Ghostty => "Ghostty",
        MigrateSource::Herdr => "herdr",
        MigrateSource::Rex => "Rex",
    }
}

pub(super) fn render(sources: &[Source], ctx: &Context<'_>) -> String {
    let config_name = ctx.config.file_name().map_or_else(
        || "config.toml".into(),
        |n| n.to_string_lossy().into_owned(),
    );
    let target = if ctx.config.exists() {
        format!(
            "The file exists. Before changing it, copy it to `{config_name}.bak-{}` in the same \
             directory, then merge into it.",
            ctx.date
        )
    } else {
        "The file does not exist yet. Create it, and its directory if needed.".to_owned()
    };
    TEMPLATE
        .replace("{{files}}", &files(sources, ctx.home))
        .replace("{{config}}", &ctx.config.display().to_string())
        .replace("{{target}}", &target)
        .replace("{{prefix}}", &format!("`{}`", ctx.prefix))
        .replace("{{sources}}", &sections(sources, ctx))
        .replace("{{conflicts}}", &conflicts(sources, ctx.home))
        .replace("{{phux}}", ctx.phux)
        .replace(
            "{{config_q}}",
            &crate::commands::sh_quote(&ctx.config.to_string_lossy()),
        )
        .replace("{{reference}}", ctx.reference)
}

/// The action list, from the dispatcher's own registry.
pub(super) fn reference() -> String {
    let mut out = String::new();
    for spec in phux_tui::attach::action_registry::ACTIONS {
        if spec.params.is_empty() {
            let _ = writeln!(out, "- `{}`: {}", spec.name, spec.description);
        } else {
            let _ = writeln!(
                out,
                "- `{}` ({}): {}",
                spec.name, spec.params, spec.description
            );
        }
    }
    out
}

fn files(sources: &[Source], home: &Path) -> String {
    let mut out = String::new();
    for source in sources {
        let _ = writeln!(out, "- {}:", name(source.kind));
        for file in &source.files {
            let _ = writeln!(out, "  - {}", tilde(file, home));
        }
        let (bindings, unbindings, settings) = source.found();
        let _ = writeln!(
            out,
            "  Found: {}, {}, {}.",
            plural(bindings, "binding"),
            plural(unbindings, "unbinding"),
            plural(settings, "setting")
        );
    }
    out
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

fn intro(kind: MigrateSource, prefix: &str) -> String {
    match kind {
        MigrateSource::Tmux => format!(
            "The table below decides every tmux line. Copy its lines; do not translate them \
             again. tmux's prefix table becomes `[keybindings.prefix-table]` (after phux's \
             prefix, {prefix} unless a line below sets it) and `bind -n` becomes \
             `[keybindings.global]`."
        ),
        MigrateSource::Ghostty => "Ghostty keybinds fire without a prefix, so they become \
             `[keybindings.global]` lines. The table below decides every line it could; copy \
             its lines and do not translate them again."
            .into(),
        MigrateSource::Zellij => format!(
            "zellij binds keys per mode inside `keybinds {{ ... }}`. Its `normal` and `shared` \
             keys work all the time: in phux they are `[keybindings.global]` lines, never a \
             mode named normal. zellij's `tmux` mode is a prefix: its keys become \
             `[keybindings.prefix-table]` lines (after {prefix}). Any other zellij mode becomes \
             `[keybindings.modes.<name>]`, entered with `{{ action = \"enter-mode\", name = \
             \"<name>\" }}`, only where every key in it has a phux action. zellij spells keys \
             `Ctrl a` and `Alt h`: in phux those are `C-a` and `M-h`."
        ),
        MigrateSource::Herdr => format!(
            "herdr's `[keys]` lines name an action and its keys. `prefix+n` is the prefix, then \
             n: `\"n\"` in `[keybindings.prefix-table]` (after {prefix}). `ctrl+alt+n` is a key \
             on its own: `\"C-M-n\"` in `[keybindings.global]`. herdr's own `prefix` line \
             becomes `[keybindings] prefix` only when you ask me first. herdr spells keys \
             `ctrl+b` and names punctuation (`minus`, `comma`, `plus`, `backtick`): in phux \
             those are `C-b`, `-`, `,`, `+` and the backtick."
        ),
        MigrateSource::Rex => format!(
            "Rex binds keys in Lua: `rex.bind(\"KEY\", \"ACTION\", {{ args }})` and \
             `rex.unbind(\"KEY\")`. A key on its own, such as `ctrl+alt+n`, is `\"C-M-n\"` in \
             `[keybindings.global]`. Keys joined by `>` are a sequence: when the first key is \
             the phux prefix ({prefix}) the rest goes in `[keybindings.prefix-table]`; any \
             other first key is reported, and becomes `[keybindings] prefix` only when you \
             ask me first. A key with `cmd` is reported, not written: phux key bindings have \
             no Cmd modifier. A key bound to a Lua function is reported with what the \
             function does, unless one phux action does exactly that. Lines that are not \
             `rex.bind` or `rex.unbind` are there for context only."
        ),
    }
}

fn sections(sources: &[Source], ctx: &Context<'_>) -> String {
    let home = ctx.home;
    let prefix = format!("`{}`", ctx.prefix);
    let mut out = String::new();
    for source in sources {
        let program = name(source.kind);
        let _ = writeln!(out, "## {program}");
        let _ = writeln!(out, "{}", intro(source.kind, &prefix));
        if let Some(note) = &source.note {
            let _ = writeln!(out, "{note}");
        }
        let (decided, raw): (Vec<&Row>, Vec<&Row>) = source
            .rows
            .iter()
            .partition(|row| row.verdict != Verdict::Raw);
        if !decided.is_empty() {
            let _ = writeln!(
                out,
                "\n### Every {program} line, one by one\nWrite every line marked \"write\" \
                 exactly as given, in the table it names, once. A line marked \"not written\", \
                 \"nothing to do\" or \"already phux's default\" gets nothing. Each line goes in \
                 the report under the list its marking names."
            );
            for row in decided {
                let _ = writeln!(out, "- {}\n  {}", quote(row, home), verdict(&row.verdict));
            }
        }
        if !raw.is_empty() {
            let _ = writeln!(
                out,
                "\n### {program}: translate these yourself\nFor each binding below, write a \
                 phux line only where an action under \"Actions you may bind\" does exactly \
                 what it does; otherwise it goes under \"Could not migrate\" with one sentence \
                 on why. Lines that only open or close a block are context and need no entry."
            );
            for row in raw {
                let _ = writeln!(out, "- {}", quote(row, home));
            }
        }
        out.push('\n');
    }
    out
}

fn quote(row: &Row, home: &Path) -> String {
    format!("{}:{} `{}`", tilde(&row.file, home), row.line, row.text)
}

/// `"key" = value`, the line as it goes in the file.
fn line(key: &str, value: &Value) -> String {
    format!("{} = {value}", Value::String(key.to_owned()))
}

fn verdict(verdict: &Verdict) -> String {
    match verdict {
        Verdict::Write {
            table,
            key,
            value,
            note,
            replaces,
        } => {
            let written = if table.binds_keys() {
                line(key, value)
            } else {
                format!("{key} = {value}")
            };
            let mut out = format!("write `{written}` in `{}`", table.header());
            if let Some(note) = note {
                let _ = write!(out, "; differs from the source: {note}");
            }
            if let Some(replaces) = replaces {
                let _ = write!(out, " (replaces the phux default `{replaces}`)");
            }
            out
        }
        Verdict::Default { .. } => "already phux's default, not written".into(),
        Verdict::Skip(reason) => format!("not written: {reason}"),
        Verdict::Covered(reason) => format!("nothing to do: {reason}"),
        Verdict::Raw => String::new(),
    }
}

/// Keys more than one source writes differently, grouped by the pair of
/// sources the agent asks about.
fn conflicts(sources: &[Source], home: &Path) -> String {
    let mut by_key: BTreeMap<_, Vec<(MigrateSource, &Row)>> = BTreeMap::new();
    for source in sources {
        for row in &source.rows {
            if let Some(id) = identity(row) {
                by_key.entry(id).or_default().push((source.kind, row));
            }
        }
    }
    let mut pairs: BTreeMap<(MigrateSource, MigrateSource), Vec<String>> = BTreeMap::new();
    for claims in by_key.values() {
        let Some(((first_kind, first), rest)) = claims.split_first() else {
            continue;
        };
        for (kind, row) in rest {
            if kind == first_kind || written(row) == written(first) {
                continue;
            }
            let (Verdict::Write { table, key, .. } | Verdict::Default { table, key }) =
                &first.verdict
            else {
                continue;
            };
            pairs.entry((*first_kind, *kind)).or_default().push(format!(
                "- `{key}` in `{}`: {} {} {}; {} {} {}",
                table.header(),
                name(*first_kind),
                quote(first, home),
                written(first),
                name(*kind),
                quote(row, home),
                written(row),
            ));
        }
    }
    let mut out = String::new();
    if pairs.is_empty() {
        out.push_str("No key is bound differently by two of the sources above.\n");
    }
    for ((a, b), keys) in &pairs {
        let _ = writeln!(
            out,
            "{} and {} bind these keys differently. Ask me once which of the two wins for \
             all of them (or key by key, if I say so), wait for my answer, then write only \
             the winner's line:",
            name(*a),
            name(*b)
        );
        for key in keys {
            let _ = writeln!(out, "{key}");
        }
    }
    out.push_str(
        "If a line already in the target file binds a key that a line above binds differently, \
         ask me once about all such keys the same way. Keys you translate yourself follow the \
         same rule.\n",
    );
    out
}

fn written(row: &Row) -> String {
    match &row.verdict {
        Verdict::Write { value, .. } => format!("writes `{value}`"),
        Verdict::Default { .. } => "keeps the phux default".into(),
        _ => String::new(),
    }
}
