use std::path::Path;
use std::process::ExitCode;

use pair::PairAction;
use phux_client::attach::AttachError;
use phux_client::attach::connection::Connection;
use phux_protocol::wire::frame::{Command as WireCommand, CommandResult, TerminalSignal};
use report::ReportAction;
use usage::{Args, Subcommands, ValueEnum};

/// CLI signal names for `phux signal TARGET SIGNAL` (ADR-0033), mapped to the
/// wire [`TerminalSignal`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum SignalArg {
    /// SIGINT — the Ctrl-C equivalent.
    Interrupt,
    /// SIGSTOP — pause the process group (reversible via `resume`).
    Freeze,
    /// SIGCONT — resume a frozen process group.
    Resume,
    /// SIGTERM — request graceful termination.
    Terminate,
    /// SIGKILL — force termination.
    Kill,
}

/// Split axis for explicit pane placement — the one `--split` vocabulary
/// shared by `spawn`, `launch`, `play`, `insert-pane`, and `move-pane`
/// (ADR-0065 §6). `h` / `v` are accepted shorthands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum SpawnSplit {
    #[usage(visible_alias = "h")]
    Horizontal,
    #[usage(visible_alias = "v")]
    Vertical,
}

/// Output format for a recording, shared by `--rec-format` on the attach
/// path and `--format` on the `rec` verb.
///
/// `Apng` covers both the `.png` and `.apng` extensions: a recording is an
/// animation, and this surface never produces a still frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum CompletionShell {
    Bash,
    Elvish,
    Zsh,
    Fish,
    Nu,
    PowerShell,
}

impl From<CompletionShell> for usage::complete::Shell {
    fn from(shell: CompletionShell) -> Self {
        match shell {
            CompletionShell::Bash => Self::Bash,
            CompletionShell::Elvish => Self::Elvish,
            CompletionShell::Zsh => Self::Zsh,
            CompletionShell::Fish => Self::Fish,
            CompletionShell::Nu => Self::Nu,
            CompletionShell::PowerShell => Self::PowerShell,
        }
    }
}

/// One `KEY=VALUE` assignment for `phux new --env`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnvAssignment {
    pub key: String,
    pub value: String,
}

impl std::str::FromStr for EnvAssignment {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        parse_env_assignment(value).map(|(key, value)| Self { key, value })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum RecFormat {
    /// asciinema cast — the archival, re-renderable artifact.
    Cast,
    /// Animated GIF — shareable and embeddable anywhere.
    Gif,
    /// Animated PNG — truecolor, no quantization, larger files.
    Apng,
}

/// The `--rec` / `--rec-format` pair, declared on exactly the two paths that
/// raise a TUI: the root command (naked `phux`) and `phux attach`.
///
/// Shared through `#[usage(flatten)]` rather than a `global` arg on
/// the root: a global would parse — and advertise itself in `--help` — on
/// every verb, including the headless ones that can never tee a composited
/// frame. Scoping it here makes `phux ls --help` honest by construction
/// instead of by a runtime rejection. Headless capture is `phux rec`.
#[derive(Debug, Args)]
pub(crate) struct RecOpts {
    /// Record this session to PATH (.cast, .gif, or .apng)
    // Headless capture of one pane is `phux rec`, which is where the
    // format prose lives; the naked attach and `phux attach` keep one line.
    #[usage(long = "rec", value_name = "PATH")]
    pub(crate) rec: Option<std::path::PathBuf>,

    /// Recording format, overriding PATH's extension
    #[usage(long = "rec-format", value_enum, value_name = "FMT", requires("--rec"))]
    pub(crate) rec_format: Option<RecFormat>,
}

/// The shared `--json` declaration for the core server-talking verbs
/// (ADR-0065 §3, phux-i0e8.8.2).
///
/// `--json` stays verb-scoped rather than global so help stays honest by
/// construction (a global would advertise itself on verbs with no JSON
/// projection); this one flattened struct is the single declaration those
/// verbs share, so the doc string and the error contract cannot drift
/// per-verb. Deliberately **no `-j` short flag**: considered and rejected in
/// ADR-0065 §7 (`--json` is typed by scripts and agents, where explicitness
/// beats two saved characters), cross-referenced in
/// `docs/consumers/agents.md` §5.3.
#[derive(Debug, Args)]
pub(crate) struct JsonOpt {
    /// Emit stable, versioned JSON on stdout instead of the human view.
    /// On failure, stdout stays empty and stderr carries one JSON error
    /// object.
    #[usage(long)]
    pub(crate) json: bool,
}

/// The `--remote` target the headless session verbs share (phux-c2td.2),
/// flattened like [`JsonOpt`] so the help text and the resolution rule
/// cannot drift per verb. Resolution lives in [`server_target`].
#[derive(Debug, Args)]
pub(crate) struct RemoteOpt {
    /// Run against the phux server on another machine instead of the local
    /// socket, ssh-style: `--remote me@mini`. Same target and resolution as
    /// `phux attach --remote`: a registered host is dialed directly over
    /// QUIC or WSS, and an unregistered one is paired over your ssh trust
    /// first and remembered (with `--json` it is refused instead, naming the
    /// remedies). PORT defaults to 8788. Cannot combine with `--socket`.
    #[usage(long, value_name = "[USER@]HOST[:PORT]")]
    pub(crate) remote: Option<String>,
}

impl RemoteOpt {
    /// Pair this verb's `--remote` with the global `--socket`.
    pub(crate) fn with_socket(
        self,
        socket: Option<std::path::PathBuf>,
    ) -> server_target::ServerSpec {
        server_target::ServerSpec {
            socket,
            remote: self.remote,
        }
    }
}

/// The verb-scoped `--remote` this command carries, if any.
///
/// One accessor for every verb that takes the flag, so the post-parse checks
/// (a malformed target, the `--socket` collision) cover each of them without
/// a per-verb list of their own.
pub(crate) fn verb_remote(command: &Command) -> Option<&str> {
    match command {
        Command::Attach { remote, .. } => remote.as_deref(),
        Command::Ls { remote, .. }
        | Command::Whoami { remote, .. }
        | Command::New { remote, .. }
        | Command::Kill { remote, .. }
        | Command::Rename { remote, .. }
        | Command::Detach { remote, .. } => remote.remote.as_deref(),
        _ => None,
    }
}

/// Parse one environment assignment without imposing shell-variable naming
/// rules: `execve` only forbids an empty key, `=`, and NUL.
fn parse_env_assignment(value: &str) -> Result<(String, String), String> {
    let (key, value) = value
        .split_once('=')
        .ok_or_else(|| "environment assignment must be KEY=VALUE".to_owned())?;
    if key.is_empty() {
        return Err("environment key must not be empty".to_owned());
    }
    if key.contains('\0') || value.contains('\0') {
        return Err("environment assignment must not contain NUL".to_owned());
    }
    Ok((key.to_owned(), value.to_owned()))
}

impl From<SignalArg> for TerminalSignal {
    fn from(arg: SignalArg) -> Self {
        match arg {
            SignalArg::Interrupt => Self::Interrupt,
            SignalArg::Freeze => Self::Freeze,
            SignalArg::Resume => Self::Resume,
            SignalArg::Terminate => Self::Terminate,
            SignalArg::Kill => Self::Kill,
        }
    }
}

pub(crate) mod agent;
pub(crate) mod ask;
pub(crate) mod attach;
pub(crate) mod bootstrap;
pub(crate) mod channel;
pub(crate) mod cockpit;
pub(crate) mod completion;
pub(crate) mod config;
pub(crate) mod config_action;
pub(crate) mod detach;
pub(crate) mod doctor;
pub(crate) mod enroll;
pub(crate) mod gen_reference_docs;
pub(crate) mod host;
pub(crate) mod json_err;
pub(crate) mod kill;
pub(crate) mod launch;
pub(crate) mod logs;
pub(crate) mod ls;
pub(crate) mod mcp;
pub(crate) mod new;
pub(crate) mod pair;
pub(crate) mod partial;
pub(crate) mod paste;
pub(crate) mod perf;
pub(crate) mod play;
pub(crate) mod plugin;
pub(crate) mod rec;
pub(crate) mod relay;
pub(crate) mod remote;
pub(crate) mod remote_target;
pub(crate) mod rename;
pub(crate) mod report;
pub(crate) mod resize;
pub(crate) mod run;
pub(crate) mod runtime_info;
pub(crate) mod satellite;
pub(crate) mod send_keys;
pub(crate) mod server;
pub(crate) mod server_target;
pub(crate) mod service;
pub(crate) mod snapshot;
pub(crate) mod spatial;
pub(crate) mod spawn;
pub(crate) mod ssh_bootstrap;
pub(crate) mod status;
pub(crate) mod stdio_bridge;
pub(crate) mod supervise;
pub(crate) mod tag;
pub(crate) mod toml_registry;
pub(crate) mod update;
pub(crate) mod upgrade;
pub(crate) mod wait;
// Stalled peers for the run/wait deadline tests (phux-69pq.10).
#[cfg(test)]
mod stall_peer;
pub(crate) mod watch;
pub(crate) mod whoami;
pub(crate) mod workload;
pub(crate) mod workspace;
pub(crate) mod worktree;

/// Default name the `phux server` subcommand pre-seeds, and the name
/// the `phux attach` auto-spawn path requests when the user doesn't
/// provide one. Keeping both halves on a single constant means
/// "`phux` with no arguments after a fresh boot" Just Works.
pub(crate) const DEFAULT_SESSION_NAME: &str = "default";

/// The verb's display name when the resolved (sub)command never dials a
/// server socket, or `None` when it consumes the root `--socket` global.
///
/// `--socket` is declared once, `global`, on the root `Cli`
/// (ADR-0065), so clap accepts it on every invocation path — including the
/// verbs that are pure local operations (config scaffolding, registry
/// edits, completions). Those must refuse a provided `--socket` rather
/// than silently ignore it: a user who typed `phux pair --socket X`
/// believes the flag did something. `main` turns a `Some` from here into
/// a one-line teaching error.
#[allow(
    clippy::match_same_arms,
    reason = "one arm per verb, grouped by namespace; merging arms across namespaces would hide which verbs are listed"
)]
pub(crate) const fn socketless_verb(command: &Command) -> Option<&'static str> {
    match command {
        Command::Agent { action } => match action {
            agent::AgentAction::InstallClaude { .. } => Some("agent install-claude"),
            agent::AgentAction::UninstallClaude => Some("agent uninstall-claude"),
            agent::AgentAction::HookPayload => Some("agent hook-payload"),
            _ => None,
        },
        Command::Config { action } => match action {
            config_action::ConfigAction::Init { .. } => Some("config init"),
            config_action::ConfigAction::Path => Some("config path"),
            config_action::ConfigAction::Check { .. } => Some("config check"),
            config_action::ConfigAction::Show { .. } => Some("config show"),
            config_action::ConfigAction::Plugins { .. } => Some("config plugins"),
            config_action::ConfigAction::Run { .. } => Some("config run"),
            // `agents` best-effort reads live state; `reload` rings the
            // server doorbell.
            config_action::ConfigAction::Agents { .. } | config_action::ConfigAction::Reload => {
                None
            }
        },
        Command::Workspace { action } => match action {
            WorkspaceAction::Inspect { .. } => Some("workspace inspect"),
            WorkspaceAction::Save { .. } | WorkspaceAction::Restore { .. } => None,
        },
        Command::Service { action } => match action {
            // `install` bakes the socket into the generated unit.
            ServiceAction::Install { .. } => None,
            // `reconcile` reads the socket out of the unit it is reconciling —
            // the only socket whose liveness is relevant — so a `--socket` on
            // the command line would be silently ignored.
            ServiceAction::Reconcile { .. } => Some("service reconcile"),
            ServiceAction::Uninstall => Some("service uninstall"),
            ServiceAction::Status => Some("service status"),
            ServiceAction::Logs { .. } => Some("service logs"),
            ServiceAction::PruneLogs { .. } => Some("service prune-logs"),
        },
        Command::Plugin { .. } => Some("plugin"),
        Command::Host { .. } => Some("host"),
        Command::Relay { .. } => Some("relay"),
        Command::Pair { .. } => Some("pair"),
        Command::Workload { .. } => Some("workload"),
        Command::Completion { .. } => Some("completion"),
        Command::Mcp { .. } => Some("mcp"),
        Command::Cockpit { .. } => Some("cockpit"),
        Command::Skill { .. } => Some("skill"),
        Command::Logs { .. } => Some("logs"),
        Command::RuntimeInfo { .. } => Some("runtime-info"),
        Command::Report { .. } => Some("report"),
        Command::GenReferenceDocs { .. } => Some("gen-reference-docs"),
        _ => None,
    }
}

