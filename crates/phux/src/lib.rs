//! phux CLI — subcommand parsing and dispatch, exposed as [`run`].
//!
//! A library so `src/main.rs` and the `dhat-heap` profiling binary
//! (`src/bin/dhat_heap.rs`) share it: keeping dhat's allocator in a separate
//! `required-features` target means `--all-features` never wires it into the
//! `phux` binary the integration tests spawn.

#![forbid(unsafe_code)]
#![allow(
    clippy::print_stderr,
    reason = "binary entry point; stderr is the report"
)]
// No crate-level `clippy::print_stdout` allow: stdout goes through
// `outln!` / `out!`, which survive a closed reader (`phux ls | head`).
#![allow(
    clippy::redundant_pub_crate,
    reason = "internal submodules expose items to the crate root via pub(crate) rather than plain `pub`; the crate's only real public API is `run`, everything else stays crate-private on purpose"
)]

use std::ffi::OsStr;
use std::process::ExitCode;

use commands::Command;

// First, so `outln!` is in scope for every module below.
#[macro_use]
mod output;

mod capabilities;
mod commands;
mod companion;
mod deprecations;
mod environment;
mod exit_codes;
mod refdocs;
mod skill;

#[cfg(test)]
mod help_inventory;

/// The environment variable an auto-spawned daemon reads its idle backstop
/// from, so the integration harness can arm it by name rather than by a
/// literal of its own (see `AutoSpawnedServer::IDLE_BACKSTOP`).
pub use commands::server::AUTO_SPAWN_IDLE_ENV;
pub use commands::server::ENSURE_TIMEOUT_ENV;

/// phux — a libghostty-backed terminal multiplexer and control plane.
#[derive(Debug, usage::Cli)]
#[usage(
    bin = "phux",
    version = env!("PHUX_VERSION_LABEL"),
    unknown_flags = "error",
    completion,
    // `--rec` and `--remote` belong to the naked attach; their scope is a
    // post-parse check because globals parse on either side of a verb.
    about = "A terminal multiplexer you can drive by hand or script.",
    long_about = "A terminal multiplexer you can drive by hand or script.\n\n\
        Run `phux` alone to attach to your session; every other verb is headless.",
    // The root page is laid out for 80 columns whatever the terminal is:
    // the groups read as one table, and the width test on it is exact.
    term_width = 80,
    // `phux help ...` is answered before the parser runs (see `help_request`).
    disable_help_subcommand,
    // The renderer prints its default command group first, under this
    // title, so the first group of the inventory is declared as the
    // default rather than leaving an empty `Commands:` heading above it.
    subcommand_help_heading = "Sessions"
)]
struct Cli {
    /// Recording options for the naked `phux` attach. `phux attach` carries
    /// its own copy; every other verb is pointed at `phux rec`.
    #[usage(flatten)]
    rec: commands::RecOpts,

    /// Server socket to dial (default: `$PHUX_SOCKET`)
    // Global (ADR-0065): `phux --socket X ls` == `phux ls --socket X`.
    // Verbs that never dial refuse it (`commands::socketless_verb`).
    #[usage(long, global, value_name = "PATH")]
    socket: Option<std::path::PathBuf>,

    /// Print agent guidance and exit
    #[usage(
        long,
        value_enum,
        num_args = 0..=1,
        default_missing = "full",
        exclusive,
        value_name = "SCOPE"
    )]
    skill: Option<skill::SkillScope>,

    /// Attach to a phux server on another machine
    // The naked attach's copy; verbs that take `--remote` carry their own.
    #[usage(long, value_name = "[USER@]HOST")]
    remote: Option<String>,

    /// Print machine-readable capabilities (with --json)
    #[usage(long)]
    capabilities: bool,

    /// Subcommand. Defaults to attaching to the last session if omitted.
    #[usage(subcommand)]
    command: Option<Command>,
}

/// The footer appended to the root long page by [`render_help_page`] (not
/// `after_help`, which would reflow its two columns). Its topics are answered
/// by [`help_topic`].
const ROOT_LEARN_MORE: &str = "\
Learn more:
  phux <command> --help    Flags and examples for one command
  phux help targets        How TARGET names sessions, windows, panes, agents
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
  name:W            Window W of a session         phux kill work:1
  name:W.P          Pane P of window W            phux send-keys work:1.0 C-c
  @N                A pane by id (`phux ls`)      phux run @7 \"cargo test\"
  host/@N           A pane on a federation peer   phux snapshot edge/@7
  #tag              Every pane carrying a tag     phux kill #build
  %agent            The pane an agent runs in     phux wait %reviewer
  .                 The focused pane              phux signal . kill

  `=` is reserved: it means the attached view's focus history, which a
  headless caller does not have, so the verbs refuse it and say so.
  Window and pane numbers count from zero. A name that is also a
  registered host (`phux host ls`) dials that host from `phux attach`;
  pass `--socket` to force the local reading.
";

/// Deliberately small `phux -h` start-here view. `phux --help` owns the full
/// inventory; keeping the two surfaces distinct makes the first one useful.
const SHORT_HELP: &str = "A terminal multiplexer you can drive by hand or script.\n\n\
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

fn short_help_requested() -> bool {
    short_help_requested_in(std::env::args_os().skip(1))
}

fn short_help_requested_in<I, S>(args: I) -> bool
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

/// The teaching error for a root `--rec` in front of a verb. Post-parse
/// because the root global `--socket` rules out
/// `args_conflicts_with_subcommands`.
const fn root_rec_before_verb(cli: &Cli) -> Option<&'static str> {
    if cli.command.is_some() && (cli.rec.rec.is_some() || cli.rec.rec_format.is_some()) {
        Some(
            "phux: a root `--rec` belongs to the naked `phux` attach alone; \
             use `phux attach --rec PATH` to record an attach, or \
             `phux rec TARGET -o PATH` for headless capture",
        )
    } else {
        None
    }
}

/// `--qr` belongs to minting a credential. usage-rs lets parent flags
/// parse next to a subcommand, so the refusal is post-parse.
const fn pair_qr_with_action(cli: &Cli) -> Option<&'static str> {
    match &cli.command {
        Some(Command::Pair {
            action: Some(_),
            qr: true,
            ..
        }) => Some(
            "phux: --qr belongs to minting a credential; it cannot combine with ls, prune, rotate, or revoke",
        ),
        _ => None,
    }
}

/// The teaching error for a root `--remote` in front of a verb (same scope
/// rule as the root `--rec`).
const fn root_remote_before_verb(cli: &Cli) -> Option<&'static str> {
    if cli.command.is_some() && cli.remote.is_some() {
        Some(
            "phux: a root `--remote` belongs to the naked `phux` attach alone; \
             place it after the verb instead (`phux ls --remote HOST`; `ls`, `new`, `kill`, \
             `rename`, and `detach` take it), or use `phux attach --remote HOST` to name a \
             session, a `--code`, or `--no-enroll`",
        )
    } else {
        None
    }
}

/// The parse error for this invocation's `--remote`, root or verb-scoped;
/// checked before the TTY preflight.
fn malformed_remote_target(cli: &Cli) -> Option<String> {
    commands::remote_target::RemoteTarget::parse(invocation_remote(cli)?).err()
}

/// The `--remote` this invocation carries: the root copy for the naked
/// attach, otherwise the verb's own (see [`commands::verb_remote`]).
fn invocation_remote(cli: &Cli) -> Option<&str> {
    cli.command
        .as_ref()
        .map_or(cli.remote.as_deref(), commands::verb_remote)
}

/// Whether this invocation pairs the local-UDS `--socket` with the network
/// `--remote`. Post-parse because conflicts are validated per parser and
/// `--socket` is a root global.
fn socket_and_remote_collide(cli: &Cli) -> bool {
    cli.socket.is_some() && invocation_remote(cli).is_some()
}

/// usage-rs `requires(a, b)` is all-of, so `--token` / `--cert-fingerprint` /
/// `--tls-server-name` cannot declare "needs `--quic` or `--ws`" at parse
/// time. The any-of rule lives here (same reason `phux logs -f` moved off
/// the parser).
const fn dial_auth_without_transport(
    quic: Option<&str>,
    ws: Option<&str>,
    token: Option<&str>,
    cert_fingerprint: Option<&str>,
    tls_server_name: Option<&str>,
) -> Option<&'static str> {
    if (token.is_some() || cert_fingerprint.is_some() || tls_server_name.is_some())
        && quic.is_none()
        && ws.is_none()
    {
        Some("phux: --token, --cert-fingerprint, and --tls-server-name need --quic or --ws")
    } else {
        None
    }
}

/// Resolve a `--remote` target and attach to it.
///
/// One helper for both the root and the verb-scoped spelling, so the two
/// cannot drift in what a target means.
fn attach_remote_target(
    target: &str,
    session: Option<String>,
    code: Option<&str>,
    no_enroll: bool,
    rec: Option<&commands::rec::RecordSpec>,
) -> ExitCode {
    use commands::remote_target::{Bootstrap, RemoteAttach, RemoteTarget};

    let target = match RemoteTarget::parse(target) {
        Ok(target) => target,
        Err(err) => {
            eprintln!("phux: {err}");
            return ExitCode::from(2);
        }
    };
    commands::remote_target::run(RemoteAttach {
        target,
        session,
        code,
        bootstrap: if no_enroll {
            Bootstrap::Never
        } else {
            Bootstrap::Auto
        },
        rec,
    })
}

fn flag_exists_on_any_verb(cmd: &usage::Command<'_>, long: &str) -> bool {
    cmd.flags.iter().any(|flag| flag.longs.contains(&long))
        || cmd
            .subcommands
            .iter()
            .any(|sub| flag_exists_on_any_verb(sub, long))
}

