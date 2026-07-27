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
mod selector;

#[cfg(test)]
mod help_inventory;

/// phux — a libghostty-backed terminal multiplexer and control plane.
#[derive(Debug, Parser)]
#[command(
    version,
    // The root command's only args are the `--rec` pair, and they belong to
    // the naked `phux` attach alone. Letting clap reject `phux --rec X <verb>`
    // outright is what makes deleting the old hand-rolled scope check safe:
    // `phux attach --rec X` has its own copy of the flag, and everything else
    // wants `phux rec <target> -o X`, so a root `--rec` in front of a verb is
    // always a mistake and now says so instead of being silently dropped.
    args_conflicts_with_subcommands = true,
    about = "A terminal multiplexer you can drive by hand or script.",
    long_about = "phux — a libghostty-backed terminal multiplexer and control plane.\n\n\
        Run `phux` with no arguments to attach to your session (auto-starting a\n\
        server if needed). The control verbs below read and drive panes without a\n\
        TTY, and most accept `--json` for clean, scriptable output.\n\n\
        ATTACH / SERVE\n  \
          attach     Attach to a session (interactive)\n  \
          server     Run a server in the foreground\n  \
          remote     Register the servers `phux attach <name>` can reach\n  \
          service    Keep a server running across logout and reboot\n  \
          upgrade    Hot-swap the running server binary, keeping sessions alive\n\n\
        INSPECT\n  \
          ls         List sessions\n  \
          snapshot   Capture a pane's screen as JSON or a boxed view\n  \
          watch      Stream a pane's live events (bell, title, output, lifecycle)\n  \
          rec        Record a pane to an asciinema cast, a GIF, or an APNG\n  \
          agent      List, show, explain, set, or clear per-pane agent state\n\n\
        DRIVE\n  \
          new        Create a session\n  \
          kill       Kill a session, window, or pane\n  \
          detach     Detach clients from a session\n  \
          insert-pane Insert an already-created pane into a layout\n  \
          move-pane  Move an existing pane within a session layout\n  \
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
          config     Inspect config and run configured plugin actions\n  \
          plugin     Manage local plugin manifests in config\n  \
          workspace  Inspect worktrees and save/restore session archives\n  \
          worktree   Create, open, list, and remove worktree-bound sessions\n\n\
        FEDERATION\n  \
          satellite  Manage configured federation satellites\n  \
          pair       Mint a pairing token for a remote consumer\n  \
          enroll     Set up a remote server over ssh, end to end\n  \
          relay      Run a standalone relay, or enroll a route with it\n  \
          stdio-bridge  Bridge stdio to the local server socket (SSH-stdio)\n\n\
        TARGET is the selector grammar: a session name, `name:window`,\n\
        `name:window.pane`, `@id`, or `.` (focused). `=` is reserved for the attached TUI's client-local focus MRU. The same\n\
        grammar works across kill/snapshot/send-keys/run/wait/ask.",
    after_long_help = "ENVIRONMENT\n  \
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
        `phux help server` and docs/operations.md for the remote/TLS details."
)]
struct Cli {
    /// Recording options for the naked `phux` attach. `phux attach` carries
    /// its own copy; every other verb is pointed at `phux rec`.
    #[command(flatten)]
    rec: commands::RecOpts,

