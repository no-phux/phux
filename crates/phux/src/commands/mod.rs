use std::path::Path;
use std::process::ExitCode;

use clap::{Args, Subcommand, ValueEnum};
use phux_client::attach::AttachError;
use phux_client::attach::connection::Connection;
use phux_protocol::wire::frame::{Command as WireCommand, CommandResult, TerminalSignal};

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
    #[value(alias = "h")]
    Horizontal,
    #[value(alias = "v")]
    Vertical,
}

/// Output format for a recording, shared by `--rec-format` on the attach
/// path and `--format` on the `rec` verb.
///
/// `Apng` covers both the `.png` and `.apng` extensions: a recording is an
/// animation, and this surface never produces a still frame.
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
/// Shared through `#[command(flatten)]` rather than a `global = true` arg on
/// the root: a global would parse — and advertise itself in `--help` — on
/// every verb, including the headless ones that can never tee a composited
/// frame. Scoping it here makes `phux ls --help` honest by construction
/// instead of by a runtime rejection. Headless capture is `phux rec`.
#[derive(Debug, Args)]
pub(crate) struct RecOpts {
    /// Record this session while it runs and write the result to PATH.
    // Written out as `long_help` because clap reflows doc-comment paragraphs
    // into one run-on line; the example block only survives with real
    // newlines.
    #[arg(
        long = "rec",
        value_name = "PATH",
        long_help = "Record this session while it runs and write the result to PATH.\n\n\
            The format follows the extension (.cast, .gif, .png, .apng); pass\n\
            --rec-format to override. A path with no extension gets `.gif`.\n\n\
            Examples:\n  \
            phux --rec demo.gif\n  \
            phux attach work --rec demo.cast"
    )]
    pub(crate) rec: Option<std::path::PathBuf>,

    /// Output format for --rec, overriding the extension.
    #[arg(long = "rec-format", value_enum, value_name = "FMT", requires = "rec")]
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
    #[arg(long)]
    pub(crate) json: bool,
}