#[derive(Debug, Subcommands)]
pub(crate) enum Command {
    /// Inspect this binary's protocol and capabilities
    #[usage(help_heading = "More", display_order = 64)]
    RuntimeInfo {
        #[usage(flatten)]
        json: JsonOpt,
    },
    /// Attach to a session, here or on a registered host
    ///
    /// Interactive: requires a TTY. With no name, attaches to the
    /// most-recently-focused session, auto-spawning a server if none is
    /// running.
    ///
    /// A name registered as a host (`phux host add`) shadows a local session of the same name: `phux attach
    /// NAME` dials the registered host instead of the local socket.
    /// Pass `--socket` to force the local reading of the name.
    #[usage(alias = "a")]
    #[usage(help_heading = "Sessions", display_order = 10)]
    Attach {
        /// Session name (matches the name used at creation time).
        ///
        /// Omit to attach to the most-recently-focused session.
        session: Option<String>,

        /// Attach over QUIC to a remote `phux server --quic` listener at this
        /// `HOST:PORT` instead of the local Unix socket. HOST may be an IP
        /// literal or a DNS name (e.g. a Tailscale `MagicDNS` name), resolved
        /// before dialing. QUIC is always TLS 1.3-encrypted. A target
        /// resolving to loopback trusts the server's self-signed cert for
        /// local dev; any routable address requires `--cert-fingerprint`
        /// (the value `phux pair` prints on the server host).
        #[usage(
            long,
            value_name = "HOST:PORT",
            group = "remote_transport",
            help_heading = "Direct dial"
        )]
        quic: Option<String>,

        /// Attach over WebSocket to a `phux server --listen` endpoint. Use
        /// `ws://HOST:PORT` for loopback dev, or `wss://HOST:PORT` with
        /// `--token` and `--cert-fingerprint` for routable remote attach. This
        /// is the TCP fallback when UDP/QUIC is blocked.
        #[usage(
            long,
            value_name = "URL",
            group = "remote_transport",
            help_heading = "Direct dial"
        )]
        ws: Option<String>,

        /// Bearer pairing token (hex) for an authenticated QUIC listener, as
        /// minted by `phux pair`. QUIC sends it as the stream's opening
        /// preamble; WebSocket sends it as `Authorization: Bearer`.
        /// Requires `--quic` or `--ws`.
        // No parser-level `requires("--quic", "--ws")`: usage-rs treats that
        // as all-of, which rejected a single transport. The any-of rule is
        // enforced in `run_attach` (exit 2), matching `phux logs -f`.
        #[usage(long, help_heading = "Direct dial")]
        token: Option<String>,

        /// Pin the QUIC server's certificate by its SHA-256 fingerprint (the
        /// value `phux pair` prints). Required to dial any non-loopback
        /// `--quic`/`--ws wss://` address. Requires `--quic` or `--ws`.
        #[usage(long, value_name = "FP", help_heading = "Direct dial")]
        cert_fingerprint: Option<String>,

        /// TLS server name (SNI) to offer the remote listener. QUIC defaults
        /// to `localhost`; WebSocket defaults to the URL host. Requires
        /// `--quic` or `--ws`.
        #[usage(long, value_name = "NAME", help_heading = "Direct dial")]
        tls_server_name: Option<String>,

        /// Attach to a phux server on another machine, ssh-style:
        /// `--remote me@mini`. Resolves to a registered host when there is
        /// one, otherwise pairs the host first (over a `--code`, or over
        /// your existing ssh trust) and registers the result, so every
        /// later attach is a direct QUIC dial with no ssh in the path.
        ///
        /// PORT defaults to 8788, the port a server auto-binds on its
        /// overlay address. The `user@` half names the ssh destination used
        /// to pair; it is not sent on the wire.
        #[usage(
            long,
            value_name = "[USER@]HOST[:PORT]",
            conflicts("--quic", "--ws", "--ssh"),
            help_heading = "Remote host"
        )]
        remote: Option<String>,

        /// Pair `--remote` from a `https://phux.phall.io/connect?...` link
        /// (or its `phux://connect?...` spelling) instead of over ssh — the
        /// same link `phux pair` prints and `phux pair --qr` renders. Quote
        /// it: it contains `&`.
        #[usage(
            long,
            value_name = "LINK",
            requires("--remote"),
            help_heading = "Remote host"
        )]
        code: Option<String>,

        /// Never shell out to ssh for `--remote`. An unregistered host is
        /// refused with its remedies named instead of paired.
        #[usage(long, requires("--remote"), help_heading = "Remote host")]
        no_enroll: bool,

        /// Attach mosh-style over ssh: run `phux bootstrap` on the host
        /// through ssh, which starts the server there if needed and opens a
        /// QUIC listener for this attach alone, then dial it directly. ssh
        /// authenticates you (password and 2FA prompts work) and exits once
        /// the session is up; the session itself rides QUIC, so it roams and
        /// renders locally. Needs no pairing, service, or overlay network on
        /// the host. Falls back to `ssh -t HOST phux attach` when UDP cannot
        /// reach it. The host is anything ssh accepts, including
        /// `ssh://user@host:port` and aliases from `~/.ssh/config`.
        #[usage(
            long,
            value_name = "[USER@]HOST",
            conflicts("--quic", "--ws", "--remote"),
            help_heading = "Remote host"
        )]
        ssh: Option<String>,

        /// The `phux` to run on the `--ssh` host, for when a non-interactive
        /// ssh shell's `PATH` does not find it (a Homebrew or Nix install).
        #[usage(
            long,
            value_name = "PATH",
            requires("--ssh"),
            default = "phux",
            help_heading = "Remote host"
        )]
        remote_phux: String,

        /// Bind the `--ssh` host's listener to a UDP port in this inclusive
        /// range, e.g. `60000-61000`, so one firewall rule covers every
        /// attach. Any free port by default.
        #[usage(
            long,
            value_name = "MIN-MAX",
            requires("--ssh"),
            help_heading = "Remote host"
        )]
        udp_ports: Option<String>,

        /// Tee this attach's composited output to a recording. Declared here
        /// (and on the root command) rather than globally so it only shows up
        /// on the verbs that raise a TUI.
        #[usage(flatten, next_help_heading = "Recording")]
        rec: RecOpts,
    },

    /// Run a server in the foreground
    ///
    /// Binds a Unix domain socket, pre-seeds a session whose initial
    /// pane spawns the user's `$SHELL` inside a real PTY, and serves
    /// `ATTACH` requests until Ctrl-C.
    #[usage(help_heading = "Machines", display_order = 41)]
    Server {
        /// Ensure the selected local socket accepts, then exit without a TUI.
        ///
        /// Reuses a live coordinator or starts one with the same session-name
        /// template and spawn-on-attach policy as naked `phux`. Uses `--socket`,
        /// then `PHUX_SOCKET`, then the profile default (`PHUX_PROFILE`). Exits 0
        /// only after a successful socket connection; startup or timeout
        /// failures exit 1 with a diagnostic on stderr. Startup is bounded to
        /// 10 seconds, including lock contention. Does not attach or create
        /// another session on an existing server.
        #[usage(
            long,
            conflicts(
                "--session",
                "--listen",
                "--quic",
                "--webtransport",
                "--connect",
                "--hub",
                "--exit-after-idle"
            )
        )]
        ensure: bool,

        /// Name of the pre-seeded session. Matches what
        /// `phux attach <name>` will request.
        #[usage(long, default = "default")]
        session: String,

        /// Start with no pre-seeded session. `phux new --empty` starts a
        /// server this way when none is running, so the only session is the
        /// empty one it asked for.
        #[usage(long, hide, conflicts("--seed-command"))]
        no_seed: bool,

        /// Also accept WebSocket clients on this `HOST:PORT` (the UDS stays
        /// on). Loopback (e.g. `127.0.0.1:8787`) is plaintext for local
        /// browser dev; any routable address (e.g. `0.0.0.0:8787`)
        /// auto-provisions TLS and requires a `phux pair` token.
        /// Overrides `$PHUX_WS_ADDR`.
        #[usage(long, value_name = "HOST:PORT")]
        listen: Option<std::net::SocketAddr>,

        /// Also accept QUIC clients on this `HOST:PORT` (the UDS stays on).
        /// QUIC is always TLS 1.3-encrypted; a loopback address skips token
        /// auth (local dev), while any routable address requires a `phux pair`
        /// token sent as the stream's opening preamble.
        /// Overrides `$PHUX_QUIC_ADDR`.
        #[usage(long, value_name = "HOST:PORT")]
        quic: Option<std::net::SocketAddr>,

        /// Also accept WebTransport (HTTP/3 over QUIC) clients on this
        /// `HOST:PORT` (the UDS stays on) — the browser's door to QUIC-class
        /// transport; the browser client dials it, falling back to WebSocket.
        /// Always TLS 1.3-encrypted; a loopback address skips token auth
        /// (local dev), while any routable address requires a `phux pair`
        /// token carried in the CONNECT request (`Authorization: Bearer`
        /// from native consumers, `?token=<hex>` on the session URL from
        /// browsers). Overrides `$PHUX_WT_ADDR`.
        #[usage(long, value_name = "HOST:PORT")]
        webtransport: Option<std::net::SocketAddr>,

        /// Dial one relay outbound on `HOST:PORT`. If a matching
        /// `[[connector]]` entry exists, its token file and certificate pin
        /// are used; otherwise only a loopback endpoint is accepted for
        /// unauthenticated development. Without this flag, every configured
        /// connector is supervised independently.
        #[usage(long, value_name = "HOST:PORT")]
        connect: Option<String>,

        /// Run as a federation hub: consume the `[[satellites]]`
        /// registry from `config.toml` at startup, validating every enabled
        /// entry's endpoint (`quic://`, `ws://`, `wss://`, or `ssh://`) into
        /// the runtime satellite table, then dial and maintain one outbound
        /// link per satellite (QUIC and WebSocket links authenticate with a
        /// bearer token; `ssh://` bridges over `ssh HOST phux stdio-bridge`),
        /// relaying satellite-tagged frames over the links.
        /// A malformed enabled endpoint or a duplicate satellite name fails
        /// startup. Without this flag the registry is ignored.
        #[usage(long)]
        hub: bool,

        /// Exit once no client has been connected for SECS, even if panes
        /// are still running. For ephemeral servers: a test harness or CI
        /// job that bootstraps a private server per run and cannot
        /// guarantee its own cleanup step will execute. The clock starts at
        /// startup, so a server nobody ever connects to also exits.
        ///
        /// Without this flag the server keeps the multiplexer contract and
        /// lives until its last pane is gone.
        #[usage(
            long = "exit-after-idle",
            value_name = "SECS",
            validate = "int(value) >= 1 && int(value) <= 86400",
            validate_error = "must be between 1 and 86400 seconds"
        )]
        exit_after_idle: Option<u64>,

        /// Detach from the controlling terminal via `setsid(2)` before
        /// binding. Set by the auto-spawn path so the server outlives
        /// the launching client's terminal; a foreground `phux server`
        /// run by hand leaves this off so Ctrl-C still works.
        #[usage(long, hide, conflicts("--ensure"))]
        daemonize: bool,

        /// Run this command (via `$SHELL -c`) as the pre-seeded session's
        /// initial program instead of a bare shell. The naked-`phux`
        /// auto-spawn path passes `defaults.spawn-on-attach` here;
        /// `phux new` deliberately does not, so an
        /// explicitly-created session still gets a shell.
        #[usage(long, hide, conflicts("--ensure"))]
        seed_command: Option<String>,

        /// Graceful-upgrade resume: read the handoff state blob
        /// from this inherited descriptor, adopt the inherited listener, and
        /// rebuild the live session tree instead of starting fresh. Set by
        /// the upgrade orchestrator's re-exec; never passed by hand.
        #[usage(long, hide, conflicts("--ensure"))]
        resume: Option<std::os::fd::RawFd>,
    },

    /// List sessions
    ///
    /// Queries the running server and prints one line per session. Does not
    /// start a server: with no server running it reports as much and exits
    /// non-zero (like `tmux ls`). Pass `--json` for the stable, versioned
    /// machine shape instead of the human text.
    #[usage(alias = "list")]
    #[usage(help_heading = "Sessions", display_order = 12)]
    Ls {
        #[usage(flatten)]
        json: JsonOpt,

        #[usage(flatten)]
        remote: RemoteOpt,
    },

    /// Report who this connection is to the server
    ///
    /// Prints the principal and credential id (for a paired device), the
    /// auth route (the local socket, or a QUIC / WebSocket bearer
    /// credential), the peer uid (local socket), the OS user and host the
    /// server runs as, and the server's version, one field per line. Read
    /// only: a phux server never switches users, so the serving user is also
    /// the user every pane runs as. With `--remote HOST` it reports what
    /// that dial authenticated as there. Does not start a server.
    #[usage(help_heading = "More", display_order = 63)]
    Whoami {
        #[usage(flatten)]
        json: JsonOpt,

        #[usage(flatten)]
        remote: RemoteOpt,
    },

    /// Report the running server: pid, uptime, clients
    ///
    /// One glance at the server behind the socket: whether it is running
    /// and as which pid, since when, the protocol version it speaks, how
    /// many clients are attached, the sessions it holds, and where its
    /// logs live. Does not start a server: with no server running it
    /// reports as much and exits non-zero. Pass `--json` for the stable,
    /// versioned machine shape instead of the human text; with no server
    /// that shape is `{"running": false, ...}` on stdout, still exiting
    /// non-zero.
    // `status` is the one verb whose JSON failure shape differs from the
    // shared `JsonOpt` contract: "no server" is an answer, not an error, so
    // it lands on stdout as `{"running": false, ...}`. The flattened struct
    // cannot carry per-verb help, so the arg's help is overridden here to
    // state the exception next to the flag (phux-i0e8.11.6 wave-8 nit).
    #[usage(help_heading = "Maintain", display_order = 50)]
    Status {
        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Show the server's performance telemetry
    ///
    /// Reads the always-on latency histograms, throughput counters, and
    /// process figures the server keeps about itself (`GET_PERF`) and
    /// prints them as a table grouped by pipeline stage: `pty.*` (child
    /// output arriving), `echo.server` (input to first output on the same
    /// pane), `tick.*` and `pump.*` (fan-out to clients), `wire.*` (socket
    /// writes), `cmd.*` / `attach.*` (control plane), and `consumer.*`
    /// (per-client backpressure). Without `--watch` the numbers cover the
    /// server's lifetime; with `--watch SECS` the verb polls and prints
    /// each interval on its own, so counters become rates and a stall
    /// shows up in the second it happened. Does not start a server.
    #[usage(help_heading = "More", display_order = 62)]
    Perf {
        #[usage(flatten)]
        json: JsonOpt,
        /// Poll every SECS seconds and print each interval as a delta.
        #[usage(long, value_name = "SECS")]
        watch: Option<f64>,
        /// Zero the server's metrics after each snapshot.
        #[usage(long)]
        reset: bool,
    },

    /// Create a session and attach to it
    ///
    /// Creates the named session if it does not already exist, then
    /// attaches. Auto-starts a server if none is running. A name already
    /// in use is an error; omit the name to take the configured
    /// `session-name-template`, disambiguated with fresh `${random-name}`
    /// picks when the template draws one, then a numeric suffix.
    ///
    /// With `--json`, creates the session *without* attaching and prints
    /// the seed pane's id as JSON instead. This neither attaches nor
    /// resizes, and the create is atomic server-side (no attach race).
    /// `--json` requires an explicit `-s NAME`, and a name already in use
    /// is an error (create-only, never create-or-attach).
    // The `--json` ⇒ `-s` rule is enforced here at the clap level (the
    // group's `requires` fires whenever `--json` is present) rather than as
    // a runtime gate, so the refusal is a usage error with usage text
    // (phux-i0e8.8.4). A group is used because `json` lives on the shared
    // flattened `JsonOpt` and cannot carry a per-verb `requires` itself.
    #[usage(help_heading = "Sessions", display_order = 11)]
    New {
        /// Session name. `phux new work` creates a session named "work".
        /// Omitted ⇒ the `session-name-template` (default: the cwd
        /// basename), redrawn if it uses `${random-name}` and the pick is
        /// taken, then disambiguated with a numeric suffix.
        #[usage(value_name = "NAME")]
        name: Option<String>,

        /// Session name in flag form — equivalent to the positional NAME,
        /// and the form required by `--json`. An error if it conflicts
        /// with NAME.
        #[usage(short = 's', long = "session")]
        session: Option<String>,

        /// Working directory for the seed pane.
        #[usage(short = 'c', long = "cwd")]
        cwd: Option<std::path::PathBuf>,

        /// Emit stable, versioned JSON on stdout instead of the human view.
        /// On failure, stdout stays empty and stderr carries one JSON error
        /// object. Requires an explicit `-s NAME` (a positional NAME is not
        /// enough).
        #[usage(long, requires("--session"))]
        json: bool,

        /// Environment assignment for the seed process. Repeat for multiple
        /// variables. Headless `--json` mode only.
        #[usage(
            short = 'e',
            long = "env",
            value_name = "KEY=VALUE",
            requires("--json")
        )]
        env: Vec<EnvAssignment>,

        #[usage(flatten)]
        remote: RemoteOpt,

        /// Create the session with no terminal. An empty session is
        /// keep-empty: it survives its last window and only `phux kill`
        /// removes it. Without `--json` the new session is attached and shows
        /// an empty state; open a window from there.
        #[usage(long, conflicts("--command", "--env", "--cwd"))]
        empty: bool,

        /// Command (and arguments) to run in the seed pane instead of the
        /// default shell. Must follow `--`: `phux new work -- htop`.
        #[usage(double_dash = "required")]
        command: Vec<String>,
    },

    /// Create a pane without attaching
    ///
    /// With `--target`, the pane is inserted beside an exact local owner;
    /// otherwise it joins the server's most recently active session. The new
    /// pane's id prints to stdout. With `--satellite NAME` on a
    /// federation hub (`phux server --hub`), the spawn is routed over
    /// the hub's link to that satellite and the returned id is
    /// qualified with that host — addressable through the hub by every
    /// satellite-capable verb. Does not auto-start a server.
    #[usage(help_heading = "Panes", display_order = 20)]
    Spawn {
        /// Route the spawn to a configured federation satellite (a name
        /// from `phux host ls --role satellite`, on a server running
        /// `--hub`).
        #[usage(long, value_name = "NAME")]
        satellite: Option<String>,

        /// Existing local pane beside which to place the new pane.
        #[usage(long, value_name = "TARGET", conflicts("--satellite"))]
        target: Option<String>,

        /// Split axis for explicit placement (requires `--target`).
        #[usage(long, value_enum, default = "horizontal", requires("--target"))]
        split: SpawnSplit,

        /// Fraction of the split retained by TARGET (requires `--target`).
        #[usage(
            long,
            default = "0.5",
            default_value_t = 0.5,
            requires("--target"),
            validate = "float(value) > 0 && float(value) < 1",
            validate_error = "ratio must be finite and strictly between 0 and 1"
        )]
        ratio: f32,

        /// Working directory for the new pane.
        #[usage(short = 'c', long = "cwd")]
        cwd: Option<String>,

        #[usage(flatten)]
        json: JsonOpt,

        /// Command (and arguments) to run instead of the default shell.
        /// Must follow `--`: `phux spawn -- htop`.
        #[usage(trailing_var_arg)]
        command: Vec<String>,
    },

    /// Start an agent integration in a new pane
    ///
    /// Resolves INTEGRATION (a `phux launch --list` id) to its `[launch]`
    /// command from an enabled plugin's integration template, then creates a
    /// pane running it. The integration also gives the pane its agent name
    /// and kind automatically, with no alias or per-shell config.
    ///
    /// `--print` resolves and prints the argv without spawning (a server-free
    /// dry run). Extra agent arguments follow `--`:
    /// `phux launch codex -- --model o3`.
    #[usage(help_heading = "Agents", display_order = 31)]
    Launch {
        /// Integration id to launch (from `phux launch --list`).
        #[usage(value_name = "INTEGRATION", required_unless("--list"))]
        integration: Option<String>,

        /// List launchable integrations from enabled plugins and exit.
        #[usage(long)]
        list: bool,

        /// Resolve and print the launch argv (and cwd) without spawning a
        /// pane — a server-free dry run.
        #[usage(long, alias = "dry-run")]
        print: bool,

        #[usage(flatten)]
        json: JsonOpt,

        /// Existing local pane beside which to place the launched pane.
        #[usage(long, value_name = "TARGET", conflicts("--list", "--print"))]
        target: Option<String>,

        /// Split axis for explicit placement (requires `--target`).
        #[usage(long, value_enum, default = "horizontal", requires("--target"))]
        split: SpawnSplit,

        /// Fraction of the split retained by TARGET (requires `--target`).
        #[usage(
            long,
            default = "0.5",
            default_value_t = 0.5,
            requires("--target"),
            validate = "float(value) > 0 && float(value) < 1",
            validate_error = "ratio must be finite and strictly between 0 and 1"
        )]
        ratio: f32,

        /// Working directory for a `working_directory = "workspace"`
        /// template. Defaults to the current directory.
        #[usage(short = 'c', long = "cwd", value_name = "DIR")]
        cwd: Option<std::path::PathBuf>,

        /// Extra arguments appended to the agent command, after `--`.
        #[usage(trailing_var_arg)]
        extra: Vec<String>,
    },

    /// Kill a session, window, pane, or the server
    ///
    /// `TARGET` uses the selector grammar (see the top-level help):
    /// `name`, `name:N`, `name:N.M`, `name:tag`, `@N`, `.`. The selector
    /// is resolved client-side against a server-state snapshot to a set of
    /// Terminals; the server is then asked to kill each.
    ///
    /// `--server` stops the server process instead, ending every session on
    /// it. Local socket only: the server accepts that stop on its local
    /// socket alone, so `--server` cannot combine with `--remote`.
    #[usage(help_heading = "Sessions", display_order = 13)]
    Kill {
        /// What to kill (selector).
        #[usage(group = "kill_what")]
        target: Option<String>,
        /// Stop the running server, ending every session it holds.
        ///
        /// The server exits cleanly, so a supervised one stays stopped rather
        /// than being restarted. Note that the next `phux attach`/`new` will
        /// auto-spawn a fresh server: this stops the current one, it does not
        /// disable phux.
        #[usage(long, group = "kill_what")]
        server: bool,

        #[usage(flatten)]
        remote: RemoteOpt,
    },

    /// Insert an existing pane into a layout
    ///
    /// Both selectors must each resolve to exactly one local pane in the same
    /// session. This command does not spawn: create `NEW_PANE` first with
    /// `phux spawn`, then insert it. Omitted direction defaults horizontal.
    #[usage(name = "insert-pane")]
    #[usage(help_heading = "Panes", display_order = 29)]
    InsertPane {
        /// Existing layout leaf beside which `NEW_PANE` is inserted.
        target: String,
        /// Already-created pane to insert; no implicit spawn occurs.
        new_pane: String,
        /// Split axis: `horizontal` stacks the panes, `vertical` places
        /// them side-by-side.
        #[usage(long, value_enum, default = "horizontal")]
        split: SpawnSplit,
        /// Fraction assigned to TARGET; must be strictly between 0 and 1.
        #[usage(
            long,
            default = "0.5",
            default_value_t = 0.5,
            validate = "float(value) > 0 && float(value) < 1",
            validate_error = "ratio must be finite and strictly between 0 and 1"
        )]
        ratio: f32,
        /// Emit a schema-versioned JSON result or error.
        #[usage(long)]
        json: bool,
    },

    /// Move a pane beside another, across sessions too
    ///
    /// SOURCE is collapsed out of its current tree position and inserted
    /// beside TARGET. Both selectors must resolve to exactly one local pane.
    /// When TARGET lives in a different session the pane is re-parented on
    /// the server first — its process, scrollback, and id survive the move.
    #[usage(name = "move-pane")]
    #[usage(help_heading = "Panes", display_order = 30)]
    MovePane {
        /// Pane to relocate.
        source: String,
        /// Existing destination pane.
        target: String,
        /// Destination split axis: `horizontal` stacks the panes,
        /// `vertical` places them side-by-side.
        #[usage(long, value_enum, default = "horizontal")]
        split: SpawnSplit,
        /// Fraction assigned to TARGET; must be strictly between 0 and 1.
        #[usage(
            long,
            default = "0.5",
            default_value_t = 0.5,
            validate = "float(value) > 0 && float(value) < 1",
            validate_error = "ratio must be finite and strictly between 0 and 1"
        )]
        ratio: f32,
        /// Emit a schema-versioned JSON result or error.
        #[usage(long)]
        json: bool,
    },

    /// Swap two panes in a layout
    ///
    /// Both selectors must each resolve to exactly one local pane. Split
    /// geometry is preserved and attached clients retain their local focus.
    #[usage(name = "swap-pane")]
    #[usage(help_heading = "Panes", display_order = 31)]
    SwapPane {
        /// First pane selector.
        first: String,
        /// Second pane selector.
        second: String,
        /// Emit a schema-versioned JSON result or error.
        #[usage(long)]
        json: bool,
    },

    /// Set a pane's grid size, with no TTY.
    // Spelled out in `long_about` (the shape `rec` and `play` set) because
    // clap reflows doc-comment paragraphs: as a doc comment the examples
    // below collapse onto one run-on line.
    #[usage(
        help = "Set a pane's grid size",
        long_help = "Set a pane's grid size, with no TTY.\n\n\
            The headless counterpart to resizing your terminal window: names one \
            pane and gives it an exact cell geometry. Nothing attaches and \
            nothing subscribes, so the pane is never dragged toward the 80x24 \
            size a program with no terminal would otherwise report.\n\n\
            The new size takes effect immediately, even with someone attached. \
            It is not permanent against an attached view: under the default \
            `window-size = \"smallest\"` policy the next attach, detach, or window \
            resize recomputes the pane's geometry from the attached views and \
            overrides it. Set `window-size = \"manual\"` when an explicit size \
            must hold. Either way this verb reads the server's real size back \
            before exiting, and exits nonzero if it is not the one you asked \
            for, so a script can never mistake a delivered request for an \
            applied one.\n\n\
            Examples:\n  \
            phux resize demo 120x40\n  \
            phux resize @7 200x50 --json"
    )]
    #[usage(help_heading = "Panes", display_order = 27)]
    Resize {
        /// Target selector: session, session:window, session:window.pane,
        /// @id, or `.` (focused). `=` is unsupported by headless commands.
        target: String,

        /// New grid size, e.g. 120x40. Both axes are whole numbers of
        /// cells and at least 1.
        #[usage(value_name = "COLSxROWS")]
        geometry: resize::Geometry,

        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Detach clients from a session
    ///
    /// The CLI counterpart to the `C-a d` keybinding. With `SESSION`, detaches
    /// every client attached to that session; with no argument, detaches every
    /// attached client on the server. Each target client's TUI exits cleanly.
    /// Useful for scripting or reclaiming a session that's attached (or wedged)
    /// elsewhere.
    #[usage(help_heading = "Sessions", display_order = 14)]
    Detach {
        /// Session to detach clients from. Omit to detach every attached
        /// client on the server.
        session: Option<String>,

        #[usage(flatten)]
        remote: RemoteOpt,
    },

    /// Take exclusive input control of a pane
    ///
    /// Seizes exclusive input authority over the resolved pane: while held,
    /// only this connection's input reaches the PTY — every other client's
    /// keystrokes (and any agent's `send-keys`) are locked out. Use it to
    /// grab control of a pane an agent is driving. Release with `phux give`.
    /// TARGET is a selector (see the top-level help).
    #[usage(help_heading = "Agents", display_order = 33)]
    Take {
        /// Target selector (resolves to one pane).
        target: String,
    },

    /// Give back input control taken with `take`
    ///
    /// Releases the input lease taken with `phux take`, returning the pane to
    /// open input. A no-op if you do not hold the lease. TARGET is a selector.
    #[usage(help_heading = "Agents", display_order = 34)]
    Give {
        /// Target selector (resolves to one pane).
        target: String,
    },

    /// Signal a pane's process group.
    // `long_about` for the same reason `rec` spells one out: clap reflows
    // doc-comment paragraphs and the examples need real newlines.
    #[usage(
        help = "Send a signal to a pane's process group",
        long_help = "Signal a pane's process group.\n\n\
            Delivers a POSIX signal to the program running in the resolved pane and \
            every subprocess it spawned — distinct from `phux kill`, which destroys \
            the pane. `freeze` (SIGSTOP) pauses the process mid-step; `resume` \
            (SIGCONT) lets it run again — the reversible brake for an agent about to \
            do something rash. TARGET is a selector.\n\n\
            Examples:\n  \
            phux signal build freeze\n  \
            phux signal . kill"
    )]
    #[usage(help_heading = "Agents", display_order = 35)]
    Signal {
        /// Target selector (resolves to one pane).
        target: String,

        /// Which signal to deliver.
        #[usage(value_enum)]
        signal: SignalArg,
    },

    /// Update phux to the latest stable or next release, keeping sessions alive.
    // `long_about` spelled out for the same reason `rec` and `signal` do it:
    // clap reflows doc-comment paragraphs and the worked examples need real
    // newlines.
    #[usage(
        help = "Update phux, keeping sessions alive",
        long_help = "Update phux to the latest stable or next release, keeping sessions alive.\n\n\
            Checks the published release, downloads the archive for this platform, \
            verifies it against the checksum published beside it, replaces the \
            binaries atomically, and asks a running server to re-exec so live panes \
            survive. `--channel next` follows green `main` instead of the latest \
            `vX.Y.Z`; `--channel latest` is the numbered releases. `phux channel` \
            is the same switch without the flag. A server, its local clients, its \
            satellites, and its relays must \
            all run the same release, so this is the command that moves a whole \
            deployment in one step.\n\n\
            phux updates only installs it maintains: a release archive unpacked into \
            $PHUX_INSTALL_DIR, ~/.local/bin, ~/bin, /usr/local/bin, or /opt/phux/bin. \
            A Homebrew, Cargo, or Nix install is never modified — the exact native \
            command is printed instead — and an unrecognized location is refused \
            rather than overwritten.\n\n\
            The previous binaries are kept beside the new ones; `--rollback` puts \
            them back.\n\n\
            Examples:\n  \
            phux update --check\n  \
            phux update --check --json\n  \
            phux update\n  \
            phux update --channel next\n  \
            phux update --channel latest\n  \
            phux update --dry-run --version v1.2.3\n  \
            phux update --rollback"
    )]
    #[usage(help_heading = "Maintain", display_order = 56)]
    Update {
        /// Update options.
        #[usage(flatten)]
        opts: update::UpdateOpts,
    },

    /// Show or switch the release channel.
    // `long_help` spelled out so the examples keep real newlines.
    #[usage(
        help = "Show or switch the release channel",
        long_help = "Show or switch the release channel.\n\n\
            Bare `phux channel` reports the rail this install follows and what \
            is published there. `phux channel next` follows green `main`; \
            `phux channel latest` (also `stable`) follows the numbered GitHub \
            releases. Switching persists the choice and runs the same update \
            path as `phux update --channel`, so live panes survive.\n\n\
            Examples:\n  \
            phux channel\n  \
            phux channel next\n  \
            phux channel latest"
    )]
    #[usage(help_heading = "Maintain", display_order = 58)]
    Channel {
        /// Channel to follow. Omit to report the current rail without changing it.
        #[usage(value_enum, value_name = "CHANNEL")]
        channel: Option<update::channel::Channel>,

        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Open the native macOS Cockpit app.
    #[usage(
        help = "Open the native macOS Cockpit app",
        long_help = "Open the native macOS Cockpit app.\n\n\
            Finds Phux Cockpit.app in /Applications or ~/Applications and opens \
            it through Launch Services. Set PHUX_COCKPIT_APP to pin a specific \
            bundle. macOS-only; if the app is missing the remedy is the curl \
            installer.\n\n\
            Examples:\n  \
            phux cockpit\n  \
            phux cockpit --json"
    )]
    #[usage(help_heading = "More", display_order = 67)]
    Cockpit {
        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Hot-swap the running server to the installed binary
    ///
    /// Asks the server to snapshot every pane, re-exec the on-disk binary, and
    /// re-adopt the live PTYs, so the shells / editors / agents in every
    /// session survive a binary update (e.g. after `cargo install` /
    /// `brew upgrade`). Clients briefly disconnect and reconnect. This is the
    /// low-level primitive: it re-execs whatever is already on disk and
    /// downloads nothing. `phux update` is the command that puts a new binary
    /// there first.
    #[usage(help_heading = "Maintain", display_order = 57)]
    Upgrade {},

    /// Rename a session
    ///
    /// Reassigns `SESSION`'s human-readable name to `NEW_NAME` in one
    /// round-trip. The server is authoritative;
    /// attached clients pick up the new name on their next snapshot. An
    /// unknown `SESSION` or a `NEW_NAME` already in use is an error.
    #[usage(help_heading = "Sessions", display_order = 15)]
    Rename {
        /// Current session name.
        session: String,

        /// New session name.
        new_name: String,

        #[usage(flatten)]
        remote: RemoteOpt,
    },

    /// Read a pane's screen as text or JSON
    ///
    /// The agent "floor": read what's on screen as JSON (`--json`) or a
    /// boxed text view, without a TTY or tmux. The read is side-effect-free
    /// — the server walks its own grid, so this neither attaches nor
    /// resizes the pane, and is safe to poll against a pane another client
    /// is using.
    ///
    /// TARGET is a selector (see the top-level help); omit it for the
    /// most-recently-focused session.
    #[usage(help = "Read a pane's screen as text or JSON")]
    #[usage(help_heading = "Panes", display_order = 21)]
    Snapshot {
        /// Target selector. Omit for the most-recently-focused session.
        #[usage(value_name = "TARGET")]
        session: Option<String>,

        #[usage(flatten)]
        json: JsonOpt,

        /// Include scrollback history above the viewport.
        /// Bare `--scrollback` requests all retained history; `--scrollback
        /// N` requests the most-recent N rows. History appears in the JSON
        /// `scrollback` field; the boxed view shows it above the viewport.
        #[usage(long, value_name = "N", num_args = 0..=1, default_missing = "0")]
        scrollback: Option<u32>,

        /// Include per-cell OSC-133 semantic marks + styles.
        /// Populates the JSON `cells` array (sparse: only cells with a
        /// non-default style or a semantic mark). No effect on the boxed
        /// view, which is plain text.
        #[usage(long)]
        cells: bool,

        /// Return the last N rendered rows (history above the viewport,
        /// then the viewport). Bare `--tail` returns 80; `--tail 0` returns
        /// all, capped at 10000. The viewport is a floor — a grid is never
        /// returned in part — and `truncated` reports any dropped rows.
        // The literals are `phux_core::screen::ROW_WINDOW_DEFAULT` and
        // `ROW_WINDOW_MAX`; clap needs a `&'static str` here, so
        // `commands::snapshot`'s tests pin the two spellings together.
        #[usage(long, value_name = "N", num_args = 0..=1, default_missing = "80")]
        tail: Option<u32>,

        /// Join soft-wrapped rows into logical lines (rows as written, not
        /// as painted). Cannot be combined with `--cells`: cell coordinates
        /// are grid coordinates and do not survive the join.
        #[usage(long, conflicts("--cells"))]
        unwrap: bool,

        /// Emit the CLIENT's composited multi-pane view — the assembled
        /// frame (layout tiling + dividers + status bar) as the human's glass
        /// shows it — as dense structured cells. Unlike the
        /// default side-effect-free read this ATTACHES (drives the headless
        /// client render path). Mutually exclusive with `--cells` /
        /// `--scrollback` / `--tail` / `--unwrap`; sizes the composite via
        /// `--cols` / `--rows`.
        #[usage(long, conflicts("--cells", "--scrollback", "--tail", "--unwrap"))]
        rendered: bool,

        /// Composited viewport width for `--rendered` (no TTY to measure).
        #[usage(long, value_name = "COLS", default = "80", default_value_t = 80)]
        cols: u16,

        /// Composited viewport height for `--rendered`.
        #[usage(long, value_name = "ROWS", default = "24", default_value_t = 24)]
        rows: u16,
    },

    /// Send keys to a pane.
    // `long_about` for the same reason `rec` spells one out: clap reflows
    // doc-comment paragraphs and the examples need real newlines.
    #[usage(
        name = "send-keys",
        help = "Send keys to a pane",
        long_help = "Send keys to a pane.\n\n\
            tmux-shaped: each KEY is a named key (`Enter`, `Tab`, `Escape`, \
            `Up`, `C-c`, `M-x`, …) or a literal string. Literals normally type \
            character by character; a literal run immediately before `Enter` is \
            delivered as a submission-safe paste followed by the real key, honoring \
            the pane's live bracketed-paste mode. TARGET is resolved client-side to \
            one pane, so the live pane is neither attached nor resized.\n\n\
            Flags (`--socket`) MUST precede TARGET: KEYS is a trailing var-arg, \
            so anything after TARGET is taken as a key to send.\n\n\
            Examples:\n  \
            phux send-keys demo \"echo hi\" Enter\n  \
            phux send-keys work:1.0 C-c"
    )]
    #[usage(help_heading = "Panes", display_order = 22)]
    SendKeys {
        /// Target selector: session, session:window, session:window.pane,
        /// @id, or `.` (focused). `=` is unsupported by headless commands.
        target: String,

        /// Keys to send: named keys and/or literal strings, in order.
        #[usage(trailing_var_arg, required)]
        keys: Vec<String>,
    },

    /// Paste text into a pane.
    // `long_about` for the same reason `rec` spells one out: clap reflows
    // doc-comment paragraphs and the examples need real newlines.
    #[usage(
        help = "Paste text into a pane",
        long_help = "Paste text into a pane.\n\n\
            Delivers the payload as ONE paste event to the resolved pane \
            (`ROUTE_INPUT`), so the live pane is neither attached nor resized. \
            When the pane's program has bracketed paste (DEC mode 2004) switched \
            on, the server wraps the payload in paste markers and the program \
            receives it as a single block — auto-indent stays off and multiline \
            text arrives intact. Without the mode, the raw bytes are delivered as \
            if typed.\n\n\
            A paste INSERTS; it does not SUBMIT. Paste-aware shells and REPLs \
            buffer the block until a real Enter — follow with \
            `phux send-keys TARGET Enter` to run what you pasted.\n\n\
            TEXT is the payload; omit it to read the payload from stdin. \
            Payloads are trusted by default (you vouch for content you \
            composed); `--untrusted` opts into the server's safety gate.\n\n\
            Examples:\n  \
            phux paste demo 'SELECT count(*) FROM users;'\n  \
            git diff | phux paste review"
    )]
    #[usage(help_heading = "Panes", display_order = 23)]
    Paste {
        /// Target selector: session, session:window, session:window.pane,
        /// @id, or `.` (focused). `=` is unsupported by headless commands.
        target: String,

        /// Text to paste. Omit to read the payload from stdin.
        text: Option<String>,

        /// Mark the payload untrusted: the server classifies it and the
        /// pane's untrusted-paste policy (reject by default) may silently
        /// drop an unsafe payload — e.g. anything multiline. Without this
        /// flag the paste is trusted and forwarded verbatim.
        #[usage(long)]
        untrusted: bool,
    },

    /// Block until a pane meets a condition.
    // `long_about` for the same reason `rec` spells one out: clap reflows
    // doc-comment paragraphs and the examples need real newlines.
    #[usage(
        help = "Block until a pane meets a condition",
        long_help = "Block until a pane meets a condition.\n\n\
            Polls the side-effect-free screen read — the poll \
            floor of the event surface: always works, no shell integration. \
            Exits 0 when the condition is met, and 124 when `--timeout` expires \
            first. The timeout is one budget for the whole wait — connecting, \
            target resolution, and every screen read — so a server that stops \
            answering still ends the wait on time. The first read always gets \
            at least 2 seconds, so `--timeout 0` checks the condition once. \
            TARGET is a selector (see the \
            top-level help); omit it for the most-recently-focused session.\n\n\
            Matching is against the lines as WRITTEN: rows the terminal \
            soft-wrapped at its right edge are joined first, so text that \
            straddles a wrap is found rather than silently never matching.\n\n\
            Flags (`--until`, `--regex`, `--idle`, `--tail`, `--output-only`, \
            `--timeout`, `--json`, `--socket`) MUST precede TARGET if you give \
            one.\n\n\
            Examples:\n  \
            phux wait --until \"BUILD SUCCESSFUL\" build\n  \
            phux wait --regex \"test result: (ok|FAILED)\" --output-only build\n  \
            phux wait --idle 750 repl"
    )]
    #[usage(help_heading = "Panes", display_order = 25)]
    Wait {
        /// Target selector. Omit for the most-recently-focused session.
        #[usage(value_name = "TARGET")]
        session: Option<String>,

        /// Succeed once any line contains this substring. NOTE: this matches
        /// ANY line, including the shell's echo of a command you just typed
        /// — match on text that appears only in OUTPUT, or pass
        /// `--output-only`.
        #[usage(long, value_name = "TEXT", conflicts("--regex"))]
        until: Option<String>,

        /// Succeed once any line matches this Rust regular expression. One
        /// line at a time, so `^` and `$` anchor to a line you can see. An
        /// invalid pattern is a usage error (exit 2) reported before the
        /// wait starts, never a wait that quietly never matches.
        #[usage(long, value_name = "PATTERN")]
        regex: Option<phux_client::wait::MatchRegex>,

        /// Match only within the last N lines, and read that much history to
        /// do it. Bare `--tail` uses 80; `--tail 0` uses all retained
        /// history, capped at 10000. Without it, only the viewport is read.
        /// N counts logical lines AFTER wrapped rows are joined and ignores
        /// the blank rows under the cursor, and unlike `snapshot --tail` the
        /// viewport is not a floor: `--tail 3` really does mean only the
        /// last three lines with content count — including the prompt block
        /// already back on screen, so leave room for it. A bare `--tail` reads
        /// the next word as N, so spell N out when you also pass TARGET
        /// (`--tail 80 build`, not `--tail build`).
        // The literals are `phux_core::screen::ROW_WINDOW_DEFAULT` and
        // `ROW_WINDOW_MAX`; clap needs a `&'static str` here, so
        // `commands::wait`'s tests pin the two spellings together.
        #[usage(long, value_name = "N", num_args = 0..=1, default_missing = "80")]
        tail: Option<u32>,

        /// Ignore lines the shell marked as your own typed input, so a wait
        /// cannot be satisfied by the echo of the command that started the
        /// work. Needs a shell with OSC-133 integration; with none, nothing
        /// is filtered and phux says so on stderr rather than pretending.
        #[usage(long)]
        output_only: bool,

        /// Succeed once the matched lines hold still for this many
        /// milliseconds (the pane has settled). Default when neither
        /// `--until` nor `--regex` is given. With `--tail N`, only those
        /// lines have to hold still — a spinner further up does not count.
        #[usage(long, value_name = "MS")]
        idle: Option<u64>,

        /// Give up after this many seconds (exit 124), counted from the start
        /// of the command: connecting, resolving TARGET, and every screen
        /// read share the one budget. The first read always gets at least
        /// 2s, so 0 checks the condition once. Default: wait forever.
        #[usage(long, value_name = "SECS")]
        timeout: Option<u64>,

        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Stream a pane's live events (the push half of the agent surface).
    ///
    /// Subscribes to the server's event stream and prints one event per
    /// line until EOF or Ctrl-C. The
    /// subscription neither attaches nor resizes the pane — safe to watch
    /// a pane a human or another agent is actively using. This is the
    /// latency-cutting accelerator of `phux wait`'s poll floor: events
    /// (bell, title change, output dirty/idle, pane spawn/close) arrive as
    /// they happen rather than on a poll tick.
    ///
    /// TARGET is a selector (see the top-level help); omit it for the
    /// most-recently-focused session. With `--json`, each line is a JSON
    /// object (stdout stays pure JSON); otherwise each line is a compact
    /// human form.
    ///
    /// `--until EVENT` and `--timeout SECS` bound the stream, so a script
    /// need not background the watch and kill it on a sleep.
    ///
    ///   phux watch build
    ///   phux watch --json work:1.0
    ///   phux watch --until asked --timeout 120 reviewer
    // `long_about` because clap reflows doc-comment paragraphs and the exit
    // codes need to survive as their own lines.
    #[usage(
        help = "Stream a pane's events as they happen",
        long_help = "Stream a pane's live events (the push half of the agent surface).\n\n\
            Subscribes to the server's event stream and prints one event per line. The \
            subscription neither attaches nor resizes the pane — safe to watch a pane a human \
            or another agent is actively using. TARGET is a selector (see the top-level help); \
            omit it for the most-recently-focused session.\n\n\
            With no bounds the stream runs until EOF or Ctrl-C. `--until EVENT` makes it a \
            gate: the first matching event is printed and `watch` exits 0. `--timeout SECS` \
            gives up and exits 124, the same code `phux wait` uses. If the server closes the \
            stream before an `--until` event arrives, that is exit 1 — the event did not \
            happen and can no longer happen.\n\n\
            With `--json` each line is one JSON object and nothing else is written to stdout: \
            no per-line schema_version, and no summary line on timeout.\n\n\
            Examples:\n  \
            phux watch build\n  \
            phux watch --json work:1.0\n  \
            phux watch --until asked --timeout 120 reviewer"
    )]
    #[usage(help_heading = "Panes", display_order = 26)]
    Watch {
        /// Target selector. Omit for the most-recently-focused session.
        #[usage(value_name = "TARGET")]
        session: Option<String>,

        /// Exit 0 as soon as an event with this name arrives. Repeatable;
        /// any one of them satisfies the watch. The vocabulary is the one
        /// this stream prints: `agent_state`, `asked`, `bell`,
        /// `command_finished`, `command_started`, `dirty`, `idle`,
        /// `pane_closed`, `pane_spawned`, `title_changed`, `unknown`. An
        /// unrecognized name is a usage error (exit 2) reported before the
        /// watch starts, never a watch that quietly never matches.
        #[usage(long, value_name = "EVENT")]
        until: Vec<String>,

        /// Give up after this many seconds (exit 124). Applies with or
        /// without `--until`. Default: stream until EOF or Ctrl-C.
        #[usage(long, value_name = "SECS")]
        timeout: Option<u64>,

        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Record a pane and export it as an asciinema cast, an animated GIF, or
    /// an APNG.
    // The user-facing text is spelled out in `long_about` (the same shape the
    // root command uses) because clap reflows doc-comment paragraphs: as a
    // doc comment the three examples below collapse onto one run-on line.
    #[usage(
        help = "Record a pane to a cast, GIF, or APNG",
        long_help = "Record a pane and export it as an asciinema cast, an animated GIF, or an APNG.\n\n\
            TARGET is a selector (default: the focused pane). Recording is a pure observer: \
            it does not attach the session and never resizes the pane, so it is safe to run \
            against a live session someone is using.\n\n\
            The format follows the output extension (.cast, .gif, .png, .apng); pass --format \
            to override. Use --from to re-render an existing recording without capturing \
            anything.\n\n\
            Examples:\n  \
            phux rec -o demo.gif\n  \
            phux rec work:1.0 -o demo.cast --duration 30\n  \
            phux rec --from demo.cast -o demo.gif --fps 20"
    )]
    #[usage(help_heading = "More", display_order = 60)]
    Rec {
        /// Pane selector. Defaults to the focused pane.
        #[usage(value_name = "TARGET")]
        target: Option<String>,

        /// Output path. The extension picks the format unless --format is
        /// given; a path with no extension gets `.gif`.
        #[usage(short = 'o', long = "out", value_name = "PATH")]
        out: std::path::PathBuf,

        /// Output format, overriding the extension.
        #[usage(long, value_enum, value_name = "FMT")]
        format: Option<RecFormat>,

        /// Re-render an existing .cast instead of capturing a live pane.
        #[usage(long, value_name = "FILE", conflicts("--target", "--duration"))]
        from: Option<std::path::PathBuf>,

        /// Stop after SECS of recording (default: until Ctrl-C or the pane
        /// exits).
        #[usage(long, value_name = "SECS")]
        duration: Option<u64>,

        /// Animation sample rate for GIF/APNG output.
        #[usage(
            long,
            value_name = "FPS",
            default = "10",
            default_value_t = 10,
            validate = "int(value) >= 1 && int(value) <= 50",
            validate_error = "must be between 1 and 50"
        )]
        fps: u8,

        /// Collapse any pause longer than SECS down to SECS. 0 disables.
        #[usage(
            long = "idle-limit",
            value_name = "SECS",
            default = "2.0",
            default_value_t = 2.0
        )]
        idle_limit: f64,

        /// Stop encoding and warn once the output reaches BYTES.
        #[usage(long = "max-bytes", value_name = "BYTES", default = "8388608", default_value_t = 8 * 1024 * 1024)]
        max_bytes: u64,

        /// asciicast format version to write (2 is the interoperable
        /// default).
        #[usage(
            long = "cast-version",
            value_name = "N",
            default = "2",
            default_value_t = 2,
            validate = "int(value) >= 2 && int(value) <= 3",
            validate_error = "must be 2 or 3"
        )]
        cast_version: u8,

        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Play a recording back as a live pane.
    // Spelled out in `long_about` for the same reason `rec` is: clap reflows
    // doc-comment paragraphs into one run-on line and the examples need real
    // newlines.
    #[usage(
        help = "Play a recording back as a live pane",
        long_help = "Play a recording back as a live pane.\n\n\
            Creates a new Terminal whose PTY is fed from FILE, then prints its id. The \
            result is an ordinary pane: attach it, `phux snapshot` it, `phux resize` it, \
            watch it from an agent, or `phux kill` it. It is not a viewer for your own \
            shell — for that, `asciinema play FILE` is the right tool and needs no server.\n\n\
            TARGET says WHERE the pane goes: the playback pane is created beside it, \
            splitting its window. TARGET is never written to, and no flag makes playback \
            take over a pane that already has a shell in it. The default is `.`, the \
            focused pane.\n\n\
            The pane is resized to the recording's own grid first, and to each resize the \
            recording contains, so lines wrap where they wrapped when it was captured; \
            --no-fit leaves the grid alone. When the recording ends the pane holds its \
            final frame until you kill it, so nothing races the last byte; --close ends \
            the pane instead.\n\n\
            Examples:\n  \
            phux play demo.cast\n  \
            phux play demo.cast work:1.0 --speed 2\n  \
            phux play demo.cast --loop --idle-limit 0.5 --json"
    )]
    #[usage(help_heading = "More", display_order = 61)]
    Play {
        /// The .cast file to play.
        #[usage(value_name = "FILE")]
        file: std::path::PathBuf,

        /// Selector for the pane the playback pane is created beside.
        /// Defaults to `.` (the focused pane). Never written to.
        #[usage(value_name = "TARGET")]
        target: Option<String>,

        /// Playback rate. 1 is real time, 2 is twice as fast, 0.5 half
        /// speed. Between 0.01 and 100; no events are ever dropped.
        #[usage(long, value_name = "N", default = "1")]
        speed: play::SpeedArg,

        /// Collapse any pause longer than SECS down to SECS. Defaults to
        /// the idle limit the recording itself declares; 0 plays the raw
        /// timeline.
        #[usage(long = "idle-limit", value_name = "SECS")]
        idle_limit: Option<f64>,

        /// Repeat the recording. Bare `--loop` repeats until the pane is
        /// killed; `--loop N` plays it N times.
        #[usage(long = "loop", value_name = "N", num_args = 0..=1,
              default_missing = "0")]
        loops: Option<u32>,

        /// Split axis for the new pane.
        #[usage(long, value_enum, default = "horizontal")]
        split: SpawnSplit,

        /// Fraction of the split retained by TARGET.
        #[usage(
            long,
            default = "0.5",
            default_value_t = 0.5,
            validate = "float(value) > 0 && float(value) < 1",
            validate_error = "ratio must be finite and strictly between 0 and 1"
        )]
        ratio: f32,

        /// Leave the pane's grid alone instead of fitting it to the
        /// recording's. Output wider than the pane will wrap.
        #[usage(long = "no-fit")]
        no_fit: bool,

        /// Close the pane when playback ends, instead of holding the final
        /// frame until it is killed.
        #[usage(long)]
        close: bool,

        #[usage(flatten)]
        json: JsonOpt,

        /// Internal: this process IS the pane, so write the recording to
        /// stdout rather than spawning one. Hidden because it is an
        /// implementation detail of the pane this verb creates, not a
        /// promise that phux ships a shell-level cast viewer.
        #[usage(long = "pty-writer", hide)]
        pty_writer: bool,
    },

    /// Report that an agent in a pane is waiting on a human answer.
    // `long_about` for the same reason `rec` spells one out: clap reflows
    // doc-comment paragraphs and the examples need real newlines.
    #[usage(
        help = "Report that an agent is waiting on a human",
        long_help = "Report that an agent in a pane is waiting on a human answer.\n\n\
            This is the opt-in hook contract for configured integrations: it emits \
            the same `asked` event as the `phux-ask` title sentinel without writing \
            escape sequences into the target terminal. TARGET is resolved \
            client-side and the command neither attaches nor resizes the pane.\n\n\
            Examples:\n  \
            phux ask work:1.0 --id deploy --suggest Yes --suggest No \"Deploy?\"\n  \
            phux ask @3 --json \"Need approval\""
    )]
    #[usage(help_heading = "Agents", display_order = 32)]
    Ask {
        /// Target selector: session, session:window, session:window.pane,
        /// @id, or `.` (focused). `=` is unsupported by headless commands.
        target: String,

        /// Stable question id for answer correlation.
        #[usage(long, default = "")]
        id: String,

        /// Suggested answer. Repeat to preserve display order.
        #[usage(long = "suggest", value_name = "TEXT")]
        suggestions: Vec<String>,

        /// Seconds the agent has already been waiting.
        #[usage(long, value_name = "SECS")]
        elapsed_seconds: Option<u64>,

        #[usage(flatten)]
        json: JsonOpt,

        /// Human-facing question text.
        question: String,
    },

    /// See and drive the agents running in panes
    ///
    /// Inference (`list`/`show`/`explain`) reports the agent phux infers is
    /// running in each pane. `set`/`clear` write and delete an explicit
    /// per-pane agent identity that overrides inference.
    #[usage(help_heading = "Agents", display_order = 30)]
    Agent {
        #[usage(subcommand)]
        action: agent::AgentAction,
    },

    /// Run a command in a pane and capture its exit code.
    // `long_about` for the same reason `rec` spells one out: clap reflows
    // doc-comment paragraphs and the examples need real newlines.
    #[usage(
        help = "Run a command in a pane and capture its exit code",
        long_help = "Run a command in a pane and capture its exit code.\n\n\
            Reports the command's exit code, output, and duration. \
            Brackets the command with sentinels to capture `$?`, so it \
            assumes a POSIX shell (sh/bash/zsh). The process exit code mirrors \
            the command's — and is 125 when `phux` gives up on `--timeout` — so \
            `phux run … && next` composes like a shell. The timeout is one \
            budget for the whole run — connecting, target resolution, input \
            submission, and every screen read — so a server that stops \
            answering still ends the run on time. Input is never started after \
            the timeout, and once started it gets up to 2 more seconds to \
            finish, so the pane is not left holding a half-typed line; the \
            diagnostic says whether nothing, all, or possibly part of the input \
            was delivered. Giving up does not stop the command or retract input \
            already delivered. TARGET is a selector \
            (see the top-level help), resolved client-side to one pane; the \
            command routes to it by id (no attach, no resize).\n\n\
            Flags (`--timeout`, `--json`, `--socket`) MUST precede TARGET, or \
            they are swallowed into the trailing command.\n\n\
            Examples:\n  \
            phux run build \"cargo test\"\n  \
            phux run --timeout 30 work:1.0 \"cargo test\""
    )]
    #[usage(help_heading = "Panes", display_order = 24)]
    Run {
        /// Target selector: session, session:window, session:window.pane,
        /// @id, or `.` (focused). `=` is unsupported by headless commands.
        target: String,

        /// The command line: all trailing args, joined with spaces.
        #[usage(trailing_var_arg, required)]
        command: Vec<String>,

        /// Give up after this many seconds (exit 125), counted from the start
        /// of the command: connecting, resolving TARGET, submitting the
        /// command, and every screen read share the one budget; input that
        /// has started gets up to 2s more to finish. Default: 600s. Pass 0 to
        /// wait indefinitely.
        #[usage(long, value_name = "SECS")]
        timeout: Option<u64>,

        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Inspect, scaffold, and reload the config file
    ///
    /// phux is config-driven: defaults ship in the binary and
    /// your `config.toml` is a sparse overlay merged on top. These
    /// subcommands never touch a running server, except `reload`,
    /// which signals attached clients to re-read their config in place.
    #[usage(help_heading = "Maintain", display_order = 54)]
    Config {
        #[usage(subcommand)]
        action: config_action::ConfigAction,
    },

    /// Manage plugin manifests in the config
    ///
    /// This is a client-local config operation: it validates
    /// `phux-plugin.toml` manifests and edits `[[plugins]]` entries in the
    /// user's config without contacting a running server.
    #[usage(help_heading = "Maintain", display_order = 55)]
    Plugin {
        #[usage(subcommand)]
        action: PluginAction,
    },

    /// Inspect a git workspace and its worktrees
    ///
    /// This is a local repo operation: it never contacts a running phux server
    /// and never creates or deletes worktrees. Agents use it to map code
    /// checkouts to phux sessions/panes before spawning or attaching work.
    #[usage(help_heading = "More", display_order = 65)]
    Workspace {
        #[usage(subcommand)]
        action: WorkspaceAction,
    },

    /// Read and write pane tags (address them with #tag)
    ///
    /// Tags are freeform strings attached to panes. Once a pane is tagged,
    /// the `#tag` selector addresses every pane carrying that tag — e.g.
    /// `phux kill #build`, `phux snapshot #web`.
    #[usage(help_heading = "Panes", display_order = 28)]
    Tag {
        #[usage(subcommand)]
        action: TagAction,
    },

    /// Bridge stdin/stdout to the local server socket for SSH-stdio transport.
    ///
    /// The remote end of the SSH-stdio transport: `ssh HOST phux
    /// stdio-bridge` gives the dialing side a byte-transparent pipe to the
    /// phux server's Unix socket on HOST — the federation hub dials
    /// `ssh://` satellites through it. The bridge neither
    /// parses nor injects bytes; stdout is protocol-only and diagnostics
    /// go to stderr. Exits when either side closes.
    // Hidden: machine-only plumbing that `ssh HOST phux stdio-bridge`
    // invokes — a human never types it, so it stays out of `--help`, the
    // generated completions, and the docs/reference pages while continuing
    // to parse (phux-i0e8.12.5, re-landed by phux-06nn).
    #[usage(name = "stdio-bridge", hide)]
    StdioBridge {},

    /// Open a one-attach QUIC listener on this host and print how to reach it.
    ///
    /// The far end of `phux attach --ssh`: starts the server if none is
    /// running, asks it for a listener that admits only a token minted for
    /// it, and prints one JSON line naming the port, the certificate
    /// fingerprint to pin, and the token.
    // Hidden: `phux attach --ssh` runs it over ssh and no human types it,
    // the same reasoning as `stdio-bridge` above.
    #[usage(name = "bootstrap", hide)]
    Bootstrap {
        /// The version of the phux that asked, named in a mismatch report.
        #[usage(long, value_name = "VERSION")]
        client_version: Option<String>,

        /// Inclusive UDP port range to bind from, e.g. `60000-61000`.
        #[usage(long, value_name = "MIN-MAX")]
        port_range: Option<String>,

        /// Seconds the listener stays open with nobody connected; `0` asks
        /// for the server default.
        #[usage(long, value_name = "SECS", default = "0", default_value_t = 0)]
        linger: u32,
    },

    /// Run a standalone relay, or enroll a route with it
    ///
    /// The relay is a separate rendezvous process for reaching a phux
    /// server that cannot accept inbound connections: the server dials
    /// OUT to the relay and registers a tunnel for a named route, remote
    /// consumers dial IN naming that route, and the relay splices the
    /// two as opaque bytes — it never reads what crosses. `run` serves
    /// in the foreground; `pair` enrolls a route name and mints the
    /// token the server's tunnel authenticates with. Relay state (the
    /// route-token store and a self-signed certificate) lives at fixed
    /// paths under the phux state directory.
    #[usage(help_heading = "Machines", display_order = 44)]
    Relay {
        #[usage(subcommand)]
        action: relay::RelayAction,
    },

    /// Mint, rotate, or revoke remote credentials
    ///
    /// With no subcommand, mint one credential into the server's store and
    /// print its stable ID, one-time bearer secret, and certificate fingerprint.
    /// `rotate` replaces the bearer with a bounded overlap; `revoke` denies all
    /// generations on future connections. These operations update the store
    /// directly and take effect without restarting the server.
    ///
    /// This never contacts a running server — it only writes the token file.
    #[usage(help_heading = "Machines", display_order = 43)]
    Pair {
        #[usage(subcommand)]
        action: Option<PairAction>,

        /// Versioned credential store to update. Defaults to `PHUX_WS_TOKENS`.
        #[usage(long, global, value_name = "PATH")]
        tokens: Option<std::path::PathBuf>,

        /// Server certificate PEM, used to print the pairing fingerprint.
        /// Defaults to `PHUX_WS_TLS_CERT`.
        #[usage(long, value_name = "PATH")]
        cert: Option<std::path::PathBuf>,

        /// Also render the pairing payload as a scannable QR code. The QR
        /// encodes the same `https://phux.phall.io/connect` one-tap link
        /// printed as text, so a phone can pair by scanning instead of typing. Needs a server
        /// address: pass `--host`, or let it fall back to a detected overlay
        /// address plus the `PHUX_WS_ADDR` port.
        #[usage(long)]
        qr: bool,

        /// Server address (`host:port`, or a full `ws://`/`wss://` URL) to
        /// embed in the connect link so it is fully self-contained. Omitted:
        /// derived from the detected overlay address and the `PHUX_WS_ADDR`
        /// port when possible; otherwise no link is printed (the device
        /// enters the address itself).
        #[usage(long, value_name = "HOST:PORT")]
        host: Option<String>,

        /// Human-readable server name to embed in the connect link, shown by
        /// the device in its server list. Omitted: the device picks a default.
        #[usage(long, value_name = "NAME")]
        name: Option<String>,

        /// Emit the mint, rotation, or revocation result as JSON on stdout.
        /// `phux host add` consumes the mint document over ssh.
        #[usage(long, global)]
        json: bool,

        /// Explicitly convert legacy anonymous token lines before pairing.
        /// Conversion preserves each bearer secret but stores only its verifier.
        #[usage(long)]
        migrate_legacy: bool,
    },

    /// Manage the mTLS workload authority
    ///
    /// The workload CA and the registry of client credentials it admits over
    /// mutual TLS. `authority` prints the CA fingerprint (`--init` creates
    /// the CA); `add-key` enrolls a client certificate or signs a CSR read
    /// from stdin or `--file`, never from the command line; `list` and
    /// `revoke` show and retire credentials. These write the state directory
    /// directly and never contact a server; a running server applies each
    /// change on its next connection, with no restart.
    #[usage(help_heading = "Machines", display_order = 45)]
    Workload {
        #[usage(subcommand)]
        action: workload::WorkloadAction,

        /// Emit the result as JSON on stdout.
        #[usage(long, global)]
        json: bool,
    },

    /// Add and manage the machines phux reaches
    ///
    /// One namespace over both machine registries. `--role remote` (the
    /// default) manages the servers `phux attach <name>` dials; `--role
    /// satellite` manages the peers a federation hub dials for its users.
    /// The two registries stay separate in config (`[[remote]]` vs
    /// `[[satellites]]`) because they encode opposite trust directions;
    /// this verb absorbs the split into a flag.
    // The successor to the former `remote`, `satellite`, and top-level
    // `enroll` verbs (ADR-0066), removed in v0.12.1 once their deprecation
    // window closed (phux-dpjf).
    #[usage(alias = "machine", help_heading = "Machines", display_order = 40)]
    Host {
        #[usage(subcommand)]
        action: host::HostAction,
    },

    /// Keep a server running across logout and reboot
    ///
    /// Generates this host's native per-user service unit — a `launchd`
    /// `LaunchAgent` on macOS, a systemd user unit on Linux — with the
    /// server's environment baked in, so a rebooted host comes back with a
    /// server instead of waiting for someone to log in and start one.
    /// A restarted server has no terminals: every pane's process died with
    /// the host. `install --restore` brings back session names, layout, and
    /// cwd, not running processes.
    #[usage(help_heading = "Machines", display_order = 42)]
    Service {
        #[usage(subcommand)]
        action: ServiceAction,
    },
    /// Print a shell completion script on stdout.
    // `long_about` for the same reason `rec` spells one out: clap reflows
    // doc-comment paragraphs and the three install commands need real
    // newlines — run together on one line they do copy-paste damage.
    #[usage(
        help = "Print a shell completion script",
        long_help = "Print a shell completion script on stdout.\n\n\
            The script is generated from the binary's own argument parser, so it \
            always matches the verbs this build actually accepts. It contacts no \
            server and reads no config, which is what makes it safe to run from a \
            shell startup file.\n\n\
            Regenerate after upgrading phux; a stale script completes verbs the \
            installed binary no longer has.\n\n\
            Install it the way your shell prefers. Examples:\n  \
            phux completion zsh  > ~/.zfunc/_phux   (~/.zfunc must be on $fpath)\n  \
            phux completion bash > ~/.local/share/bash-completion/completions/phux\n  \
            phux completion fish > ~/.config/fish/completions/phux.fish"
    )]
    #[usage(help_heading = "More", display_order = 70)]
    Completion {
        /// Shell dialect to generate for.
        #[usage(value_enum, value_name = "SHELL")]
        shell: CompletionShell,
    },

    /// Run the bundled MCP stdio adapter
    ///
    /// This is a transparent launcher for the separate MCP companion
    /// binary. All arguments are forwarded unchanged. With no arguments it
    /// serves MCP over stdin/stdout; discovery modes include `--skill`,
    /// `--schema`, `--help`, and `--version`.
    #[usage(help_heading = "More", display_order = 68)]
    Mcp {
        /// Arguments forwarded unchanged to the MCP companion.
        #[usage(value_name = "ARGS", trailing_var_arg, allow_hyphen_values)]
        args: Vec<std::ffi::OsString>,
    },

    /// Print the agent skill this binary ships with, on stdout.
    // `long_about` spelled out for the same reason `completion` spells one
    // out: clap reflows doc-comment paragraphs, and the install one-liners
    // need real newlines or they run together and do copy-paste damage.
    #[usage(
        help = "Print the agent skill this binary ships with",
        long_help = "Print the agent skill this binary ships with, on stdout.\n\n\
            The text is compiled into the executable, so it describes the verbs \
            and flags THIS build actually has — it cannot drift from the binary \
            the way a copied file can. It contacts no server and reads no \
            config.\n\n\
            Give it to any agent that needs to drive phux: it teaches the \
            read-act-wait loop, the selector grammar, the difference between a \
            level read and an observed transition, the exit codes, and the \
            safety rules for driving a terminal a human may also be using.\n\n\
            Scopes are `quick` (core loop and safety), `agent` (lifecycle and \
            identity), `terminal` (screen and input mechanics), and `full` \
            (everything, the default). Examples:\n  \
            phux skill\n  \
            phux skill agent\n  \
            phux --skill=terminal\n  \
            phux --skill=quick | pbcopy"
    )]
    #[usage(help_heading = "More", display_order = 69)]
    Skill {
        /// Amount and subject of guidance to print.
        #[usage(value_enum, default = "full", value_name = "SCOPE")]
        scope: crate::skill::SkillScope,
    },

    /// Diagnose the install: config, socket, server
    ///
    /// Composes the checks that already exist as separate verbs and reports
    /// one verdict, because knowing which four commands to run and how to
    /// read each one is exactly what someone debugging phux does not have.
    ///
    /// Read-only. Exits 1 if any check failed; warnings alone exit 0,
    /// since a stopped server is a normal state and not a broken install.
    #[usage(help_heading = "Maintain", display_order = 51)]
    Doctor {
        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Manage git worktrees and their bound sessions
    ///
    /// Each worktree binds to one session whose name is derived from the
    /// worktree's directory basename. The derivation is a pure function of
    /// the path, so the binding is computed on demand and can never go
    /// stale — phux stores no worktree state and the server knows no git.
    #[usage(help_heading = "More", display_order = 66)]
    Worktree {
        #[usage(subcommand)]
        action: WorktreeAction,
    },

    /// Show where the logs live, or tail one
    ///
    /// Bare `phux logs` prints the inventory: the canonical server log
    /// (every spawn path writes it), the per-pid client logs, and the state
    /// dir that holds them — with existence, size, and age, so a fresh
    /// machine reads "not created yet" instead of an error. `--server`
    /// tails the server log, `--client` the newest client log (`--pid`
    /// picks a specific one), and `--cockpit` the native macOS app's log;
    /// `-f` follows and `-n` sets the tail length. `--json` emits the
    /// inventory as a stable document.
    #[usage(help_heading = "Maintain", display_order = 52)]
    Logs {
        /// Tail the canonical server log.
        #[usage(long, group = "which")]
        server: bool,

        /// Tail the newest per-pid client log (or the one `--pid` names).
        #[usage(long, group = "which")]
        client: bool,

        /// Tail the Phux Cockpit app's log (macOS; `PHUX_COCKPIT_LOG` overrides
        /// the path).
        #[usage(long, group = "which")]
        cockpit: bool,

        /// With --client: the client pid whose log to tail, instead of the
        /// newest.
        #[usage(long, value_name = "PID", requires("--client"))]
        pid: Option<u32>,

        /// Follow the tailed log as it grows (needs --server, --client, or
        /// --cockpit).
        #[usage(short, long)]
        follow: bool,

        /// How many trailing lines to show; 200 when omitted (needs
        /// --server, --client, or --cockpit).
        #[usage(short = 'n', long, value_name = "NUM")]
        lines: Option<u32>,

        /// Emit the path inventory as a stable JSON document instead of
        /// human text. Inventory only — it cannot combine with a tail.
        #[usage(
            long,
            conflicts("--server", "--client", "--cockpit", "--pid", "--follow", "--lines")
        )]
        json: bool,
    },

    /// Capture or list local bug reports
    ///
    /// Bare `phux report` lists bundles under the profile state directory
    /// (newest first; `latest` is printed first). `phux report show [ID]`
    /// prints one `report.md` (omit ID for the newest). `phux report new`
    /// writes a logs-and-version bundle from a shell; prefer the TUI action
    /// `report-bug` (`C-a B`) while attached so the live session, pane, and
    /// screen are included. An agent given a report path can `cat` it or
    /// run `phux report show`.
    #[usage(alias = "bug")]
    #[usage(help_heading = "Maintain", display_order = 53)]
    Report {
        #[usage(subcommand)]
        action: Option<ReportAction>,
        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Regenerate the repository's generated reference pages (internal).
    ///
    /// Hidden developer tooling behind `just docs-gen`, not part of the
    /// user-facing surface: renders the reference pages from this binary's
    /// own inventories and writes them into the checkout. A unit test
    /// byte-compares the checked-in pages against this generator, so the
    /// published reference can never drift from the compiled binary.
    #[usage(name = "gen-reference-docs", hide)]
    GenReferenceDocs {
        /// Directory to write the pages into. Defaults to the checkout's
        /// generated-reference tree; run from the repository root.
        #[usage(long, value_name = "DIR")]
        out: Option<std::path::PathBuf>,
    },
}

