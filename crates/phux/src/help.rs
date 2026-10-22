//! `phux help`, the short and long help pages, and the help topics that
//! are not commands.

use std::process::ExitCode;

use crate::{Cli, environment, exit_codes, output};

/// The footer appended to the root long page by [`render_help_page`] (not
/// `after_help`, which would reflow its two columns). Its topics are answered
/// by [`help_topic`].
const ROOT_LEARN_MORE: &str = "\
Learn more:
  phux <command> --help    Flags and examples for one command
  phux help targets        How TARGET names sessions, groups, panes, agents
  phux help environment    Environment variables phux reads
  phux help exit-codes     Exit statuses, for scripts
";

/// The `phux help targets` topic: the selector grammar every TARGET-taking
/// verb shares. The sigils are the ones `selector::Selector` parses; the
/// help-inventory test that walks the parser keeps this list honest.
const TARGETS_HELP: &str = "\
TARGETS
  A TARGET names what a verb acts on. Every verb that takes one
  (kill, snapshot, send-keys, paste, run, wait, watch, resize, tag,
  take, give, signal, ask) reads the same grammar:

  name              A session by name             phux snapshot work
  name:G            Group (tab) G of a session    phux kill work:1
  name:G.P          Pane P of group G             phux send-keys work:1.0 C-c
  @N                A pane by id (`phux ls`)      phux run @7 \"cargo test\"
  host/@N           A pane on a federation peer   phux snapshot edge/@7
  #tag              Every pane carrying a tag     phux kill #build
  %agent            The pane an agent runs in     phux wait %reviewer
  ^key              A session or pane by key      phux kill ^3f9a2c
  .                 The focused pane              phux signal . kill

  `=` is reserved: it means the attached view's focus history, which a
  headless caller does not have, so the verbs refuse it and say so.
  Group and pane numbers count from zero; a group name works in place
  of G. A key prefix (`phux ls -l` shows keys) needs six or more digits. A name that is also a
  registered host (`phux host ls`) dials that host from `phux attach`;
  pass `--socket` to force the local reading.
";

/// Deliberately small `phux -h` start-here view. `phux --help` owns the full
/// inventory; keeping the two surfaces distinct makes the first one useful.
pub(crate) const SHORT_HELP: &str = "A terminal multiplexer you can drive by hand or script.\n\n\
Usage: phux [OPTIONS] [COMMAND]\n\n\
Start here:\n  \
  phux                     Attach to your session (starts phux if needed)\n  \
  phux new NAME            Create and attach to a session\n  \
  phux ls                  List sessions\n  \
  phux spawn -- COMMAND    Create a pane without attaching\n  \
  phux snapshot TARGET     Read a pane\n  \
  phux send-keys TARGET K  Send input to a pane\n  \
  phux host add me@HOST    Reach another machine over ssh\n  \
  phux agent list          See agents and their current state\n  \
  phux --skill             Teach an agent how to drive phux\n\n\
Run `phux --help` for every command or `phux <command> --help` for details.\n";

pub(crate) fn short_help_requested() -> bool {
    short_help_requested_in(std::env::args_os().skip(1))
}

pub(crate) fn short_help_requested_in<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut args = args.into_iter();
    let mut requested = false;
    while let Some(arg) = args.next() {
        let arg = arg.as_ref();
        if arg == "-h" {
            requested = true;
        } else if arg == "--socket" || arg == "--rec" {
            if args.next().is_none() {
                return false;
            }
        } else if !arg.to_string_lossy().starts_with("--socket=")
            && !arg.to_string_lossy().starts_with("--rec=")
        {
            return false;
        }
    }
    requested
}

/// The names (and aliases) `phux help <topic>` answers: pages that are not
/// commands.
const HELP_TOPICS: &[(&str, &[&str])] = &[
    ("targets", &["target", "selectors", "selector"]),
    ("environment", &["env", "environment-variables"]),
    ("exit-codes", &["exit-status", "exit-code", "exit"]),
];

/// Render one help topic by name or alias, or `None` for a word that is
/// not a topic.
pub(crate) fn help_topic(word: &str) -> Option<String> {
    let (topic, _) = HELP_TOPICS
        .iter()
        .find(|(name, aliases)| *name == word || aliases.contains(&word))?;
    Some(match *topic {
        "targets" => TARGETS_HELP.to_owned(),
        "environment" => environment::environment_section(),
        "exit-codes" => {
            let mut page = exit_codes::exit_status_section();
            page.push('\n');
            page
        }
        _ => return None,
    })
}

