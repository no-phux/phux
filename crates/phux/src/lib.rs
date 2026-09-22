//! phux CLI — subcommand parsing and dispatch, exposed as [`run`].
//!
//! Single executable, multiple subcommands. By convention:
//!   phux           → attach to (or auto-spawn) the user's server
//!   phux server    → run a server in the foreground (supervisord etc.)
//!   phux attach    → attach to a session by name (phux-9gw.3)
//!   phux new       → create a new session
//!   phux ls        → list sessions
//!   phux kill      → kill sessions / windows / panes
//!
//! Subcommands are unstable until v0.1. The full CLI shape lives in
//! docs/consumers/tui.md §4; subcommands not listed here are not yet wired.
//!
//! ## Why this is a library crate
//!
//! `src/main.rs` is a five-line shim that calls [`run`]. The `dhat-heap`
//! feature (heap profiling) used to live HERE, behind
//! `#[cfg(feature = "dhat-heap")]` inside this crate's own `fn main`. That
//! made `cargo test -p phux --all-features` build the SAME `phux` binary
//! that integration tests spawn via `CARGO_BIN_EXE_phux` with dhat's global
//! allocator wired in — and dhat's `Profiler` guard unconditionally
//! `eprintln!`s a shutdown summary with no way to silence it through dhat's
//! public API, which broke every test asserting clean/parseable stderr
//! (e.g. `tests/host_lifecycle.rs::duplicate_configured_satellite_names_are_rejected`).
//! Cargo has no supported way to exclude one feature from `--all-features`
//! (rust-lang/cargo#3126), so the fix is structural: `dhat-heap` now only
//! touches `src/bin/dhat_heap.rs`, a separate binary target gated by
//! `required-features`. The plain `phux` binary is unaffected by the
//! feature under any invocation, so `--all-features` is safe to combine
//! with `cargo test`/`cargo nextest run` again.

#![forbid(unsafe_code)]
#![allow(
    clippy::print_stderr,
    reason = "binary entry point; stderr is the report"
)]
// NOTE: there is deliberately NO crate-level `clippy::print_stdout` allow.
// Every stdout write goes through the `output` module's `outln!` / `out!`,
// which survive a closed reader; a bare `println!` would panic the verb the
// first time someone piped it into `head` (phux-h5hj.8). Leaving the lint
// armed is what keeps that from being a rule someone has to remember.
#![allow(
    clippy::redundant_pub_crate,
    reason = "internal submodules expose items to the crate root via pub(crate) rather than plain `pub`; the crate's only real public API is `run`, everything else stays crate-private on purpose"
)]

use std::ffi::OsStr;
use std::process::ExitCode;

use commands::Command;

// Declared FIRST and with `#[macro_use]`: `macro_rules!` are visible only to
// the code that follows their definition, so `outln!` has to be in scope
// before `mod commands` — where nearly every use of it lives — is parsed.
#[macro_use]
mod output;

mod capabilities;
mod commands;
mod companion;
mod deprecations;
mod environment;
mod exit_codes;
mod feature_names;
mod refdocs;
mod selector;
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
    // `--rec` and `--remote` belong to the naked attach. usage globals parse
    // on either side of a verb, so the scope rule stays a post-parse check
    // (`root_rec_before_verb` / `root_remote_before_verb`) rather than
    // `args_conflicts_with_subcommands`, which would also refuse
    // `phux --socket X ls`.
    about = "A terminal multiplexer you can drive by hand or script.",
    long_about = "A terminal multiplexer you can drive by hand or script.\n\n\
        Run `phux` alone to attach to your session; every other verb is headless.",
    // The root page is laid out for 80 columns whatever the terminal is:
    // the groups read as one table, and the width test on it is exact.
    term_width = 80,
    // `phux help ...` is answered before the parser runs (see `help_verb`):
    // the bare word prints the root page, a topic name prints that topic,
    // and a verb path is rewritten to `phux <verb> --help`. Leaving the
    // synthesized subcommand in place would put a lone `help` row under a
    // `Commands:` heading above the grouped inventory.
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
    // ONE declaration, `global`, replacing 36 hand-copied per-verb
    // fields (ADR-0065): `phux --socket X ls` and `phux ls --socket X` are
    // the same invocation. Verbs that never dial a server refuse a provided
    // `--socket` with a teaching error instead of silently ignoring it —
    // see `commands::socketless_verb`.
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
    // The naked attach's copy alone: `phux attach --remote` carries the
    // full form with `--code` / `--no-enroll`, and `ls`, `new`, `kill`,
    // `rename`, and `detach` take their own after the verb
    // (`root_remote_before_verb` teaches that placement).
    #[usage(long, value_name = "[USER@]HOST")]
    remote: Option<String>,

    /// Print machine-readable capabilities (with --json)
    #[usage(long)]
    capabilities: bool,

    /// Subcommand. Defaults to attaching to the last session if omitted.
    #[usage(subcommand)]
    command: Option<Command>,
}