/// `phux service <action>` — manage the per-user service unit.
#[derive(Debug, Subcommands)]
pub(crate) enum ServiceAction {
    /// Write the unit and hand it to the init system.
    ///
    /// Idempotent: rerunning reconciles an existing unit, so changing a
    /// listener address is `install` again with the new flag.
    Install {
        /// Accept QUIC clients on this `HOST:PORT`. A routable address
        /// (e.g. `0.0.0.0:8788`) engages TLS and requires a `phux pair`
        /// token. Prefer this over `--listen` where UDP is open.
        // The same `SocketAddr` type as `server --quic`, so a bad address
        // fails at parse time here instead of at the supervised server's
        // first start (phux-i0e8.8.4).
        #[usage(long, value_name = "HOST:PORT")]
        quic: Option<std::net::SocketAddr>,

        /// Accept WebSocket clients on this `HOST:PORT`. The fallback for
        /// networks that block UDP.
        #[usage(long, value_name = "HOST:PORT")]
        listen: Option<String>,

        /// Save the workspace on stop and restore it on start. Off by
        /// default: a session list repopulated with fresh shells is a
        /// surprise unless asked for. Restores names, layout, and cwd —
        /// never running processes.
        #[usage(long)]
        restore: bool,

        /// Run the supervised server as a federation hub. The service loads
        /// enabled `[[satellites]]` entries and keeps their links connected
        /// across login, logout, and reboot.
        #[usage(long)]
        hub: bool,

        /// Never stop a running server to install. When one is live, write
        /// the unit and arm it instead of loading it, so the incumbent keeps
        /// its panes and the supervisor takes over the next time a server
        /// starts.
        ///
        /// Without this, an install over a live server is refused, because
        /// loading the unit would supervise a process that cannot bind the
        /// socket. With it, nothing is stopped and nothing crash-loops. The
        /// running process itself is never adopted: neither launchd nor
        /// systemd can restart-supervise a process it did not start.
        #[usage(long)]
        adopt: bool,

        /// Print the unit (and the restore wrapper) to stdout without
        /// writing or loading anything.
        #[usage(long)]
        print: bool,
    },

