//! phux binary entry point.
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
    reason = "bin-internal submodules expose items to the crate root via pub(crate); plain `pub` would trip unreachable_pub in a binary with no external API"
)]

// Opt-in dhat heap profiling. Swaps the global allocator for
// `dhat::Alloc` and the `Profiler::new_heap()` guard installed in
// `main()` writes `dhat-heap.json` to CWD on clean shutdown. View with
// https://nnethercote.github.io/dh_view/dh_view.html. The instrumented
// allocator is significantly slower than the system allocator — debug
// / profiling use only.
#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

use std::process::ExitCode;

use clap::Parser;
use commands::Command;

// Declared FIRST and with `#[macro_use]`: `macro_rules!` are visible only to
// the code that follows their definition, so `outln!` has to be in scope
// before `mod commands` — where nearly every use of it lives — is parsed.
#[macro_use]
mod output;

mod commands;
mod refdocs;
mod selector;

#[cfg(test)]
mod help_inventory;

/// phux — a libghostty-backed terminal multiplexer and control plane.
#[derive(Debug, Parser)]
#[command(
    version,
    // NOTE: the root deliberately does NOT set `args_conflicts_with_subcommands`.
    // That setting would also refuse `phux --socket X ls` — clap 4.5 rejects
    // ANY matched root arg followed by a subcommand, with no exemption for
    // `global = true` args (clap_builder parser.rs, subcommand_conflict). The
    // `--rec`-belongs-to-naked-`phux` rule that setting used to enforce is a
    // post-parse check instead: see `root_rec_before_verb` below (ADR-0065).
    about = "A terminal multiplexer you can drive by hand or script.",
    long_about = "phux — a libghostty-backed terminal multiplexer and control plane.\n\n\
        Run `phux` with no arguments to attach to your session (auto-starting a\n\
        server if needed). The control verbs below read and drive panes without a\n\
        TTY, and most accept `--json` for clean, scriptable output.\n\n\
        ATTACH / SERVE\n  \
          attach     Attach to a session (interactive)\n  \
          server     Run a server in the foreground\n  \
          host       Register the machines phux talks to: remotes and satellites\n  \
          service    Keep a server running across logout and reboot\n  \
          upgrade    Hot-swap the running server binary, keeping sessions alive\n\n\
        INSPECT\n  \
          ls         List sessions\n  \
          status     Report the running server: pid, uptime, protocol, clients, logs\n  \
          snapshot   Capture a pane's screen as JSON or a boxed view\n  \
          watch      Stream a pane's live events (bell, title, output, lifecycle)\n  \
          rec        Record a pane to an asciinema cast, a GIF, or an APNG\n  \
          play       Play a recording back as a live pane\n  \
          agent      List, show, explain, set, or clear per-pane agent state\n\n\
        DRIVE\n  \
          new        Create a session\n  \
          kill       Kill a session, window, or pane\n  \
          detach     Detach clients from a session\n  \
          insert-pane Insert an already-created pane into a layout\n  \
          move-pane  Move an existing pane beside another, across sessions too\n  \
          swap-pane  Swap two existing pane leaves\n  \
          rename     Rename a session\n  \
          resize     Set a pane's grid size, with no TTY\n  \
          send-keys  Send keys to a pane\n  \
          paste      Paste text into a pane (bracketed when the pane asks)\n  \
          run        Run a command in a pane and capture its exit code\n  \
          wait       Block until a pane meets a condition\n  \
          ask        Report an agent ask event for a pane\n\n\
        SUPERVISE\n  \
          take       Seize exclusive input authority over a pane\n  \
          give       Release the input authority taken with `take`\n  \
          signal     Send a POSIX signal to a pane's process group\n\n\
        ORGANIZE\n  \
          tag        Read and write a pane's tags (address them with #tag)\n  \
          completion Print a shell completion script for phux\n  \
          doctor     Diagnose the install: config, socket, server, plugins\n  \
          logs       Show where phux's logs live, or tail one of them\n  \
          config     Inspect config and run configured plugin actions\n  \
          plugin     Manage local plugin manifests in config\n  \
          workspace  Inspect worktrees and save/restore session archives\n  \
          worktree   Create, open, list, and remove worktree-bound sessions\n\n\
        FEDERATION\n  \
          pair       Mint a pairing token for a remote consumer\n  \
          relay      Run a standalone relay, or enroll a route with it\n  \
          stdio-bridge  Bridge stdio to the local server socket (SSH-stdio)\n\n\
        TARGET is the selector grammar: a session name, `name:window`,\n\
        `name:window.pane`, `@id`, or `.` (focused). `=` is reserved for the attached TUI's client-local focus MRU. The same\n\
        grammar works across kill/snapshot/send-keys/run/wait/ask.",
    // The EXIT STATUS semantics are the ones `commands::partial` documents:
    // 3 is distinct from 1 so a script can branch — retry is right for 3 and
    // wrong for 1. `run` mirrors the child's code, which is why its timeout
    // is 125 and not wait's 124.
    after_long_help = "EXIT STATUS\n  \
        0     Success.\n  \
        1     Failure: no server, no such target, or the verb itself failed.\n  \
        2     Usage error, or the server refused the request.\n  \
        3     Unanswerable: the selector was resolved against a partial view\n  \
        \x20       of the fleet (a federation satellite was unreachable). Retry\n  \
        \x20       once the link is back — unlike 1, the target may exist.\n  \
        124   `phux wait` gave up because `--timeout` expired.\n  \
        125   `phux run` gave up because `--timeout` expired; otherwise\n  \
        \x20       `run` mirrors the exit code of the command it ran, so\n  \
        \x20       `phux run … && next` composes like a shell.\n\n\
        ENVIRONMENT\n  \
        PHUX_SOCKET        UDS path for the CLI verbs and the server. A `--socket`\n  \
        \x20                 flag overrides it; default is\n  \
        \x20                 $XDG_RUNTIME_DIR/phux/phux.sock (or /tmp/phux-$USER/...).\n  \
        PHUX_WS_ADDR       Also accept WebSocket clients on HOST:PORT. Equivalent to\n  \
        \x20                 `phux server --listen`, which overrides it.\n  \
        PHUX_WS_SECURE     Force TLS + token auth on a loopback --listen address\n  \
        \x20                 (exercise the remote path locally).\n  \
        PHUX_WS_TLS_CERT   Operator-supplied server cert/key (PEM), instead of the\n  \
        PHUX_WS_TLS_KEY    auto-provisioned self-signed pair used off-loopback.\n  \
        PHUX_WS_TOKENS     Pairing-token store the server reads and `phux pair` writes.\n  \
        PHUX_QUIC_ADDR     Also accept QUIC clients on HOST:PORT. Equivalent to\n  \
        \x20                 `phux server --quic`, which overrides it.\n  \
        PHUX_WT_ADDR       Also accept WebTransport (HTTP/3 over QUIC) clients on\n  \
        \x20                 HOST:PORT. Equivalent to `phux server --webtransport`.\n  \
        PHUX_SSH           OpenSSH-compatible program a federation hub spawns to\n  \
        \x20                 dial ssh:// satellites (default: `ssh` on PATH).\n  \
        PHUX_TAILSCALE     Tailscale-compatible CLI `phux pair` runs to detect the\n  \
        \x20                 overlay address (default: `tailscale` on PATH).\n  \
        PHUX_LOG           Write logs to this file (server tees; client writes here).\n  \
        PHUX_LOG_FORMAT    text (default) or json — log line format.\n  \
        RUST_LOG           tracing level filter, e.g. phux=debug.\n\n\
        Run `phux server --listen 127.0.0.1:8787` to expose a port; see\n  \
        `phux help server` for the remote/TLS details."
)]
struct Cli {
    /// Recording options for the naked `phux` attach. `phux attach` carries
    /// its own copy; every other verb is pointed at `phux rec`.
    #[command(flatten)]
    rec: commands::RecOpts,