/// Validates a split ratio as finite and strictly between zero and one.
fn parse_spawn_ratio(value: &str) -> Result<f32, String> {
    let ratio: f32 = value
        .parse()
        .map_err(|_| "ratio must be a number".to_owned())?;
    if ratio.is_finite() && ratio > 0.0 && ratio < 1.0 {
        Ok(ratio)
    } else {
        Err("ratio must be finite and strictly between 0 and 1".to_owned())
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
pub(crate) mod new;
pub(crate) mod pair;
pub(crate) mod partial;
pub(crate) mod paste;
pub(crate) mod play;
pub(crate) mod plugin;
pub(crate) mod rec;
pub(crate) mod relay;
pub(crate) mod remote;
pub(crate) mod rename;
pub(crate) mod resize;
pub(crate) mod run;
pub(crate) mod satellite;
pub(crate) mod send_keys;
pub(crate) mod server;
pub(crate) mod service;
pub(crate) mod snapshot;
pub(crate) mod spatial;
pub(crate) mod spawn;
pub(crate) mod status;
pub(crate) mod stdio_bridge;
pub(crate) mod supervise;
pub(crate) mod tag;
pub(crate) mod toml_registry;
pub(crate) mod update;
pub(crate) mod upgrade;
pub(crate) mod wait;
pub(crate) mod watch;
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
/// `--socket` is declared once, `global = true`, on the root `Cli`
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
            ServiceAction::Uninstall => Some("service uninstall"),
            ServiceAction::Status => Some("service status"),
            ServiceAction::Logs { .. } => Some("service logs"),
            ServiceAction::PruneLogs { .. } => Some("service prune-logs"),
        },
        Command::Plugin { .. } => Some("plugin"),
        Command::Host { .. } => Some("host"),
        Command::Relay { .. } => Some("relay"),
        Command::Pair { .. } => Some("pair"),
        Command::Completion { .. } => Some("completion"),
        Command::Skill {} => Some("skill"),
        Command::Logs { .. } => Some("logs"),
        Command::GenReferenceDocs { .. } => Some("gen-reference-docs"),
        _ => None,
    }
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Attach to a session (interactive).
    ///
    /// With no name, attaches to the most-recently-focused session,
    /// auto-spawning a server if none is running. Requires a TTY.
    ///
    /// A name enrolled in the host registry (`phux host enroll`, `phux
    /// host add`) shadows a local session of the same name: `phux attach
    /// NAME` dials the registered host instead of the local socket.
    /// Pass `--socket` to force the local reading of the name.
    #[command(group = clap::ArgGroup::new("remote").args(["quic", "ws"]).multiple(false))]
    #[command(visible_alias = "a")]
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
        #[arg(long, value_name = "HOST:PORT")]
        quic: Option<String>,

        /// Attach over WebSocket to a `phux server --listen` endpoint. Use
        /// `ws://HOST:PORT` for loopback dev, or `wss://HOST:PORT` with
        /// `--token` and `--cert-fingerprint` for routable remote attach. This
        /// is the TCP fallback when UDP/QUIC is blocked.
        #[arg(long, value_name = "URL")]
        ws: Option<String>,

        /// Bearer pairing token (hex) for an authenticated QUIC listener, as
        /// minted by `phux pair`. QUIC sends it as the stream's opening
        /// preamble; WebSocket sends it as `Authorization: Bearer`.
        /// Requires `--quic` or `--ws`.
        #[arg(long, requires = "remote")]
        token: Option<String>,

        /// Pin the QUIC server's certificate by its SHA-256 fingerprint (the
        /// value `phux pair` prints). Required to dial any non-loopback
        /// `--quic`/`--ws wss://` address. Requires `--quic` or `--ws`.
        #[arg(long, value_name = "FP", requires = "remote")]
        cert_fingerprint: Option<String>,

        /// TLS server name (SNI) to offer the remote listener. QUIC defaults
        /// to `localhost`; WebSocket defaults to the URL host. Requires
        /// `--quic` or `--ws`.
        #[arg(long, value_name = "NAME", requires = "remote")]
        tls_server_name: Option<String>,

        /// Tee this attach's composited output to a recording. Declared here
        /// (and on the root command) rather than globally so it only shows up
        /// on the verbs that raise a TUI.
        #[command(flatten)]
        rec: RecOpts,
    },

    /// Run a phux server in the foreground.
    ///
    /// Binds a Unix domain socket, pre-seeds a session whose initial
    /// pane spawns the user's `$SHELL` inside a real PTY, and serves
    /// `ATTACH` requests until Ctrl-C.
    Server {
        /// Name of the pre-seeded session. Matches what
        /// `phux attach <name>` will request.
        #[arg(long, default_value = DEFAULT_SESSION_NAME)]
        session: String,

        /// Also accept WebSocket clients on this `HOST:PORT` (the UDS stays
        /// on). Loopback (e.g. `127.0.0.1:8787`) is plaintext for local
        /// browser dev; any routable address (e.g. `0.0.0.0:8787`)
        /// auto-provisions TLS and requires a `phux pair` token.
        /// Overrides `$PHUX_WS_ADDR`.
        #[arg(long, value_name = "HOST:PORT")]
        listen: Option<std::net::SocketAddr>,

        /// Also accept QUIC clients on this `HOST:PORT` (the UDS stays on).
        /// QUIC is always TLS 1.3-encrypted; a loopback address skips token
        /// auth (local dev), while any routable address requires a `phux pair`
        /// token sent as the stream's opening preamble.
        /// Overrides `$PHUX_QUIC_ADDR`.
        #[arg(long, value_name = "HOST:PORT")]
        quic: Option<std::net::SocketAddr>,

        /// Also accept WebTransport (HTTP/3 over QUIC) clients on this
        /// `HOST:PORT` (the UDS stays on) — the browser's door to QUIC-class
        /// transport; the browser client dials it, falling back to WebSocket.
        /// Always TLS 1.3-encrypted; a loopback address skips token auth
        /// (local dev), while any routable address requires a `phux pair`
        /// token carried in the CONNECT request (`Authorization: Bearer`
        /// from native consumers, `?token=<hex>` on the session URL from
        /// browsers). Overrides `$PHUX_WT_ADDR`.
        #[arg(long, value_name = "HOST:PORT")]
        webtransport: Option<std::net::SocketAddr>,

        /// Dial one relay outbound on `HOST:PORT`. If a matching
        /// `[[connector]]` entry exists, its token file and certificate pin
        /// are used; otherwise only a loopback endpoint is accepted for
        /// unauthenticated development. Without this flag, every configured
        /// connector is supervised independently.
        #[arg(long, value_name = "HOST:PORT")]
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
        #[arg(long)]
        hub: bool,

        /// Exit once no client has been connected for SECS, even if panes
        /// are still running. For ephemeral servers: a test harness or CI
        /// job that bootstraps a private server per run and cannot
        /// guarantee its own cleanup step will execute. The clock starts at
        /// startup, so a server nobody ever connects to also exits.
        ///
        /// Without this flag the server keeps the multiplexer contract and
        /// lives until its last pane is gone.
        #[arg(long = "exit-after-idle", value_name = "SECS",
              value_parser = clap::value_parser!(u64).range(1..=86_400))]
        exit_after_idle: Option<u64>,

        /// Detach from the controlling terminal via `setsid(2)` before
        /// binding. Set by the auto-spawn path so the server outlives
        /// the launching client's terminal; a foreground `phux server`
        /// run by hand leaves this off so Ctrl-C still works.
        #[arg(long, hide = true)]
        daemonize: bool,

        /// Run this command (via `$SHELL -c`) as the pre-seeded session's
        /// initial program instead of a bare shell. The naked-`phux`
        /// auto-spawn path passes `defaults.spawn-on-attach` here;
        /// `phux new` deliberately does not, so an
        /// explicitly-created session still gets a shell.
        #[arg(long, hide = true)]
        seed_command: Option<String>,

        /// Graceful-upgrade resume: read the handoff state blob
        /// from this inherited descriptor, adopt the inherited listener, and
        /// rebuild the live session tree instead of starting fresh. Set by
        /// the upgrade orchestrator's re-exec; never passed by hand.
        #[arg(long, hide = true)]
        resume: Option<std::os::fd::RawFd>,
    },

    /// List sessions on the running server.
    ///
    /// Queries the running server and prints one line per session. Does not
    /// start a server: with no server running it reports as much and exits
    /// non-zero (like `tmux ls`). Pass `--json` for the stable, versioned
    /// machine shape instead of the human text.
    #[command(visible_alias = "list")]
    Ls {
        #[command(flatten)]
        json: JsonOpt,
    },

    /// Report the running server: pid, up since, protocol, clients, logs.
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
    #[command(mut_arg("json", |a| a
        .help("Emit stable, versioned JSON on stdout instead of the human view")
        .long_help(
            "Emit stable, versioned JSON on stdout instead of the human view. \
             Exception to the shared failure contract: with no server running, \
             stdout carries the `{\"running\": false, ...}` document (still \
             exiting non-zero); any other failure leaves stdout empty and puts \
             one JSON error object on stderr",
        )))]
    Status {
        #[command(flatten)]
        json: JsonOpt,
    },

    /// Create a new session and attach to it.
    ///
    /// Creates the named session if it does not already exist, then
    /// attaches. Auto-starts a server if none is running. A name already
    /// in use is an error; omit the name to take the configured
    /// `session-name-template`, disambiguated with a numeric suffix.
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
    #[command(group = clap::ArgGroup::new("json_mode").arg("json").requires("session"))]
    New {
        /// Session name. `phux new work` creates a session named "work".
        /// Omitted ⇒ the `session-name-template` (e.g. "default"),
        /// disambiguated with a numeric suffix if that name is taken.
        #[arg(value_name = "NAME")]
        name: Option<String>,

        /// Session name in flag form — equivalent to the positional NAME,
        /// and the form required by `--json`. An error if it conflicts
        /// with NAME.
        #[arg(short = 's', long = "session")]
        session: Option<String>,

        /// Working directory for the seed pane.
        #[arg(short = 'c', long = "cwd")]
        cwd: Option<std::path::PathBuf>,

        #[command(flatten)]
        json: JsonOpt,

        /// Environment assignment for the seed process. Repeat for multiple
        /// variables. Headless `--json` mode only.
        #[arg(
            short = 'e',
            long = "env",
            value_name = "KEY=VALUE",
            requires = "json",
            value_parser = parse_env_assignment
        )]
        env: Vec<(String, String)>,

        /// Command (and arguments) to run in the seed pane instead of the
        /// default shell. Must follow `--`: `phux new work -- htop`.
        #[arg(last = true)]
        command: Vec<String>,
    },

    /// Spawn a Terminal without attaching (`SPAWN_TERMINAL`).
    ///
    /// With `--target`, the pane is inserted beside an exact local owner;
    /// otherwise it joins the server's most recently active session. The new
    /// Terminal's id prints to stdout. With `--satellite NAME` on a
    /// federation hub (`phux server --hub`), the spawn is routed over
    /// the hub's link to that satellite and the returned id is
    /// satellite-tagged — addressable through the hub by every
    /// satellite-capable verb. Does not auto-start a server.
    Spawn {
        /// Route the spawn to a configured federation satellite (a name
        /// from `phux host ls --role satellite`, on a server running
        /// `--hub`).
        #[arg(long, value_name = "NAME")]
        satellite: Option<String>,

        /// Existing local pane beside which to place the new pane.
        #[arg(long, value_name = "TARGET", conflicts_with = "satellite")]
        target: Option<String>,

        /// Split axis for explicit placement (requires `--target`).
        #[arg(long, value_enum, default_value = "horizontal", requires = "target")]
        split: SpawnSplit,

        /// Fraction of the split retained by TARGET (requires `--target`).
        #[arg(long, default_value_t = 0.5, requires = "target", value_parser = parse_spawn_ratio)]
        ratio: f32,

        /// Working directory for the new pane.
        #[arg(short = 'c', long = "cwd")]
        cwd: Option<String>,

        #[command(flatten)]
        json: JsonOpt,

        /// Command (and arguments) to run instead of the default shell.
        /// Must follow `--`: `phux spawn -- htop`.
        #[arg(last = true)]
        command: Vec<String>,
    },

    /// Launch an agent integration in a new pane.
    ///
    /// Resolves INTEGRATION (a `phux launch --list` id) to its `[launch]`
    /// command from an enabled plugin's integration template, then spawns a
    /// pane running it. The template routes the agent through its identity
    /// wrapper, so the pane self-declares its `phux.agent/v1` identity with
    /// no alias or per-shell config: the server injects `PHUX_TERMINAL_ID`,
    /// the wrapper targets its own pane with it, and writes name + kind at
    /// launch.
    ///
    /// `--print` resolves and prints the argv without spawning (a server-free
    /// dry run). Extra agent arguments follow `--`:
    /// `phux launch codex -- --model o3`.
    Launch {
        /// Integration id to launch (from `phux launch --list`).
        #[arg(value_name = "INTEGRATION", required_unless_present = "list")]
        integration: Option<String>,

        /// List launchable integrations from enabled plugins and exit.
        #[arg(long)]
        list: bool,

        /// Resolve and print the launch argv (and cwd) without spawning a
        /// pane — a server-free dry run.
        #[arg(long, visible_alias = "dry-run")]
        print: bool,

        #[command(flatten)]
        json: JsonOpt,

        /// Existing local pane beside which to place the launched pane.
        #[arg(long, value_name = "TARGET", conflicts_with_all = ["list", "print"])]
        target: Option<String>,

        /// Split axis for explicit placement (requires `--target`).
        #[arg(long, value_enum, default_value = "horizontal", requires = "target")]
        split: SpawnSplit,

        /// Fraction of the split retained by TARGET (requires `--target`).
        #[arg(long, default_value_t = 0.5, requires = "target", value_parser = parse_spawn_ratio)]
        ratio: f32,

        /// Working directory for a `working_directory = "workspace"`
        /// template. Defaults to the current directory.
        #[arg(short = 'c', long = "cwd", value_name = "DIR")]
        cwd: Option<std::path::PathBuf>,

        /// Extra arguments appended to the agent command, after `--`.
        #[arg(last = true)]
        extra: Vec<String>,
    },

    /// Kill a session, window, pane, or the server itself.
    ///
    /// `TARGET` uses the selector grammar (see the top-level help):
    /// `name`, `name:N`, `name:N.M`, `name:tag`, `@N`, `.`. The selector
    /// is resolved client-side against a server-state snapshot to a set of
    /// Terminals; the server is then asked to kill each.
    ///
    /// `--server` stops the server process instead, ending every session on
    /// it. Local socket only.
    #[command(group = clap::ArgGroup::new("kill_what").required(true).args(["target", "server"]))]
    Kill {
        /// What to kill (selector).
        target: Option<String>,
        /// Stop the running server, ending every session it holds.
        ///
        /// The server exits cleanly, so a supervised one stays stopped rather
        /// than being restarted. Note that the next `phux attach`/`new` will
        /// auto-spawn a fresh server: this stops the current one, it does not
        /// disable phux.
        #[arg(long)]
        server: bool,
    },

    /// Insert an already-created pane into a session layout.
    ///
    /// Both selectors must each resolve to exactly one local pane in the same
    /// session. This command does not spawn: create `NEW_PANE` first with
    /// `phux spawn`, then insert it. Omitted direction defaults horizontal.
    #[command(name = "insert-pane")]
    InsertPane {
        /// Existing layout leaf beside which `NEW_PANE` is inserted.
        target: String,
        /// Already-created pane to insert; no implicit spawn occurs.
        new_pane: String,
        /// Split axis: `horizontal` stacks the panes, `vertical` places
        /// them side-by-side.
        #[arg(long, value_enum, default_value = "horizontal")]
        split: SpawnSplit,
        /// Fraction assigned to TARGET; must be strictly between 0 and 1.
        #[arg(long, default_value_t = 0.5, value_parser = parse_spawn_ratio)]
        ratio: f32,
        /// Emit a schema-versioned JSON result or error.
        #[arg(long)]
        json: bool,
    },

    /// Move one existing pane beside another, even across sessions.
    ///
    /// SOURCE is collapsed out of its current tree position and inserted
    /// beside TARGET. Both selectors must resolve to exactly one local pane.
    /// When TARGET lives in a different session the pane is re-parented on
    /// the server first — its process, scrollback, and id survive the move.
    #[command(name = "move-pane")]
    MovePane {
        /// Pane to relocate.
        source: String,
        /// Existing destination pane.
        target: String,
        /// Destination split axis: `horizontal` stacks the panes,
        /// `vertical` places them side-by-side.
        #[arg(long, value_enum, default_value = "horizontal")]
        split: SpawnSplit,
        /// Fraction assigned to TARGET; must be strictly between 0 and 1.
        #[arg(long, default_value_t = 0.5, value_parser = parse_spawn_ratio)]
        ratio: f32,
        /// Emit a schema-versioned JSON result or error.
        #[arg(long)]
        json: bool,
    },

    /// Swap two existing pane leaves in the same session layout.
    ///
    /// Both selectors must each resolve to exactly one local pane. Split
    /// geometry is preserved and attached clients retain their local focus.
    #[command(name = "swap-pane")]
    SwapPane {
        /// First pane selector.
        first: String,
        /// Second pane selector.
        second: String,
        /// Emit a schema-versioned JSON result or error.
        #[arg(long)]
        json: bool,
    },

    /// Set a pane's grid size, with no TTY.
    // Spelled out in `long_about` (the shape `rec` and `play` set) because
    // clap reflows doc-comment paragraphs: as a doc comment the examples
    // below collapse onto one run-on line.
    #[command(
        about = "Set a pane's grid size, with no TTY",
        long_about = "Set a pane's grid size, with no TTY.\n\n\
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
    Resize {
        /// Target selector: session, session:window, session:window.pane,
        /// @id, or `.` (focused). `=` is unsupported by headless commands.
        target: String,

        /// New grid size, e.g. 120x40. Both axes are whole numbers of
        /// cells and at least 1.
        #[arg(value_name = "COLSxROWS", value_parser = resize::parse_geometry)]
        geometry: resize::Geometry,

        #[command(flatten)]
        json: JsonOpt,
    },

    /// Detach clients from a session, from outside the attach UI.
    ///
    /// The CLI counterpart to the `C-a d` keybinding. With `SESSION`, detaches
    /// every client attached to that session; with no argument, detaches every
    /// attached client on the server. Each target client's TUI exits cleanly.
    /// Useful for scripting or reclaiming a session that's attached (or wedged)
    /// elsewhere.
    Detach {
        /// Session to detach clients from. Omit to detach every attached
        /// client on the server.
        session: Option<String>,
    },

    /// Take the input wheel of a pane.
    ///
    /// Seizes exclusive input authority over the resolved pane: while held,
    /// only this connection's input reaches the PTY — every other client's
    /// keystrokes (and any agent's `send-keys`) are locked out. Use it to
    /// grab control of a pane an agent is driving. Release with `phux give`.
    /// TARGET is a selector (see the top-level help).
    Take {
        /// Target selector (resolves to one pane).
        target: String,
    },

    /// Give back the input wheel of a pane.
    ///
    /// Releases the input lease taken with `phux take`, returning the pane to
    /// open input. A no-op if you do not hold the lease. TARGET is a selector.
    Give {
        /// Target selector (resolves to one pane).
        target: String,
    },

    /// Signal a pane's process group.
    // `long_about` for the same reason `rec` spells one out: clap reflows
    // doc-comment paragraphs and the examples need real newlines.
    #[command(
        about = "Signal a pane's process group",
        long_about = "Signal a pane's process group.\n\n\
            Delivers a POSIX signal to the program running in the resolved pane and \
            every subprocess it spawned — distinct from `phux kill`, which destroys \
            the pane. `freeze` (SIGSTOP) pauses the process mid-step; `resume` \
            (SIGCONT) lets it run again — the reversible brake for an agent about to \
            do something rash. TARGET is a selector.\n\n\
            Examples:\n  \
            phux signal build freeze\n  \
            phux signal . kill"
    )]
    Signal {
        /// Target selector (resolves to one pane).
        target: String,

        /// Which signal to deliver.
        signal: SignalArg,
    },

    /// Update phux to the latest release, keeping sessions alive.
    // `long_about` spelled out for the same reason `rec` and `signal` do it:
    // clap reflows doc-comment paragraphs and the worked examples need real
    // newlines.
    #[command(
        about = "Update phux to the latest release, keeping sessions alive",
        long_about = "Update phux to the latest release, keeping sessions alive.\n\n\
            Checks the published release, downloads the archive for this platform, \
            verifies it against the checksum published beside it, replaces the \
            binaries atomically, and asks a running server to re-exec so live panes \
            survive. A server, its local clients, its satellites, and its relays must \
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
            phux update --dry-run --version v1.2.3\n  \
            phux update --rollback"
    )]
    Update {
        /// Update options.
        #[command(flatten)]
        opts: update::UpdateOpts,
    },

    /// Graceful-upgrade the running server in place.
    ///
    /// Asks the server to snapshot every pane, re-exec the on-disk binary, and
    /// re-adopt the live PTYs, so the shells / editors / agents in every
    /// session survive a binary update (e.g. after `cargo install` /
    /// `brew upgrade`). Clients briefly disconnect and reconnect. This is the
    /// low-level primitive: it re-execs whatever is already on disk and
    /// downloads nothing. `phux update` is the command that puts a new binary
    /// there first.
    Upgrade {},

    /// Rename a session.
    ///
    /// Reassigns `SESSION`'s human-readable name to `NEW_NAME` in one
    /// round-trip. The server is authoritative;
    /// attached clients pick up the new name on their next snapshot. An
    /// unknown `SESSION` or a `NEW_NAME` already in use is an error.
    Rename {
        /// Current session name.
        session: String,

        /// New session name.
        new_name: String,
    },

    /// Capture a pane's screen as JSON or a boxed text view.
    ///
    /// The agent "floor": read what's on screen as JSON (`--json`) or a
    /// boxed text view, without a TTY or tmux. The read is side-effect-free
    /// — the server walks its own grid, so this neither attaches nor
    /// resizes the pane, and is safe to poll against a pane another client
    /// is using.
    ///
    /// TARGET is a selector (see the top-level help); omit it for the
    /// most-recently-focused session.
    #[command(about = "Capture a pane's screen as JSON or a boxed text view")]
    Snapshot {
        /// Target selector. Omit for the most-recently-focused session.
        #[arg(value_name = "TARGET")]
        session: Option<String>,

        #[command(flatten)]
        json: JsonOpt,

        /// Include scrollback history above the viewport.
        /// Bare `--scrollback` requests all retained history; `--scrollback
        /// N` requests the most-recent N rows. History appears in the JSON
        /// `scrollback` field; the boxed view shows it above the viewport.
        #[arg(long, value_name = "N", num_args = 0..=1, default_missing_value = "0")]
        scrollback: Option<u32>,

        /// Include per-cell OSC-133 semantic marks + styles.
        /// Populates the JSON `cells` array (sparse: only cells with a
        /// non-default style or a semantic mark). No effect on the boxed
        /// view, which is plain text.
        #[arg(long)]
        cells: bool,

        /// Return the last N rendered rows (history above the viewport,
        /// then the viewport). Bare `--tail` returns 80; `--tail 0` returns
        /// all, capped at 10000. The viewport is a floor — a grid is never
        /// returned in part — and `truncated` reports any dropped rows.
        // The literals are `phux_core::screen::ROW_WINDOW_DEFAULT` and
        // `ROW_WINDOW_MAX`; clap needs a `&'static str` here, so
        // `commands::snapshot`'s tests pin the two spellings together.
        #[arg(long, value_name = "N", num_args = 0..=1, default_missing_value = "80")]
        tail: Option<u32>,

        /// Join soft-wrapped rows into logical lines (rows as written, not
        /// as painted). Cannot be combined with `--cells`: cell coordinates
        /// are grid coordinates and do not survive the join.
        #[arg(long, conflicts_with = "cells")]
        unwrap: bool,

        /// Emit the CLIENT's composited multi-pane view — the assembled
        /// frame (layout tiling + dividers + status bar) as the human's glass
        /// shows it — as dense structured cells. Unlike the
        /// default side-effect-free read this ATTACHES (drives the headless
        /// client render path). Mutually exclusive with `--cells` /
        /// `--scrollback` / `--tail` / `--unwrap`; sizes the composite via
        /// `--cols` / `--rows`.
        #[arg(long, conflicts_with_all = ["cells", "scrollback", "tail", "unwrap"])]
        rendered: bool,

        /// Composited viewport width for `--rendered` (no TTY to measure).
        #[arg(long, value_name = "COLS", default_value_t = 80)]
        cols: u16,

        /// Composited viewport height for `--rendered`.
        #[arg(long, value_name = "ROWS", default_value_t = 24)]
        rows: u16,
    },

    /// Send keys to a pane.
    // `long_about` for the same reason `rec` spells one out: clap reflows
    // doc-comment paragraphs and the examples need real newlines.
    #[command(
        name = "send-keys",
        about = "Send keys to a pane",
        long_about = "Send keys to a pane.\n\n\
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
    SendKeys {
        /// Target selector: session, session:window, session:window.pane,
        /// @id, or `.` (focused). `=` is unsupported by headless commands.
        target: String,

        /// Keys to send: named keys and/or literal strings, in order.
        #[arg(trailing_var_arg = true, required = true)]
        keys: Vec<String>,
    },

    /// Paste text into a pane.
    // `long_about` for the same reason `rec` spells one out: clap reflows
    // doc-comment paragraphs and the examples need real newlines.
    #[command(
        about = "Paste text into a pane (bracketed when the pane asks for it)",
        long_about = "Paste text into a pane.\n\n\
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
        #[arg(long)]
        untrusted: bool,
    },

    /// Block until a pane meets a condition.
    // `long_about` for the same reason `rec` spells one out: clap reflows
    // doc-comment paragraphs and the examples need real newlines.
    #[command(
        about = "Block until a pane meets a condition",
        long_about = "Block until a pane meets a condition.\n\n\
            Polls the side-effect-free screen read — the poll \
            floor of the event surface: always works, no shell integration. \
            Exits 0 when the condition is met, and 124 when `--timeout` expires \
            first. TARGET is a selector (see the \
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
    Wait {
        /// Target selector. Omit for the most-recently-focused session.
        #[arg(value_name = "TARGET")]
        session: Option<String>,

        /// Succeed once any line contains this substring. NOTE: this matches
        /// ANY line, including the shell's echo of a command you just typed
        /// — match on text that appears only in OUTPUT, or pass
        /// `--output-only`.
        #[arg(long, value_name = "TEXT", conflicts_with = "regex")]
        until: Option<String>,

        /// Succeed once any line matches this Rust regular expression. One
        /// line at a time, so `^` and `$` anchor to a line you can see. An
        /// invalid pattern is a usage error (exit 2) reported before the
        /// wait starts, never a wait that quietly never matches.
        #[arg(long, value_name = "PATTERN")]
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
        #[arg(long, value_name = "N", num_args = 0..=1, default_missing_value = "80")]
        tail: Option<u32>,

        /// Ignore lines the shell marked as your own typed input, so a wait
        /// cannot be satisfied by the echo of the command that started the
        /// work. Needs a shell with OSC-133 integration; with none, nothing
        /// is filtered and phux says so on stderr rather than pretending.
        #[arg(long)]
        output_only: bool,

        /// Succeed once the matched lines hold still for this many
        /// milliseconds (the pane has settled). Default when neither
        /// `--until` nor `--regex` is given. With `--tail N`, only those
        /// lines have to hold still — a spinner further up does not count.
        #[arg(long, value_name = "MS")]
        idle: Option<u64>,

        /// Give up after this many seconds (exit 124). Default: wait forever.
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,

        #[command(flatten)]
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
    #[command(
        about = "Stream a pane's live events (bell, title, dirty/idle, lifecycle)",
        long_about = "Stream a pane's live events (the push half of the agent surface).\n\n\
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
    Watch {
        /// Target selector. Omit for the most-recently-focused session.
        #[arg(value_name = "TARGET")]
        session: Option<String>,

        /// Exit 0 as soon as an event with this name arrives. Repeatable;
        /// any one of them satisfies the watch. The vocabulary is the one
        /// this stream prints: `agent_state`, `asked`, `bell`,
        /// `command_finished`, `command_started`, `dirty`, `idle`,
        /// `pane_closed`, `pane_spawned`, `title_changed`, `unknown`. An
        /// unrecognized name is a usage error (exit 2) reported before the
        /// watch starts, never a watch that quietly never matches.
        #[arg(long, value_name = "EVENT")]
        until: Vec<String>,

        /// Give up after this many seconds (exit 124). Applies with or
        /// without `--until`. Default: stream until EOF or Ctrl-C.
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,

        #[command(flatten)]
        json: JsonOpt,
    },

    /// Record a pane and export it as an asciinema cast, an animated GIF, or
    /// an APNG.
    // The user-facing text is spelled out in `long_about` (the same shape the
    // root command uses) because clap reflows doc-comment paragraphs: as a
    // doc comment the three examples below collapse onto one run-on line.
    #[command(
        about = "Record a pane and export it as a cast, GIF, or APNG",
        long_about = "Record a pane and export it as an asciinema cast, an animated GIF, or an APNG.\n\n\
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
    Rec {
        /// Pane selector. Defaults to the focused pane.
        #[arg(value_name = "TARGET")]
        target: Option<String>,

        /// Output path. The extension picks the format unless --format is
        /// given; a path with no extension gets `.gif`.
        #[arg(short = 'o', long = "out", value_name = "PATH")]
        out: std::path::PathBuf,

        /// Output format, overriding the extension.
        #[arg(long, value_enum, value_name = "FMT")]
        format: Option<RecFormat>,

        /// Re-render an existing .cast instead of capturing a live pane.
        #[arg(long, value_name = "FILE", conflicts_with_all = ["target", "duration"])]
        from: Option<std::path::PathBuf>,

        /// Stop after SECS of recording (default: until Ctrl-C or the pane
        /// exits).
        #[arg(long, value_name = "SECS")]
        duration: Option<u64>,

        /// Animation sample rate for GIF/APNG output.
        #[arg(long, value_name = "FPS", default_value_t = 10,
              value_parser = clap::value_parser!(u8).range(1..=50))]
        fps: u8,

        /// Collapse any pause longer than SECS down to SECS. 0 disables.
        #[arg(long = "idle-limit", value_name = "SECS", default_value_t = 2.0)]
        idle_limit: f64,

        /// Stop encoding and warn once the output reaches BYTES.
        #[arg(long = "max-bytes", value_name = "BYTES", default_value_t = 8 * 1024 * 1024)]
        max_bytes: u64,

        /// asciicast format version to write (2 is the interoperable
        /// default).
        #[arg(long = "cast-version", value_name = "N", default_value_t = 2,
              value_parser = clap::value_parser!(u8).range(2..=3))]
        cast_version: u8,

        #[command(flatten)]
        json: JsonOpt,
    },

    /// Play a recording back as a live pane.
    // Spelled out in `long_about` for the same reason `rec` is: clap reflows
    // doc-comment paragraphs into one run-on line and the examples need real
    // newlines.
    #[command(
        about = "Play a recording back as a live pane",
        long_about = "Play a recording back as a live pane.\n\n\
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
    Play {
        /// The .cast file to play.
        #[arg(value_name = "FILE")]
        file: std::path::PathBuf,

        /// Selector for the pane the playback pane is created beside.
        /// Defaults to `.` (the focused pane). Never written to.
        #[arg(value_name = "TARGET")]
        target: Option<String>,

        /// Playback rate. 1 is real time, 2 is twice as fast, 0.5 half
        /// speed. Between 0.01 and 100; no events are ever dropped.
        #[arg(long, value_name = "N", default_value = "1",
              value_parser = play::parse_speed)]
        speed: phux_record::playback::Speed,

        /// Collapse any pause longer than SECS down to SECS. Defaults to
        /// the idle limit the recording itself declares; 0 plays the raw
        /// timeline.
        #[arg(long = "idle-limit", value_name = "SECS")]
        idle_limit: Option<f64>,

        /// Repeat the recording. Bare `--loop` repeats until the pane is
        /// killed; `--loop N` plays it N times.
        #[arg(long = "loop", value_name = "N", num_args = 0..=1,
              default_missing_value = "0")]
        loops: Option<u32>,

        /// Split axis for the new pane.
        #[arg(long, value_enum, default_value = "horizontal")]
        split: SpawnSplit,

        /// Fraction of the split retained by TARGET.
        #[arg(long, default_value_t = 0.5, value_parser = parse_spawn_ratio)]
        ratio: f32,

        /// Leave the pane's grid alone instead of fitting it to the
        /// recording's. Output wider than the pane will wrap.
        #[arg(long = "no-fit")]
        no_fit: bool,

        /// Close the pane when playback ends, instead of holding the final
        /// frame until it is killed.
        #[arg(long)]
        close: bool,

        #[command(flatten)]
        json: JsonOpt,

        /// Internal: this process IS the pane, so write the recording to
        /// stdout rather than spawning one. Hidden because it is an
        /// implementation detail of the pane this verb creates, not a
        /// promise that phux ships a shell-level cast viewer.
        #[arg(long = "pty-writer", hide = true)]
        pty_writer: bool,
    },

    /// Report that an agent in a pane is waiting on a human answer.
    // `long_about` for the same reason `rec` spells one out: clap reflows
    // doc-comment paragraphs and the examples need real newlines.
    #[command(
        about = "Report an agent ask event for a pane",
        long_about = "Report that an agent in a pane is waiting on a human answer.\n\n\
            This is the opt-in hook contract for configured integrations: it emits \
            the same `asked` event as the `phux-ask` title sentinel without writing \
            escape sequences into the target terminal. TARGET is resolved \
            client-side and the command neither attaches nor resizes the pane.\n\n\
            Examples:\n  \
            phux ask work:1.0 --id deploy --suggest Yes --suggest No \"Deploy?\"\n  \
            phux ask @3 --json \"Need approval\""
    )]
    Ask {
        /// Target selector: session, session:window, session:window.pane,
        /// @id, or `.` (focused). `=` is unsupported by headless commands.
        target: String,

        /// Stable question id for answer correlation.
        #[arg(long, default_value = "")]
        id: String,

        /// Suggested answer. Repeat to preserve display order.
        #[arg(long = "suggest", value_name = "TEXT")]
        suggestions: Vec<String>,

        /// Seconds the agent has already been waiting.
        #[arg(long, value_name = "SECS")]
        elapsed_seconds: Option<u64>,

        #[command(flatten)]
        json: JsonOpt,

        /// Human-facing question text.
        question: String,
    },

    /// List, show, explain, set, or clear per-pane agent state.
    ///
    /// Inference (`list`/`show`/`explain`) reports the agent phux infers is
    /// running in each pane. `set`/`clear` write and delete an explicit
    /// per-pane agent identity that overrides inference.
    Agent {
        #[command(subcommand)]
        action: agent::AgentAction,
    },

    /// Run a command in a pane and capture its exit code.
    // `long_about` for the same reason `rec` spells one out: clap reflows
    // doc-comment paragraphs and the examples need real newlines.
    #[command(
        about = "Run a command in a pane and capture its exit code",
        long_about = "Run a command in a pane and capture its exit code.\n\n\
            Reports the command's exit code, output, and duration. \
            Brackets the command with sentinels to capture `$?`, so it \
            assumes a POSIX shell (sh/bash/zsh). The process exit code mirrors \
            the command's — and is 125 when `phux` gives up on `--timeout` — so \
            `phux run … && next` composes like a shell. TARGET is a selector \
            (see the top-level help), resolved client-side to one pane; the \
            command routes to it by id (no attach, no resize).\n\n\
            Flags (`--timeout`, `--json`, `--socket`) MUST precede TARGET, or \
            they are swallowed into the trailing command.\n\n\
            Examples:\n  \
            phux run build \"cargo test\"\n  \
            phux run --timeout 30 work:1.0 \"cargo test\""
    )]
    Run {
        /// Target selector: session, session:window, session:window.pane,
        /// @id, or `.` (focused). `=` is unsupported by headless commands.
        target: String,

        /// The command line: all trailing args, joined with spaces.
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<String>,

        /// Give up after this many seconds (exit 125). Default: 600s.
        /// Pass 0 to wait indefinitely.
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,

        #[command(flatten)]
        json: JsonOpt,
    },

    /// Inspect, scaffold, and reload the phux config file.
    ///
    /// phux is config-driven: defaults ship in the binary and
    /// your `config.toml` is a sparse overlay merged on top. These
    /// subcommands never touch a running server, except `reload`,
    /// which signals attached clients to re-read their config in place.
    Config {
        #[command(subcommand)]
        action: config_action::ConfigAction,
    },

    /// Manage local plugin manifests in the phux config registry.
    ///
    /// This is a client-local config operation: it validates
    /// `phux-plugin.toml` manifests and edits `[[plugins]]` entries in the
    /// user's config without contacting a running server.
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
    },

    /// Inspect a git workspace and its worktrees for agent orchestration.
    ///
    /// This is a local repo operation: it never contacts a running phux server
    /// and never creates or deletes worktrees. Agents use it to map code
    /// checkouts to phux sessions/panes before spawning or attaching work.
    Workspace {
        #[command(subcommand)]
        action: WorkspaceAction,
    },

    /// Read and write a Terminal's L3 tags.
    ///
    /// Tags are freeform strings stored as L3 metadata (`phux.tags/v1`),
    /// the server stores them opaquely. Once a Terminal is tagged, the
    /// `#tag` selector addresses every Terminal carrying that tag — e.g.
    /// `phux kill #build`, `phux snapshot #web`.
    Tag {
        #[command(subcommand)]
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
    #[command(name = "stdio-bridge", hide = true)]
    StdioBridge {},

    /// Run a standalone relay, or enroll a route with it.
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
    Relay {
        #[command(subcommand)]
        action: relay::RelayAction,
    },

    /// Mint a pairing token for a remote consumer.
    ///
    /// Remote consumers (e.g. the native mobile app) attach over `wss://`
    /// without an SSH tunnel: TLS encrypts the link and an opaque bearer
    /// token authenticates the device. This mints one token into the store
    /// the server reads (`PHUX_WS_TOKENS`) and prints it once alongside the
    /// server certificate's SHA-256 fingerprint. Pair both into the device:
    /// the token is the credential, and verifying the fingerprint on first
    /// connect defeats a man-in-the-middle. Revoke a device by deleting its
    /// line from the token file. When an overlay network address
    /// (Tailscale/WireGuard) is detected, it is printed alongside the
    /// credentials.
    ///
    /// This never contacts a running server — it only writes the token file.
    Pair {
        /// Token store to append to. Defaults to `PHUX_WS_TOKENS`.
        #[arg(long, value_name = "PATH")]
        tokens: Option<std::path::PathBuf>,

        /// Server certificate PEM, used to print the pairing fingerprint.
        /// Defaults to `PHUX_WS_TLS_CERT`.
        #[arg(long, value_name = "PATH")]
        cert: Option<std::path::PathBuf>,

        /// Also render the pairing payload as a scannable QR code. The QR
        /// encodes the same `phux://connect` one-tap link printed as text,
        /// so a phone can pair by scanning instead of typing. Needs a server
        /// address: pass `--host`, or let it fall back to a detected overlay
        /// address plus the `PHUX_WS_ADDR` port.
        #[arg(long)]
        qr: bool,

        /// Server address (`host:port`, or a full `ws://`/`wss://` URL) to
        /// embed in the connect link so it is fully self-contained. Omitted:
        /// derived from the detected overlay address and the `PHUX_WS_ADDR`
        /// port when possible; otherwise no link is printed (the device
        /// enters the address itself).
        #[arg(long, value_name = "HOST:PORT")]
        host: Option<String>,

        /// Human-readable server name to embed in the connect link, shown by
        /// the device in its server list. Omitted: the device picks a default.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,

        /// Emit the pairing material as JSON on stdout instead of the
        /// human-readable report. This is what `phux host enroll` reads
        /// over ssh.
        #[arg(long)]
        json: bool,
    },

    /// Register the machines phux talks to: remotes and satellites.
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
    Host {
        #[command(subcommand)]
        action: host::HostAction,
    },

    /// Keep a server running across logout and reboot.
    ///
    /// Generates this host's native per-user service unit — a `launchd`
    /// `LaunchAgent` on macOS, a systemd user unit on Linux — with the
    /// server's environment baked in, so a rebooted host comes back with a
    /// server instead of waiting for someone to log in and start one.
    /// A restarted server has no terminals: every pane's process died with
    /// the host. `install --restore` brings back session names, layout, and
    /// cwd, not running processes.
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Print a shell completion script on stdout.
    // `long_about` for the same reason `rec` spells one out: clap reflows
    // doc-comment paragraphs and the three install commands need real
    // newlines — run together on one line they do copy-paste damage.
    #[command(
        about = "Print a shell completion script on stdout",
        long_about = "Print a shell completion script on stdout.\n\n\
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
    Completion {
        /// Shell dialect to generate for.
        #[arg(value_name = "SHELL")]
        shell: clap_complete::Shell,
    },

    /// Print the agent skill this binary ships with, on stdout.
    // `long_about` spelled out for the same reason `completion` spells one
    // out: clap reflows doc-comment paragraphs, and the install one-liners
    // need real newlines or they run together and do copy-paste damage.
    #[command(
        about = "Print the agent skill this binary ships with, on stdout",
        long_about = "Print the agent skill this binary ships with, on stdout.\n\n\
            The text is compiled into the executable, so it describes the verbs \
            and flags THIS build actually has — it cannot drift from the binary \
            the way a copied file can. It contacts no server and reads no \
            config.\n\n\
            Give it to any agent that needs to drive phux: it teaches the \
            read-act-wait loop, the selector grammar, the difference between a \
            level read and an observed transition, the exit codes, and the \
            safety rules for driving a terminal a human may also be using.\n\n\
            Examples:\n  \
            phux skill\n  \
            phux skill > ~/.claude/skills/phux/SKILL.md\n  \
            phux skill | pbcopy"
    )]
    Skill {},

    /// Diagnose a phux install: config, socket, server, plugins.
    ///
    /// Composes the checks that already exist as separate verbs and reports
    /// one verdict, because knowing which four commands to run and how to
    /// read each one is exactly what someone debugging phux does not have.
    ///
    /// Read-only. Exits 1 if any check failed; warnings alone exit 0,
    /// since a stopped server is a normal state and not a broken install.
    Doctor {
        /// Emit a stable JSON document instead of human text.
        #[arg(long)]
        json: bool,
    },

    /// Manage git worktrees and the sessions bound to them.
    ///
    /// Each worktree binds to one session whose name is derived from the
    /// worktree's directory basename. The derivation is a pure function of
    /// the path, so the binding is computed on demand and can never go
    /// stale — phux stores no worktree state and the server knows no git.
    #[command(subcommand)]
    Worktree(WorktreeAction),

    /// Show where phux's logs live, or tail one of them.
    ///
    /// Bare `phux logs` prints the inventory: the canonical server log
    /// (every spawn path writes it), the per-pid client logs, and the state
    /// dir that holds them — with existence, size, and age, so a fresh
    /// machine reads "not created yet" instead of an error. `--server`
    /// tails the server log and `--client` the newest client log (`--pid`
    /// picks a specific one); `-f` follows and `-n` sets the tail length.
    /// `--json` emits the inventory as a stable document.
    #[command(group = clap::ArgGroup::new("which").args(["server", "client"]).multiple(false))]
    Logs {
        /// Tail the canonical server log.
        #[arg(long)]
        server: bool,

        /// Tail the newest per-pid client log (or the one `--pid` names).
        #[arg(long)]
        client: bool,

        /// With --client: the client pid whose log to tail, instead of the
        /// newest.
        #[arg(long, value_name = "PID", requires = "client")]
        pid: Option<u32>,

        /// Follow the tailed log as it grows (needs --server or --client).
        #[arg(short, long, requires = "which")]
        follow: bool,

        /// How many trailing lines to show (needs --server or --client).
        #[arg(short = 'n', long, default_value_t = 200, requires = "which")]
        lines: u32,

        /// Emit the path inventory as a stable JSON document instead of
        /// human text. Inventory only — it cannot combine with a tail.
        #[arg(long, conflicts_with_all = ["server", "client", "pid", "follow", "lines"])]
        json: bool,
    },

    /// Regenerate the repository's generated reference pages (internal).
    ///
    /// Hidden developer tooling behind `just docs-gen`, not part of the
    /// user-facing surface: renders the reference pages from this binary's
    /// own inventories and writes them into the checkout. A unit test
    /// byte-compares the checked-in pages against this generator, so the
    /// published reference can never drift from the compiled binary.
    #[command(name = "gen-reference-docs", hide = true)]
    GenReferenceDocs {
        /// Directory to write the pages into. Defaults to the checkout's
        /// generated-reference tree; run from the repository root.
        #[arg(long, value_name = "DIR")]
        out: Option<std::path::PathBuf>,
    },
}