    /// Subcommand. Defaults to attaching to the last session if omitted.
    #[command(subcommand)]
    command: Option<Command>,
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
/// Reserved for the two long-running, human-watched entry points: a
/// foreground `phux server` and the naked-`phux` auto-spawn. It is
/// deliberately NOT printed before a one-shot control verb (`ls`,
/// `snapshot`, `send-keys`, `run`, `wait`, `new`, `kill`, `config`) so
/// those leave stderr clean for scripts and agents, and never before a
/// `--json` path. `phux --version` reports the version on stdout.
pub(crate) fn print_banner() {
    eprintln!(
        "phux {} (pre-alpha; see docs/spec/)",
        env!("CARGO_PKG_VERSION")
    );
}

/// Whether this invocation will enter the interactive TUI (raw mode +
/// alt screen) and therefore MUST keep logs off stderr.
///
/// The alt-screen-entering paths are: `phux attach`, naked `phux` (attach
/// fallback), and `phux new` *without* `--json` (which attaches after
/// creating). `phux new --json` creates without attaching, so it stays on
/// the stderr path like every other one-shot verb.
fn is_interactive_client(cli: &Cli) -> bool {
    match &cli.command {
        Some(Command::Attach { .. }) | None => true,
        Some(Command::New { json, .. }) => !json,
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

    let cli = Cli::parse();

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

    match cli.command {
        Some(Command::Attach {
            session,
            socket,
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
            socket,
            listen,
            quic,
            webtransport,
            connect,
            hub,
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
            daemonize,
            seed_command.as_deref(),
            resume,
        ),
        Some(Command::Ls { json, socket }) => commands::ls::run_ls(json, socket),
        Some(Command::New {
            name,
            session,
            cwd,
            socket,
            json,
            command,
        }) => commands::new::run_new(name, session, cwd, socket, json, command),
        Some(Command::Spawn {
            satellite,
            target,
            split,
            ratio,
            cwd,
            json,
            socket,
            command,
        }) => {
            commands::spawn::run_spawn(satellite, target, split, ratio, cwd, json, socket, command)
        }
        Some(Command::Launch {
            integration,
            list,
            print,
            json,
            target,
            split,
            ratio,
            cwd,
            socket,
            extra,
        }) => commands::launch::run_launch(
            integration,
            list,
            print,
            json,
            target,
            split,
            ratio,
            cwd,
            socket,
            &extra,
        ),
        Some(Command::Kill { target, socket }) => commands::kill::run_kill(&target, socket),
        Some(Command::Detach { session, socket }) => commands::detach::run_detach(session, socket),
        Some(Command::InsertPane {
            target,
            new_pane,
            horizontal: _,
            vertical,
            ratio,
            json,
            socket,
        }) => commands::spatial::run_insert_pane(&target, &new_pane, vertical, ratio, json, socket),
        Some(Command::MovePane {
            source,
            target,
            horizontal: _,
            vertical,
            ratio,
            json,
            socket,
        }) => commands::spatial::run_move_pane(&source, &target, vertical, ratio, json, socket),
        Some(Command::SwapPane {
            first,
            second,
            json,
            socket,
        }) => commands::spatial::run_swap_pane(&first, &second, json, socket),
        Some(Command::Resize {
            target,
            geometry,
            json,
            socket,
        }) => commands::resize::run_resize(&target, geometry, json, socket),
        Some(Command::Take { target, socket }) => commands::supervise::run_take(&target, socket),
        Some(Command::Give { target, socket }) => commands::supervise::run_give(&target, socket),
        Some(Command::Signal {
            target,
            signal,
            socket,
        }) => commands::supervise::run_signal(&target, signal, socket),
        Some(Command::Upgrade { socket }) => commands::upgrade::run_upgrade(socket),
        Some(Command::Rename {
            session,
            new_name,
            socket,
        }) => commands::rename::run_rename(&session, &new_name, socket),
        Some(Command::Snapshot {
            session,
            json,
            scrollback,
            cells,
            rendered,
            cols,
            rows,
            socket,
        }) => commands::snapshot::run_snapshot(
            session.as_deref(),
            json,
            scrollback,
            cells,
            &commands::snapshot::RenderedOpts {
                rendered,
                cols,
                rows,
            },
            socket,
        ),
        Some(Command::SendKeys {
            target,
            keys,
            socket,
        }) => commands::send_keys::run_send_keys(&target, &keys, socket),
        Some(Command::Paste {
            target,
            text,
            untrusted,
            socket,
        }) => commands::paste::run_paste(&target, text, untrusted, socket),
        Some(Command::Wait {
            session,
            until,
            idle,
            timeout,
            json,
            socket,
        }) => commands::wait::run_wait(session.as_deref(), until, idle, timeout, json, socket),
        Some(Command::Watch {
            session,
            json,
            socket,
        }) => commands::watch::run_watch(session.as_deref(), json, socket),
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
            socket,
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
            json,
            socket,
        }),
        Some(Command::Ask {
            target,
            id,
            suggestions,
            elapsed_seconds,
            json,
            question,
            socket,
        }) => commands::ask::run_ask(
            &target,
            id,
            suggestions,
            elapsed_seconds,
            json,
            question,
            socket,
        ),
        Some(Command::Agent { action }) => commands::agent::run_agent(&action),
        Some(Command::Run {
            target,
            command,
            timeout,
            json,
            socket,
        }) => commands::run::run_run(&target, &command, timeout, json, socket),
        Some(Command::Config { action }) => commands::config::run_config(&action),
        Some(Command::Plugin { action }) => commands::plugin::run_plugin(&action),
        Some(Command::Workspace { action }) => commands::workspace::run_workspace(&action),
        Some(Command::Satellite { action }) => commands::satellite::run_satellite(&action),
        Some(Command::Tag { socket, action }) => commands::tag::run_tag(&action, socket),
        Some(Command::StdioBridge { socket }) => commands::stdio_bridge::run_stdio_bridge(socket),
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
        Some(Command::Worktree(action)) => commands::worktree::run_worktree(&action),
        Some(Command::Doctor { json, socket }) => commands::doctor::run_doctor(json, socket),
        Some(Command::Enroll {
            host,
            name,
            endpoint,
            quic_port,
            no_service,
            ssh_only,
            session,
        }) => commands::enroll::run_enroll(
            &host,
            name.as_deref(),
            endpoint.as_deref(),
            quic_port,
            !no_service,
            ssh_only,
            session.as_deref(),
        ),
        Some(Command::Remote { action }) => match action {
            commands::RemoteAction::Add {
                name,
                endpoint,
                token_file,
                cert_fingerprint,
                session,
            } => commands::remote::run_add(
                &name,
                &endpoint,
                token_file.as_deref(),
                cert_fingerprint.as_deref(),
                session.as_deref(),
            ),
            commands::RemoteAction::List { json } => commands::remote::run_list(json),
            commands::RemoteAction::Remove { name } => commands::remote::run_remove(&name),
        },
        Some(Command::Service { action }) => match action {
            commands::ServiceAction::Install {
                quic,
                listen,
                restore,
                socket,
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
        None => {
            let rec_spec = match plan_rec(&cli.rec) {
                Ok(spec) => spec,
                Err(code) => return code,
            };
            commands::attach::run_naked(rec_spec.as_ref())
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
        let Some(Command::Paste {
            target,
            text,
            untrusted,
            socket,
        }) = cli.command
        else {
            panic!("expected Paste");
        };
        assert_eq!(target, "work");
        assert_eq!(text.as_deref(), Some("hello world"));
        assert!(!untrusted, "trusted is the default");
        assert_eq!(socket, None);

        // TEXT omitted ⇒ the payload comes from stdin.
        let cli = Cli::try_parse_from(["phux", "paste", "work:1.0"])
            .expect("`phux paste TARGET` (stdin form) parses");
        let Some(Command::Paste { target, text, .. }) = cli.command else {
            panic!("expected Paste");
        };
        assert_eq!(target, "work:1.0");
        assert_eq!(text, None, "omitted TEXT means stdin");

        // `--untrusted` and `--socket` parse alongside both forms.
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
        let Some(Command::Paste {
            untrusted, socket, ..
        }) = cli.command
        else {
            panic!("expected Paste");
        };
        assert!(untrusted);
        assert_eq!(
            socket.as_deref(),
            Some(std::path::Path::new("/tmp/phux.sock"))
        );

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
            // A root `--rec` in front of any verb: `phux rec` is the headless
            // capture, so this is always a mistake.
            ["phux", "--rec", "demo.gif", "ls"].as_slice(),
            ["phux", "--rec", "demo.gif", "attach", "work"].as_slice(),
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

    #[test]
    fn config_reload_parses_with_optional_socket() {
        use crate::commands::config_action::ConfigAction;

        let cli =
            Cli::try_parse_from(["phux", "config", "reload"]).expect("`config reload` parses");
        let Some(Command::Config {
            action: ConfigAction::Reload { socket },
        }) = cli.command
        else {
            panic!("expected Config Reload");
        };
        assert_eq!(socket, None);

        let cli = Cli::try_parse_from(["phux", "config", "reload", "--socket", "/tmp/phux.sock"])
            .expect("`config reload --socket` parses");
        let Some(Command::Config {
            action: ConfigAction::Reload { socket },
        }) = cli.command
        else {
            panic!("expected Config Reload");
        };
        assert_eq!(
            socket.as_deref(),
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