    /// Bring an installed unit's restart policy up to date, in place.
    ///
    /// Rewrites only the keys that carry the restart policy and leaves every
    /// other byte of the unit alone, so the listeners, hub mode and socket
    /// baked into it by an earlier `install` are preserved rather than
    /// re-derived. Nothing is stopped and no pane is lost.
    ///
    /// On Linux `systemctl --user daemon-reload` picks the change up without
    /// touching the running service. On macOS launchd cannot re-read a plist
    /// for a loaded job, so the corrected policy takes effect at the next
    /// login or reboot; the command says so rather than claiming otherwise.
    Reconcile {
        /// Print the reconciled unit to stdout without writing anything.
        #[usage(long)]
        print: bool,
    },

    /// Unload the unit and remove what `install` wrote.
    Uninstall,

    /// Report whether a unit is installed and running.
    Status,

    /// Show the supervised server's log.
    Logs {
        /// Follow the log as it grows.
        #[usage(short = 'f', long)]
        follow: bool,

        /// How many trailing lines to show.
        #[usage(short = 'n', long, default = "200", default_value_t = 200)]
        lines: u32,
    },

    /// Delete the accumulated per-pid `client-*.log` files.
    #[usage(name = "prune-logs")]
    PruneLogs {
        /// Report how many would be removed, and remove nothing.
        #[usage(long)]
        dry_run: bool,
    },
}