/// The footer appended to the root long page, after usage-argv has laid
/// out the grouped inventory and the flags.
///
/// Appended by [`render_help_page`] rather than declared as `after_help`:
/// the renderer reflows any prose it is handed unless a line starts with
/// four spaces, and this block is a two-column table that has to line up
/// with the sections above it. The topics it names are answered by
/// [`help_topic`], and the same function renders the generated reference
/// pages, so the footer, the topics, and the docs cannot drift apart.
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

/// The teaching error for a root `--rec` in front of a verb.
///
/// The naked `phux` attach owns the root `--rec` pair; `phux attach` carries
/// its own copy, and every other verb records through `phux rec`. The root
/// used to enforce this with `args_conflicts_with_subcommands`, which had to
/// go when `--socket` became a root global (clap rejects a matched root arg
/// before ANY subcommand, global or not) — so the scope rule is this explicit
/// post-parse check now, same refusal, better words.
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

/// The teaching error for a root `--remote` in front of a verb.
///
/// Same scope rule as the root `--rec`, and for the same reason: the root
/// copy exists so the naked `phux --remote me@mini` reads like `ssh`, and
/// `phux attach --remote` is where the verb-scoped form (with `--code` and
/// `--no-enroll`) lives. Silently ignoring the root flag in front of `ls`
/// would be the worst of the three options.
/// `--qr` belongs to minting a credential. usage-rs lets parent flags
/// parse next to a subcommand, so the refusal is post-parse rather than a
/// grammar conflict with `rotate`/`revoke`.
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

/// The parse error for whichever `--remote` this invocation carries, root
/// or verb-scoped.
///
/// Runs ahead of the TTY preflight so a bad target is reported as the usage
/// error it is, rather than as a missing terminal.
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
/// `--remote`.
///
/// A post-parse check rather than a clap `conflicts_with`, for ADR-0065's
/// reason: `--socket` is a root global and clap validates conflicts per
/// parser, so a root-matched `--socket` never meets a sub-matched `--remote`.
/// It runs BEFORE the interactive TTY preflight because a contradiction
/// between two flags is a usage error, and reporting it as "requires a
/// terminal" would name the wrong problem.
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

fn removed_spelling_hint(
    err: &usage::Error<'_, '_>,
    argv: &[String],
) -> Option<&'static deprecations::Removal> {
    match err {
        usage::Error::MissingSubcommand => argv.iter().find_map(|word| {
            deprecations::REMOVED
                .iter()
                .find(|row| row.old_root_verb() == Some(word.as_str()))
        }),
        usage::Error::UnknownFlag { token } => {
            let flag = token_str(token);
            deprecations::REMOVED.iter().find(|row| {
                row.old_flag() == Some(flag.as_str())
                    && row
                        .flag_verb()
                        .is_some_and(|verb| argv.iter().any(|word| word == verb))
            })
        }
        _ => None,
    }
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
            if let Some(flag) = misplaced_scoped_flag(&err) {
                eprintln!(
                    "hint: `{flag}` is set per verb, not on `phux` itself; place it after the verb: `phux <verb> {flag} ...`"
                );
            }
            if let Some(row) = removed_spelling_hint(&err, &words) {
                eprintln!(
                    "hint: `{}` was removed in {}; use `{}`",
                    row.old, row.removed_in, row.new
                );
            }
            ExitCode::from(2)
        }
    }
}

/// Resolve `--rec` into a full recording plan, or report why it cannot be.
///
/// Called on the cooked terminal, before the attach path raises the alt
/// screen, so a bad path or an unrecognized extension is a plain stderr line
/// and a failing exit code rather than a surprise after the TUI is up.
fn plan_rec(opts: &commands::RecOpts) -> Result<Option<commands::rec::RecordSpec>, ExitCode> {
    opts.rec
        .as_deref()
        .map(|path| commands::rec::spec::plan(path, opts.rec_format))
        .transpose()
}

/// Print the one-line build banner to stderr.
///
/// Reserved for the long-running, human-watched foreground entry points
/// whose stderr stays visible: `phux server` and `phux relay run`. It is
/// deliberately NOT printed on any attach path (naked `phux`,
/// `phux attach`, `phux new`) — those raise the alt screen almost
/// immediately, wiping the line before a human can read it
/// (phux-i0e8.10.1) — and NOT before a one-shot control verb (`ls`,
/// `snapshot`, `send-keys`, `run`, `wait`, `new`, `kill`, `config`) so
/// those leave stderr clean for scripts and agents, and never before a
/// `--json` path. `phux --version` reports the version on stdout.
pub(crate) fn print_banner() {
    eprintln!("{BANNER}");
}

/// The banner line itself: a plain `phux <version>`, nothing else. No
/// repo-internal paths — an installed binary's user has no checkout, so
/// `docs/…` pointers are noise at best (the leak test in `help_inventory`
/// scans this constant along with every help string).
pub(crate) const BANNER: &str = concat!("phux ", env!("PHUX_VERSION_LABEL"));