/// `phux service <action>` — manage the per-user service unit.
#[derive(Debug, Subcommand)]
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
        #[arg(long, value_name = "HOST:PORT")]
        quic: Option<std::net::SocketAddr>,

        /// Accept WebSocket clients on this `HOST:PORT`. The fallback for
        /// networks that block UDP.
        #[arg(long, value_name = "HOST:PORT")]
        listen: Option<String>,

        /// Save the workspace on stop and restore it on start. Off by
        /// default: a session list repopulated with fresh shells is a
        /// surprise unless asked for. Restores names, layout, and cwd —
        /// never running processes.
        #[arg(long)]
        restore: bool,

        /// Run the supervised server as a federation hub. The service loads
        /// enabled `[[satellites]]` entries and keeps their links connected
        /// across login, logout, and reboot.
        #[arg(long)]
        hub: bool,

        /// Print the unit (and the restore wrapper) to stdout without
        /// writing or loading anything.
        #[arg(long)]
        print: bool,
    },

    /// Unload the unit and remove what `install` wrote.
    Uninstall,

    /// Report whether a unit is installed and running.
    Status,

    /// Show the supervised server's log.
    Logs {
        /// Follow the log as it grows.
        #[arg(short, long)]
        follow: bool,

        /// How many trailing lines to show.
        #[arg(short = 'n', long, default_value_t = 200)]
        lines: u32,
    },

    /// Delete the accumulated per-pid `client-*.log` files.
    #[command(name = "prune-logs")]
    PruneLogs {
        /// Report how many would be removed, and remove nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

/// `phux worktree <action>` — git worktrees bound to sessions by name.
#[derive(Debug, Subcommand)]
pub(crate) enum WorktreeAction {
    /// List the repository's worktrees and their bound sessions.
    ///
    /// The `bound` column reads `live` when a session by the derived name
    /// exists, `-` when it does not, and `?` when no server is running —
    /// "no server" and "no session" are different facts.
    #[command(visible_alias = "ls")]
    List {
        /// Path inside the repository or worktree to list from.
        #[arg(default_value = ".")]
        path: std::path::PathBuf,

        /// Emit a stable JSON document instead of human text.
        #[arg(long)]
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
        #[arg(long, value_name = "PATH")]
        path: Option<std::path::PathBuf>,

        /// Start point for a newly created branch (default: current HEAD).
        #[arg(long, value_name = "REF")]
        from: Option<String>,

        /// Session name, overriding the name derived from the path.
        #[arg(long, short = 's', value_name = "NAME")]
        session: Option<String>,

        /// Path inside the repository the worktree belongs to.
        #[arg(long, default_value = ".", value_name = "PATH")]
        repo: std::path::PathBuf,

        /// Attach to the new session instead of creating it headlessly.
        #[arg(long)]
        attach: bool,

        /// Emit a stable JSON document — branch, path, session, and the seed
        /// pane's `terminal_id` — instead of human text. This is the first
        /// call in a fan-out script, and the id it returns is the pane the
        /// caller then sends its first prompt to. Cannot combine with
        /// `--attach`: an attached session owns stdout.
        #[arg(long, conflicts_with = "attach")]
        json: bool,

        /// Command to run in the new session instead of the default shell.
        #[arg(last = true)]
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
        #[arg(long, default_value = ".", value_name = "PATH")]
        repo: std::path::PathBuf,

        /// Attach to the session instead of only reporting its name.
        #[arg(long)]
        attach: bool,

        /// Emit the same document `worktree new --json` emits, whether the
        /// session was created now or was already live — so a script that
        /// re-enters a fleet gets the seed pane without special-casing.
        #[arg(long, conflicts_with = "attach")]
        json: bool,
    },

    /// Remove a worktree, killing the session bound to it first.
    ///
    /// The session is killed before git runs, because git refuses to remove
    /// a worktree whose files are held open and a shell sitting in that
    /// directory holds it open. Refuses the worktree you are standing in.
    #[command(visible_alias = "rm")]
    Remove {
        /// Worktree path, branch, or derived session name.
        target: String,

        /// Pass --force to git, removing a worktree with local changes.
        #[arg(long)]
        force: bool,

        /// Path inside the repository the worktree belongs to.
        #[arg(long, default_value = ".", value_name = "PATH")]
        repo: std::path::PathBuf,

        /// Emit a stable JSON document instead of human text. A fan-out
        /// teardown script has the same parsing problem creation does.
        #[arg(long)]
        json: bool,
    },
}

/// `phux tag <action>` — list and edit a Terminal's L3 tags.
///
/// Alias policy (ADR-0065 §5): every list/remove registry verb answers to
/// both spellings. This registry's canonical names were the short ones, so
/// the aliases here are the long forms.
#[derive(Debug, Subcommand)]
pub(crate) enum TagAction {
    /// List the tags on each Terminal a selector resolves to.
    #[command(visible_alias = "list")]
    Ls {
        /// Target selector (session, `session:window`, `@id`, `.`, `#tag`).
        target: String,

        #[command(flatten)]
        json: JsonOpt,
    },

    /// Add one or more tags to each Terminal a selector resolves to.
    Add {
        /// Target selector.
        target: String,
        /// Tags to add (the leading `#` is optional).
        #[arg(required = true)]
        tags: Vec<String>,

        #[command(flatten)]
        json: JsonOpt,
    },

    /// Remove one or more tags from each Terminal a selector resolves to.
    #[command(visible_alias = "remove")]
    Rm {
        /// Target selector.
        target: String,
        /// Tags to remove (the leading `#` is optional).
        #[arg(required = true)]
        tags: Vec<String>,

        #[command(flatten)]
        json: JsonOpt,
    },
}

/// `phux plugin <action>` — local plugin registry lifecycle.
#[derive(Debug, Subcommand)]
pub(crate) enum PluginAction {
    /// List configured plugin manifests.
    #[command(visible_alias = "ls")]
    List {
        /// Emit a stable JSON document instead of human text.
        #[arg(long)]
        json: bool,
    },

    /// Add or update a manifest entry in `config.toml`.
    Link {
        /// Path to a `phux-plugin.toml` file, or a directory containing one.
        manifest: std::path::PathBuf,

        /// Register the plugin but leave it disabled.
        #[arg(long)]
        disabled: bool,

        /// Emit a stable JSON document instead of human text.
        #[arg(long)]
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
        #[arg(value_name = "REF")]
        reference: String,

        /// Branch or tag to clone (git sources only).
        #[arg(long, value_name = "REV")]
        rev: Option<String>,

        /// Install and link the plugin but leave it disabled.
        #[arg(long)]
        disabled: bool,

        /// Emit a stable JSON document instead of human text.
        #[arg(long)]
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
        #[arg(long)]
        json: bool,
    },

    /// Remove a configured plugin by id.
    // `rm` / `remove` are the alias-policy spellings (ADR-0065 §5): every
    // remove-shaped registry verb answers to both, and this registry's
    // canonical name predates the policy. A code comment, not a doc comment —
    // ADR ids must not leak into `--help` (see `help_inventory`).
    #[command(visible_aliases = ["rm", "remove"])]
    Unlink {
        /// Plugin id from its manifest.
        id: String,

        /// Emit a stable JSON document instead of human text.
        #[arg(long)]
        json: bool,
    },

    /// Enable a configured plugin by id.
    Enable {
        /// Plugin id from its manifest.
        id: String,

        /// Emit a stable JSON document instead of human text.
        #[arg(long)]
        json: bool,
    },

    /// Disable a configured plugin by id.
    Disable {
        /// Plugin id from its manifest.
        id: String,

        /// Emit a stable JSON document instead of human text.
        #[arg(long)]
        json: bool,
    },

    /// Validate one manifest, or every configured manifest when omitted.
    Validate {
        /// Optional path to a `phux-plugin.toml` file or plugin directory.
        manifest: Option<std::path::PathBuf>,

        /// Emit a stable JSON document instead of human text.
        #[arg(long)]
        json: bool,
    },
}

/// `phux workspace <action>` — workspace inspection and session archives.
#[derive(Debug, Subcommand)]
pub(crate) enum WorkspaceAction {
    /// Inspect the git repository and its checked-out worktrees.
    Inspect {
        /// Path inside the repository or worktree to inspect.
        #[arg(default_value = ".")]
        path: std::path::PathBuf,

        /// Emit a stable JSON document instead of human text.
        #[arg(long)]
        json: bool,
    },

    /// Save the running phux workspace as a JSON archive.
    Save {
        /// Write the archive to a path instead of stdout.
        #[arg(long, short = 'o', value_name = "PATH")]
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
/// or subscribed there is no consumer for a `TERMINAL_OUTPUT` or an `EVENT`,
/// and no verb here can act on one.
pub(crate) async fn command_on(
    conn: &mut Connection,
    request_id: u32,
    command: WireCommand,
) -> Result<CommandResult, AttachError> {
    let (result, interleaved) = conn.request(request_id, command).await?.into_parts();
    for message in phux_client::state::degradation_notices(&interleaved) {
        eprintln!("phux: warning: partial results — {message}");
    }
    Ok(result)
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
) -> Result<phux_protocol::ids::TerminalId, ExitCode> {
    let (snapshot, degradation) = phux_client::state::get_state(socket_path)
        .await
        .map_err(|err| json_err::report_no_server(json, &err, socket_path, verb))?
        .into_parts();
    let candidates = resolve_targets(socket_path, selector, &snapshot).await;
    let picked = crate::selector::pick_target_pane(&candidates, &snapshot.focused_pane)
        .ok_or_else(|| partial::report_target_miss_keeping_status_for(json, None, &degradation))?;
    // A hit is still worth a word: the pane we picked is the best of what a
    // partial fleet offered, and the user is about to act on it.
    partial::warn_partial_view(verb, &degradation);
    Ok(picked)
}

/// Resolve `selector` to its `TerminalId`s, fetching L3 tag metadata first
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
) -> Vec<phux_protocol::ids::TerminalId> {
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
                socket_path.display(),
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
            Selector::TerminalId(42),
        );
        assert_eq!(
            parse_selector(Some("devbox/@42")).unwrap(),
            Selector::SatelliteTerminalId {
                host: "devbox".to_owned(),
                id: 42,
            },
        );
        // ADR-0075: `%name` addresses one agent by the name its
        // `phux.agent/v1` record carries. Singular by construction — it
        // resolves through `phux_client::selector::resolve_agent`, never
        // through `resolve_targets` + `pick_target_pane`.
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