fn token_str(token: &[u8]) -> String {
    String::from_utf8_lossy(token).into_owned()
}

fn misplaced_scoped_flag(err: &usage::Error<'_, '_>) -> Option<String> {
    let usage::Error::UnknownFlag { token } = err else {
        return None;
    };
    let flag = token_str(token);
    let long = flag.strip_prefix("--")?;
    flag_exists_on_any_verb(Cli::command(), long).then_some(flag)
}

/// Every verb path (`attach`, `host add`) that accepts `--long`.
fn verbs_with_flag(cmd: &usage::Command<'_>, long: &str, prefix: &str, out: &mut Vec<String>) {
    for sub in cmd.subcommands {
        let path = if prefix.is_empty() {
            sub.name.to_owned()
        } else {
            format!("{prefix} {}", sub.name)
        };
        if sub.flags.iter().any(|flag| flag.longs.contains(&long)) {
            out.push(path.clone());
        }
        verbs_with_flag(sub, long, &path, out);
    }
}

/// The teaching hint for a refused per-verb flag, keyed on where it was
/// typed. Before any verb it is a placement mistake (`phux --json ls`).
/// After a verb that does not take it, "place it after the verb" would be
/// wrong advice, so the hint names the verbs that do take it instead.
fn misplaced_flag_hint(err: &usage::Error<'_, '_>, words: &[String]) -> Option<String> {
    let flag = misplaced_scoped_flag(err)?;
    let long = flag.strip_prefix("--")?;
    let root = Cli::command();
    let at = words
        .iter()
        .position(|word| *word == flag || word.starts_with(&format!("{flag}=")))
        .unwrap_or(words.len());
    let verb = words.get(..at).unwrap_or_default().iter().find_map(|word| {
        root.subcommands
            .iter()
            .find(|sub| sub.name == word || sub.aliases.contains(&word.as_str()))
    });
    let Some(verb) = verb else {
        return Some(format!(
            "hint: `{flag}` is set per verb, not on `phux` itself; place it after the verb: \
             `phux <verb> {flag} ...`"
        ));
    };
    let mut holders = Vec::new();
    verbs_with_flag(root, long, "", &mut holders);
    let shown: Vec<String> = holders
        .iter()
        .take(6)
        .map(|path| format!("`phux {path}`"))
        .collect();
    let more = if holders.len() > shown.len() {
        ", ..."
    } else {
        ""
    };
    Some(format!(
        "hint: `phux {}` does not take `{flag}`; it belongs to {}{more}",
        verb.name,
        shown.join(", ")
    ))
}

/// Whether a `phux workload` invocation carries PEM or private-key material
/// in the words before any `--`. Only that verb is refused up front: it is
/// the one that handles key material, while other verbs legitimately carry
/// text that looks like it (input for a pane, a secret scanner's pattern).
fn workload_argv_carries_key_material(argv: &[std::ffi::OsString]) -> bool {
    let words: Vec<std::borrow::Cow<'_, str>> = argv
        .iter()
        .take_while(|word| word.as_os_str() != "--")
        .map(|word| word.to_string_lossy())
        .collect();
    let workload = words
        .iter()
        .find(|word| !word.starts_with('-'))
        .is_some_and(|verb| verb == "workload");
    workload
        && words
            .iter()
            .any(|word| word.contains("-----BEGIN") || word.contains("PRIVATE KEY"))
}

/// Shortest run of base64-alphabet characters [`redact_long_base64`] hides.
const REDACT_MIN_RUN: usize = 40;

/// Replace every run of [`REDACT_MIN_RUN`] or more characters of the base64
/// or base64url alphabet with `<redacted>`. usage-rs quotes the word it
/// refused, and a bare base64 line of a key, or a base64url secret such as a
/// JWK `d`, carries no PEM marker to catch (`workload-auth.md` §8).
fn redact_long_base64(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_') {
            run.push(c);
        } else {
            flush_base64_run(&mut out, &mut run);
            out.push(c);
        }
    }
    flush_base64_run(&mut out, &mut run);
    out
}

fn flush_base64_run(out: &mut String, run: &mut String) {
    if run.len() >= REDACT_MIN_RUN {
        out.push_str("<redacted>");
    } else {
        out.push_str(run);
    }
    run.clear();
}

fn report_parse_error(argv: &[&std::ffi::OsStr], err: usage::Error<'_, '_>) -> ExitCode {
    let words: Vec<String> = argv
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();
    match err {
        usage::Error::Help { cmd, long } => {
            if let Some(page) = render_help_page(cmd, long, help_style()) {
                output::bytes(page.as_bytes());
            }
            ExitCode::SUCCESS
        }
        usage::Error::Version { .. } => {
            output::bytes(format!("phux {}\n", env!("PHUX_VERSION_LABEL")).as_bytes());
            ExitCode::SUCCESS
        }
        err => {
            eprint!(
                "{}",
                redact_long_base64(&usage::render_failure(Cli::spec(), argv, &err))
            );
            if let Some(hint) = misplaced_flag_hint(&err, &words) {
                eprintln!("{hint}");
            }
            ExitCode::from(2)
        }
    }
}

/// Resolve `--rec` into a recording plan on the cooked terminal, before the
/// alt screen is up.
fn plan_rec(opts: &commands::RecOpts) -> Result<Option<commands::rec::RecordSpec>, ExitCode> {
    opts.rec
        .as_deref()
        .map(|path| commands::rec::spec::plan(path, opts.rec_format))
        .transpose()
}

/// Print the one-line build banner to stderr. Only the human-watched
/// foreground entry points (`phux server`, `phux relay run`) print it:
/// attach paths would wipe it with the alt screen, and one-shot verbs keep
/// stderr clean for scripts.
pub(crate) fn print_banner() {
    eprintln!("{BANNER}");
}

/// The banner line: `phux <version>`, nothing else.
pub(crate) const BANNER: &str = concat!("phux ", env!("PHUX_VERSION_LABEL"));

/// Whether this invocation enters the TUI (raw mode + alt screen) and must
/// keep logs off stderr: `attach`, `host attach`, naked `phux`, `new` without
/// `--json`, and `worktree new|open --attach`.
const fn is_interactive_client(cli: &Cli) -> bool {
    match &cli.command {
        Some(
            Command::Attach { .. }
            | Command::Host {
                action: commands::host::HostAction::Attach { .. },
            }
            | Command::Worktree {
                action:
                    commands::WorktreeAction::New { attach: true, .. }
                    | commands::WorktreeAction::Open { attach: true, .. },
            },
        )
        | None => true,
        Some(Command::New { json, .. }) => !*json,
        _ => false,
    }
}

fn json_output_requested() -> bool {
    std::env::args_os().any(|arg| arg == "--json")
}