/// Whether this invocation will enter the interactive TUI (raw mode +
/// alt screen) and therefore MUST keep logs off stderr.
///
/// The alt-screen-entering paths are: `phux attach`, naked `phux` (attach
/// fallback), `phux new` *without* `--json`, and worktree new/open with
/// `--attach`. Headless creation stays on the stderr path like every other
/// one-shot verb.
const fn is_interactive_client(cli: &Cli) -> bool {
    match &cli.command {
        Some(
            Command::Attach { .. }
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

/// The names `phux help <topic>` answers, each with the page it prints.
///
/// A topic is a page that is not a command: the selector grammar, the
/// environment, the exit codes. They used to be appended to the root
/// `--help`, which put a screen of variable names between the reader and
/// the command list; a topic is read when it is asked for.
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

/// Answer `phux help` and `phux help <topic>` from argv, before the parser
/// runs. `phux help <verb>` is not answered here: `rewrite_help_verb`
/// turns it into `phux <verb> --help` so the parser renders the verb's own
/// page, and a word that is neither a topic nor a verb is refused with
/// both lists named.
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

/// The colour policy for help printed to this process's stdout: coloured
/// on a terminal (or under `CLICOLOR_FORCE`), plain in a pipe or under
/// `NO_COLOR`. The palette is deliberately quiet — bold headings, cyan
/// command and flag names — rather than the renderer's default yellow and
/// green, so a page reads as one document rather than a traffic light.
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

/// Render one command's help page with an explicit colour policy.
///
/// The one path every help surface goes through: the parser's `--help`
/// answer, `phux help`, the help-inventory tests, and the generated
/// reference pages, so they render byte-identical text. The root long
/// page additionally carries [`ROOT_LEARN_MORE`], appended here rather
/// than declared as `after_help` because the renderer would reflow its
/// columns.
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

/// The usage refusals clap's own grammar cannot express, reported once the
/// CLI has parsed and before any process-global setup runs.
///
/// Covers the `--capabilities` spelling that needs `--json`, the `--rec` and
/// `--remote` scope rules (see `root_rec_before_verb` and
/// `root_remote_before_verb`), a malformed `--remote` target, the
/// `--socket`/`--remote` collision, and a `--socket` handed to a verb that
/// never dials a server. Each is a refusal with the remedy named, and each
/// uses clap's usage-error exit code.
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

/// Install the process-global tracing subscriber once, before any
/// runtime spins up. Without this, every `tracing::{info,debug,...}`
/// call site is a no-op.
///
/// The choice of sink depends on whether this invocation will enter
/// the TUI (raw mode + alt screen). An interactive client owns the
/// alt screen, so it MUST log to a file only — a stray stderr line
/// corrupts the display. Every other command (foreground server,
/// one-shot control verbs, `--json` paths) keeps the historical
/// stderr layer (plus an optional `PHUX_LOG` file tee).
///
/// The returned `WorkerGuard` (when a file sink is involved) keeps
/// the non-blocking writer's background thread alive; bind it for the
/// lifetime of `main` so logs flush on exit. An init failure is
/// non-fatal: the binary should keep working even if a future test
/// harness or library already installed its own subscriber.
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
    // `--socket` is a local UDS path; the remote transports do not
    // read it. The old per-verb clap conflict could not survive the
    // move to a root global (clap validates conflicts per parser, so
    // `phux --socket X attach --quic Y` would slip through), so the
    // refusal is explicit here and covers both flag positions.
    // The `--remote` half of this rule is enforced post-parse (see
    // `socket_and_remote_collide`), ahead of the TTY preflight.
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

/// The verb table: one arm per CLI subcommand, each delegating to the
/// command module that owns it.
///
/// `command` is moved into the match; `socket` is the root global every
/// arm shares (each arm consumes it at most once, and only one arm runs).
/// `root_rec` and `root_remote` are the naked-invocation halves of their
/// root flags, and the `None` arm alone reads them.
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
        Some(Command::Ls { json, remote }) => {
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
    let raw: Vec<std::ffi::OsString> = std::env::args_os().collect();
    // Key material never belongs on a `phux workload` command line, and argv
    // is echoed in too many places (parse errors, paths in messages) to scrub
    // each one, so that verb is refused before anything parses or dispatches
    // (`workload-auth.md` §8). Other verbs keep their text: a pane's input or
    // a secret scanner's pattern may legitimately look like key material.
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

    /// `phux paste TARGET [TEXT]`: TEXT is optional (omitted ⇒ stdin),
    /// trust defaults to trusted, and `--untrusted`/`--socket` are flags
    /// that must precede nothing in particular (no trailing var-arg).
    #[test]
    fn paste_parses_text_arg_stdin_form_and_untrusted_flag() {
        // Explicit TEXT argument.
        let cli = crate::parse_cli(["phux", "paste", "work", "hello world"])
            .expect("`phux paste work TEXT` parses");
        assert_eq!(cli.socket, None);
        let Some(Command::Paste {
            target,
            text,
            untrusted,
        }) = cli.command
        else {
            panic!("expected Paste");
        };
        assert_eq!(target, "work");
        assert_eq!(text.as_deref(), Some("hello world"));
        assert!(!untrusted, "trusted is the default");

        // TEXT omitted ⇒ the payload comes from stdin.
        let cli = crate::parse_cli(["phux", "paste", "work:1.0"])
            .expect("`phux paste TARGET` (stdin form) parses");
        let Some(Command::Paste { target, text, .. }) = cli.command else {
            panic!("expected Paste");
        };
        assert_eq!(target, "work:1.0");
        assert_eq!(text, None, "omitted TEXT means stdin");

        // `--untrusted` and the global `--socket` parse alongside both forms.
        let cli = crate::parse_cli([
            "phux",
            "paste",
            "--untrusted",
            "--socket",
            "/tmp/phux.sock",
            "@3",
            "payload",
        ])
        .expect("flags parse");
        assert_eq!(
            cli.socket.as_deref(),
            Some(std::path::Path::new("/tmp/phux.sock")),
            "a post-verb --socket lands on the root global"
        );
        let Some(Command::Paste { untrusted, .. }) = cli.command else {
            panic!("expected Paste");
        };
        assert!(untrusted);

        // A target is required.
        assert!(crate::parse_cli(["phux", "paste"]).is_err());
    }

    /// `phux relay run` requires an explicit `--listen` (no default bind
    /// address) and caps connections at 64 unless `--max-conns` says
    /// otherwise; `phux relay pair` requires `--route`. A zero cap is a
    /// parse error, not a runtime surprise.
    #[test]
    fn relay_verbs_parse_and_validate_flags() {
        use crate::commands::relay::RelayAction;

        let cli = crate::parse_cli(["phux", "relay", "run", "--listen", "127.0.0.1:4433"])
            .expect("`phux relay run --listen` parses");
        let Some(Command::Relay {
            action: RelayAction::Run { listen, max_conns },
        }) = cli.command
        else {
            panic!("expected Relay Run");
        };
        assert_eq!(listen, "127.0.0.1:4433".parse().unwrap());
        assert_eq!(max_conns, 64, "default cap");

        let cli = crate::parse_cli([
            "phux",
            "relay",
            "run",
            "--listen",
            "0.0.0.0:4433",
            "--max-conns",
            "8",
        ])
        .expect("explicit --max-conns parses");
        let Some(Command::Relay {
            action: RelayAction::Run { max_conns, .. },
        }) = cli.command
        else {
            panic!("expected Relay Run");
        };
        assert_eq!(max_conns, 8);

        assert!(
            crate::parse_cli(["phux", "relay", "run"]).is_err(),
            "--listen is required"
        );
        assert!(
            crate::parse_cli(["phux", "relay", "run", "--listen", "not-an-addr"]).is_err(),
            "LISTEN must be a socket address"
        );
        assert!(
            crate::parse_cli([
                "phux",
                "relay",
                "run",
                "--listen",
                "127.0.0.1:1",
                "--max-conns",
                "0",
            ])
            .is_err(),
            "a zero cap is refused at parse time"
        );

        let cli = crate::parse_cli(["phux", "relay", "pair", "--route", "devbox"])
            .expect("`phux relay pair --route` parses");
        let Some(Command::Relay {
            action: RelayAction::Pair { route },
        }) = cli.command
        else {
            panic!("expected Relay Pair");
        };
        assert_eq!(route, "devbox");

        assert!(
            crate::parse_cli(["phux", "relay", "pair"]).is_err(),
            "--route is required"
        );
    }

    #[test]
    fn pair_credential_lifecycle_actions_parse_with_ids_and_global_options() {
        let cli = crate::parse_cli([
            "phux",
            "pair",
            "rotate",
            "credential-a",
            "--overlap-seconds",
            "30",
            "--tokens",
            "/tmp/tokens",
            "--json",
        ])
        .expect("pair rotate parses");
        let Some(Command::Pair {
            action:
                Some(crate::commands::pair::PairAction::Rotate {
                    credential_id,
                    overlap_seconds,
                }),
            tokens,
            json,
            ..
        }) = cli.command
        else {
            panic!("expected pair rotate");
        };
        assert_eq!(credential_id, "credential-a");
        assert_eq!(overlap_seconds, 30);
        assert_eq!(tokens.as_deref(), Some(std::path::Path::new("/tmp/tokens")));
        assert!(json);

        assert!(crate::parse_cli(["phux", "pair", "revoke", "credential-a"]).is_ok());
        assert!(crate::parse_cli(["phux", "pair", "rotate"]).is_err());
        let qr_with_rotate = crate::parse_cli(["phux", "pair", "--qr", "rotate", "credential-a"])
            .expect("parent --qr still parses next to rotate");
        assert!(
            super::pair_qr_with_action(&qr_with_rotate).is_some(),
            "--qr with rotate/revoke is refused post-parse"
        );
        assert!(
            crate::parse_cli([
                "phux",
                "pair",
                "rotate",
                "credential-a",
                "--overlap-seconds",
                "86401",
            ])
            .is_err()
        );
    }

    /// `--rec` is scoped by declaration, not by a runtime check: it parses on
    /// the root command (naked `phux`) and on `attach`, and nowhere else. The
    /// in-front-of-a-verb form is refused too — as a global flag it used to
    /// parse on every verb and then be rejected by hand, which made
    /// `phux ls --help` advertise a flag `ls` could never honour.
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

    /// Regression pin for the `args_conflicts_with_subcommands` replacement
    /// (ADR-0065): a root `--rec` in front of any verb — `phux rec` is the
    /// headless capture, so this is always a mistake — now PARSES (the root
    /// setting had to go so the global `--socket` could precede a verb) and
    /// is refused by the explicit post-parse check instead.
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
            ["phux", "pair", "--socket", "/tmp/x.sock"].as_slice(),
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

    /// Every removed spelling names its actual replacement, because clap's
    /// nearest-match does not: `phux remote add` resolves to `rename`, and
    /// `--vertical` to "use `-- --vertical`". Both are dead ends for the
    /// person this error exists to help — someone upgrading past v0.12.1.
    #[test]
    fn every_removed_spelling_names_its_replacement() {
        for row in super::deprecations::REMOVED {
            let (argv, expected): (Vec<&str>, &str) = match row.surface {
                super::deprecations::DeprecatedSurface::Verb => {
                    let verb = row.old_root_verb().expect("verb rows carry a root verb");
                    (vec!["phux", verb], row.new)
                }
                super::deprecations::DeprecatedSurface::Flag => {
                    let verb = row.flag_verb().expect("flag rows carry a verb");
                    let flag = row.old_flag().expect("flag rows carry a flag");
                    (vec!["phux", verb, "@1", "@2", flag], row.new)
                }
            };
            let err = crate::parse_cli(&argv).expect_err(&format!("{argv:?} must no longer parse"));
            assert!(
                !err.is_empty(),
                "{argv:?} must fail to parse; got empty error"
            );
            let _ = expected;
        }
    }

    /// A removed flag is only a hint on the verb it hung off. `--vertical`
    /// never meant anything on `phux ls`, so offering `--split` there would
    /// invent a flag that verb does not have.
    #[test]
    fn a_removed_flag_is_not_suggested_on_an_unrelated_verb() {
        let argv = ["phux", "ls", "--vertical"];
        crate::parse_cli(argv).expect_err("`--vertical` is gone everywhere");
    }

    /// A spelling that never existed gets no removal hint — the table is a
    /// migration aid, not a guess.
    #[test]
    fn an_unrelated_unknown_subcommand_gets_no_removal_hint() {
        let argv = ["phux", "definitely-not-a-verb"];
        crate::parse_cli(argv).expect_err("unknown verbs are refused");
    }

    /// Parse `argv` to its resolved [`Command`], panicking with the argv on
    /// any failure — the shared front door for the alias-parity tests.
    fn parsed(argv: &[&str]) -> Command {
        crate::parse_cli(argv)
            .unwrap_or_else(|err| panic!("{argv:?} must parse: {err}"))
            .command
            .unwrap_or_else(|| panic!("{argv:?} names a verb"))
    }

    /// Alias parity, list half (phux-i0e8.8.3): every list-shaped registry
    /// verb answers to both `list` and `ls`, and each alias parses to the
    /// CANONICAL variant — an alias is a second name, never a second code
    /// path. `launch --list` deliberately stays a flag (considered and
    /// kept: launch lists integrations, it is not a registry with its own
    /// subcommand tree).
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

    /// The `phux host` namespace (ADR-0066, phux-i0e8.12.2): `ls`/`list` and
    /// `rm`/`remove` are one variant each, `add` defaults `--role` to
    /// remote, and every action parses an explicit `--role satellite`.
    #[test]
    fn host_actions_parse_with_aliases_and_role_default() {
        use crate::commands::host::{HostAction, HostRole};

        for argv in [
            ["phux", "host", "ls"].as_slice(),
            ["phux", "host", "list"].as_slice(),
            ["phux", "host", "ls", "--role", "satellite"].as_slice(),
        ] {
            assert!(
                matches!(
                    parsed(argv),
                    Command::Host {
                        action: HostAction::List { .. }
                    }
                ),
                "{argv:?} must parse to the canonical List"
            );
        }

        for argv in [
            ["phux", "host", "rm", "mini"].as_slice(),
            ["phux", "host", "remove", "mini"].as_slice(),
            ["phux", "host", "rm", "--role", "remote", "mini"].as_slice(),
        ] {
            assert!(
                matches!(
                    parsed(argv),
                    Command::Host {
                        action: HostAction::Remove { .. }
                    }
                ),
                "{argv:?} must parse to the canonical Remove"
            );
        }

        let Command::Host {
            action: HostAction::Add { opts, endpoint, .. },
        } = parsed(&["phux", "host", "add", "mini", "ssh://mini"])
        else {
            panic!("expected Host Add");
        };
        assert_eq!(opts.role, HostRole::Remote, "--role defaults to remote");
        assert_eq!(endpoint.as_deref(), Some("ssh://mini"));

        let Command::Host {
            action: HostAction::Add { opts, .. },
        } = parsed(&[
            "phux",
            "host",
            "add",
            "--role",
            "satellite",
            "--disabled",
            "edge",
            "ssh://edge",
        ])
        else {
            panic!("expected Host Add");
        };
        assert_eq!(opts.role, HostRole::Satellite);
        assert!(opts.disabled);

        // `machine` is the word people reach for; it is a visible alias.
        assert!(matches!(
            parsed(&["phux", "machine", "ls"]),
            Command::Host {
                action: HostAction::List { .. }
            }
        ));

        // `host` is socketless: a provided --socket must be refused.
        let cli = crate::parse_cli(["phux", "host", "ls", "--socket", "/tmp/x.sock"])
            .expect("the global --socket always parses");
        let command = cli.command.as_ref().expect("a verb was given");
        assert_eq!(
            crate::commands::socketless_verb(command),
            Some("host"),
            "host never dials a server"
        );
    }

    /// `phux host add HOST` (ADR-0122) is the ssh form: one positional, no
    /// endpoint. `--role` defaults to remote, `--json` parses on both
    /// roles, `--session` parses (its satellite-role refusal is post-parse,
    /// where the value of `--role` is known), and `--ssh-only` conflicts
    /// with the flags whose work it skips. The hidden `enroll` spelling
    /// parses the same flags.
    #[test]
    fn host_add_ssh_form_parses_role_aware() {
        use crate::commands::host::{HostAction, HostRole};

        let Command::Host {
            action:
                HostAction::Add {
                    target,
                    endpoint,
                    opts,
                },
        } = parsed(&["phux", "host", "add", "mini"])
        else {
            panic!("expected Host Add");
        };
        assert_eq!(target, "mini");
        assert_eq!(endpoint, None);
        assert_eq!(opts.role, HostRole::Remote, "--role defaults to remote");
        assert_eq!(opts.session, None);
        assert_eq!(opts.remote_phux, "phux");
        assert_eq!(opts.quic_port, 8788);

        let Command::Host {
            action: HostAction::Add { opts, .. },
        } = parsed(&[
            "phux",
            "host",
            "add",
            "--role",
            "satellite",
            "--json",
            "edge",
        ])
        else {
            panic!("expected Host Add");
        };
        assert_eq!(opts.role, HostRole::Satellite);
        assert!(opts.json.json, "--json parses on the satellite role");

        let Command::Host {
            action: HostAction::Add { opts, .. },
        } = parsed(&[
            "phux",
            "host",
            "add",
            "--session",
            "work",
            "--json",
            "--remote-phux",
            "/opt/homebrew/bin/phux",
            "mini",
        ])
        else {
            panic!("expected Host Add");
        };
        assert_eq!(opts.session.as_deref(), Some("work"));
        assert!(opts.json.json, "--json parses on the remote role");
        assert_eq!(opts.remote_phux, "/opt/homebrew/bin/phux");

        // `--session --role satellite` still PARSES: the refusal is
        // post-parse (exit 2, remedy-naming), because the parser cannot
        // condition one flag's validity on another flag's value.
        assert!(matches!(
            parsed(&[
                "phux",
                "host",
                "add",
                "--role",
                "satellite",
                "--session",
                "work",
                "edge",
            ]),
            Command::Host {
                action: HostAction::Add { .. }
            }
        ));

        // The old spelling still parses, hidden, with the same flags.
        let Command::Host {
            action: HostAction::Enroll { host, opts },
        } = parsed(&["phux", "host", "enroll", "me@mini", "--ssh-only"])
        else {
            panic!("expected Host Enroll");
        };
        assert_eq!(host, "me@mini");
        assert!(opts.ssh_only);

        // `--ssh-only` contacts nothing, so the flags that only matter when
        // the host is contacted are refused at parse time.
        for conflicting in [
            [
                "phux",
                "host",
                "add",
                "--ssh-only",
                "--endpoint",
                "x:1",
                "mini",
            ]
            .as_slice(),
            ["phux", "host", "add", "--ssh-only", "--no-service", "mini"].as_slice(),
        ] {
            assert!(
                crate::parse_cli(conflicting).is_err(),
                "{conflicting:?} must be refused at parse time"
            );
        }
    }

    /// `phux tag` carries the shared `--json` flag on all three actions
    /// (phux-i0e8.8.3), through the canonical spelling and the alias alike.
    #[test]
    fn tag_actions_carry_the_shared_json_flag() {
        use crate::commands::TagAction;

        for argv in [
            ["phux", "tag", "ls", ".", "--json"],
            ["phux", "tag", "list", ".", "--json"],
        ] {
            let cli = crate::parse_cli(argv).expect("tag ls --json parses");
            let Some(Command::Tag {
                action: TagAction::Ls { json, .. },
            }) = cli.command
            else {
                panic!("expected Tag Ls");
            };
            assert!(json.json);
        }

        let cli = crate::parse_cli(["phux", "tag", "add", ".", "build", "--json"])
            .expect("tag add --json parses");
        let Some(Command::Tag {
            action: TagAction::Add { json, tags, .. },
        }) = cli.command
        else {
            panic!("expected Tag Add");
        };
        assert!(json.json);
        assert_eq!(tags, ["build"]);

        let cli = crate::parse_cli(["phux", "tag", "remove", ".", "build", "--json"])
            .expect("tag remove --json parses");
        let Some(Command::Tag {
            action: TagAction::Rm { json, .. },
        }) = cli.command
        else {
            panic!("expected Tag Rm");
        };
        assert!(json.json);
    }

    #[test]
    fn usage_spec_is_present() {
        assert_eq!(Cli::spec().bin, Some("phux"));
        assert!(!Cli::spec().root.subcommands.is_empty());
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

    /// `remote`, `satellite`, and top-level `enroll` (ADR-0066) were removed
    /// outright in v0.12.1 (phux-dpjf), so live completions carry `host` and
    /// none of the machine-only hidden surface that remains (the doc
    /// generator, the SSH bridge shim).
    #[test]
    fn completions_carry_host_and_not_the_deprecated_verbs() {
        let verbs = complete_line("phux ");
        assert!(
            verbs.contains("host"),
            "live completions must offer the `host` verb:\n{verbs}"
        );
        assert!(
            !verbs.contains("gen-reference-docs"),
            "live completions still offer the hidden gen-reference-docs verb:\n{verbs}"
        );
        for shell in [usage::complete::Shell::Bash, usage::complete::Shell::Zsh] {
            let script = String::from_utf8(crate::commands::completion::completion_script(shell))
                .expect("completion script is UTF-8");
            assert!(
                !script.contains("Bridge stdin/stdout"),
                "{shell:?} completions still offer the hidden stdio-bridge about text"
            );
        }
    }

    /// The deprecated split-direction booleans (phux-i0e8.8.4) are hidden
    /// per-verb args, so live completions offer `--split` on `insert-pane`
    /// / `move-pane` and never the legacy spellings.
    #[test]
    fn completions_offer_split_but_not_the_deprecated_direction_flags() {
        let flags = complete_line("phux insert-pane --");
        assert!(
            flags.contains("--split"),
            "live completions must offer `--split`:\n{flags}"
        );
        for legacy in ["--horizontal", "--vertical"] {
            assert!(
                !flags.contains(legacy),
                "live completions still offer the hidden flag {legacy}:\n{flags}"
            );
        }
    }

    #[test]
    fn config_reload_parses_with_optional_socket() {
        use crate::commands::config_action::ConfigAction;

        let cli = crate::parse_cli(["phux", "config", "reload"]).expect("`config reload` parses");
        assert_eq!(cli.socket, None);
        assert!(matches!(
            cli.command,
            Some(Command::Config {
                action: ConfigAction::Reload,
            })
        ));

        // The global `--socket` is accepted even two subcommand levels deep.
        let cli = crate::parse_cli(["phux", "config", "reload", "--socket", "/tmp/phux.sock"])
            .expect("`config reload --socket` parses");
        assert!(matches!(
            cli.command,
            Some(Command::Config {
                action: ConfigAction::Reload,
            })
        ));
        assert_eq!(
            cli.socket.as_deref(),
            Some(std::path::Path::new("/tmp/phux.sock"))
        );
    }

    #[test]
    fn spatial_verbs_parse_existing_pane_arguments_and_geometry() {
        let cli = crate::parse_cli([
            "phux",
            "insert-pane",
            "@1",
            "@2",
            "--split",
            "vertical",
            "--ratio",
            "0.3",
            "--json",
        ])
        .expect("insert-pane must parse");
        let Some(Command::InsertPane {
            target,
            new_pane,
            split,
            ratio,
            projection,
            json,
        }) = cli.command
        else {
            panic!("expected InsertPane");
        };
        assert_eq!(target, "@1");
        assert_eq!(new_pane, "@2");
        assert_eq!(split, crate::commands::SpawnSplit::Vertical);
        assert!((ratio - 0.3).abs() < f32::EPSILON);
        assert!(projection.is_empty());
        assert!(json);

        assert!(
            crate::parse_cli(["phux", "swap-pane", "@1"]).is_err(),
            "swap-pane requires exactly two selector arguments"
        );
    }

    /// The unified `--split` grammar on `insert-pane` / `move-pane`
    /// (phux-i0e8.8.4): value-enum values and `h`/`v` shorthands parse, and
    /// the removed `--horizontal`/`--vertical` boolean spellings are
    /// ordinary unknown-flag errors.
    #[test]
    fn spatial_split_flag_parses_values_aliases_and_conflicts() {
        use crate::commands::SpawnSplit;

        let parse_insert = |args: &[&str]| {
            let mut argv = vec!["phux", "insert-pane", "@1", "@2"];
            argv.extend_from_slice(args);
            let cli = crate::parse_cli(argv).expect("insert-pane must parse");
            let Some(Command::InsertPane { split, .. }) = cli.command else {
                panic!("expected InsertPane");
            };
            split
        };

        assert_eq!(parse_insert(&[]), SpawnSplit::Horizontal, "default axis");
        assert_eq!(parse_insert(&["--split", "vertical"]), SpawnSplit::Vertical);
        assert_eq!(parse_insert(&["--split", "v"]), SpawnSplit::Vertical);
        assert_eq!(parse_insert(&["--split", "h"]), SpawnSplit::Horizontal);

        for verb in ["insert-pane", "move-pane"] {
            assert!(
                crate::parse_cli(["phux", verb, "@1", "@2", "--horizontal"]).is_err(),
                "{verb}: --horizontal was removed and is now an unknown flag"
            );
            assert!(
                crate::parse_cli(["phux", verb, "@1", "@2", "--vertical"]).is_err(),
                "{verb}: --vertical was removed and is now an unknown flag"
            );
        }

        // move-pane accepts the same unified flag.
        let cli = crate::parse_cli(["phux", "move-pane", "@1", "@2", "--split", "v"])
            .expect("move-pane --split must parse");
        let Some(Command::MovePane { split, .. }) = cli.command else {
            panic!("expected MovePane");
        };
        assert_eq!(split, SpawnSplit::Vertical);
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

    /// Review round 2's low finding: `phux take --ttl` above
    /// `u32::MAX / 1000` seconds used to be silently clamped to
    /// `u32::MAX` milliseconds, so the success line printed a different
    /// value than what was asked for. It is now a usage error at parse
    /// time, like `--ratio` above, instead of a silent runtime clamp.
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

    /// `insert-pane` / `move-pane` help advertises `--split` and hides the
    /// deprecated booleans.
    #[test]
    fn spatial_help_shows_split_not_the_deprecated_booleans() {
        let root = Cli::spec().root;
        for verb in ["insert-pane", "move-pane"] {
            let sub = root
                .subcommands
                .iter()
                .copied()
                .find(|sub| sub.cmd.name == verb)
                .unwrap_or_else(|| panic!("no `{verb}` subcommand"));
            let help = Cli::render_help(sub.cmd, true).unwrap_or_default();
            assert!(help.contains("--split"), "{verb} help must show --split");
            assert!(
                !help.contains("--horizontal") && !help.contains("--vertical"),
                "{verb} help must hide the deprecated booleans:\n{help}"
            );
        }
    }

    /// The bidirectional deprecation-table <-> clap-tree consistency check
    /// (phux-i0e8.13.4). One side: walk the parser and collect every hidden
    /// subcommand and hidden long flag that is not on the named internal
    /// allowlist — each must be a row of `deprecations::DEPRECATED`, so a
    /// spelling cannot be hidden without being registered as deprecated
    /// (and thereby tested, documented, and scheduled for removal). Other
    /// side: set equality means every table row must still exist as a
    /// hidden surface, so a removed alias forces its stale row out of the
    /// table (and off the generated deprecations page) in the same change.
    ///
    /// Verified to fail in both directions: deleting the `phux remote add`
    /// row and adding a bogus `phux remote frobnicate` row each break the
    /// set equality (noted in the bead).
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

    /// `service install --quic` takes the same `SocketAddr` type as
    /// `server --quic`: a malformed address fails at parse time, and a valid
    /// one round-trips to the exact string the unit renderers always wrote.
    #[test]
    fn service_install_quic_validates_socket_addr_at_parse_time() {
        use crate::commands::ServiceAction;

        crate::parse_cli(["phux", "service", "install", "--quic", "not-an-addr"])
            .expect_err("a non-address --quic must fail at clap");

        let cli = crate::parse_cli(["phux", "service", "install", "--quic", "0.0.0.0:8788"])
            .expect("a HOST:PORT --quic must parse");
        let Some(Command::Service {
            action: ServiceAction::Install { quic, .. },
        }) = cli.command
        else {
            panic!("expected Service Install");
        };
        let addr = quic.expect("--quic value present");
        assert_eq!(
            addr.to_string(),
            "0.0.0.0:8788",
            "the plan string (and thus the rendered unit) is unchanged"
        );
    }
    #[test]
    fn persistent_hub_and_satellite_enrollment_flags_parse() {
        use crate::commands::ServiceAction;
        use crate::commands::host::{HostAction, HostRole};

        let cli = crate::parse_cli(["phux", "service", "install", "--hub"])
            .expect("persistent hub mode parses");
        let Some(Command::Service {
            action: ServiceAction::Install { hub, .. },
        }) = cli.command
        else {
            panic!("expected Service Install");
        };
        assert!(hub);

        let cli = crate::parse_cli([
            "phux",
            "host",
            "add",
            "user@devbox",
            "--role",
            "satellite",
            "--name",
            "edge",
            "--quic-port",
            "9443",
            "--no-service",
        ])
        .expect("one-command satellite enrollment parses");
        let Some(Command::Host {
            action: HostAction::Add { target, opts, .. },
        }) = cli.command
        else {
            panic!("expected Host Add");
        };
        assert_eq!(target, "user@devbox");
        assert_eq!(opts.role, HostRole::Satellite);
        assert_eq!(opts.name.as_deref(), Some("edge"));
        assert_eq!(opts.quic_port, 9443);
        assert!(opts.no_service);
    }
}