/// `phux worktree <action>` — git worktrees bound to sessions by name.
#[derive(Debug, Subcommands)]
pub(crate) enum WorktreeAction {
    /// List the repository's worktrees and their bound sessions.
    ///
    /// The `bound` column reads `live` when a session by the derived name
    /// exists, `-` when it does not, and `?` when no server is running —
    /// "no server" and "no session" are different facts.
    #[usage(alias = "ls")]
    List {
        /// Path inside the repository or worktree to list from.
        #[usage(default = ".")]
        path: std::path::PathBuf,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Create a worktree and a session rooted in it.
    ///
    /// An existing local branch is checked out; a missing one is created,
    /// from `--from` when given and from the current HEAD otherwise. The
    /// worktree lands beside the repository as `<repo>-<branch>` unless
    /// `--path` says otherwise.
    New {
        /// Branch to check out, or to create when it does not exist.
        branch: String,

        /// Where to put the worktree. Defaults to a sibling of the repo.
        #[usage(long, value_name = "PATH")]
        path: Option<std::path::PathBuf>,

        /// Start point for a newly created branch (default: current HEAD).
        #[usage(long, value_name = "REF")]
        from: Option<String>,

        /// Session name, overriding the name derived from the path.
        #[usage(long, short = 's', value_name = "NAME")]
        session: Option<String>,

        /// Path inside the repository the worktree belongs to.
        #[usage(long, default = ".", value_name = "PATH")]
        repo: std::path::PathBuf,

        /// Attach to the new session instead of creating it headlessly.
        #[usage(long)]
        attach: bool,

        /// Emit a stable JSON document — branch, path, session, and the seed
        /// pane's `terminal_id` — instead of human text. This is the first
        /// call in a fan-out script, and the id it returns is the pane the
        /// caller then sends its first prompt to. Cannot combine with
        /// `--attach`: an attached session owns stdout.
        #[usage(long, conflicts("--attach"))]
        json: bool,

        /// Command to run in the new session instead of the default shell.
        #[usage(trailing_var_arg)]
        command: Vec<String>,
    },

    /// Open the session bound to an existing worktree, creating it if absent.
    ///
    /// Idempotent: an already-live session is reported and left alone, so
    /// scripts and keybindings can call this without checking first.
    Open {
        /// Worktree path, branch, or derived session name.
        target: String,

        /// Path inside the repository the worktree belongs to.
        #[usage(long, default = ".", value_name = "PATH")]
        repo: std::path::PathBuf,

        /// Attach to the session instead of only reporting its name.
        #[usage(long)]
        attach: bool,

        /// Emit the same document `worktree new --json` emits, whether the
        /// session was created now or was already live — so a script that
        /// re-enters a fleet gets the seed pane without special-casing.
        #[usage(long, conflicts("--attach"))]
        json: bool,
    },

    /// Remove a worktree, killing the session bound to it first.
    ///
    /// The session is killed before git runs, because git refuses to remove
    /// a worktree whose files are held open and a shell sitting in that
    /// directory holds it open. Refuses the worktree you are standing in.
    #[usage(alias = "rm")]
    Remove {
        /// Worktree path, branch, or derived session name.
        target: String,

        /// Pass --force to git, removing a worktree with local changes.
        #[usage(long)]
        force: bool,

        /// Path inside the repository the worktree belongs to.
        #[usage(long, default = ".", value_name = "PATH")]
        repo: std::path::PathBuf,

        /// Emit a stable JSON document instead of human text. A fan-out
        /// teardown script has the same parsing problem creation does.
        #[usage(long)]
        json: bool,
    },
}

/// `phux tag <action>` — list and edit a Terminal's L3 tags.
///
/// Alias policy (ADR-0065 §5): every list/remove registry verb answers to
/// both spellings. This registry's canonical names were the short ones, so
/// the aliases here are the long forms.
#[derive(Debug, Subcommands)]
pub(crate) enum TagAction {
    /// List the tags on each pane a selector resolves to.
    #[usage(alias = "list")]
    Ls {
        /// Target selector (session, `session:window`, `@id`, `.`, `#tag`).
        target: String,

        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Add one or more tags to each pane a selector resolves to.
    Add {
        /// Target selector.
        target: String,
        /// Tags to add (the leading `#` is optional).
        #[usage(required)]
        tags: Vec<String>,

        #[usage(flatten)]
        json: JsonOpt,
    },

    /// Remove one or more tags from each pane a selector resolves to.
    #[usage(alias = "remove")]
    Rm {
        /// Target selector.
        target: String,
        /// Tags to remove (the leading `#` is optional).
        #[usage(required)]
        tags: Vec<String>,

        #[usage(flatten)]
        json: JsonOpt,
    },
}

/// `phux plugin <action>` — local plugin registry lifecycle.
#[derive(Debug, Subcommands)]
pub(crate) enum PluginAction {
    /// List configured plugin manifests.
    #[usage(alias = "ls")]
    List {
        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Add or update a manifest entry in `config.toml`.
    Link {
        /// Path to a `phux-plugin.toml` file, or a directory containing one.
        manifest: std::path::PathBuf,

        /// Register the plugin but leave it disabled.
        #[usage(long)]
        disabled: bool,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Fetch, build, validate, and link a plugin package.
    ///
    /// REF is a git URL (`https://…`, `git@…`, `file://…` — cloned with
    /// the system `git`), a local plugin directory (copied), or a local
    /// tarball (`.tar`, `.tar.gz`, `.tgz` — extracted with the system
    /// `tar`). The package lands under the managed plugins directory
    /// (`$XDG_DATA_HOME/phux/plugins`, else `~/.local/share/phux/plugins`),
    /// its manifest `[[build]]` steps for this platform run with a bounded
    /// timeout and captured output, the manifest is validated (including
    /// the `min_phux_version` gate), and the result is linked into
    /// `config.toml` like `phux plugin link`. Provenance (ref, branch,
    /// resolved commit) is recorded in the managed directory's
    /// `plugins.lock` so `phux plugin update` can re-fetch it later.
    Install {
        /// Git URL, local plugin directory, or local tarball path.
        #[usage(value_name = "REF")]
        reference: String,

        /// Branch or tag to clone (git sources only).
        #[usage(long, value_name = "REV")]
        rev: Option<String>,

        /// Install and link the plugin but leave it disabled.
        #[usage(long)]
        disabled: bool,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Re-fetch, rebuild, and revalidate installed plugins.
    ///
    /// Reads the managed directory's `plugins.lock`, re-fetches each
    /// recorded source (all of them, or just NAME), reruns its `[[build]]`
    /// steps, revalidates the manifest, swaps the managed copy, and
    /// records the new resolved commit. `config.toml` is untouched — the
    /// linked manifest path does not move.
    Update {
        /// Plugin id to update. Omit to update every installed plugin.
        name: Option<String>,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Remove a configured plugin by id.
    // `rm` / `remove` are the alias-policy spellings (ADR-0065 §5): every
    // remove-shaped registry verb answers to both, and this registry's
    // canonical name predates the policy. A code comment, not a doc comment —
    // ADR ids must not leak into `--help` (see `help_inventory`).
    #[usage(visible_aliases = ["rm", "remove"])]
    Unlink {
        /// Plugin id from its manifest.
        id: String,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Enable a configured plugin by id.
    Enable {
        /// Plugin id from its manifest.
        id: String,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Disable a configured plugin by id.
    Disable {
        /// Plugin id from its manifest.
        id: String,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Validate one manifest, or every configured manifest when omitted.
    Validate {
        /// Optional path to a `phux-plugin.toml` file or plugin directory.
        manifest: Option<std::path::PathBuf>,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },
}

/// `phux workspace <action>` — workspace inspection and session archives.
#[derive(Debug, Subcommands)]
pub(crate) enum WorkspaceAction {
    /// Inspect the git repository and its checked-out worktrees.
    Inspect {
        /// Path inside the repository or worktree to inspect.
        #[usage(default = ".")]
        path: std::path::PathBuf,

        /// Emit a stable JSON document instead of human text.
        #[usage(long)]
        json: bool,
    },

    /// Save the running phux workspace as a JSON archive.
    Save {
        /// Write the archive to a path instead of stdout.
        #[usage(long, short = 'o', value_name = "PATH")]
        output: Option<std::path::PathBuf>,
    },

    /// Restore missing sessions from a workspace archive.
    Restore {
        /// JSON archive path, or '-' to read from stdin.
        archive: std::path::PathBuf,
    },
}

/// Fail fast when `socket_path` cannot fit in a `sockaddr_un` (phux-iwuc).
///
/// A too-long path can never bind or connect, so naming the platform's
/// UDS path-length limit here beats the downstream misdirection (a 2s
/// auto-spawn timeout, or a raw "path must be shorter than `SUN_LEN`").
/// Prints the diagnostic and returns the failure exit code to bubble.
pub(crate) fn ensure_socket_path_fits(socket_path: &Path) -> Result<(), ExitCode> {
    phux_server::runtime::validate_socket_path_len(socket_path).map_err(|err| {
        eprintln!("phux: {err}");
        ExitCode::FAILURE
    })
}

/// Build a current-thread tokio runtime, or print why and return the
/// failure exit code.
pub(crate) fn cli_runtime() -> Result<tokio::runtime::Runtime, ExitCode> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| {
            eprintln!("failed to build runtime: {err}");
            ExitCode::FAILURE
        })
}

/// Send one command over `conn` and return the matching `COMMAND_RESULT`,
/// reporting anything the server interleaved ahead of it (SPEC §5).
///
/// The CLI is a request/response consumer: it opens a connection, runs a
/// verb, and exits without attaching or subscribing, so the only frames that
/// can precede an ack here are the ones a handler emits itself. In practice
/// that is the hub's federation degradation notice — one uncorrelated `ERROR`
/// per unreachable satellite, pushed ahead of the merged `GET_STATE` snapshot
/// by `handle_get_state_federated` precisely so the caller can say the view is
/// partial. The loop this replaced discarded them, which turned every
/// federated CLI verb into a confident report of an incomplete fleet.
///
/// Unlike the library paths, this one owns stderr, so it prints rather than
/// logging into a `tracing` subscriber a CLI user has not installed. Any
/// *other* interleaved frame is dropped: on a connection that never attached
/// or subscribed there is no consumer for a `RESOURCE_OUTPUT` or an `EVENT`,
/// and no verb here can act on one.
pub(crate) async fn command_on(
    conn: &mut Connection,
    request_id: u32,
    command: WireCommand,
) -> Result<CommandResult, AttachError> {
    let (result, interleaved) = conn.request(request_id, command).await?.into_parts();
    warn_interleaved_degradation(&phux_client::state::Degradation::from_interleaved(
        &interleaved,
    ));
    Ok(result)
}

/// Print `degradation`'s notices exactly as [`command_on`] always has.
///
/// A `phux-client` function that returns its own `Degradation` (the
/// `kill`/`detach`/`spawn`/`tags` library homes) uses this instead of
/// `command_on`'s inline loop, so every verb prints the identical line for
/// an uncorrelated `ERROR` interleaved ahead of its reply — a hub's
/// per-satellite unreachability notice — regardless of which path fetched
/// it.
pub(crate) fn warn_interleaved_degradation(degradation: &phux_client::state::Degradation) {
    for message in degradation.notices() {
        eprintln!("phux: warning: partial results — {message}");
    }
}

/// One-shot: open a fresh connection, send `command`, return its result.
pub(crate) async fn request_command(
    socket_path: &Path,
    command: WireCommand,
) -> Result<CommandResult, AttachError> {
    let mut conn = Connection::connect(socket_path).await?;
    command_on(&mut conn, 1, command).await
}

/// Print a "no server" diagnostic for a connect-time error, or a generic
/// one otherwise. Returns the failure exit code for the caller to bubble.
///
/// Every arm ends with its remedy (phux-i0e8.7.3): the no-server arm names
/// the exact start commands, and all arms name the canonical server log and
/// `phux doctor` — the two places the *reason* lives when the sentence here
/// is not enough.
pub(crate) fn report_no_server(err: &AttachError, socket_path: &Path, verb: &str) -> ExitCode {
    for line in no_server_lines(
        err,
        socket_path,
        verb,
        &phux_server::telemetry::server_log_path(),
    ) {
        eprintln!("{line}");
    }
    ExitCode::FAILURE
}

/// The lines [`report_no_server`] prints, pure so tests can pin every arm
/// without capturing stderr (the `session_lines` pattern in `ls.rs`).
///
/// Continuation lines are indented two spaces so the remedy block reads as
/// one diagnostic, not four independent errors.
fn no_server_lines(
    err: &AttachError,
    socket_path: &Path,
    verb: &str,
    server_log: &Path,
) -> Vec<String> {
    let mut lines = match err {
        AttachError::Io(io_err)
            if matches!(
                io_err.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound,
            ) =>
        {
            vec![
                format!("phux: no server running at {}", socket_path.display()),
                "  start one with `phux` (attaches, auto-starting a server) or `phux server`"
                    .to_owned(),
            ]
        }
        AttachError::Disconnected => {
            vec![format!("phux: server closed the connection during {verb}")]
        }
        other => vec![format!("phux: {verb} failed: {other}")],
    };
    lines.push(format!("  server log: {}", server_log.display()));
    lines.push("  run `phux doctor` for a health check".to_owned());
    lines
}

/// Parse an optional target string into a [`crate::selector::Selector`],
/// defaulting to the focused session when absent. On a parse error,
/// prints a diagnostic and returns the failure exit code for the caller to
/// bubble.
pub(crate) fn parse_selector(session: Option<&str>) -> Result<crate::selector::Selector, ExitCode> {
    session.map_or(Ok(crate::selector::Selector::Current), |target| {
        crate::selector::parse(target).map_err(|err| {
            eprintln!("phux: invalid target '{target}': {err}");
            ExitCode::FAILURE
        })
    })
}

/// Resolve `selector` to a single pane against a fresh `GET_STATE`
/// snapshot. Prefers the focused pane when the selector spans several
/// (e.g. a whole session); otherwise the first in snapshot order. Prints
/// diagnostics and returns the failure exit code on no-server / miss.
///
/// This is the shared front door for every verb that addresses one pane
/// (`snapshot`, `send-keys`, `paste`, `run`, `wait`, `watch`, `resize`,
/// `signal`, `rec`, `ask`), so it is also where the partial-fleet distinction
/// is drawn for all of them: a miss against a hub that could not reach a
/// satellite is reported as unresolvable, never as absent.
///
/// It uses [`partial::report_target_miss_keeping_status`] rather than the
/// distinct exit status `kill`/`tag`/`agent` return, because two of the verbs
/// behind this door have already spent their status space — `run` mirrors the
/// child's own exit code and `wait` owns `124`. A shared resolver cannot hand
/// out a code that means one thing for `kill` and collides for `run`, so the
/// distinction stays in the sentence, which is where the user reads it.
///
/// `json` selects the failure channel per the JSON error contract
/// ([`json_err`], phux-i0e8.8.2): verbs without a `--json` flag pass `false`
/// and keep the historical prose.
pub(crate) async fn resolve_target(
    socket_path: &Path,
    selector: &crate::selector::Selector,
    verb: &str,
    json: bool,
) -> Result<phux_protocol::ids::ResourceId, ExitCode> {
    resolve_target_with(socket_path, selector, verb, json, false).await
}

/// [`resolve_target`] for the verbs that deliver input into the pane
/// (`send-keys`, `paste`, `run`, `signal`, and the acknowledged agent
/// writes): identical, except that a `%name` whose record has the withdrawn
/// shape is refused (ADR-0075 point 5) rather than resolved.
pub(crate) async fn resolve_target_for_input(
    socket_path: &Path,
    selector: &crate::selector::Selector,
    verb: &str,
    json: bool,
) -> Result<phux_protocol::ids::ResourceId, ExitCode> {
    resolve_target_with(socket_path, selector, verb, json, true).await
}

async fn resolve_target_with(
    socket_path: &Path,
    selector: &crate::selector::Selector,
    verb: &str,
    json: bool,
    for_input: bool,
) -> Result<phux_protocol::ids::ResourceId, ExitCode> {
    let (snapshot, degradation) = phux_client::state::get_state(socket_path)
        .await
        .map_err(|err| json_err::report_no_server(json, &err, socket_path, verb))?
        .into_parts();
    // `%name` is singular: it resolves to exactly one agent or refuses, and
    // never travels the set-valued path below where `pick_target_pane` would
    // narrow it (ADR-0075 point 3). A Terminal-facet verb acts on the named
    // agent's pane; the session verbs resolve their own side.
    if let crate::selector::Selector::Agent(name) = selector {
        let target =
            phux_client::state::resolve_agent_target(socket_path, name, &snapshot, for_input)
                .await
                .map_err(|err| report_agent_resolve_error(json, &err, true))?;
        partial::warn_partial_view(verb, &degradation);
        return Ok(target.terminal);
    }
    let candidates = resolve_targets(socket_path, selector, &snapshot).await;
    let picked = crate::selector::pick_target_pane(&candidates, &snapshot.focused_resource)
        .ok_or_else(|| partial::report_target_miss_keeping_status_for(json, None, &degradation))?;
    // A hit is still worth a word: the pane we picked is the best of what a
    // partial fleet offered, and the user is about to act on it.
    partial::warn_partial_view(verb, &degradation);
    Ok(picked)
}

/// Report a `%name` refusal on the shared error contract and return its
/// exit status.
///
/// Every variant is a refusal to guess (ADR-0075 point 3), so each lands on
/// a distinct code: a miss is `no_such_target` (1); two records or two
/// sessions sharing the name are `selector_not_single` (2); a kind constant
/// is `invalid_agent_name` (2); a withdrawn record on an input verb is
/// `agent_withdrawn` (2); an index that did not finish is `partial_view` —
/// exit 3, or 1 when `keep_status` for the verbs whose status is already
/// spoken for.
pub(crate) fn report_agent_resolve_error(
    json: bool,
    err: &phux_client::selector::AgentResolveError,
    keep_status: bool,
) -> ExitCode {
    use phux_client::selector::AgentResolveError;
    let (code, exit_code, remedy) = match err {
        AgentResolveError::Unknown { .. } => (
            json_err::codes::NO_SUCH_TARGET,
            crate::exit_codes::EXIT_FAILURE,
            "`phux agent list` shows every declared name; set one with `phux agent set \
             TARGET --name <name>`",
        ),
        AgentResolveError::Ambiguous { .. } | AgentResolveError::AmbiguousSession { .. } => (
            json_err::codes::SELECTOR_NOT_SINGLE,
            crate::exit_codes::EXIT_USAGE,
            "address one candidate directly by @N",
        ),
        AgentResolveError::KindConstant { .. } => (
            json_err::codes::INVALID_AGENT_NAME,
            crate::exit_codes::EXIT_USAGE,
            "name one pane with `phux agent set @N --name <name>` and address that",
        ),
        AgentResolveError::Withdrawn { .. } => (
            json_err::codes::AGENT_WITHDRAWN,
            crate::exit_codes::EXIT_USAGE,
            "inspect the pane with `phux agent explain`; address it by @N to write anyway",
        ),
        AgentResolveError::PartialIndex { .. } => (
            json_err::codes::PARTIAL_VIEW,
            if keep_status {
                crate::exit_codes::EXIT_FAILURE
            } else {
                crate::exit_codes::EXIT_PARTIAL_VIEW
            },
            "retry once the fleet is whole, or address the pane by @N",
        ),
    };
    json_err::emit(
        json,
        &json_err::CliError::new(code, err.to_string(), remedy),
        exit_code,
    )
}

/// Resolve `selector` to its `ResourceId`s, fetching L3 tag metadata first
/// only when the selector is `#tag` (`phux-f8wi`). Non-tag selectors resolve
/// purely against `snapshot`, so the common path pays no extra round-trip.
///
/// A tag fetch that fails (no server mid-flight, a malformed value) degrades
/// to an empty tag index, so a `#tag` selector then resolves to nothing —
/// the caller reports it as a selector miss, never a hang.
pub(crate) async fn resolve_targets(
    socket_path: &Path,
    selector: &crate::selector::Selector,
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
) -> Vec<phux_protocol::ids::ResourceId> {
    phux_client::state::resolve_targets(socket_path, selector, snapshot).await
}

/// Print an `AttachError` as a one-line, actionable message on stderr.
///
/// `phux-roz` (5): the previous output was `attach failed: connection
/// refused` — accurate but useless. The new shape names the socket and
/// suggests the exact `phux server --session …` invocation, so the
/// user can copy-paste their way out of the failure mode.
pub(crate) fn print_attach_error(err: &AttachError, socket_path: &Path, session: &str) {
    for line in attach_error_lines(
        err,
        socket_path,
        session,
        &phux_server::telemetry::default_client_log_path(),
    ) {
        eprintln!("{line}");
    }
}

/// The lines [`print_attach_error`] prints, pure so tests can pin every arm.
///
/// The first three arms are self-explaining (each names its own remedy or
/// cause), so they stay single-line. `Disconnected` gets its own arm
/// (phux-i0e8.2.3): the server vanished mid-session, so the remedy is its
/// log and doctor, not this client's. The fallthrough —
/// `Protocol`/`Terminal`/`Ghostty`/… — is where the sentence alone was a
/// dead end (phux-i0e8.7.3): those failures leave their reason in this
/// client's own log, and a `Protocol` error in particular usually means the
/// binaries disagree, so the remedy block names the client log, `phux
/// doctor`, and this client's protocol triple for the comparison.
fn attach_error_lines(
    err: &AttachError,
    socket_path: &Path,
    session: &str,
    client_log: &Path,
) -> Vec<String> {
    match err {
        AttachError::Io(io_err)
            if matches!(
                io_err.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound,
            ) =>
        {
            vec![format!(
                "phux: no server at {}. Start one with: phux server --session {session}",
                socket_path.display()
            )]
        }
        AttachError::Refused(message) => {
            vec![format!("phux: server refused attach: {message}")]
        }
        AttachError::NotATty => {
            vec!["phux: attach requires an interactive terminal (stdin is not a TTY).".to_owned()]
        }
        // phux-i0e8.2.3: a dedicated arm for the mid-session disconnect that
        // reaches here WITHOUT the reconnect window (e.g. `phux new`'s
        // attach tail; `attach_with_reconnect` reports its own failures and
        // its call sites skip this printer for `Disconnected`). The server
        // went away, so the reason lives in its log; name it and doctor
        // instead of the old dead-end "attach failed: connection closed by
        // server before DETACHED".
        AttachError::Disconnected => vec![
            "phux: the server closed the connection unexpectedly".to_owned(),
            format!(
                "  server log: {}",
                phux_server::telemetry::server_log_path().display()
            ),
            "  run `phux doctor` for a health check".to_owned(),
        ],
        other => {
            let version = phux_protocol::PROTOCOL_VERSION;
            vec![
                format!("phux: attach failed: {other}"),
                format!("  client log: {}", client_log.display()),
                format!(
                    "  run `phux doctor` for a health check (client protocol {}.{}.{})",
                    version.major, version.minor, version.patch,
                ),
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use phux_client::attach::AttachError;

    use crate::commands::{attach_error_lines, no_server_lines, parse_selector};
    use crate::selector::{Selector, WindowRef};

    fn refused_io() -> AttachError {
        AttachError::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused))
    }

    /// phux-i0e8.7.3: the no-server arm must name the exact start commands,
    /// and every arm must end with the server log and the doctor pointer —
    /// an error that does not name its remedy fails the `print_attach_error`
    /// bar this helper used to miss.
    #[test]
    fn no_server_lines_name_start_commands_log_and_doctor() {
        let socket = Path::new("/tmp/phux-test.sock");
        let log = Path::new("/state/phux/server.log");

        let lines = no_server_lines(&refused_io(), socket, "ls", log);
        assert_eq!(lines[0], "phux: no server running at /tmp/phux-test.sock");
        assert_eq!(
            lines[1],
            "  start one with `phux` (attaches, auto-starting a server) or `phux server`"
        );
        assert_eq!(lines[2], "  server log: /state/phux/server.log");
        assert_eq!(lines[3], "  run `phux doctor` for a health check");
        assert_eq!(lines.len(), 4);
    }

    /// A mid-command disconnect is not "no server": the server was there and
    /// went away, so there is no start command to suggest — but the reason it
    /// went away lives in its log, so that and doctor still close the arm.
    #[test]
    fn no_server_disconnect_arm_names_log_and_doctor() {
        let lines = no_server_lines(
            &AttachError::Disconnected,
            Path::new("/tmp/s.sock"),
            "kill",
            Path::new("/state/phux/server.log"),
        );
        assert_eq!(lines[0], "phux: server closed the connection during kill");
        assert_eq!(lines[1], "  server log: /state/phux/server.log");
        assert_eq!(lines[2], "  run `phux doctor` for a health check");
    }

    /// The generic arm keeps the error's own Display sentence first and still
    /// ends with the remedy block.
    #[test]
    fn no_server_fallthrough_keeps_the_error_and_adds_remedies() {
        let lines = no_server_lines(
            &AttachError::Refused("policy said no".to_owned()),
            Path::new("/tmp/s.sock"),
            "tag",
            Path::new("/log/server.log"),
        );
        assert_eq!(
            lines[0],
            "phux: tag failed: server refused attach: policy said no"
        );
        assert_eq!(lines[1], "  server log: /log/server.log");
        assert_eq!(lines[2], "  run `phux doctor` for a health check");
    }

    /// The three self-explaining attach arms stay single-line and keep their
    /// established sentences (`phux-roz`: the no-server one is copy-pasteable).
    #[test]
    fn attach_error_named_arms_stay_single_line() {
        let socket = Path::new("/tmp/a.sock");
        let log = Path::new("/state/phux/client-42.log");

        assert_eq!(
            attach_error_lines(&refused_io(), socket, "main", log),
            ["phux: no server at /tmp/a.sock. Start one with: phux server --session main"]
        );
        assert_eq!(
            attach_error_lines(
                &AttachError::Refused("no such session".to_owned()),
                socket,
                "main",
                log,
            ),
            ["phux: server refused attach: no such session"]
        );
        assert_eq!(
            attach_error_lines(&AttachError::NotATty, socket, "main", log),
            ["phux: attach requires an interactive terminal (stdin is not a TTY)."]
        );
    }

    /// phux-i0e8.2.3: a mid-session disconnect that reaches the printer
    /// without the reconnect window (e.g. `phux new`'s attach tail) names
    /// its cause, the SERVER log (the reason the server went away lives
    /// there, not in this client's log), and the doctor remedy.
    #[test]
    fn attach_error_disconnected_arm_names_the_remedy() {
        let lines = attach_error_lines(
            &AttachError::Disconnected,
            Path::new("/tmp/a.sock"),
            "main",
            Path::new("/state/phux/client-42.log"),
        );
        assert_eq!(
            lines[0],
            "phux: the server closed the connection unexpectedly"
        );
        assert_eq!(
            lines[1],
            format!(
                "  server log: {}",
                phux_server::telemetry::server_log_path().display()
            )
        );
        assert_eq!(lines[2], "  run `phux doctor` for a health check");
        assert_eq!(lines.len(), 3);
    }

    /// phux-i0e8.7.3: the fallthrough (`Protocol`/`Terminal`/…)
    /// used to end at "attach failed: {err}" with nowhere to go. It must now
    /// name this client's own log, doctor, and the client protocol triple —
    /// a `Protocol` error usually means the binaries disagree, and doctor
    /// prints both sides.
    #[test]
    fn attach_error_fallthrough_names_client_log_doctor_and_triple() {
        let lines = attach_error_lines(
            &AttachError::Protocol("bad frame".to_owned()),
            Path::new("/tmp/a.sock"),
            "main",
            Path::new("/state/phux/client-42.log"),
        );
        assert_eq!(lines[0], "phux: attach failed: protocol error: bad frame");
        assert_eq!(lines[1], "  client log: /state/phux/client-42.log");
        let version = phux_protocol::PROTOCOL_VERSION;
        assert_eq!(
            lines[2],
            format!(
                "  run `phux doctor` for a health check (client protocol {}.{}.{})",
                version.major, version.minor, version.patch,
            )
        );
        assert_eq!(lines.len(), 3);
    }

    /// The full `TARGET` grammar now feeds run/send-keys/snapshot/wait/kill
    /// alike (phux-n95). `parse_selector` is the shared CLI front door:
    /// `None` defaults to the focused session, and every documented
    /// form parses to its [`Selector`] variant.
    #[test]
    fn parse_selector_accepts_every_grammar_form() {
        // Absent target defaults to the focused session. Headless callers
        // have no client-local MRU, so `=` is an explicit error.
        assert_eq!(parse_selector(None).unwrap(), Selector::Current);
        assert_eq!(parse_selector(Some(".")).unwrap(), Selector::Current);
        assert!(parse_selector(Some("=")).is_err());
        assert_eq!(
            parse_selector(Some("work")).unwrap(),
            Selector::Session("work".to_owned()),
        );
        assert_eq!(
            parse_selector(Some("work:1")).unwrap(),
            Selector::Window("work".to_owned(), WindowRef::Index(1)),
        );
        assert_eq!(
            parse_selector(Some("work:editor")).unwrap(),
            Selector::Window("work".to_owned(), WindowRef::Tag("editor".to_owned())),
        );
        assert_eq!(
            parse_selector(Some("work:1.2")).unwrap(),
            Selector::Pane("work".to_owned(), WindowRef::Index(1), 2),
        );
        assert_eq!(
            parse_selector(Some("work:editor.0")).unwrap(),
            Selector::Pane("work".to_owned(), WindowRef::Tag("editor".to_owned()), 0),
        );
        assert_eq!(
            parse_selector(Some("@42")).unwrap(),
            Selector::ResourceId(42),
        );
        assert_eq!(
            parse_selector(Some("devbox/@42")).unwrap(),
            Selector::SatelliteResourceId {
                host: "devbox".to_owned(),
                id: 42,
            },
        );
        // ADR-0075 reserves `%name` for singular agent-name resolution. No
        // shipped command calls that resolver yet, so it currently fails
        // closed through the set-valued path.
        assert_eq!(
            parse_selector(Some("%build")).unwrap(),
            Selector::Agent("build".to_owned()),
        );
    }

    /// Malformed targets fail at parse time with the CLI failure code,
    /// before any server round trip (so run/send-keys reject bad syntax up
    /// front rather than resolving it). A nonexistent-but-well-formed target
    /// parses fine here; it fails later as a resolution miss.
    #[test]
    fn parse_selector_rejects_malformed_targets() {
        // Explicit empty string is a parse error (distinct from `None`).
        assert!(parse_selector(Some("")).is_err());
        // `@N` with a non-numeric id.
        assert!(parse_selector(Some("@nope")).is_err());
        // Pane index after the `.` must be numeric.
        assert!(parse_selector(Some("work:1.x")).is_err());
        // A well-formed but unknown session is NOT a parse error — it
        // resolves to nothing later.
        assert_eq!(
            parse_selector(Some("ghost")).unwrap(),
            Selector::Session("ghost".to_owned()),
        );
        // ADR-0075 point 4: the addressable agent grammar is
        // `^[a-z][a-z0-9_-]{0,31}$`, checked here so a typo costs no round
        // trip. A bare `%` is a parse error, as a bare `#` is.
        assert!(parse_selector(Some("%")).is_err());
        assert!(parse_selector(Some("%Build")).is_err());
        assert!(parse_selector(Some("%my agent")).is_err());
        // But an addressable name that no pane currently carries is NOT a
        // parse error — it refuses later, as a selector miss.
        assert_eq!(
            parse_selector(Some("%ghost")).unwrap(),
            Selector::Agent("ghost".to_owned()),
        );
    }
}