/// Answer `phux help` and `phux help <topic>` before the parser runs;
/// `phux help <verb>` is rewritten by `rewrite_help_verb`, and anything else is
/// refused naming the topics.
pub(crate) fn help_request(args: &[std::ffi::OsString]) -> Option<ExitCode> {
    let at = help_word_index(args)?;
    let Some(word) = args.get(at + 1) else {
        if let Some(page) = render_help_page(Cli::spec().root.cmd, true, help_style()) {
            output::bytes(page.as_bytes());
        }
        return Some(ExitCode::SUCCESS);
    };
    let word = word.to_string_lossy();
    if let Some(page) = help_topic(&word) {
        output::bytes(page.as_bytes());
        return Some(ExitCode::SUCCESS);
    }
    if is_top_level_verb(&word) || word.starts_with('-') {
        return None;
    }
    let topics: Vec<&str> = HELP_TOPICS.iter().map(|(name, _)| *name).collect();
    error!("no command or help topic named `{word}`");
    hint!("topics: {}; commands: see `phux --help`", topics.join(", "));
    Some(ExitCode::from(2))
}

/// Whether `word` names a top-level verb or one of its aliases, hidden or
/// visible: `phux help stdio-bridge` should render that page even though
/// the inventory does not advertise it.
pub(crate) fn is_top_level_verb(word: &str) -> bool {
    Cli::spec()
        .root
        .subcommands
        .iter()
        .any(|sub| sub.cmd.name == word || sub.cmd.aliases.contains(&word))
}

/// `phux help <verb> [<sub>...]` becomes `phux <verb> [<sub>...] --help`,
/// so the parser answers it with the verb's own page. Only the exact
/// leading `help` word is rewritten; a bare `phux help` and the topics
/// were already answered by [`help_request`].
pub(crate) fn rewrite_help_verb(mut raw: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    if let Some(at) = help_word_index(&raw[1..])
        && raw.len() > at + 2
    {
        raw.remove(at + 1);
        raw.push("--help".into());
    }
    raw
}

/// Where the `help` word sits in argv (after the binary), looking past a
/// leading global `--socket PATH` the way `-h` detection does, so
/// `phux --socket X help ls` reads the same as `phux help ls`. `None` when
/// the first verb-position word is anything else.
pub(crate) fn help_word_index(args: &[std::ffi::OsString]) -> Option<usize> {
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        if arg == "help" {
            return Some(at);
        }
        if arg == "--socket" {
            at += 2;
        } else if arg.to_string_lossy().starts_with("--socket=") {
            at += 1;
        } else {
            return None;
        }
    }
    None
}

/// The colour policy for help on stdout: coloured on a terminal (or under
/// `CLICOLOR_FORCE`), plain in a pipe or under `NO_COLOR`, with a quiet
/// bold/cyan palette.
pub(crate) fn help_style() -> usage::help::Style {
    use usage::help::{Palette, Style};
    let style = if output::color_enabled(std::io::IsTerminal::is_terminal(&std::io::stdout())) {
        Style::COLOURED
    } else {
        Style::PLAIN
    };
    style.palette(
        Palette::DEFAULT
            .heading("bold")
            .command("cyan+bold")
            .option("cyan+bold")
            .metavar("cyan"),
    )
}

/// Render one command's help page. Every help surface (`--help`, `phux
/// help`, the generated reference) goes through here so they agree; the
/// root long page gains [`ROOT_LEARN_MORE`], appended rather than declared
/// as `after_help` because the renderer would reflow its columns.
pub(crate) fn render_help_page(
    cmd: &usage::Command<'_>,
    long: bool,
    style: usage::help::Style,
) -> Option<String> {
    let mut page = usage::help::render_styled(Cli::spec(), cmd, long, style)?;
    if long && std::ptr::eq(cmd, Cli::spec().root.cmd) {
        page.push('\n');
        // Colour is decided by the style, not the palette: a palette on a
        // plain style still renders plain.
        if style.palette(usage::help::Palette::DEFAULT) == usage::help::Style::PLAIN {
            page.push_str(ROOT_LEARN_MORE);
        } else {
            // The same weight the palette gives every other heading.
            let (heading, rest) = ROOT_LEARN_MORE
                .split_once('\n')
                .unwrap_or((ROOT_LEARN_MORE, ""));
            page.push_str("\x1b[1m");
            page.push_str(heading);
            page.push_str("\x1b[0m\n");
            page.push_str(rest);
        }
    }
    Some(page)
}
