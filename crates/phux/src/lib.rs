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

// usage 7's derive output names the package, even when Cargo calls it `usage`.
extern crate usage as usage_rs;

use std::ffi::OsStr;
use std::process::ExitCode;

use commands::Command;
use dispatch::dispatch;
#[cfg(test)]
pub(crate) use help::help_topic;
pub(crate) use help::render_help_page;
use help::{SHORT_HELP, help_request, rewrite_help_verb, short_help_requested};
use parse_error::{report_parse_error, workload_argv_carries_key_material};
use refusals::{emit_usage_error, usage_refusal};

// First, so `outln!` is in scope for every module below.
#[macro_use]
mod output;

mod capabilities;
mod commands;
mod companion;
mod deprecations;
mod dispatch;
mod environment;
mod exit_codes;
mod help;
mod parse_error;
mod refdocs;
mod refusals;
mod skill;

#[cfg(test)]
mod help_inventory;

/// The environment variable an auto-spawned daemon reads its idle backstop
/// from, so the integration harness can arm it by name rather than by a
/// literal of its own (see `AutoSpawnedServer::IDLE_BACKSTOP`).
pub use commands::server::AUTO_SPAWN_IDLE_ENV;
pub use commands::server::ENSURE_TIMEOUT_ENV;

/// Record this build's source checkout `distros/` directory.
///
/// It is searched last for a bundled `--distro` name. The `phux` binary
/// passes it from `CARGO_MANIFEST_DIR`; it lives there, not in this library, because a
/// compile-time checkout path keys a crate, and everything that links it,
/// to one checkout in a content-addressed build cache.
pub fn set_checkout_distros(dir: &'static str) {
    let _ = commands::config::CHECKOUT_DISTROS.set(std::path::Path::new(dir));
}

/// phux — a libghostty-backed terminal multiplexer and control plane.
#[derive(Debug, usage::Cli)]
#[usage(
    bin = "phux",
    version = env!("PHUX_VERSION_LABEL"),
    unknown_flags = "error",
    completion,
    // `--rec` and `--remote` belong to the naked attach; their scope is a
    // post-parse check because globals parse on either side of a verb.
    about = "A terminal multiplexer whose server you can drive from code.",
    long_about = "A terminal multiplexer whose server you can drive from code.\n\n\
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

/// Print the one-line build banner to stderr. Only the human-watched
/// foreground entry points (`phux server`, `phux relay run`) print it:
/// attach paths would wipe it with the alt screen, and one-shot verbs keep
/// stderr clean for scripts.
pub(crate) fn print_banner() {
    // Stable foreground build banner, not a diagnostic.
    eprintln!("{BANNER}");
}

/// The banner line: `phux <version>`, nothing else.
pub(crate) const BANNER: &str = concat!("phux ", env!("PHUX_VERSION_LABEL"));

/// Whether this invocation enters the TUI (raw mode + alt screen) and must
/// keep logs off stderr: `attach`, `host attach`, naked `phux`, `new` without
/// `--json`, `worktree new|open --attach`, and `project open` without `--json`.
const fn is_interactive_client(cli: &Cli) -> bool {
    match &cli.command {
        Some(
            Command::Attach(..)
            | Command::Host(commands::HostArgs {
                action: commands::host::HostAction::Attach { .. },
            })
            | Command::Worktree(commands::WorktreeArgs {
                action:
                    commands::WorktreeAction::New { attach: true, .. }
                    | commands::WorktreeAction::Open { attach: true, .. },
            }),
        )
        | None => true,
        Some(
            Command::New(commands::NewArgs { json, .. })
            | Command::Project(commands::ProjectArgs {
                action: commands::ProjectAction::Open { json, .. },
            }),
        ) => !*json,
        _ => false,
    }
}

fn json_output_requested() -> bool {
    // usage-rs treats attached values as the same presence flag, even false
    // or an empty value. Match that grammar before parser/preflight errors.
    std::env::args_os().any(|arg| {
        let bytes = arg.as_encoded_bytes();
        bytes == b"--json" || bytes.starts_with(b"--json=")
    })
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
            return Some(emit_usage_error("--capabilities --json is a standalone endpoint and cannot be combined with other arguments".to_owned()));
        }
        return Some(capabilities::run());
    }

    if short_help_requested() {
        output::bytes(SHORT_HELP.as_bytes());
        return Some(ExitCode::SUCCESS);
    }

    help_request(args)
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
            warn!("client tracing init failed (continuing): {err}");
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
                warn!("tracing init failed (continuing): {err}");
            }
            None
        }
    }
}

#[must_use]
pub fn run() -> ExitCode {
    // A `switch-host` successor starts on the alt screen its predecessor
    // left up; until the attach adopts it, every way out (a diagnostic, a
    // signal, a panic, a plain return) hands the shell its screen back.
    phux_tui::attach::adopt_alt_screen_handoff();
    let code = run_cli();
    phux_tui::attach::leave_inherited_alt_screen();
    code
}

fn run_cli() -> ExitCode {
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
        return emit_usage_error(refusal);
    }
    let raw: Vec<std::ffi::OsString> = std::env::args_os().collect();
    // Key material never belongs on a `phux workload` command line, and argv is
    // echoed in too many places to scrub, so that verb is refused before parsing
    // (`docs/remote.md#who-can-connect`).
    if workload_argv_carries_key_material(&raw[1..]) {
        return commands::json_err::emit(
            json_output_requested(),
            &commands::json_err::CliError::new(
                commands::json_err::codes::WORKLOAD,
                "the command line was refused and is not echoed, because it appears to contain key material",
                "pass certificates and CSRs on stdin or with --file, and keep private keys out of arguments entirely",
            ),
            2,
        );
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
            return emit_usage_error(
                "--skill is a standalone endpoint and cannot be combined with a command".to_owned(),
            );
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
    if let Some(Command::Mcp(commands::McpArgs { args })) = &cli.command {
        return commands::mcp::run(args);
    }

    // Runtime discovery must not open logs, load config, or contact a server.
    // In particular a caller's PHUX_LOG may be a blocking FIFO.
    if let Some(Command::RuntimeInfo(commands::RuntimeInfoArgs { json })) = &cli.command {
        return commands::runtime_info::run(json.json);
    }

    // The one-shot watchdog must precede any potentially blocking log open
    // (PHUX_LOG may name a FIFO). Ensure initializes tracing on its bounded
    // worker; its watchdog reports failures directly on stderr.
    if let Some(Command::Server(commands::ServerArgs {
        ensure: true, json, ..
    })) = &cli.command
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
mod tests;