    /// Override the UDS path of the server to dial. Defaults to
    /// `$PHUX_SOCKET`, else `$XDG_RUNTIME_DIR/phux/phux.sock` (or
    /// `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set).
    // ONE declaration, `global = true`, replacing 36 hand-copied per-verb
    // fields (ADR-0065): `phux --socket X ls` and `phux ls --socket X` are
    // the same invocation. Verbs that never dial a server refuse a provided
    // `--socket` with a teaching error instead of silently ignoring it —
    // see `commands::socketless_verb`.
    #[arg(long, global = true, value_name = "PATH")]
    socket: Option<std::path::PathBuf>,

    /// Subcommand. Defaults to attaching to the last session if omitted.
    #[command(subcommand)]
    command: Option<Command>,
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

/// Whether any verb in `cmd`'s subtree declares a long flag named `long`.
fn any_verb_has_long(cmd: &clap::Command, long: &str) -> bool {
    cmd.get_subcommands().any(|sub| {
        sub.get_arguments().any(|arg| arg.get_long() == Some(long)) || any_verb_has_long(sub, long)
    })
}

/// If `err` is clap refusing an unknown root flag that actually exists on
/// one of the verbs (`phux --json ls`), name the flag so the error can teach
/// "place it after the verb" instead of leaving a dead end.
fn misplaced_scoped_flag(err: &clap::Error) -> Option<String> {
    use clap::CommandFactory;

    if err.kind() != clap::error::ErrorKind::UnknownArgument {
        return None;
    }
    let invalid = err
        .get(clap::error::ContextKind::InvalidArg)
        .map(std::string::ToString::to_string)?;
    // `--flag=value` reports the whole token; the flag alone is the id.
    let flag = invalid.split('=').next().unwrap_or(&invalid);
    let long = flag.strip_prefix("--")?;
    any_verb_has_long(&Cli::command(), long).then(|| flag.to_owned())
}

/// Print a clap parse failure, appending the scoped-flag teaching hint when
/// it applies, and map it to the exit code clap itself would use (0 for
/// `--help`/`--version`, 2 for a usage error).
fn report_parse_error(err: &clap::Error) -> ExitCode {
    if err.use_stderr() {
        // A usage error: clap writes it to stderr, which this crate leaves
        // un-settled by design (see the `output` module doc).
        let _ = err.print();
        if let Some(flag) = misplaced_scoped_flag(err) {
            eprintln!(
                "hint: `{flag}` is set per verb, not on `phux` itself; place it after the verb: `phux <verb> {flag} ...`"
            );
        }
        return ExitCode::from(2);
    }
    // `--help`/`--version`: clap writes them to stdout, so a reader that
    // hung up (`phux --help | head`) must end the process the same way as
    // every other stdout write here — `settle` exits 0 on `EPIPE`, running
    // no destructors (`Cli::parse()`'s internal `Error::exit()` behaved the
    // same way). Returning through `main` instead would run Drops that
    // write diagnostics to stderr (the `dhat-heap` profiler), breaking the
    // hang-up-in-silence contract pinned by `output_hygiene`.
    output::settle(err.print());
    ExitCode::SUCCESS
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

/// Resolve `insert-pane` / `move-pane`'s `--split` against the hidden
/// deprecated `--horizontal` / `--vertical` booleans, printing the single
/// deprecation line to stderr when a boolean spelling was used. Stderr is
/// safe in `--json` mode too: the contract only reserves stdout.
fn split_direction_warning_deprecated(
    split: commands::SpawnSplit,
    horizontal: bool,
    vertical: bool,
) -> commands::spatial::Direction {
    let (direction, deprecation) = commands::spatial::resolve_split(split, horizontal, vertical);
    if let Some(line) = deprecation {
        eprintln!("{line}");
    }
    direction
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
pub(crate) const BANNER: &str = concat!("phux ", env!("CARGO_PKG_VERSION"));

/// Whether this invocation will enter the interactive TUI (raw mode +
/// alt screen) and therefore MUST keep logs off stderr.
///
/// The alt-screen-entering paths are: `phux attach`, naked `phux` (attach
/// fallback), and `phux new` *without* `--json` (which attaches after
/// creating). `phux new --json` creates without attaching, so it stays on
/// the stderr path like every other one-shot verb.
const fn is_interactive_client(cli: &Cli) -> bool {
    match &cli.command {
        Some(Command::Attach { .. }) | None => true,
        Some(Command::New { json, .. }) => !json.json,
        _ => false,
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one match arm per CLI subcommand; the dispatch is a flat verb table, clearer whole than split."
)]
fn main() -> ExitCode {
    // Heap profiler must outlive everything else in `main` — its Drop
    // is what flushes `dhat-heap.json`. Bind to `_dhat` (NOT `_`, which
    // would drop immediately) so the guard lives until `main` returns.
    #[cfg(feature = "dhat-heap")]
    let _dhat = dhat::Profiler::new_heap();

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => return report_parse_error(&err),
    };

    // Usage errors caught after clap: the `--rec` scope rule (see
    // `root_rec_before_verb`) and a `--socket` handed to a verb that never
    // dials a server. Both are refusals with the remedy named, and both use
    // clap's usage-error exit code.
    if let Some(message) = root_rec_before_verb(&cli) {
        eprintln!("{message}");
        return ExitCode::from(2);
    }
    if cli.socket.is_some()
        && let Some(verb) = cli.command.as_ref().and_then(commands::socketless_verb)
    {
        eprintln!(
            "phux: `phux {verb}` never dials a server, so --socket has no effect here; drop it"
        );
        return ExitCode::from(2);
    }

    // Install the process-global tracing subscriber once, before any
    // runtime spins up. Without this, every `tracing::{info,debug,...}`
    // call site is a no-op.
    //
    // The choice of sink depends on whether this invocation will enter
    // the TUI (raw mode + alt screen). An interactive client owns the
    // alt screen, so it MUST log to a file only — a stray stderr line
    // corrupts the display. Every other command (foreground server,
    // one-shot control verbs, `--json` paths) keeps the historical
    // stderr layer (plus an optional `PHUX_LOG` file tee).
    //
    // The returned `WorkerGuard` (when a file sink is involved) keeps
    // the non-blocking writer's background thread alive; bind it for the
    // lifetime of `main` so logs flush on exit. An init failure is
    // non-fatal: the binary should keep working even if a future test
    // harness or library already installed its own subscriber.
    let _log_guard: Option<phux_server::telemetry::WorkerGuard> = if is_interactive_client(&cli) {
        // The client uses a synchronous file writer (no guard) so its trace
        // survives the `process::exit` detach path; see `init_client`.
        if let Err(err) = phux_server::telemetry::init_client() {
            // The client never logs to stderr, but a one-line init failure
            // on the cooked terminal (before alt screen) is acceptable and
            // beats a silent no-op subscriber.
            eprintln!("phux: client tracing init failed (continuing): {err}");
        }
        None
    } else {
        match phux_server::telemetry::init() {
            Ok(guard) => guard,
            Err(err) => {
                eprintln!("phux: tracing init failed (continuing): {err}");
                None
            }
        }
    };

    // `command` is moved into the match; `socket` is the root global every
    // arm shares (each arm consumes it at most once, and only one arm runs).
    let Cli {
        rec: root_rec,
        socket,
        command,
    } = cli;

    match command {
        Some(Command::Attach {
            session,
            quic,
            ws,
            token,
            cert_fingerprint,
            tls_server_name,
            rec,
        }) => {
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
            if socket.is_some() && (quic.is_some() || ws.is_some()) {
                eprintln!(
                    "phux: --socket dials a local UDS and cannot combine with --quic/--ws; drop one"
                );
                return ExitCode::from(2);
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
        Some(Command::Server {
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
        }) => commands::server::run_server(
            &session,
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
        Some(Command::Ls { json }) => commands::ls::run_ls(json.json, socket),
        Some(Command::Status { json }) => commands::status::run_status(json.json, socket),
        Some(Command::New {
            name,
            session,
            cwd,
            json,
            env,
            command,
        }) => commands::new::run_new(name, session, cwd, socket, json.json, command, env),
        Some(Command::Spawn {
            satellite,
            target,
            split,
            ratio,
            cwd,
            json,
            command,
        }) => commands::spawn::run_spawn(
            satellite, target, split, ratio, cwd, json.json, socket, command,
        ),
        Some(Command::Launch {
            integration,
            list,
            print,
            json,
            target,
            split,
            ratio,
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
            cwd,
            socket,
            &extra,
        ),
        Some(Command::Kill { target }) => commands::kill::run_kill(&target, socket),
        Some(Command::Detach { session }) => commands::detach::run_detach(session, socket),
        Some(Command::InsertPane {
            target,
            new_pane,
            split,
            horizontal,
            vertical,
            ratio,
            json,
        }) => commands::spatial::run_insert_pane(
            &target,
            &new_pane,
            split_direction_warning_deprecated(split, horizontal, vertical),
            ratio,
            json,
            socket,
        ),
        Some(Command::MovePane {
            source,
            target,
            split,
            horizontal,
            vertical,
            ratio,
            json,
        }) => commands::spatial::run_move_pane(
            &source,
            &target,
            split_direction_warning_deprecated(split, horizontal, vertical),
            ratio,
            json,
            socket,
        ),
        Some(Command::SwapPane {
            first,
            second,
            json,
        }) => commands::spatial::run_swap_pane(&first, &second, json, socket),
        Some(Command::Resize {
            target,
            geometry,
            json,
        }) => commands::resize::run_resize(&target, geometry, json.json, socket),
        Some(Command::Take { target }) => commands::supervise::run_take(&target, socket),
        Some(Command::Give { target }) => commands::supervise::run_give(&target, socket),
        Some(Command::Signal { target, signal }) => {
            commands::supervise::run_signal(&target, signal, socket)
        }
        Some(Command::Upgrade {}) => commands::upgrade::run_upgrade(socket),
        Some(Command::Rename { session, new_name }) => {
            commands::rename::run_rename(&session, &new_name, socket)
        }
        Some(Command::Snapshot {
            session,
            json,
            scrollback,
            cells,
            rendered,
            cols,
            rows,
        }) => commands::snapshot::run_snapshot(
            session.as_deref(),
            json.json,
            scrollback,
            cells,
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
            idle,
            timeout,
            json,
        }) => commands::wait::run_wait(session.as_deref(), until, idle, timeout, json.json, socket),
        Some(Command::Watch { session, json }) => {
            commands::watch::run_watch(session.as_deref(), json.json, socket)
        }
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
            speed,
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
            json,
        }) => commands::run::run_run(&target, &command, timeout, json.json, socket),
        Some(Command::Config { action }) => commands::config::run_config(&action, socket),
        Some(Command::Plugin { action }) => commands::plugin::run_plugin(&action),
        Some(Command::Workspace { action }) => commands::workspace::run_workspace(&action, socket),
        Some(Command::Tag { action }) => commands::tag::run_tag(&action, socket),
        Some(Command::StdioBridge {}) => commands::stdio_bridge::run_stdio_bridge(socket),
        Some(Command::Relay { action }) => commands::relay::run_relay(action),
        Some(Command::Pair {
            tokens,
            cert,
            qr,
            host,
            name,
            json,
        }) => commands::pair::run_pair(tokens, cert, qr, host, name, json),
        Some(Command::Completion { shell }) => commands::completion::run_completion(shell),
        Some(Command::Worktree(action)) => commands::worktree::run_worktree(&action, socket),
        Some(Command::Doctor { json }) => commands::doctor::run_doctor(json, socket),
        Some(Command::Logs {
            server,
            client,
            pid,
            follow,
            lines,
            json,
        }) => commands::logs::run_logs(server, client, pid, follow, lines, json),
        // The three hidden deprecation aliases (ADR-0066): each runs the
        // `host` implementation behind one stderr deprecation note.
        Some(
            command @ (Command::Enroll { .. } | Command::Remote { .. } | Command::Satellite { .. }),
        ) => commands::host::run_deprecated_alias(command),
        Some(Command::Host { action }) => commands::host::run_host(&action),
        Some(Command::Service { action }) => match action {
            commands::ServiceAction::Install {
                quic,
                listen,
                restore,
                hub,
                print,
            } => commands::service::run_install(quic, listen, restore, socket, hub, print),
            commands::ServiceAction::Uninstall => commands::service::run_uninstall(),
            commands::ServiceAction::Status => commands::service::run_status(),
            commands::ServiceAction::Logs { follow, lines } => {
                commands::service::run_logs(follow, lines)
            }
            commands::ServiceAction::PruneLogs { dry_run } => {
                commands::service::run_prune_logs(dry_run)
            }
        },
        Some(Command::GenReferenceDocs { out }) => {
            commands::gen_reference_docs::run_gen_reference_docs(out)
        }
        None => {
            let rec_spec = match plan_rec(&root_rec) {
                Ok(spec) => spec,
                Err(code) => return code,
            };
            commands::attach::run_naked(socket, rec_spec.as_ref())
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::Cli;
    use crate::commands::Command;

    /// `phux new <NAME>` must read the bare positional as the SESSION NAME,
    /// not as a command to spawn (the phux-new-foo bug: `phux new foo` tried
    /// to exec `foo` in an auto-named "0" session). The seed command is only
    /// taken after `--`.
    #[test]
    fn new_positional_is_session_name_command_requires_dash_dash() {
        let cli = Cli::try_parse_from(["phux", "new", "foo"]).expect("`phux new foo` must parse");
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
        let cli = Cli::try_parse_from(["phux", "new", "work", "--", "htop", "-d", "1"])
            .expect("`phux new work -- htop -d 1` must parse");
        let Some(Command::New { name, command, .. }) = cli.command else {
            panic!("expected New");
        };
        assert_eq!(name.as_deref(), Some("work"));
        assert_eq!(command, vec!["htop", "-d", "1"]);

        // No name, command-only via `--` ⇒ auto-named session running CMD.
        let cli =
            Cli::try_parse_from(["phux", "new", "--", "htop"]).expect("`phux new -- htop` parses");
        let Some(Command::New { name, command, .. }) = cli.command else {
            panic!("expected New");
        };
        assert_eq!(name, None, "no positional ⇒ auto-name");
        assert_eq!(command, vec!["htop"]);

        // `-s` still works and stays distinct from a positional.
        let cli = Cli::try_parse_from(["phux", "new", "-s", "flagged"])
            .expect("`phux new -s flagged` parses");
        let Some(Command::New { name, session, .. }) = cli.command else {
            panic!("expected New");
        };
        assert_eq!(name, None);
        assert_eq!(session.as_deref(), Some("flagged"));
    }

    #[test]
    fn new_json_accepts_repeatable_environment_assignments() {
        let cli = Cli::try_parse_from([
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
            env,
            vec![
                ("GC_SESSION".to_owned(), "managed".to_owned()),
                ("COMPLEX".to_owned(), "a=b".to_owned()),
            ],
        );

        assert!(
            Cli::try_parse_from(["phux", "new", "-s", "interactive", "--env", "KEY=value"])
                .is_err(),
            "--env must require headless --json until CreateIfMissing carries environment",
        );
        assert!(
            Cli::try_parse_from([
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
        let cli = Cli::try_parse_from([
            "phux", "spawn", "--target", ".", "--split", "vertical", "--ratio", "0.3",
        ])
        .expect("explicit spawn placement parses");
        let Some(Command::Spawn { target, ratio, .. }) = cli.command else {
            panic!("expected Spawn");
        };
        assert_eq!(target.as_deref(), Some("."));
        assert!((ratio - 0.3).abs() < f32::EPSILON);

        assert!(Cli::try_parse_from(["phux", "spawn", "--ratio", "0.3"]).is_err());
        assert!(Cli::try_parse_from(["phux", "spawn", "--target", ".", "--ratio", "1.0"]).is_err());
        assert!(
            Cli::try_parse_from(["phux", "spawn", "--target", ".", "--satellite", "edge"]).is_err()
        );
        assert!(
            Cli::try_parse_from([
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
        let cli = Cli::try_parse_from(["phux", "paste", "work", "hello world"])
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
        let cli = Cli::try_parse_from(["phux", "paste", "work:1.0"])
            .expect("`phux paste TARGET` (stdin form) parses");
        let Some(Command::Paste { target, text, .. }) = cli.command else {
            panic!("expected Paste");
        };
        assert_eq!(target, "work:1.0");
        assert_eq!(text, None, "omitted TEXT means stdin");

        // `--untrusted` and the global `--socket` parse alongside both forms.
        let cli = Cli::try_parse_from([
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
        assert!(Cli::try_parse_from(["phux", "paste"]).is_err());
    }

    /// `phux relay run` requires an explicit `--listen` (no default bind
    /// address) and caps connections at 64 unless `--max-conns` says
    /// otherwise; `phux relay pair` requires `--route`. A zero cap is a
    /// parse error, not a runtime surprise.
    #[test]
    fn relay_verbs_parse_and_validate_flags() {
        use crate::commands::relay::RelayAction;

        let cli = Cli::try_parse_from(["phux", "relay", "run", "--listen", "127.0.0.1:4433"])
            .expect("`phux relay run --listen` parses");
        let Some(Command::Relay {
            action: RelayAction::Run { listen, max_conns },
        }) = cli.command
        else {
            panic!("expected Relay Run");
        };
        assert_eq!(listen, "127.0.0.1:4433".parse().unwrap());
        assert_eq!(max_conns, 64, "default cap");

        let cli = Cli::try_parse_from([
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
            Cli::try_parse_from(["phux", "relay", "run"]).is_err(),
            "--listen is required"
        );
        assert!(
            Cli::try_parse_from(["phux", "relay", "run", "--listen", "not-an-addr"]).is_err(),
            "LISTEN must be a socket address"
        );
        assert!(
            Cli::try_parse_from([
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

        let cli = Cli::try_parse_from(["phux", "relay", "pair", "--route", "devbox"])
            .expect("`phux relay pair --route` parses");
        let Some(Command::Relay {
            action: RelayAction::Pair { route },
        }) = cli.command
        else {
            panic!("expected Relay Pair");
        };
        assert_eq!(route, "devbox");

        assert!(
            Cli::try_parse_from(["phux", "relay", "pair"]).is_err(),
            "--route is required"
        );
    }

    /// `--rec` is scoped by declaration, not by a runtime check: it parses on
    /// the root command (naked `phux`) and on `attach`, and nowhere else. The
    /// in-front-of-a-verb form is refused too — as a global flag it used to
    /// parse on every verb and then be rejected by hand, which made
    /// `phux ls --help` advertise a flag `ls` could never honour.
    #[test]
    fn rec_is_scoped_to_the_two_attaching_paths() {
        let cli = Cli::try_parse_from(["phux", "--rec", "demo.gif"]).expect("naked `phux --rec`");
        assert_eq!(
            cli.rec.rec.as_deref(),
            Some(std::path::Path::new("demo.gif"))
        );
        assert!(cli.command.is_none());

        let cli = Cli::try_parse_from(["phux", "attach", "work", "--rec", "demo.cast"])
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
            assert!(
                Cli::try_parse_from(argv).is_err(),
                "{argv:?} must not parse"
            );
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
            let cli = Cli::try_parse_from(argv)
                .expect("root --rec before a verb parses; the refusal is post-parse");
            let message = super::root_rec_before_verb(&cli)
                .expect("a root --rec in front of a verb must be refused");
            assert!(
                message.contains("--rec") && message.contains("phux attach --rec"),
                "the refusal must teach the two correct spellings; got {message:?}"
            );
        }

        // The two legitimate homes stay untouched by the check.
        let cli = Cli::try_parse_from(["phux", "--rec", "demo.gif"]).expect("naked form");
        assert!(super::root_rec_before_verb(&cli).is_none());
        let cli = Cli::try_parse_from(["phux", "attach", "--rec", "demo.gif"]).expect("attach");
        assert!(super::root_rec_before_verb(&cli).is_none());
    }

    /// The global `--socket` parses in both positions and lands on the same
    /// root field either way; the two spellings are one invocation.
    #[test]
    fn socket_parses_before_and_after_the_verb() {
        let before = Cli::try_parse_from(["phux", "--socket", "/tmp/x.sock", "ls"])
            .expect("`phux --socket X ls` parses");
        let after = Cli::try_parse_from(["phux", "ls", "--socket", "/tmp/x.sock"])
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
            let cli = Cli::try_parse_from(argv).expect("the global --socket always parses");
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
            let cli = Cli::try_parse_from(argv).expect("consumer verbs parse");
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
        let err = Cli::try_parse_from(["phux", "--json", "ls"])
            .expect_err("`--json` is per-verb; the root must refuse it");
        assert_eq!(
            super::misplaced_scoped_flag(&err).as_deref(),
            Some("--json"),
            "the hint must name the misplaced flag"
        );

        // A flag that exists nowhere in the tree gets no hint — the plain
        // clap error already says everything true about it.
        let err = Cli::try_parse_from(["phux", "--no-such-flag", "ls"])
            .expect_err("unknown flags are refused");
        assert_eq!(super::misplaced_scoped_flag(&err), None);
    }

    /// Parse `argv` to its resolved [`Command`], panicking with the argv on
    /// any failure — the shared front door for the alias-parity tests.
    fn parsed(argv: &[&str]) -> Command {
        Cli::try_parse_from(argv)
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
        use crate::commands::{PluginAction, RemoteAction, SatelliteAction, TagAction};

        for argv in [["phux", "remote", "list"], ["phux", "remote", "ls"]] {
            assert!(matches!(
                parsed(&argv),
                Command::Remote {
                    action: RemoteAction::List { .. }
                }
            ));
        }
        for argv in [["phux", "worktree", "list"], ["phux", "worktree", "ls"]] {
            assert!(matches!(
                parsed(&argv),
                Command::Worktree(crate::commands::WorktreeAction::List { .. })
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
        for argv in [["phux", "satellite", "list"], ["phux", "satellite", "ls"]] {
            assert!(matches!(
                parsed(&argv),
                Command::Satellite {
                    action: SatelliteAction::List { .. }
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
        use crate::commands::{PluginAction, RemoteAction, SatelliteAction, TagAction};

        for argv in [
            ["phux", "remote", "remove", "mini"],
            ["phux", "remote", "rm", "mini"],
        ] {
            assert!(matches!(
                parsed(&argv),
                Command::Remote {
                    action: RemoteAction::Remove { .. }
                }
            ));
        }
        for argv in [
            ["phux", "worktree", "remove", "feat"],
            ["phux", "worktree", "rm", "feat"],
        ] {
            assert!(matches!(
                parsed(&argv),
                Command::Worktree(crate::commands::WorktreeAction::Remove { .. })
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
        for argv in [
            ["phux", "satellite", "remove", "edge"],
            ["phux", "satellite", "rm", "edge"],
        ] {
            assert!(matches!(
                parsed(&argv),
                Command::Satellite {
                    action: SatelliteAction::Remove { .. }
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
            action: HostAction::Add { role, .. },
        } = parsed(&["phux", "host", "add", "mini", "ssh://mini"])
        else {
            panic!("expected Host Add");
        };
        assert_eq!(role, HostRole::Remote, "--role defaults to remote");

        let Command::Host {
            action: HostAction::Add { role, disabled, .. },
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
        assert_eq!(role, HostRole::Satellite);
        assert!(disabled);

        // `host` is socketless: a provided --socket must be refused.
        let cli = Cli::try_parse_from(["phux", "host", "ls", "--socket", "/tmp/x.sock"])
            .expect("the global --socket always parses");
        let command = cli.command.as_ref().expect("a verb was given");
        assert_eq!(
            crate::commands::socketless_verb(command),
            Some("host"),
            "host never dials a server"
        );
    }

    /// `phux host enroll` (ADR-0066 pt. 4, phux-i0e8.12.3): one role-aware
    /// enrollment verb. `--role` defaults to remote, `--json` parses on both
    /// roles, `--session` parses (its satellite-role refusal is post-parse,
    /// where the value of `--role` is known), and `--ssh-only` conflicts
    /// with the flags whose work it skips.
    #[test]
    fn host_enroll_parses_role_aware() {
        use crate::commands::host::{HostAction, HostRole};

        let Command::Host {
            action:
                HostAction::Enroll {
                    host,
                    role,
                    session,
                    ..
                },
        } = parsed(&["phux", "host", "enroll", "mini"])
        else {
            panic!("expected Host Enroll");
        };
        assert_eq!(host, "mini");
        assert_eq!(role, HostRole::Remote, "--role defaults to remote");
        assert_eq!(session, None);

        let Command::Host {
            action: HostAction::Enroll { role, json, .. },
        } = parsed(&[
            "phux",
            "host",
            "enroll",
            "--role",
            "satellite",
            "--json",
            "edge",
        ])
        else {
            panic!("expected Host Enroll");
        };
        assert_eq!(role, HostRole::Satellite);
        assert!(json.json, "--json parses on the satellite role");

        let Command::Host {
            action: HostAction::Enroll { session, json, .. },
        } = parsed(&[
            "phux",
            "host",
            "enroll",
            "--session",
            "work",
            "--json",
            "mini",
        ])
        else {
            panic!("expected Host Enroll");
        };
        assert_eq!(session.as_deref(), Some("work"));
        assert!(json.json, "--json parses on the remote role");

        // `--session --role satellite` still PARSES: the refusal is
        // post-parse (exit 2, remedy-naming), because clap cannot condition
        // one flag's validity on another flag's value.
        assert!(matches!(
            parsed(&[
                "phux",
                "host",
                "enroll",
                "--role",
                "satellite",
                "--session",
                "work",
                "edge",
            ]),
            Command::Host {
                action: HostAction::Enroll { .. }
            }
        ));

        // `--ssh-only` contacts nothing, so the flags that only matter when
        // the host is contacted are refused at parse time.
        for conflicting in [
            [
                "phux",
                "host",
                "enroll",
                "--ssh-only",
                "--endpoint",
                "x:1",
                "mini",
            ]
            .as_slice(),
            [
                "phux",
                "host",
                "enroll",
                "--ssh-only",
                "--no-service",
                "mini",
            ]
            .as_slice(),
        ] {
            assert!(
                Cli::try_parse_from(conflicting).is_err(),
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
            let cli = Cli::try_parse_from(argv).expect("tag ls --json parses");
            let Some(Command::Tag {
                action: TagAction::Ls { json, .. },
            }) = cli.command
            else {
                panic!("expected Tag Ls");
            };
            assert!(json.json);
        }

        let cli = Cli::try_parse_from(["phux", "tag", "add", ".", "build", "--json"])
            .expect("tag add --json parses");
        let Some(Command::Tag {
            action: TagAction::Add { json, tags, .. },
        }) = cli.command
        else {
            panic!("expected Tag Add");
        };
        assert!(json.json);
        assert_eq!(tags, ["build"]);

        let cli = Cli::try_parse_from(["phux", "tag", "remove", ".", "build", "--json"])
            .expect("tag remove --json parses");
        let Some(Command::Tag {
            action: TagAction::Rm { json, .. },
        }) = cli.command
        else {
            panic!("expected Tag Rm");
        };
        assert!(json.json);
    }

    /// The clap tree is internally consistent (conflicts, requires, groups,
    /// and the propagated global all resolve). `debug_assert` is clap's own
    /// full-tree validation pass; it must survive the root-settings rework.
    #[test]
    fn clap_tree_debug_assert_holds() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    /// The generated completions are built from the same clap tree, so the
    /// single global `--socket` declaration must still reach them.
    #[test]
    fn completions_still_carry_socket() {
        use clap::CommandFactory;
        let mut buf = Vec::new();
        clap_complete::generate(
            clap_complete::Shell::Bash,
            &mut Cli::command(),
            "phux",
            &mut buf,
        );
        let script = String::from_utf8(buf).expect("completion script is UTF-8");
        assert!(
            script.contains("--socket"),
            "bash completions lost --socket after the root-settings rework"
        );
    }

    /// The three deprecated aliases (ADR-0066) are hidden, so the generated
    /// completions — built from the visible tree only — carry `host` and
    /// none of the legacy top-level verbs.
    #[test]
    fn completions_carry_host_and_not_the_deprecated_verbs() {
        use clap::CommandFactory;
        for shell in [clap_complete::Shell::Bash, clap_complete::Shell::Zsh] {
            let mut buf = Vec::new();
            clap_complete::generate(shell, &mut Cli::command(), "phux", &mut buf);
            let script = String::from_utf8(buf).expect("completion script is UTF-8");
            assert!(
                script.contains("host"),
                "{shell} completions must offer the `host` verb"
            );
            // The per-subcommand markers clap_complete generates: bash
            // emits `phux__<verb>` state names, zsh `phux <verb>` case
            // patterns via the same joined token.
            for legacy in ["phux__remote", "phux__satellite", "phux__enroll"] {
                assert!(
                    !script.contains(legacy),
                    "{shell} completions still offer the hidden alias {legacy}"
                );
            }
        }
    }

    /// Each hidden alias still parses — full arg surface — and maps onto
    /// the `host` action named by the ADR-0066 alias table, with a
    /// deprecation line naming that exact replacement.
    #[test]
    fn deprecated_aliases_map_to_host_actions_per_the_alias_table() {
        use crate::commands::host::{HostAction, HostRole, deprecated_alias};

        let mapped = |argv: &[&str]| {
            deprecated_alias(parsed(argv))
                .unwrap_or_else(|| panic!("{argv:?} must map to a host action"))
        };

        // phux remote add NAME ENDPOINT -> phux host add NAME ENDPOINT
        let (action, note) = mapped(&[
            "phux", "remote", "add", "mini", "ssh://mini", "--session", "work",
        ]);
        let HostAction::Add {
            name,
            endpoint,
            role,
            session,
            disabled,
            json,
            ..
        } = action
        else {
            panic!("expected Add, got {action:?}");
        };
        assert_eq!((name.as_str(), endpoint.as_str()), ("mini", "ssh://mini"));
        assert_eq!(role, HostRole::Remote);
        assert_eq!(session.as_deref(), Some("work"));
        assert!(!disabled && !json.json);
        assert!(note.contains("`phux remote add`") && note.contains("`phux host add`"));

        // phux remote list -> phux host ls (role-filtered)
        let (action, note) = mapped(&["phux", "remote", "list", "--json"]);
        assert!(
            matches!(
                action,
                HostAction::List {
                    role: Some(HostRole::Remote),
                    json: crate::commands::JsonOpt { json: true },
                }
            ),
            "got {action:?}"
        );
        assert!(note.contains("`phux host ls`"));

        // phux remote remove NAME -> phux host rm NAME (role-filtered)
        let (action, note) = mapped(&["phux", "remote", "rm", "mini"]);
        assert!(
            matches!(
                &action,
                HostAction::Remove {
                    name,
                    role: Some(HostRole::Remote),
                    ..
                } if name == "mini"
            ),
            "got {action:?}"
        );
        assert!(note.contains("`phux host rm`"));

        // phux satellite add NAME ENDPOINT -> phux host add --role satellite
        let (action, note) = mapped(&[
            "phux",
            "satellite",
            "add",
            "edge",
            "ssh://edge",
            "--disabled",
            "--json",
        ]);
        let HostAction::Add {
            role,
            disabled,
            session,
            json,
            ..
        } = action
        else {
            panic!("expected Add, got {action:?}");
        };
        assert_eq!(role, HostRole::Satellite);
        assert!(disabled && json.json);
        assert_eq!(session, None, "a satellite entry has no session");
        assert!(note.contains("`phux host add --role satellite`"));

        // phux satellite list -> phux host ls --role satellite
        let (action, note) = mapped(&["phux", "satellite", "ls"]);
        assert!(matches!(
            action,
            HostAction::List {
                role: Some(HostRole::Satellite),
                ..
            }
        ));
        assert!(note.contains("`phux host ls --role satellite`"));

        // phux satellite remove NAME -> phux host rm --role satellite NAME
        let (action, note) = mapped(&["phux", "satellite", "remove", "edge", "--json"]);
        assert!(
            matches!(
                &action,
                HostAction::Remove {
                    name,
                    role: Some(HostRole::Satellite),
                    json: crate::commands::JsonOpt { json: true },
                } if name == "edge"
            ),
            "got {action:?}"
        );
        assert!(note.contains("`phux host rm --role satellite`"));

        // phux satellite enroll HOST -> phux host enroll --role satellite
        let (action, note) = mapped(&[
            "phux",
            "satellite",
            "enroll",
            "edge",
            "--quic-port",
            "9000",
            "--ssh-only",
        ]);
        let HostAction::Enroll {
            host,
            role,
            quic_port,
            ssh_only,
            session,
            ..
        } = action
        else {
            panic!("expected Enroll, got {action:?}");
        };
        assert_eq!(host, "edge");
        assert_eq!(role, HostRole::Satellite);
        assert_eq!(quic_port, 9000);
        assert!(ssh_only);
        assert_eq!(session, None);
        assert!(note.contains("`phux host enroll --role satellite`"));

        // phux enroll HOST -> phux host enroll HOST
        let (action, note) = mapped(&["phux", "enroll", "me@mini", "--session", "work"]);
        let HostAction::Enroll {
            host,
            role,
            session,
            json,
            ..
        } = action
        else {
            panic!("expected Enroll, got {action:?}");
        };
        assert_eq!(host, "me@mini");
        assert_eq!(role, HostRole::Remote);
        assert_eq!(session.as_deref(), Some("work"));
        assert!(!json.json, "the legacy enroll has no --json flag");
        assert!(note.contains("`phux enroll`") && note.contains("`phux host enroll`"));

        // A visible verb is not an alias.
        assert!(
            deprecated_alias(parsed(&["phux", "host", "ls"])).is_none(),
            "`host` itself must not map as a deprecated alias"
        );
    }

    #[test]
    fn config_reload_parses_with_optional_socket() {
        use crate::commands::config_action::ConfigAction;

        let cli =
            Cli::try_parse_from(["phux", "config", "reload"]).expect("`config reload` parses");
        assert_eq!(cli.socket, None);
        assert!(matches!(
            cli.command,
            Some(Command::Config {
                action: ConfigAction::Reload,
            })
        ));

        // The global `--socket` is accepted even two subcommand levels deep.
        let cli = Cli::try_parse_from(["phux", "config", "reload", "--socket", "/tmp/phux.sock"])
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
        let cli = Cli::try_parse_from([
            "phux",
            "insert-pane",
            "@1",
            "@2",
            "--vertical",
            "--ratio",
            "0.3",
            "--json",
        ])
        .expect("insert-pane must parse");
        let Some(Command::InsertPane {
            target,
            new_pane,
            vertical,
            ratio,
            json,
            ..
        }) = cli.command
        else {
            panic!("expected InsertPane");
        };
        assert_eq!(target, "@1");
        assert_eq!(new_pane, "@2");
        assert!(vertical);
        assert!((ratio - 0.3).abs() < f32::EPSILON);
        assert!(json);

        assert!(
            Cli::try_parse_from([
                "phux",
                "move-pane",
                "@1",
                "@2",
                "--horizontal",
                "--vertical",
            ])
            .is_err(),
            "directions are mutually exclusive"
        );
        assert!(
            Cli::try_parse_from(["phux", "swap-pane", "@1"]).is_err(),
            "swap-pane requires exactly two selector arguments"
        );
    }

    /// The unified `--split` grammar on `insert-pane` / `move-pane`
    /// (phux-i0e8.8.4): value-enum values and `h`/`v` shorthands parse, the
    /// hidden deprecated booleans still parse, and every conflicting pair is
    /// refused at the clap level.
    #[test]
    fn spatial_split_flag_parses_values_aliases_and_conflicts() {
        use crate::commands::SpawnSplit;

        let parse_insert = |args: &[&str]| {
            let mut argv = vec!["phux", "insert-pane", "@1", "@2"];
            argv.extend_from_slice(args);
            let cli = Cli::try_parse_from(argv).expect("insert-pane must parse");
            let Some(Command::InsertPane {
                split,
                horizontal,
                vertical,
                ..
            }) = cli.command
            else {
                panic!("expected InsertPane");
            };
            (split, horizontal, vertical)
        };

        assert_eq!(parse_insert(&[]).0, SpawnSplit::Horizontal, "default axis");
        assert_eq!(
            parse_insert(&["--split", "vertical"]).0,
            SpawnSplit::Vertical
        );
        assert_eq!(parse_insert(&["--split", "v"]).0, SpawnSplit::Vertical);
        assert_eq!(parse_insert(&["--split", "h"]).0, SpawnSplit::Horizontal);
        assert_eq!(
            parse_insert(&["--vertical"]),
            (SpawnSplit::Horizontal, false, true),
            "deprecated boolean still parses, leaving --split at its default"
        );
        assert_eq!(
            parse_insert(&["--horizontal"]),
            (SpawnSplit::Horizontal, true, false)
        );

        for verb in ["insert-pane", "move-pane"] {
            assert!(
                Cli::try_parse_from(["phux", verb, "@1", "@2", "--horizontal", "--vertical"])
                    .is_err(),
                "{verb}: booleans are mutually exclusive"
            );
            assert!(
                Cli::try_parse_from(["phux", verb, "@1", "@2", "--split", "v", "--horizontal"])
                    .is_err(),
                "{verb}: an explicit --split refuses --horizontal"
            );
            assert!(
                Cli::try_parse_from(["phux", verb, "@1", "@2", "--split", "h", "--vertical"])
                    .is_err(),
                "{verb}: an explicit --split refuses --vertical"
            );
        }

        // move-pane accepts the same unified flag.
        let cli = Cli::try_parse_from(["phux", "move-pane", "@1", "@2", "--split", "v"])
            .expect("move-pane --split must parse");
        let Some(Command::MovePane { split, .. }) = cli.command else {
            panic!("expected MovePane");
        };
        assert_eq!(split, SpawnSplit::Vertical);
    }

    /// End-to-end direction resolution pins the old parsed-then-discarded
    /// `--horizontal` bug dead: every spelling reaches `run_insert_pane` /
    /// `run_move_pane` as the direction the user asked for, and a deprecated
    /// boolean produces exactly one warning line.
    #[test]
    fn spatial_split_resolves_to_the_requested_direction() {
        use crate::commands::spatial::{Direction, resolve_split};

        let resolve = |args: &[&str]| {
            let mut argv = vec!["phux", "insert-pane", "@1", "@2"];
            argv.extend_from_slice(args);
            let cli = Cli::try_parse_from(argv).expect("insert-pane must parse");
            let Some(Command::InsertPane {
                split,
                horizontal,
                vertical,
                ..
            }) = cli.command
            else {
                panic!("expected InsertPane");
            };
            resolve_split(split, horizontal, vertical)
        };

        assert_eq!(resolve(&[]), (Direction::Horizontal, None));
        assert_eq!(
            resolve(&["--split", "vertical"]),
            (Direction::Vertical, None)
        );
        assert_eq!(resolve(&["--split", "v"]), (Direction::Vertical, None));
        assert_eq!(
            resolve(&["--split", "horizontal"]),
            (Direction::Horizontal, None)
        );

        let (direction, deprecation) = resolve(&["--vertical"]);
        assert_eq!(direction, Direction::Vertical);
        let line = deprecation.expect("--vertical must warn");
        assert!(
            !line.contains('\n'),
            "exactly one deprecation line: {line:?}"
        );
        assert!(
            line.contains("--split vertical"),
            "warning teaches the new spelling"
        );

        let (direction, deprecation) = resolve(&["--horizontal"]);
        assert_eq!(
            direction,
            Direction::Horizontal,
            "--horizontal must be honored, not discarded"
        );
        let line = deprecation.expect("--horizontal must warn");
        assert!(
            !line.contains('\n'),
            "exactly one deprecation line: {line:?}"
        );
        assert!(line.contains("--split horizontal"));
    }

    /// `--ratio` on the spatial verbs now validates at parse time
    /// (phux-i0e8.8.4): out-of-range or non-numeric ratios are clap usage
    /// errors, not runtime failures.
    #[test]
    fn spatial_ratio_validates_at_parse_time() {
        for verb in ["insert-pane", "move-pane"] {
            for bad in ["1.5", "0", "1", "-0.2", "NaN", "bogus"] {
                assert!(
                    Cli::try_parse_from(["phux", verb, "@1", "@2", "--ratio", bad]).is_err(),
                    "{verb} --ratio {bad} must fail at clap"
                );
            }
            assert!(
                Cli::try_parse_from(["phux", verb, "@1", "@2", "--ratio", "0.25"]).is_ok(),
                "{verb} --ratio 0.25 must parse"
            );
        }
    }

    /// `insert-pane` / `move-pane` help advertises `--split` and hides the
    /// deprecated booleans.
    #[test]
    fn spatial_help_shows_split_not_the_deprecated_booleans() {
        use clap::CommandFactory;

        let root = Cli::command();
        for verb in ["insert-pane", "move-pane"] {
            let sub = root
                .get_subcommands()
                .find(|sub| sub.get_name() == verb)
                .unwrap_or_else(|| panic!("no `{verb}` subcommand"));
            let help = sub.clone().render_long_help().to_string();
            assert!(help.contains("--split"), "{verb} help must show --split");
            assert!(
                !help.contains("--horizontal") && !help.contains("--vertical"),
                "{verb} help must hide the deprecated booleans:\n{help}"
            );
        }
    }

    /// `new --json` without `-s NAME` is refused by clap itself
    /// (`requires = session` via the verb's arg group), replacing the old
    /// runtime gate.
    #[test]
    fn new_json_requires_session_at_the_clap_level() {
        let err = Cli::try_parse_from(["phux", "new", "--json"])
            .expect_err("`new --json` without -s must be a usage error");
        assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
        assert_eq!(err.exit_code(), 2, "usage errors exit 2");

        // A positional NAME does not satisfy the rule: `--json` documents an
        // explicit `-s`.
        assert!(
            Cli::try_parse_from(["phux", "new", "work", "--json"]).is_err(),
            "positional NAME must not satisfy --json's -s requirement"
        );

        assert!(
            Cli::try_parse_from(["phux", "new", "--json", "-s", "work"]).is_ok(),
            "`new --json -s NAME` must parse"
        );
        assert!(
            Cli::try_parse_from(["phux", "new", "-s", "work"]).is_ok(),
            "-s without --json stays valid"
        );
    }

    /// `service install --quic` takes the same `SocketAddr` type as
    /// `server --quic`: a malformed address fails at parse time, and a valid
    /// one round-trips to the exact string the unit renderers always wrote.
    #[test]
    fn service_install_quic_validates_socket_addr_at_parse_time() {
        use crate::commands::ServiceAction;

        let err = Cli::try_parse_from(["phux", "service", "install", "--quic", "not-an-addr"])
            .expect_err("a non-address --quic must fail at clap");
        assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation);

        let cli = Cli::try_parse_from(["phux", "service", "install", "--quic", "0.0.0.0:8788"])
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
        use crate::commands::{SatelliteAction, ServiceAction};

        let cli = Cli::try_parse_from(["phux", "service", "install", "--hub"])
            .expect("persistent hub mode parses");
        let Some(Command::Service {
            action: ServiceAction::Install { hub, .. },
        }) = cli.command
        else {
            panic!("expected Service Install");
        };
        assert!(hub);

        let cli = Cli::try_parse_from([
            "phux",
            "satellite",
            "enroll",
            "user@devbox",
            "--name",
            "edge",
            "--quic-port",
            "9443",
            "--no-service",
        ])
        .expect("one-command satellite enrollment parses");
        let Some(Command::Satellite {
            action:
                SatelliteAction::Enroll {
                    host,
                    name,
                    quic_port,
                    no_service,
                    ..
                },
        }) = cli.command
        else {
            panic!("expected Satellite Enroll");
        };
        assert_eq!(host, "user@devbox");
        assert_eq!(name.as_deref(), Some("edge"));
        assert_eq!(quic_port, 9443);
        assert!(no_service);
    }
}