/// Endpoints answered straight from argv, before clap parses anything.
///
/// Returns `Some(code)` when this invocation is one of them and no further
/// setup should run.
fn preparse_endpoint(args: &[std::ffi::OsString]) -> Option<ExitCode> {
    // Recognize the canonical launcher spelling before clap so every trailing
    // byte belongs to the companion, including names that overlap root flags
    // today or are introduced by the MCP binary in a future release.
    if args.first().is_some_and(|arg| arg == "mcp") {
        return Some(commands::mcp::run(&args[1..]));
    }

    let wants_capabilities = args.iter().any(|arg| arg == "--capabilities");
    let wants_json = args.iter().any(|arg| arg == "--json");
    if wants_capabilities && wants_json {
        if args.len() != 2 {
            eprintln!(
                "phux: --capabilities --json is a standalone endpoint and cannot be combined with other arguments"
            );
            return Some(ExitCode::from(2));
        }
        return Some(capabilities::run());
    }

    if short_help_requested() {
        output::bytes(SHORT_HELP.as_bytes());
        return Some(ExitCode::SUCCESS);
    }

    help_request(args)
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
fn help_request(args: &[std::ffi::OsString]) -> Option<ExitCode> {
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
    eprintln!(
        "phux: no command or help topic named `{word}`\n\
         topics: {}\n\
         commands: see `phux --help`",
        topics.join(", ")
    );
    Some(ExitCode::from(2))
}

/// Whether `word` names a top-level verb or one of its aliases, hidden or
/// visible: `phux help stdio-bridge` should render that page even though
/// the inventory does not advertise it.
fn is_top_level_verb(word: &str) -> bool {
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
fn rewrite_help_verb(mut raw: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
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
fn help_word_index(args: &[std::ffi::OsString]) -> Option<usize> {
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
fn help_style() -> usage::help::Style {
    use usage::help::{Palette, Style};
    Style::auto().palette(
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

/// The usage refusals the grammar cannot express, checked after parsing and
/// before any global setup: `--capabilities` without `--json`, the root
/// `--rec`/`--remote` scope rules, a malformed `--remote`, `--socket` with
/// `--remote`, and `--socket` on a verb that never dials. Each exits 2.
fn usage_refusal(cli: &Cli) -> Option<ExitCode> {
    if cli.capabilities {
        eprintln!("phux: --capabilities requires --json");
        return Some(ExitCode::from(2));
    }

    if matches!(
        &cli.command,
        Some(Command::Server {
            ensure: false,
            json: commands::JsonOpt { json: true },
            ..
        })
    ) {
        eprintln!("phux: `phux server --json` requires `--ensure`");
        return Some(ExitCode::from(2));
    }

    if let Some(message) = root_rec_before_verb(cli) {
        eprintln!("{message}");
        return Some(ExitCode::from(2));
    }
    if let Some(message) = root_remote_before_verb(cli) {
        eprintln!("{message}");
        return Some(ExitCode::from(2));
    }
    if let Some(message) = pair_qr_with_action(cli) {
        eprintln!("{message}");
        return Some(ExitCode::from(2));
    }
    // A malformed `--remote` target is a usage error, so it is reported here
    // — before the interactive TTY preflight below. Otherwise a typo in a
    // script would surface as "interactive use requires a terminal", which
    // names the wrong problem entirely.
    if let Some(message) = malformed_remote_target(cli) {
        eprintln!("phux: {message}");
        return Some(ExitCode::from(2));
    }
    if socket_and_remote_collide(cli) {
        eprintln!("{}", commands::server_target::SOCKET_REMOTE_CONFLICT);
        return Some(ExitCode::from(2));
    }
    if cli.socket.is_some()
        && let Some(verb) = cli.command.as_ref().and_then(commands::socketless_verb)
    {
        eprintln!(
            "phux: `phux {verb}` never dials a server, so --socket has no effect here; drop it"
        );
        return Some(ExitCode::from(2));
    }

    None
}

/// Install the process-global tracing subscriber before any runtime starts.
/// An interactive client logs to a file only (a stderr line would corrupt the
/// alt screen); everything else logs to stderr plus an optional `PHUX_LOG`
/// tee. Bind the returned guard for `main`'s lifetime so logs flush. Init
/// failure is non-fatal.
fn init_tracing(cli: &Cli) -> Option<phux_server::telemetry::WorkerGuard> {
    if is_interactive_client(cli) {
        // The client uses a synchronous file writer (no guard) so its trace
        // survives the `process::exit` detach path; see `init_client`.
        if let Err(err) = phux_server::telemetry::init_client() {
            // The client never logs to stderr, but a one-line init failure
            // on the cooked terminal (before alt screen) is acceptable and
            // beats a silent no-op subscriber.
            eprintln!("phux: client tracing init failed (continuing): {err}");
        }
        return None;
    }

    init_noninteractive_tracing()
}

fn init_noninteractive_tracing() -> Option<phux_server::telemetry::WorkerGuard> {
    match phux_server::telemetry::init() {
        Ok(guard) => guard,
        Err(err) => {
            if !json_output_requested() {
                eprintln!("phux: tracing init failed (continuing): {err}");
            }
            None
        }
    }
}

/// The fully-parsed inputs of `phux attach`, carried as one value so the
/// verb's body can live in [`run_attach`] rather than inline in the dispatch
/// table. The field names mirror the clap variant's, so the arm builds this
/// by field shorthand.
struct AttachInvocation {
    session: Option<String>,
    quic: Option<String>,
    ws: Option<String>,
    token: Option<String>,
    cert_fingerprint: Option<String>,
    tls_server_name: Option<String>,
    remote: Option<String>,
    code: Option<String>,
    no_enroll: bool,
    ssh: Option<String>,
    remote_phux: String,
    udp_ports: Option<String>,
    viewer: bool,
    take: bool,
    rec: commands::RecOpts,
    socket: Option<std::path::PathBuf>,
}

/// The attach role `--viewer` / `--take` declare (ADR-0127); the parser
/// already refuses both at once.
const fn declared_attach_role(viewer: bool, take: bool) -> phux_protocol::wire::frame::RolePolicy {
    use phux_protocol::wire::frame::RolePolicy;
    if viewer {
        RolePolicy::VIEWER
    } else if take {
        RolePolicy::TAKEOVER
    } else {
        RolePolicy::PRIMARY
    }
}

/// Run `phux attach`: resolve the recording plan, then dial whichever
/// transport the flags named.
fn run_attach(invocation: AttachInvocation) -> ExitCode {
    let AttachInvocation {
        session,
        quic,
        ws,
        token,
        cert_fingerprint,
        tls_server_name,
        remote,
        code,
        no_enroll,
        ssh,
        remote_phux,
        udp_ports,
        viewer,
        take,
        rec,
        socket,
    } = invocation;
    // Every ATTACH this process sends carries it, whichever transport the
    // flags below pick, so a reconnect keeps the role too.
    phux_tui::attach::set_attach_role(declared_attach_role(viewer, take));

    // `phux attach` owns its own `--rec`; the root copy is reserved
    // for the naked invocation below.
    let rec_spec = match plan_rec(&rec) {
        Ok(spec) => spec,
        Err(code) => return code,
    };
    let rec_spec = rec_spec.as_ref();
    // `--socket` is a local UDS path and cannot combine with the remote
    // transports; checked here because it is a root global.
    if socket.is_some() && (quic.is_some() || ws.is_some() || ssh.is_some()) {
        eprintln!(
            "phux: --socket dials a local UDS and cannot combine with --quic/--ws/--ssh; drop one"
        );
        return ExitCode::from(2);
    }
    // The parser cannot say "one of --quic/--ws" (`requires` is every listed
    // flag), so a lone `--token` would otherwise fall through to a local
    // attach and silently drop the credentials.
    if let Some(message) = dial_auth_without_transport(
        quic.as_deref(),
        ws.as_deref(),
        token.as_deref(),
        cert_fingerprint.as_deref(),
        tls_server_name.as_deref(),
    ) {
        eprintln!("{message}");
        return ExitCode::from(exit_codes::EXIT_USAGE);
    }
    if let Some(destination) = ssh {
        return commands::ssh_bootstrap::run(commands::ssh_bootstrap::SshAttach {
            destination,
            session,
            remote_phux,
            udp_ports,
            rec: rec_spec,
        });
    }
    if let Some(target) = remote {
        return attach_remote_target(&target, session, code.as_deref(), no_enroll, rec_spec);
    }
    match (quic, ws) {
        (Some(addr), None) => commands::attach::run_attach_quic(
            session,
            addr,
            token,
            cert_fingerprint,
            tls_server_name,
            rec_spec,
        ),
        (None, Some(url)) => commands::attach::run_attach_ws(
            session,
            url,
            token,
            cert_fingerprint,
            tls_server_name,
            rec_spec,
        ),
        (None, None) => commands::attach::run_attach_rec(session, socket, rec_spec),
        (Some(_), Some(_)) => {
            eprintln!("phux: choose only one remote attach transport (--quic or --ws)");
            ExitCode::FAILURE
        }
    }
}

/// Run `phux service`: the installed-supervisor lifecycle verbs.
fn run_service(action: commands::ServiceAction, socket: Option<std::path::PathBuf>) -> ExitCode {
    match action {
        commands::ServiceAction::Install {
            quic,
            listen,
            restore,
            hub,
            adopt,
            print,
        } => commands::service::run_install(
            quic,
            listen,
            restore,
            socket,
            hub,
            if adopt {
                commands::service::Takeover::Adopt
            } else {
                commands::service::Takeover::Refuse
            },
            print,
        ),
        commands::ServiceAction::Reconcile { print } => commands::service::run_reconcile(print),
        commands::ServiceAction::Uninstall => commands::service::run_uninstall(),
        commands::ServiceAction::Status => commands::service::run_status(),
        commands::ServiceAction::Logs { follow, lines } => {
            commands::service::run_logs(follow, lines)
        }
        commands::ServiceAction::PruneLogs { dry_run } => {
            commands::service::run_prune_logs(dry_run)
        }
    }
}

/// Run the naked `phux` — no verb, so attach to the user's session, locally
/// or at the root `--remote` target.
fn run_naked_invocation(
    root_rec: &commands::RecOpts,
    root_remote: Option<String>,
    socket: Option<std::path::PathBuf>,
) -> ExitCode {
    let rec_spec = match plan_rec(root_rec) {
        Ok(spec) => spec,
        Err(code) => return code,
    };
    if let Some(target) = root_remote {
        // The `--socket` collision was already refused post-parse.
        return attach_remote_target(&target, None, None, false, rec_spec.as_ref());
    }
    commands::attach::run_naked(socket, rec_spec.as_ref())
}

/// The verb table: one arm per subcommand. `socket` is the shared root
/// global; `root_rec` and `root_remote` are read only by the naked (`None`)
/// arm.
#[allow(
    clippy::too_many_lines,
    reason = "one match arm per CLI subcommand; the dispatch is a flat verb table, clearer whole than split."
)]
fn dispatch(
    command: Option<Command>,
    socket: Option<std::path::PathBuf>,
    root_rec: &commands::RecOpts,
    root_remote: Option<String>,
) -> ExitCode {
    match command {
        Some(Command::Attach {
            session,
            quic,
            ws,
            token,
            cert_fingerprint,
            tls_server_name,
            remote,
            code,
            no_enroll,
            ssh,
            remote_phux,
            udp_ports,
            viewer,
            take,
            rec,
        }) => run_attach(AttachInvocation {
            session,
            quic,
            ws,
            token,
            cert_fingerprint,
            tls_server_name,
            remote,
            code,
            no_enroll,
            ssh,
            remote_phux,
            udp_ports,
            viewer,
            take,
            rec,
            socket,
        }),
        Some(Command::Server {
            // --ensure returns from run before tracing and this dispatch.
            ensure: _,
            json: _,
            session,
            listen,
            quic,
            webtransport,
            connect,
            hub,
            exit_after_idle,
            daemonize,
            seed_command,
            resume,
            no_seed,
        }) => commands::server::run_server(
            (!no_seed).then_some(session.as_str()),
            socket,
            listen,
            quic,
            webtransport,
            connect,
            hub,
            exit_after_idle,
            daemonize,
            seed_command.as_deref(),
            resume,
        ),
        Some(Command::Ls {
            json, all: true, ..
        }) => commands::ls::run_ls_all(json.json, socket),
        Some(Command::Ls { json, remote, .. }) => {
            commands::ls::run_ls(json.json, remote.with_socket(socket))
        }
        Some(Command::Whoami { json, remote }) => {
            commands::whoami::run_whoami(json.json, remote.with_socket(socket))
        }
        Some(Command::Status { json }) => commands::status::run_status(json.json, socket),
        Some(Command::RuntimeInfo { json }) => commands::runtime_info::run(json.json),
        Some(Command::Perf { json, watch, reset }) => commands::perf::run_perf(
            commands::perf::PerfOptions {
                json: json.json,
                watch,
                reset,
            },
            socket,
        ),
        Some(Command::New {
            name,
            session,
            cwd,
            json,
            env,
            idempotency_key,
            remote,
            empty,
            command,
        }) => match commands::spawn::parse_key_arg(idempotency_key.as_deref(), json) {
            Ok(idempotency_key) => commands::new::run_new(
                name,
                session,
                cwd,
                remote.with_socket(socket),
                commands::new::NewMode {
                    json,
                    empty,
                    idempotency_key,
                },
                command,
                env.into_iter().map(|item| (item.key, item.value)).collect(),
            ),
            Err(code) => code,
        },
        Some(Command::Spawn {
            satellite,
            target,
            split,
            ratio,
            projection,
            cwd,
            retain,
            idempotency_key,
            json,
            command,
        }) => match commands::spawn::parse_key_arg(idempotency_key.as_deref(), json.json) {
            Ok(idempotency_key) => commands::spawn::run_spawn(
                satellite,
                target,
                split,
                ratio,
                projection.as_deref(),
                cwd,
                json.json,
                socket,
                command,
                commands::spawn::SpawnDurability {
                    retain_secs: retain,
                    idempotency_key,
                },
            ),
            Err(code) => code,
        },
        Some(Command::Launch {
            integration,
            list,
            print,
            json,
            target,
            split,
            ratio,
            projection,
            cwd,
            extra,
        }) => commands::launch::run_launch(
            integration,
            list,
            print,
            json.json,
            target,
            split,
            ratio,
            projection.as_deref(),
            cwd,
            socket,
            &extra,
        ),
        Some(Command::Kill {
            target,
            server,
            idempotency_key,
            yes,
            remote,
        }) => match commands::spawn::parse_key_arg(idempotency_key.as_deref(), false) {
            Ok(key) => commands::kill::run(target, server, key, yes, remote.with_socket(socket)),
            Err(code) => code,
        },
        Some(Command::Detach {
            session,
            yes,
            remote,
        }) => commands::detach::run_detach(session, yes, remote.with_socket(socket)),
        Some(Command::InsertPane {
            target,
            new_pane,
            split,
            ratio,
            projection,
            json,
        }) => commands::spatial::run_insert_pane(
            &target,
            &new_pane,
            split.into(),
            ratio,
            projection,
            json,
            socket,
        ),
        Some(Command::MovePane {
            source,
            target,
            split,
            ratio,
            projection,
            json,
        }) => commands::spatial::run_move_pane(
            &source,
            &target,
            split.into(),
            ratio,
            projection,
            json,
            socket,
        ),
        Some(Command::SwapPane {
            first,
            second,
            projection,
            json,
        }) => commands::spatial::run_swap_pane(&first, &second, projection, json, socket),
        Some(Command::Resize {
            target,
            geometry,
            json,
        }) => commands::resize::run_resize(&target, geometry, json.json, socket),
        Some(Command::Take { target, ttl }) => commands::supervise::run_take(&target, ttl, socket),
        Some(Command::Give { target }) => commands::supervise::run_give(&target, socket),
        Some(Command::Signal {
            target,
            signal,
            idempotency_key,
            yes,
        }) => match commands::spawn::parse_key_arg(idempotency_key.as_deref(), false) {
            Ok(key) => commands::supervise::run_signal(&target, signal, key, yes, socket),
            Err(code) => code,
        },
        Some(Command::Approvals { json }) => commands::approvals::run_approvals(json.json, socket),
        Some(Command::Approve { id, yes }) => commands::approvals::run_approve(&id, yes, socket),
        Some(Command::Deny { id }) => commands::approvals::run_deny(&id, socket),
        Some(Command::Update { opts }) => commands::update::run_update(&opts, socket),
        Some(Command::Channel { channel, json }) => {
            commands::channel::run(channel, json.json, socket)
        }
        Some(Command::Cockpit { json }) => commands::cockpit::run(json.json),
        Some(Command::Upgrade {}) => commands::upgrade::run_upgrade(socket),
        Some(Command::Rename {
            session,
            new_name,
            remote,
        }) => commands::rename::run_rename(&session, &new_name, remote.with_socket(socket)),
        Some(Command::Snapshot {
            session,
            json,
            scrollback,
            cells,
            tail,
            unwrap,
            rendered,
            format,
            cols,
            rows,
        }) => commands::snapshot::run_snapshot(
            session.as_deref(),
            json.json,
            &commands::snapshot::ReadOpts {
                scrollback,
                cells,
                tail,
                unwrap,
                format,
            },
            &commands::snapshot::RenderedOpts {
                rendered,
                cols,
                rows,
            },
            socket,
        ),
        Some(Command::SendKeys { target, keys }) => {
            commands::send_keys::run_send_keys(&target, &keys, socket)
        }
        Some(Command::Paste {
            target,
            text,
            untrusted,
        }) => commands::paste::run_paste(&target, text, untrusted, socket),
        Some(Command::Wait {
            session,
            until,
            regex,
            tail,
            output_only,
            idle,
            timeout,
            json,
        }) => commands::wait::run_wait(commands::wait::WaitArgs {
            session: session.as_deref(),
            until,
            regex,
            idle,
            tail,
            output_only,
            timeout,
            json: json.json,
            socket,
        }),
        Some(Command::Watch {
            session,
            until,
            timeout,
            after,
            json,
        }) => commands::watch::run_watch(commands::watch::WatchArgs {
            session: session.as_deref(),
            until: &until,
            timeout,
            after: after.as_deref(),
            json: json.json,
            socket,
        }),
        Some(Command::Resource { action }) => commands::resource::run_resource(&action, socket),
        Some(Command::Rec {
            target,
            out,
            format,
            from,
            duration,
            fps,
            idle_limit,
            max_bytes,
            cast_version,
            json,
        }) => commands::rec::run_rec(commands::rec::RecArgs {
            target: target.as_deref(),
            out: &out,
            format,
            from: from.as_deref(),
            duration,
            fps,
            idle_limit,
            max_bytes,
            cast_version,
            json: json.json,
            socket,
        }),
        Some(Command::Play {
            file,
            target,
            speed,
            idle_limit,
            loops,
            split,
            ratio,
            no_fit,
            close,
            json,
            pty_writer,
        }) => commands::play::run_play(&commands::play::PlayArgs {
            file: &file,
            target: target.as_deref(),
            speed: speed.0,
            idle_limit,
            // The CLI spells "repeat forever" as `--loop` with no value,
            // which clap fills in as 0; `passes` carries that as `None` so
            // the player's loop condition is a plain "count remaining".
            passes: match loops {
                None => Some(1),
                Some(0) => None,
                Some(n) => Some(n),
            },
            split,
            ratio,
            no_fit,
            close,
            json: json.json,
            socket,
            pty_writer,
        }),
        Some(Command::Ask {
            target,
            id,
            suggestions,
            elapsed_seconds,
            json,
            question,
        }) => commands::ask::run_ask(
            &target,
            id,
            suggestions,
            elapsed_seconds,
            json.json,
            question,
            socket,
        ),
        Some(Command::Agent { action }) => commands::agent::run_agent(&action, socket),
        Some(Command::Run {
            target,
            command,
            timeout,
            force,
            json,
        }) => commands::run::run_run(&target, &command, timeout, force, json.json, socket),
        Some(Command::Config { action }) => commands::config::run_config(&action, socket),
        Some(Command::Plugin { action }) => commands::plugin::run_plugin(&action),
        Some(Command::Workspace { action }) => commands::workspace::run_workspace(&action, socket),
        Some(Command::Tag { action }) => commands::tag::run_tag(&action, socket),
        Some(Command::StdioBridge {}) => commands::stdio_bridge::run_stdio_bridge(socket),
        Some(Command::Bootstrap {
            client_version,
            port_range,
            linger,
        }) => commands::bootstrap::run(&commands::bootstrap::BootstrapArgs {
            socket,
            client_version,
            port_range,
            linger,
        }),
        Some(Command::Relay { action }) => commands::relay::run_relay(action),
        Some(Command::Pair {
            action,
            tokens,
            cert,
            qr,
            host,
            name,
            json,
            migrate_legacy,
            replace_token,
        }) => commands::pair::run_pair(
            action,
            socket,
            tokens,
            cert,
            qr,
            host,
            name,
            json,
            migrate_legacy,
            replace_token,
        ),
        Some(Command::Workload { action, json }) => commands::workload::run(action, json),
        Some(Command::Completion { shell }) => commands::completion::run_completion(shell.into()),
        // Returned above, before process-global setup.
        Some(Command::Mcp { .. }) => ExitCode::FAILURE,
        Some(Command::Skill { scope }) => skill::run(scope),
        Some(Command::Worktree { action }) => commands::worktree::run_worktree(&action, socket),
        Some(Command::Doctor { json }) => commands::doctor::run_doctor(json, socket),
        Some(Command::Logs {
            server,
            client,
            cockpit,
            pid,
            follow,
            lines,
            json,
        }) => commands::logs::run_logs(server, client, cockpit, pid, follow, lines, json),
        Some(Command::Report { action, json }) => commands::report::run_report(action, json.json),
        Some(Command::Host { action }) => commands::host::run_host(&action),
        Some(Command::Service { action }) => run_service(action, socket),
        Some(Command::GenReferenceDocs { out }) => {
            commands::gen_reference_docs::run_gen_reference_docs(out)
        }
        None => run_naked_invocation(root_rec, root_remote, socket),
    }
}

#[must_use]
pub fn run() -> ExitCode {
    // A server about to hot-swap into this binary asks what kind of build it
    // is (`phux_config::instance::BuildKind`) before anything else runs.
    if std::env::var_os(phux_config::instance::PROBE_BUILD_KIND_ENV).is_some() {
        let kind = phux_config::instance::build_kind();
        output::bytes(format!("{}\n", kind.as_str()).as_bytes());
        return ExitCode::SUCCESS;
    }
    // A development build whose state directory resolves to production
    // (`PHUX_PROFILE=default` outside a temp sandbox) would log, record, and
    // provision into the day-to-day installation's state. Nothing runs.
    if let Err(refusal) =
        phux_config::production::refuse_dev_on_production_state(&phux_config::instance::state_dir())
    {
        eprintln!("phux: {refusal}");
        return ExitCode::from(2);
    }
    let raw: Vec<std::ffi::OsString> = std::env::args_os().collect();
    // Key material never belongs on a `phux workload` command line, and argv is
    // echoed in too many places to scrub, so that verb is refused before parsing
    // (`workload-auth.md` §8).
    if workload_argv_carries_key_material(&raw[1..]) {
        eprintln!(
            "phux: the command line was refused and is not echoed, because it appears to contain key material"
        );
        eprintln!(
            "hint: pass certificates and CSRs on stdin or with --file, and keep private keys out of arguments entirely"
        );
        return ExitCode::from(2);
    }
    if let Some(code) = preparse_endpoint(&raw[1..]) {
        return code;
    }
    let raw = rewrite_help_verb(raw);

    let refs: Vec<&OsStr> = raw.iter().map(std::ffi::OsStr::new).collect();
    if let Some(answer) = Cli::completion_request(&raw[1..]) {
        output::bytes(answer.as_bytes());
        return ExitCode::SUCCESS;
    }
    let cli = match Cli::parse_from_argv(&refs) {
        Ok(cli) => cli,
        Err(err) => return report_parse_error(&refs[1..], err),
    };

    // Clap's root args intentionally coexist with subcommands so the global
    // --socket works on either side of a verb. That also means `exclusive`
    // alone does not reject a following subcommand, so close that edge here
    // rather than silently ignoring the verb.
    if let Some(scope) = cli.skill {
        if cli.command.is_some() {
            eprintln!(
                "phux: --skill is a standalone endpoint and cannot be combined with a command"
            );
            return ExitCode::from(2);
        }
        // Like clap's built-in --help and --version actions, this socketless
        // endpoint exits before tracing, config, or TTY setup.
        return skill::run(scope);
    }

    // Usage errors caught after clap: the `--rec` scope rule (see
    // `root_rec_before_verb`) and a `--socket` handed to a verb that never
    // dials a server. Both are refusals with the remedy named, and both use
    // clap's usage-error exit code.
    if let Some(code) = usage_refusal(&cli) {
        return code;
    }

    // The launcher must be transparent: replace this process before tracing,
    // config, socket, or TTY setup can alter the MCP stdio contract.
    if let Some(Command::Mcp { args }) = &cli.command {
        return commands::mcp::run(args);
    }

    // Runtime discovery must not open logs, load config, or contact a server.
    // In particular a caller's PHUX_LOG may be a blocking FIFO.
    if let Some(Command::RuntimeInfo { json }) = &cli.command {
        return commands::runtime_info::run(json.json);
    }

    // The one-shot watchdog must precede any potentially blocking log open
    // (PHUX_LOG may name a FIFO). Ensure initializes tracing on its bounded
    // worker; its watchdog reports failures directly on stderr.
    if let Some(Command::Server {
        ensure: true, json, ..
    }) = &cli.command
    {
        return commands::server::run_ensure(cli.socket, json.json);
    }

    // Refuse every alt-screen path before telemetry, dialing, server spawn,
    // filesystem mutation, or terminal-control output can happen.
    if is_interactive_client(&cli)
        && let Err(code) = commands::attach::interactive_tty_preflight()
    {
        return code;
    }

    let _log_guard: Option<phux_server::telemetry::WorkerGuard> = init_tracing(&cli);

    let Cli {
        rec: root_rec,
        skill: _,
        remote: root_remote,
        capabilities: _,
        socket,
        command,
    } = cli;

    dispatch(command, socket, &root_rec, root_remote)
}

#[cfg(test)]
pub(crate) fn parse_cli<I, S>(args: I) -> Result<Cli, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let owned: Vec<String> = args.into_iter().map(|s| s.as_ref().to_owned()).collect();
    let words: Vec<&OsStr> = owned.iter().map(|s| OsStr::new(s.as_str())).collect();
    Cli::parse_from_argv(&words).map_err(|err| format!("{err:?}"))
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use crate::commands::Command;

    fn argv(words: &[&str]) -> Vec<std::ffi::OsString> {
        words.iter().map(std::ffi::OsString::from).collect()
    }

    /// The up-front refusal is the `workload` verb's alone, and only before
    /// `--`: other verbs carry key-looking text for panes and scanners.
    #[test]
    fn only_the_workload_verb_refuses_key_material_before_the_separator() {
        let key = "-----BEGIN PRIVATE KEY-----\nMIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg\n-----END PRIVATE KEY-----";
        let cert_out = format!("--cert-out={key}");
        assert!(super::workload_argv_carries_key_material(&argv(&[
            "workload",
            "add-key",
            "--scope",
            "observe@global",
            &cert_out,
        ])));
        for passes in [
            vec!["run", "--", "grep", "PRIVATE KEY", "/dev/null"],
            vec!["send-keys", "%1", key],
            vec!["send-keys", "%1", "workload", key],
            vec!["workload", "add-key", "--", key],
        ] {
            assert!(
                !super::workload_argv_carries_key_material(&argv(&passes)),
                "{passes:?}"
            );
        }
        let secret = "kG-fUyzXcIZ2qVoMkH7Se-XnBIgz8qr4is4e0PFiWQ0";
        let redacted = super::redact_long_base64(&format!("error: unexpected argument '{secret}'"));
        assert_eq!(redacted, "error: unexpected argument '<redacted>'");
    }

    /// Key material on a command line is caught by its PEM markers before
    /// anything parses, and a bare base64 line is redacted from usage errors
    /// without mangling the usage text around it.
    #[test]
    fn key_material_is_caught_on_argv_and_redacted_from_usage_errors() {
        assert!(super::workload_argv_carries_key_material(&argv(&[
            "workload",
            "add-key",
            "--cert-out=-----BEGIN EC PRIVATE KEY-----",
        ])));
        assert!(!super::workload_argv_carries_key_material(&argv(&[
            "run", "--", "grep", "-r", "BEGIN",
        ])));
        let line = "MHcCAQEEIBDpHVEl6z9Z0t6xw5Vg3F1wVZ3n7xHqAoGCCqGSM49AwEHoUQDQgAE";
        let rendered = format!(
            "error: unexpected argument '{line}'\nUsage: phux workload add-key --scope <VERBS@SELECTOR>"
        );
        let redacted = super::redact_long_base64(&rendered);
        assert!(!redacted.contains(line), "{redacted}");
        assert!(redacted.contains("'<redacted>'"), "{redacted}");
        assert!(
            redacted.contains("phux workload add-key --scope <VERBS@SELECTOR>"),
            "{redacted}"
        );
    }

    /// `phux help <verb...>` becomes `phux <verb...> --help`; a bare `help`
    /// and anything else pass through untouched.
    #[test]
    fn help_verb_is_rewritten_to_the_verbs_own_help_flag() {
        assert_eq!(
            super::rewrite_help_verb(argv(&["phux", "help", "host", "add"])),
            argv(&["phux", "host", "add", "--help"])
        );
        assert_eq!(
            super::rewrite_help_verb(argv(&["phux", "help"])),
            argv(&["phux", "help"])
        );
        assert_eq!(
            super::rewrite_help_verb(argv(&["phux", "ls", "help"])),
            argv(&["phux", "ls", "help"])
        );
        assert_eq!(
            super::rewrite_help_verb(argv(&["phux", "--socket", "/s", "help", "ls"])),
            argv(&["phux", "--socket", "/s", "ls", "--help"])
        );
        assert_eq!(
            super::help_word_index(&argv(&["--socket=/s", "help"])),
            Some(1)
        );
        assert_eq!(super::help_word_index(&argv(&["--socket", "/s"])), None);
        assert_eq!(super::help_word_index(&argv(&["ls"])), None);
    }

    /// Hidden verbs and aliases count: `phux help stdio-bridge` and
    /// `phux help a` render pages, while a topic name is not a verb.
    #[test]
    fn top_level_verbs_include_hidden_ones_and_aliases() {
        for word in ["attach", "a", "stdio-bridge", "bootstrap", "list"] {
            assert!(super::is_top_level_verb(word), "{word} is a verb");
        }
        for word in ["targets", "environment", "nonsense", ""] {
            assert!(!super::is_top_level_verb(word), "{word} is not a verb");
        }
    }

    /// The coloured page and the plain page carry the same text, and the
    /// footer follows the style rather than the palette.
    #[test]
    fn styled_root_page_strips_to_the_plain_one() {
        use usage::help::{Palette, Style};
        let plain =
            super::render_help_page(Cli::spec().root.cmd, true, Style::PLAIN).expect("root page");
        let quiet_plain = super::render_help_page(
            Cli::spec().root.cmd,
            true,
            Style::PLAIN.palette(Palette::DEFAULT.heading("bold")),
        )
        .expect("root page");
        assert_eq!(
            plain, quiet_plain,
            "a palette on a plain style must stay plain"
        );
        assert!(!plain.contains('\x1b'));

        let coloured = super::render_help_page(Cli::spec().root.cmd, true, Style::COLOURED)
            .expect("root page");
        assert!(coloured.contains("\x1b[1mLearn more:\x1b[0m"));
        let stripped: String = {
            let mut out = String::new();
            let mut rest = coloured.as_str();
            while let Some(at) = rest.find('\x1b') {
                out.push_str(&rest[..at]);
                let after = &rest[at..];
                let end = after.find('m').map_or(after.len(), |m| m + 1);
                rest = &after[end..];
            }
            out.push_str(rest);
            out
        };
        // The colouring pass paints `code` spans and drops their backticks,
        // so the comparison is on the text with backticks removed.
        assert_eq!(stripped.replace('`', ""), plain.replace('`', ""));

        // A subcommand page carries no footer.
        let attach = Cli::spec()
            .root
            .subcommands
            .iter()
            .find(|sub| sub.cmd.name == "attach")
            .expect("attach");
        let page = super::render_help_page(attach.cmd, true, Style::PLAIN).expect("attach page");
        assert!(!page.contains("Learn more:"));
    }

    /// `phux new <NAME>` must read the bare positional as the SESSION NAME,
    /// not as a command to spawn (the phux-new-foo bug: `phux new foo` tried
    /// to exec `foo` in an auto-named "0" session). The seed command is only
    /// taken after `--`.
    #[test]
    fn new_positional_is_session_name_command_requires_dash_dash() {
        let cli = crate::parse_cli(["phux", "new", "foo"]).expect("`phux new foo` must parse");
        let Some(Command::New {
            name,
            session,
            command,
            ..
        }) = cli.command
        else {
            panic!("expected New");
        };
        assert_eq!(
            name.as_deref(),
            Some("foo"),
            "positional is the session name"
        );
        assert_eq!(session, None, "-s not given");
        assert!(
            command.is_empty(),
            "no command without `--`; got {command:?}"
        );

        // Name + an explicit `-- CMD …`.
        let cli = crate::parse_cli(["phux", "new", "work", "--", "htop", "-d", "1"])
            .expect("`phux new work -- htop -d 1` must parse");
        let Some(Command::New { name, command, .. }) = cli.command else {
            panic!("expected New");
        };
        assert_eq!(name.as_deref(), Some("work"));
        assert_eq!(command, vec!["htop", "-d", "1"]);

        // No name, command-only via `--` ⇒ auto-named session running CMD.
        let cli =
            crate::parse_cli(["phux", "new", "--", "htop"]).expect("`phux new -- htop` parses");
        let Some(Command::New { name, command, .. }) = cli.command else {
            panic!("expected New");
        };
        assert_eq!(name, None, "no positional ⇒ auto-name");
        assert_eq!(command, vec!["htop"]);

        // `-s` still works and stays distinct from a positional.
        let cli = crate::parse_cli(["phux", "new", "-s", "flagged"])
            .expect("`phux new -s flagged` parses");
        let Some(Command::New { name, session, .. }) = cli.command else {
            panic!("expected New");
        };
        assert_eq!(name, None);
        assert_eq!(session.as_deref(), Some("flagged"));
    }

    #[test]
    fn every_attach_capable_root_path_is_classified_before_dispatch() {
        for argv in [
            ["phux", "attach"].as_slice(),
            ["phux", "new", "work"].as_slice(),
            ["phux", "worktree", "new", "branch", "--attach"].as_slice(),
            ["phux", "worktree", "open", "branch", "--attach"].as_slice(),
        ] {
            let cli = crate::parse_cli(argv).expect("interactive invocation parses");
            assert!(super::is_interactive_client(&cli), "missed {argv:?}");
        }

        for argv in [
            ["phux", "new", "-s", "work", "--json"].as_slice(),
            ["phux", "worktree", "open", "branch"].as_slice(),
            ["phux", "ls"].as_slice(),
        ] {
            let cli = crate::parse_cli(argv).expect("headless invocation parses");
            assert!(
                !super::is_interactive_client(&cli),
                "misclassified {argv:?}"
            );
        }
    }

    #[test]
    fn root_short_help_survives_global_option_placement() {
        for argv in [
            ["-h"].as_slice(),
            ["--socket", "/tmp/phux.sock", "-h"].as_slice(),
            ["-h", "--socket=/tmp/phux.sock"].as_slice(),
        ] {
            assert!(super::short_help_requested_in(argv), "missed {argv:?}");
        }
        assert!(!super::short_help_requested_in(["attach", "-h"]));
    }

    #[test]
    fn new_json_accepts_repeatable_environment_assignments() {
        let cli = crate::parse_cli([
            "phux",
            "new",
            "--json",
            "-s",
            "managed",
            "--env",
            "GC_SESSION=managed",
            "--env",
            "COMPLEX=a=b",
        ])
        .expect("`phux new --json --env KEY=VALUE` must parse");
        let Some(Command::New { env, .. }) = cli.command else {
            panic!("expected New");
        };
        assert_eq!(
            env.iter()
                .map(|item| (item.key.as_str(), item.value.as_str()))
                .collect::<Vec<_>>(),
            vec![("GC_SESSION", "managed"), ("COMPLEX", "a=b")],
        );

        assert!(
            crate::parse_cli(["phux", "new", "-s", "interactive", "--env", "KEY=value"]).is_err(),
            "--env must require headless --json until CreateIfMissing carries environment",
        );
        assert!(
            crate::parse_cli([
                "phux",
                "new",
                "--json",
                "-s",
                "managed",
                "--env",
                "MISSING_EQUALS",
            ])
            .is_err(),
            "--env must reject values that are not KEY=VALUE",
        );
    }

    /// phux-foz.5: `phux config reload` parses, with and without an
    /// explicit `--socket`.
    #[test]
    fn spawn_and_launch_placement_flags_validate() {
        let cli = crate::parse_cli([
            "phux", "spawn", "--target", ".", "--split", "vertical", "--ratio", "0.3",
        ])
        .expect("explicit spawn placement parses");
        let Some(Command::Spawn { target, ratio, .. }) = cli.command else {
            panic!("expected Spawn");
        };
        assert_eq!(target.as_deref(), Some("."));
        assert!((ratio - 0.3).abs() < f32::EPSILON);

        assert!(crate::parse_cli(["phux", "spawn", "--ratio", "0.3"]).is_err());
        assert!(crate::parse_cli(["phux", "spawn", "--target", ".", "--ratio", "1.0"]).is_err());
        assert!(
            crate::parse_cli(["phux", "spawn", "--target", ".", "--satellite", "edge"]).is_err()
        );
        assert!(
            crate::parse_cli([
                "phux", "launch", "codex", "--target", ".", "--split", "vertical"
            ])
            .is_ok()
        );
    }

    /// `--rec` parses only on the root (naked `phux`) and on `attach`.
    #[test]
    fn rec_is_scoped_to_the_two_attaching_paths() {
        let cli = crate::parse_cli(["phux", "--rec", "demo.gif"]).expect("naked `phux --rec`");
        assert_eq!(
            cli.rec.rec.as_deref(),
            Some(std::path::Path::new("demo.gif"))
        );
        assert!(cli.command.is_none());

        let cli = crate::parse_cli(["phux", "attach", "work", "--rec", "demo.cast"])
            .expect("`phux attach NAME --rec PATH`");
        assert!(
            cli.rec.rec.is_none(),
            "the subcommand's own --rec is the one that carries the value"
        );
        let Some(Command::Attach { session, rec, .. }) = cli.command else {
            panic!("expected Attach");
        };
        assert_eq!(session.as_deref(), Some("work"));
        assert_eq!(rec.rec.as_deref(), Some(std::path::Path::new("demo.cast")));

        for argv in [
            ["phux", "ls", "--rec", "demo.gif"].as_slice(),
            ["phux", "snapshot", "--rec", "demo.gif"].as_slice(),
            // --rec-format is meaningless without a destination.
            ["phux", "--rec-format", "gif"].as_slice(),
            ["phux", "attach", "--rec-format", "gif"].as_slice(),
        ] {
            assert!(crate::parse_cli(argv).is_err(), "{argv:?} must not parse");
        }
    }

    /// A root `--rec` before a verb parses (so the global `--socket` can precede
    /// a verb) and is refused post-parse.
    #[test]
    fn root_rec_before_a_verb_is_refused_post_parse() {
        for argv in [
            ["phux", "--rec", "demo.gif", "ls"].as_slice(),
            ["phux", "--rec", "demo.gif", "attach", "work"].as_slice(),
        ] {
            let cli = crate::parse_cli(argv)
                .expect("root --rec before a verb parses; the refusal is post-parse");
            let message = super::root_rec_before_verb(&cli)
                .expect("a root --rec in front of a verb must be refused");
            assert!(
                message.contains("--rec") && message.contains("phux attach --rec"),
                "the refusal must teach the two correct spellings; got {message:?}"
            );
        }

        // The two legitimate homes stay untouched by the check.
        let cli = crate::parse_cli(["phux", "--rec", "demo.gif"]).expect("naked form");
        assert!(super::root_rec_before_verb(&cli).is_none());
        let cli = crate::parse_cli(["phux", "attach", "--rec", "demo.gif"]).expect("attach");
        assert!(super::root_rec_before_verb(&cli).is_none());
    }

    /// usage-rs `requires("--quic", "--ws")` is all-of, so a single-transport
    /// dial with `--token` (the documented `phux attach --quic HOST --token HEX`
    /// form) used to fail at parse time. The any-of rule is post-parse.
    #[test]
    fn attach_dial_auth_flags_need_one_transport() {
        for argv in [
            [
                "phux",
                "attach",
                "--quic",
                "127.0.0.1:8788",
                "--token",
                "ab",
            ]
            .as_slice(),
            [
                "phux",
                "attach",
                "--ws",
                "ws://127.0.0.1:8787",
                "--token",
                "ab",
            ]
            .as_slice(),
            [
                "phux",
                "attach",
                "--quic",
                "127.0.0.1:8788",
                "--cert-fingerprint",
                "cd",
            ]
            .as_slice(),
            [
                "phux",
                "attach",
                "--ws",
                "wss://host:8787",
                "--tls-server-name",
                "host",
            ]
            .as_slice(),
        ] {
            let cli = crate::parse_cli(argv)
                .unwrap_or_else(|err| panic!("{argv:?} must parse (single transport): {err}"));
            let Some(Command::Attach {
                quic,
                ws,
                token,
                cert_fingerprint,
                tls_server_name,
                ..
            }) = cli.command
            else {
                panic!("expected Attach for {argv:?}");
            };
            assert!(
                super::dial_auth_without_transport(
                    quic.as_deref(),
                    ws.as_deref(),
                    token.as_deref(),
                    cert_fingerprint.as_deref(),
                    tls_server_name.as_deref(),
                )
                .is_none(),
                "{argv:?} names a transport"
            );
        }

        for flag in ["--token", "--cert-fingerprint", "--tls-server-name"] {
            let argv = ["phux", "attach", flag, "x"];
            let cli = crate::parse_cli(argv).unwrap_or_else(|err| {
                panic!("{argv:?} must parse; the refusal is post-parse: {err}")
            });
            let Some(Command::Attach {
                quic,
                ws,
                token,
                cert_fingerprint,
                tls_server_name,
                ..
            }) = cli.command
            else {
                panic!("expected Attach for {argv:?}");
            };
            let message = super::dial_auth_without_transport(
                quic.as_deref(),
                ws.as_deref(),
                token.as_deref(),
                cert_fingerprint.as_deref(),
                tls_server_name.as_deref(),
            )
            .unwrap_or_else(|| panic!("{argv:?} must be refused without a transport"));
            assert!(
                message.contains("--quic") && message.contains("--ws"),
                "{argv:?} refusal must name both transports; got {message:?}"
            );
        }
    }

    /// The global `--socket` parses in both positions and lands on the same
    /// root field either way; the two spellings are one invocation.
    #[test]
    fn socket_parses_before_and_after_the_verb() {
        let before = crate::parse_cli(["phux", "--socket", "/tmp/x.sock", "ls"])
            .expect("`phux --socket X ls` parses");
        let after = crate::parse_cli(["phux", "ls", "--socket", "/tmp/x.sock"])
            .expect("`phux ls --socket X` parses");
        for cli in [before, after] {
            assert!(matches!(cli.command, Some(Command::Ls { .. })));
            assert_eq!(
                cli.socket.as_deref(),
                Some(std::path::Path::new("/tmp/x.sock"))
            );
        }
    }

    /// A `--socket` handed to a verb that never dials a server is refused
    /// (via `socketless_verb`), not silently ignored.
    #[test]
    fn socketless_verbs_are_named_and_socket_consumers_are_not() {
        for argv in [
            ["phux", "pair", "ls", "--socket", "/tmp/x.sock"].as_slice(),
            ["phux", "pair", "revoke", "id", "--socket", "/tmp/x.sock"].as_slice(),
            ["phux", "--socket", "/tmp/x.sock", "config", "path"].as_slice(),
            ["phux", "plugin", "list", "--socket", "/tmp/x.sock"].as_slice(),
            ["phux", "logs", "--socket", "/tmp/x.sock"].as_slice(),
            ["phux", "completion", "zsh", "--socket", "/tmp/x.sock"].as_slice(),
        ] {
            let cli = crate::parse_cli(argv).expect("the global --socket always parses");
            let command = cli.command.as_ref().expect("a verb was given");
            assert!(
                crate::commands::socketless_verb(command).is_some(),
                "{argv:?} names a socketless verb and must be refused"
            );
        }

        for argv in [
            ["phux", "ls", "--socket", "/tmp/x.sock"].as_slice(),
            ["phux", "pair", "--socket", "/tmp/x.sock"].as_slice(),
            ["phux", "config", "reload", "--socket", "/tmp/x.sock"].as_slice(),
            ["phux", "tag", "ls", "work", "--socket", "/tmp/x.sock"].as_slice(),
            ["phux", "service", "install", "--socket", "/tmp/x.sock"].as_slice(),
            ["phux", "worktree", "list", "--socket", "/tmp/x.sock"].as_slice(),
        ] {
            let cli = crate::parse_cli(argv).expect("consumer verbs parse");
            let command = cli.command.as_ref().expect("a verb was given");
            assert!(
                crate::commands::socketless_verb(command).is_none(),
                "{argv:?} consumes --socket and must not be refused"
            );
        }
    }

    /// A scoped flag given before the verb gets the teaching hint: the
    /// interception recognizes `--json` (and any other per-verb long flag)
    /// in clap's unknown-argument refusal.
    #[test]
    fn misplaced_scoped_flag_is_recognized_for_the_hint() {
        crate::parse_cli(["phux", "--json", "ls"])
            .expect_err("`--json` is per-verb; the root must refuse it");
        crate::parse_cli(["phux", "--no-such-flag", "ls"]).expect_err("unknown flags are refused");
    }

    /// The hint follows where the flag was typed: before the verb it says
    /// to move it; after a verb that does not take it, moving it would not
    /// help, so it names the verbs that do.
    #[test]
    fn misplaced_flag_hint_follows_the_flag_position() {
        let hint = |argv: &[&str]| {
            let os: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
            let err = Cli::parse_from_argv(&os).expect_err("refused");
            let words: Vec<String> = argv.iter().map(|w| (*w).to_owned()).collect();
            super::misplaced_flag_hint(&err, &words)
        };
        let before = hint(&["phux", "--json", "ls"]).expect("hint");
        assert!(before.contains("place it after the verb"), "{before}");
        let after = hint(&["phux", "ls", "--host", "mini"]).expect("hint");
        assert!(
            after.contains("`phux ls` does not take `--host`"),
            "{after}"
        );
        assert!(!after.contains("place it after the verb"), "{after}");
        assert_eq!(hint(&["phux", "ls", "--no-such-flag"]), None);
    }

    /// Parse `argv` to its resolved [`Command`], panicking with the argv on
    /// any failure — the shared front door for the alias-parity tests.
    fn parsed(argv: &[&str]) -> Command {
        crate::parse_cli(argv)
            .unwrap_or_else(|err| panic!("{argv:?} must parse: {err}"))
            .command
            .unwrap_or_else(|| panic!("{argv:?} names a verb"))
    }

    /// Every list-shaped registry verb answers to both `list` and `ls`, parsing
    /// to the canonical variant.
    #[test]
    fn list_aliases_map_to_the_canonical_variants() {
        use crate::commands::{PluginAction, TagAction};

        for argv in [["phux", "worktree", "list"], ["phux", "worktree", "ls"]] {
            assert!(matches!(
                parsed(&argv),
                Command::Worktree {
                    action: crate::commands::WorktreeAction::List { .. },
                }
            ));
        }
        // tag's canonical name is the short one; `list` is the alias.
        for argv in [["phux", "tag", "ls", "."], ["phux", "tag", "list", "."]] {
            assert!(matches!(
                parsed(&argv),
                Command::Tag {
                    action: TagAction::Ls { .. }
                }
            ));
        }
        for argv in [["phux", "plugin", "list"], ["phux", "plugin", "ls"]] {
            assert!(matches!(
                parsed(&argv),
                Command::Plugin {
                    action: PluginAction::List { .. }
                }
            ));
        }
        // The root registry verb keeps its established pair.
        for argv in [["phux", "ls"], ["phux", "list"]] {
            assert!(matches!(parsed(&argv), Command::Ls { .. }));
        }
    }

    /// Alias parity, remove half (phux-i0e8.8.3): every remove-shaped
    /// registry verb answers to both spellings — including `plugin unlink`,
    /// whose canonical name predates the policy and now also answers to
    /// `rm` / `remove`.
    #[test]
    fn remove_aliases_map_to_the_canonical_variants() {
        use crate::commands::{PluginAction, TagAction};

        for argv in [
            ["phux", "worktree", "remove", "feat"],
            ["phux", "worktree", "rm", "feat"],
        ] {
            assert!(matches!(
                parsed(&argv),
                Command::Worktree {
                    action: crate::commands::WorktreeAction::Remove { .. },
                }
            ));
        }
        // tag's canonical name is the short one; `remove` is the alias.
        for argv in [
            ["phux", "tag", "rm", ".", "build"],
            ["phux", "tag", "remove", ".", "build"],
        ] {
            assert!(matches!(
                parsed(&argv),
                Command::Tag {
                    action: TagAction::Rm { .. }
                }
            ));
        }
        for argv in [
            ["phux", "plugin", "unlink", "x.y"],
            ["phux", "plugin", "rm", "x.y"],
            ["phux", "plugin", "remove", "x.y"],
        ] {
            assert!(matches!(
                parsed(&argv),
                Command::Plugin {
                    action: PluginAction::Unlink { .. }
                }
            ));
        }
    }

    /// usage-rs completion scripts are thin shells that ask the live
    /// binary (`__complete_word__`). Candidates come from that request,
    /// not from names baked into the script.
    fn complete_line(line: &str) -> String {
        Cli::completion_request(&[
            "__complete_word__".into(),
            "--shell".into(),
            "bash".into(),
            "--line".into(),
            line.into(),
        ])
        .unwrap_or_default()
    }

    /// The generated completions are built from the same usage spec, so the
    /// single global `--socket` declaration must still reach them.
    #[test]
    fn completions_still_carry_socket() {
        let answer = complete_line("phux --");
        assert!(
            answer.contains("--socket"),
            "live completions lost --socket after the root-settings rework:\n{answer}"
        );
    }

    /// `--ratio` on the spatial verbs now validates at parse time
    /// (phux-i0e8.8.4): out-of-range or non-numeric ratios are clap usage
    /// errors, not runtime failures.
    #[test]
    fn spatial_ratio_validates_at_parse_time() {
        for verb in ["insert-pane", "move-pane"] {
            for bad in ["1.5", "0", "1", "-0.2", "NaN", "bogus"] {
                assert!(
                    crate::parse_cli(["phux", verb, "@1", "@2", "--ratio", bad]).is_err(),
                    "{verb} --ratio {bad} must fail at clap"
                );
            }
            assert!(
                crate::parse_cli(["phux", verb, "@1", "@2", "--ratio", "0.25"]).is_ok(),
                "{verb} --ratio 0.25 must parse"
            );
        }
    }

    /// `take --ttl` beyond the u32 millisecond range is a usage error, not a
    /// silent clamp.
    #[test]
    fn take_ttl_above_u32_ms_range_validates_at_parse_time() {
        assert!(
            crate::parse_cli(["phux", "take", "@1", "--ttl", "4294968"]).is_err(),
            "4294968s * 1000 overflows u32 ms and must fail at clap"
        );
        assert!(
            crate::parse_cli(["phux", "take", "@1", "--ttl", "4294967295"]).is_err(),
            "a wildly out-of-range value must fail at clap"
        );
        assert!(
            crate::parse_cli(["phux", "take", "@1", "--ttl", "4294967"]).is_ok(),
            "the exact u32-ms boundary (4294967 * 1000 <= u32::MAX) must parse"
        );
        assert!(
            crate::parse_cli(["phux", "take", "@1", "--ttl", "0"]).is_ok(),
            "0 (explicit no-TTL) must still parse"
        );
        assert!(
            crate::parse_cli(["phux", "take", "@1"]).is_ok(),
            "omitting --ttl entirely must still parse"
        );
    }

    /// ADR-0127: `--viewer` and `--take` parse on `attach`, refuse each
    /// other at clap, and map onto the declared role every ATTACH carries.
    #[test]
    fn attach_viewer_and_take_parse_refuse_each_other_and_map_to_roles() {
        use phux_protocol::wire::frame::RolePolicy;
        assert!(crate::parse_cli(["phux", "attach", "--viewer"]).is_ok());
        assert!(crate::parse_cli(["phux", "attach", "--take", "work"]).is_ok());
        assert!(
            crate::parse_cli(["phux", "attach", "--viewer", "--take"]).is_err(),
            "a viewer cannot take over: the flags conflict at parse time"
        );
        assert_eq!(crate::declared_attach_role(true, false), RolePolicy::VIEWER);
        assert_eq!(
            crate::declared_attach_role(false, true),
            RolePolicy::TAKEOVER
        );
        assert_eq!(
            crate::declared_attach_role(false, false),
            RolePolicy::PRIMARY,
            "no flag is the default attach, which writes no role byte"
        );
    }

    /// `new --json` without `-s NAME` is refused by clap itself
    /// (`requires = session` via the verb's arg group), replacing the old
    /// runtime gate.
    #[test]
    fn new_json_requires_session_at_the_clap_level() {
        crate::parse_cli(["phux", "new", "--json"])
            .expect_err("`new --json` without -s must be a usage error");

        // A positional NAME does not satisfy the rule: `--json` documents an
        // explicit `-s`.
        assert!(
            crate::parse_cli(["phux", "new", "work", "--json"]).is_err(),
            "positional NAME must not satisfy --json's -s requirement"
        );

        assert!(
            crate::parse_cli(["phux", "new", "--json", "-s", "work"]).is_ok(),
            "`new --json -s NAME` must parse"
        );
        assert!(
            crate::parse_cli(["phux", "new", "-s", "work"]).is_ok(),
            "-s without --json stays valid"
        );
    }

    /// Every hidden subcommand or long flag outside the internal allowlist
    /// must be a `deprecations::DEPRECATED` row, and every row must still be
    /// hidden in the parser, so neither side can drift.
    #[test]
    fn deprecation_table_matches_the_clap_tree_bidirectionally() {
        use std::collections::BTreeSet;

        use crate::deprecations::DEPRECATED;

        /// Hidden surfaces that are machine plumbing, not deprecations:
        /// each is hidden because no human should type it, and none has a
        /// replacement spelling to migrate to.
        const INTERNAL: &[&str] = &[
            // The refdocs generator (ADR-0069): machine-only since it
            // shipped.
            "phux gen-reference-docs",
            // The SSH remoting shim (`ssh HOST phux stdio-bridge`): machine
            // -invoked by `attach --host`, hidden from humans (phux-06nn),
            // not deprecated.
            "phux stdio-bridge",
            // Far end of `phux attach --ssh` (ADR-0120): machine-invoked
            // over ssh, same reasoning as stdio-bridge.
            "phux bootstrap",
            // Auto-spawn / upgrade plumbing on `phux server`.
            "phux server --daemonize",
            "phux server --seed-command",
            "phux server --resume",
            // `phux new --empty` starts its server unseeded (ADR-0105).
            "phux server --no-seed",
            // `phux play`'s in-pane writer half.
            "phux play --pty-writer",
            // The Claude shim's stdin JSON reader: invoked only by the
            // generated wrapper, one line of shell-safe tokens out.
            "phux agent hook-payload",
        ];

        /// Collect every hidden row of the tree under `path`: hidden long
        /// flags as `<path> --<flag>`, hidden subcommands expanded to one
        /// row per leaf action (matching the table's `old` spellings).
        fn walk(meta: &usage::spec::CommandMeta<'_>, path: &str, rows: &mut BTreeSet<String>) {
            for flag in meta.flags {
                if flag.hide {
                    for long in flag.flag.longs {
                        rows.insert(format!("{path} --{long}"));
                    }
                }
            }
            for sub in meta.subcommands {
                let sub_path = format!("{path} {}", sub.cmd.name);
                if sub.hide {
                    if sub.subcommands.is_empty() {
                        rows.insert(sub_path);
                    } else {
                        for leaf in sub.subcommands {
                            rows.insert(format!("{sub_path} {}", leaf.cmd.name));
                        }
                    }
                } else {
                    walk(sub, &sub_path, rows);
                }
            }
        }

        let mut tree_rows = BTreeSet::new();
        walk(Cli::spec().root, "phux", &mut tree_rows);
        for internal in INTERNAL {
            assert!(
                tree_rows.remove(*internal),
                "{internal} is allowlisted as internal but no longer hidden \
                 in the tree; prune the allowlist"
            );
        }

        let table_rows: BTreeSet<String> =
            DEPRECATED.iter().map(|row| row.old.to_owned()).collect();
        assert_eq!(
            table_rows.len(),
            DEPRECATED.len(),
            "duplicate `old` spellings in the deprecation table"
        );
        assert_eq!(
            tree_rows, table_rows,
            "hidden clap surface and the deprecation table must agree: a \
             row only in the tree is an unregistered hidden alias (add it \
             to deprecations::DEPRECATED); a row only in the table is \
             stale (the alias is gone — delete the row and regenerate \
             docs/reference/deprecations.md)"
        );
    }

    /// Every verb row's stderr line is the alias table's exact rendering of
    /// its own `old`/`new` columns, and every flag row's line names the
    /// deprecated flag and its `--split` replacement — so the generated
    /// deprecations page, the warning, and the audit test all say the same
    /// thing.
    #[test]
    fn deprecation_rows_render_their_own_notes() {
        use crate::deprecations::{DEPRECATED, DeprecatedSurface};

        for row in DEPRECATED {
            match row.surface {
                DeprecatedSurface::Verb => assert_eq!(
                    row.note,
                    format!(
                        "phux: `{}` is deprecated and will be removed; use `{}`",
                        row.old, row.new
                    ),
                    "verb row {} must render its note from its own columns",
                    row.old
                ),
                DeprecatedSurface::Flag => {
                    let flag = row.old_flag().expect("flag rows end in a long flag");
                    let axis = flag.trim_start_matches("--");
                    assert_eq!(
                        row.note,
                        format!(
                            "phux: {flag} is deprecated and will be removed; \
                             use `--split {axis}` (or `--split {short}`)",
                            short = &axis[..1]
                        ),
                        "flag row {} must warn toward its --split replacement",
                        row.old
                    );
                    assert!(
                        row.new.ends_with(&format!("--split {axis}")),
                        "flag row {} must advertise the --split spelling",
                        row.old
                    );
                }
            }
        }
    }
}
