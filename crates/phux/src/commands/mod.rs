use std::path::Path;
use std::process::ExitCode;

use clap::{Subcommand, ValueEnum};
use phux_client::attach::AttachError;
use phux_client::attach::connection::Connection;
use phux_protocol::wire::frame::{
    Command as WireCommand, CommandResult, FrameKind, TerminalSignal,
};

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

/// Split axis for explicit `spawn` / `launch` placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum SpawnSplit {
    Horizontal,
    Vertical,
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
pub(crate) mod kill;
pub(crate) mod launch;
pub(crate) mod ls;
pub(crate) mod new;
pub(crate) mod overlay;
pub(crate) mod pair;
pub(crate) mod paste;
pub(crate) mod plugin;
pub(crate) mod remote;
pub(crate) mod rename;
pub(crate) mod run;
pub(crate) mod satellite;
pub(crate) mod send_keys;
pub(crate) mod server;
pub(crate) mod service;
pub(crate) mod snapshot;
pub(crate) mod spatial;
pub(crate) mod spawn;
pub(crate) mod stdio_bridge;
pub(crate) mod supervise;
pub(crate) mod tag;
pub(crate) mod toml_registry;
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

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Attach to a session (interactive).
    ///
    /// With no name, attaches to the most-recently-focused session,
    /// auto-spawning a server if none is running. Requires a TTY.
    #[command(group = clap::ArgGroup::new("remote").args(["quic", "ws"]).multiple(false))]
    #[command(visible_alias = "a")]
    Attach {
        /// Session name (matches the name used at creation time).
        ///
        /// Omit to attach to the most-recently-focused session.
        session: Option<String>,

        /// Override the UDS path. Defaults to `$XDG_RUNTIME_DIR/phux/phux.sock`
        /// (or `/tmp/phux-$USER/phux.sock` if `XDG_RUNTIME_DIR` isn't set).
        #[arg(long)]
        socket: Option<std::path::PathBuf>,

        /// Attach over QUIC to a remote `phux server --quic` listener at this
        /// `HOST:PORT` instead of the local Unix socket. HOST may be an IP
        /// literal or a DNS name (e.g. a Tailscale `MagicDNS` name), resolved
        /// before dialing. QUIC is always TLS 1.3-encrypted. A target
        /// resolving to loopback trusts the server's self-signed cert for
        /// local dev; any routable address requires `--cert-fingerprint`
        /// (the value `phux pair` prints on the server host).
        #[arg(long, value_name = "HOST:PORT", conflicts_with = "socket")]
        quic: Option<String>,

        /// Attach over WebSocket to a `phux server --listen` endpoint. Use
        /// `ws://HOST:PORT` for loopback dev, or `wss://HOST:PORT` with
        /// `--token` and `--cert-fingerprint` for routable remote attach. This
        /// is the TCP fallback when UDP/QUIC is blocked.
        #[arg(long, value_name = "URL", conflicts_with = "socket")]
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

        /// Override the UDS path. Defaults to `$PHUX_SOCKET`, else
        /// `$XDG_RUNTIME_DIR/phux/phux.sock` (or `/tmp/phux-$USER/phux.sock`
        /// if `XDG_RUNTIME_DIR` isn't set).
        #[arg(long)]
        socket: Option<std::path::PathBuf>,

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
        /// Emit the session list as stable, versioned JSON instead of
        /// human text.
        #[arg(long)]
        json: bool,

        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
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

        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,

        /// Create without attaching and print the seed pane's id as JSON.
        /// Requires `-s NAME`.
        #[arg(long)]
        json: bool,

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
        /// from `phux satellite list`, on a server running `--hub`).
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

        /// Emit the result as JSON:
        /// `{"terminal_id": N, "satellite": "NAME" | null}`.
        #[arg(long)]
        json: bool,

        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,

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

        /// Emit the result as JSON instead of the human view.
        #[arg(long)]
        json: bool,

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

        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,

        /// Extra arguments appended to the agent command, after `--`.
        #[arg(last = true)]
        extra: Vec<String>,
    },

    /// Kill a session, window, or pane.
    ///
    /// `TARGET` uses the selector grammar (`docs/consumers/tui.md` §3):
    /// `name`, `name:N`, `name:N.M`, `name:tag`, `@N`, `.`. The selector
    /// is resolved client-side against a server-state snapshot to a set of
    /// Terminals; the server is then asked to kill each.
    Kill {
        /// What to kill (selector).
        target: String,

        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
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
        /// Use a horizontal split (stacked panes; the default).
        #[arg(long, conflicts_with = "vertical")]
        horizontal: bool,
        /// Use a vertical split (side-by-side panes).
        #[arg(long, conflicts_with = "horizontal")]
        vertical: bool,
        /// Fraction assigned to TARGET; must be strictly between 0 and 1.
        #[arg(long, default_value_t = 0.5)]
        ratio: f32,
        /// Emit a schema-versioned JSON result or error.
        #[arg(long)]
        json: bool,
        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
    },

    /// Move one existing pane beside another in the same session.
    ///
    /// SOURCE is collapsed out of its current tree position and inserted
    /// beside TARGET. Both selectors must resolve to exactly one local pane.
    #[command(name = "move-pane")]
    MovePane {
        /// Pane to relocate.
        source: String,
        /// Existing destination pane.
        target: String,
        /// Use a horizontal destination split (the default).
        #[arg(long, conflicts_with = "vertical")]
        horizontal: bool,
        /// Use a vertical destination split.
        #[arg(long, conflicts_with = "horizontal")]
        vertical: bool,
        /// Fraction assigned to TARGET; must be strictly between 0 and 1.
        #[arg(long, default_value_t = 0.5)]
        ratio: f32,
        /// Emit a schema-versioned JSON result or error.
        #[arg(long)]
        json: bool,
        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
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
        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
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

        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
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

        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
    },

    /// Give back the input wheel of a pane.
    ///
    /// Releases the input lease taken with `phux take`, returning the pane to
    /// open input. A no-op if you do not hold the lease. TARGET is a selector.
    Give {
        /// Target selector (resolves to one pane).
        target: String,

        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
    },

    /// Signal a pane's process group.
    ///
    /// Delivers a POSIX signal to the program running in the resolved pane and
    /// every subprocess it spawned — distinct from `phux kill`, which destroys
    /// the pane. `freeze` (SIGSTOP) pauses the process mid-step; `resume`
    /// (SIGCONT) lets it run again — the reversible brake for an agent about to
    /// do something rash. TARGET is a selector.
    ///
    ///   phux signal build freeze
    ///   phux signal . kill
    Signal {
        /// Target selector (resolves to one pane).
        target: String,

        /// Which signal to deliver.
        signal: SignalArg,

        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
    },

    /// Graceful-upgrade the running server in place.
    ///
    /// Asks the server to snapshot every pane, re-exec the on-disk binary, and
    /// re-adopt the live PTYs, so the shells / editors / agents in every
    /// session survive a binary update (e.g. after `cargo install` /
    /// `brew upgrade`). Clients briefly disconnect and reconnect.
    Upgrade {
        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
    },

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

        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
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

        /// Emit JSON (stable schema) instead of the human boxed view.
        #[arg(long)]
        json: bool,

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

        /// Emit the CLIENT's composited multi-pane view — the assembled
        /// frame (layout tiling + dividers + status bar) as the human's glass
        /// shows it — as dense structured cells. Unlike the
        /// default side-effect-free read this ATTACHES (drives the headless
        /// client render path). Mutually exclusive with `--cells` /
        /// `--scrollback`; sizes the composite via `--cols` / `--rows`.
        #[arg(long, conflicts_with_all = ["cells", "scrollback"])]
        rendered: bool,

        /// Composited viewport width for `--rendered` (no TTY to measure).
        #[arg(long, value_name = "COLS", default_value_t = 80)]
        cols: u16,

        /// Composited viewport height for `--rendered`.
        #[arg(long, value_name = "ROWS", default_value_t = 24)]
        rows: u16,

        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
    },

    /// Send keys to a pane.
    ///
    /// tmux-shaped: each KEY is a named key (`Enter`, `Tab`, `Escape`,
    /// `Up`, `C-c`, `M-x`, …) or a literal string sent character by
    /// character. TARGET is a selector (see the top-level help); it
    /// resolves client-side to one pane and the events route to it by id,
    /// so the live pane is neither attached nor resized.
    ///
    /// Flags (`--socket`) MUST precede TARGET: KEYS is a trailing var-arg,
    /// so anything after TARGET is taken as a key to send.
    ///
    ///   phux send-keys demo "echo hi" Enter
    ///   phux send-keys work:1.0 C-c
    #[command(name = "send-keys", about = "Send keys to a pane")]
    SendKeys {
        /// Target selector: session, session:window, session:window.pane,
        /// @id, or `.` (focused). `=` is unsupported by headless commands.
        target: String,

        /// Keys to send: named keys and/or literal strings, in order.
        #[arg(trailing_var_arg = true, required = true)]
        keys: Vec<String>,

        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
    },

    /// Paste text into a pane.
    ///
    /// Delivers the payload as ONE paste event to the resolved pane
    /// (`ROUTE_INPUT`), so the live pane is neither attached nor resized.
    /// When the pane's program has bracketed paste (DEC mode 2004) switched
    /// on, the server wraps the payload in paste markers and the program
    /// receives it as a single block — auto-indent stays off and multiline
    /// text arrives intact, unlike `send-keys`, which types character by
    /// character. Without the mode, the raw bytes are delivered as if typed.
    ///
    /// A paste INSERTS; it does not SUBMIT. Paste-aware shells and REPLs
    /// buffer the block until a real Enter — follow with
    /// `phux send-keys TARGET Enter` to run what you pasted.
    ///
    /// TEXT is the payload; omit it to read the payload from stdin.
    /// Payloads are trusted by default (you vouch for content you
    /// composed); `--untrusted` opts into the server's safety gate.
    ///
    ///   phux paste demo 'SELECT count(*) FROM users;'
    ///   git diff | phux paste review
    #[command(about = "Paste text into a pane (bracketed when the pane asks for it)")]
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

        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
    },

    /// Block until a pane meets a condition.
    ///
    /// Polls the side-effect-free screen read — the poll
    /// floor of the event surface: always works, no shell integration.
    /// Exits 0 when met, 124 on `--timeout`. TARGET is a selector (see the
    /// top-level help); omit it for the most-recently-focused session.
    ///
    /// Flags (`--until`, `--idle`, `--timeout`, `--json`, `--socket`) MUST
    /// precede TARGET if you give one.
    ///
    ///   phux wait --until "BUILD SUCCESSFUL" build
    ///   phux wait --idle 750 repl
    #[command(about = "Block until a pane meets a condition")]
    Wait {
        /// Target selector. Omit for the most-recently-focused session.
        #[arg(value_name = "TARGET")]
        session: Option<String>,

        /// Succeed once any visible line contains this substring. NOTE: this
        /// matches ANY visible row, including the shell's echo of a command
        /// you just typed — match on text that appears only in OUTPUT.
        #[arg(long, value_name = "TEXT")]
        until: Option<String>,

        /// Succeed once the screen holds still for this many milliseconds
        /// (the pane has settled). Default when no `--until` is given.
        #[arg(long, value_name = "MS")]
        idle: Option<u64>,

        /// Give up after this many seconds (exit 124). Default: wait forever.
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,

        /// Emit the final screen as JSON instead of staying silent.
        #[arg(long)]
        json: bool,

        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
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
    ///   phux watch build
    ///   phux watch --json work:1.0
    #[command(about = "Stream a pane's live events (bell, title, dirty/idle, lifecycle)")]
    Watch {
        /// Target selector. Omit for the most-recently-focused session.
        #[arg(value_name = "TARGET")]
        session: Option<String>,

        /// Emit one JSON object per line instead of the human form. stdout
        /// stays pure JSON (diagnostics go to stderr).
        #[arg(long)]
        json: bool,

        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
    },

    /// Report that an agent in a pane is waiting on a human answer.
    ///
    /// This is the opt-in hook contract for configured integrations: it emits
    /// the same `asked` event as the `phux-ask` title sentinel without writing
    /// escape sequences into the target terminal. TARGET is resolved
    /// client-side and the command neither attaches nor resizes the pane.
    ///
    ///   phux ask work:1.0 --id deploy --suggest Yes --suggest No "Deploy?"
    ///   phux ask @3 --json "Need approval"
    #[command(about = "Report an agent ask event for a pane")]
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

        /// Emit the reported event as JSON.
        #[arg(long)]
        json: bool,

        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,

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
    ///
    /// Reports the command's exit code, output, and duration.
    /// Brackets the command with sentinels to capture `$?`, so it
    /// assumes a POSIX shell (sh/bash/zsh). The process exit code mirrors
    /// the command's (125 if `phux` gives up on `--timeout`), so
    /// `phux run … && next` composes like a shell. TARGET is a selector
    /// (see the top-level help), resolved client-side to one pane; the
    /// command routes to it by id (no attach, no resize).
    ///
    /// Flags (`--timeout`, `--json`, `--socket`) MUST precede TARGET, or
    /// they are swallowed into the trailing command.
    ///
    ///   phux run build "cargo test"
    ///   phux run --timeout 30 work:1.0 "cargo test"
    #[command(about = "Run a command in a pane and capture its exit code")]
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

        /// Emit the result as JSON instead of the human view.
        #[arg(long)]
        json: bool,

        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
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

    /// Manage configured federation satellites.
    ///
    /// This is a local config operation: it edits `[[satellites]]` entries and
    /// never contacts a running server. Hub routing consumes the registry in a
    /// later federation slice.
    Satellite {
        #[command(subcommand)]
        action: SatelliteAction,
    },

    /// Read and write a Terminal's L3 tags.
    ///
    /// Tags are freeform strings stored as L3 metadata (`phux.tags/v1`),
    /// the server stores them opaquely. Once a Terminal is tagged, the
    /// `#tag` selector addresses every Terminal carrying that tag — e.g.
    /// `phux kill #build`, `phux snapshot #web`.
    Tag {
        /// Override the UDS path. Global so it may precede or follow the
        /// action (`phux tag --socket … add` and `phux tag add … --socket`
        /// both parse).
        #[arg(long, global = true)]
        socket: Option<std::path::PathBuf>,

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
    #[command(name = "stdio-bridge")]
    StdioBridge {
        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
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
        /// human-readable report. This is what `phux enroll` reads over ssh.
        #[arg(long)]
        json: bool,
    },

    /// Set up a remote server over ssh, end to end.
    ///
    /// Confirms phux is installed on HOST, installs its service unit so the
    /// server survives reboot, mints a pairing token there, and registers
    /// the result locally — so `phux attach HOST` works afterwards with no
    /// flags and no hex strings typed by hand. Uses the ssh trust you
    /// already have; it grants nothing ssh did not already grant.
    ///
    /// A host with no reachable listener falls back to an ssh:// entry,
    /// which still gives you sessions that outlive the connection.
    Enroll {
        /// ssh destination, exactly as you would type it after `ssh`
        /// (`mini`, `me@mini`, or a `~/.ssh/config` alias).
        host: String,

        /// Local label to register. Defaults to HOST without any `user@`.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,

        /// Address to register instead of the remote's detected overlay
        /// address. Accepts `HOST:PORT` (dialed over QUIC) or a full
        /// `quic://`/`wss://` URI.
        #[arg(long, value_name = "HOST:PORT")]
        endpoint: Option<String>,

        /// QUIC port to configure on the remote and register.
        #[arg(long, value_name = "PORT", default_value_t = crate::commands::enroll::default_quic_port())]
        quic_port: u16,

        /// Skip installing the remote's service unit. The server will not
        /// come back on its own after a reboot.
        #[arg(long)]
        no_service: bool,

        /// Register an ssh:// entry without contacting the host at all.
        #[arg(long, conflicts_with_all = ["endpoint", "no_service"])]
        ssh_only: bool,

        /// Session to attach on arrival.
        #[arg(long, value_name = "NAME")]
        session: Option<String>,
    },

    /// Manage the registry of remote phux servers this machine attaches to.
    ///
    /// A registered name is what `phux attach <name>` resolves: endpoint,
    /// certificate pin, and a path to the pairing token, stored once.
    /// `phux enroll HOST` writes these entries for you over ssh.
    Remote {
        #[command(subcommand)]
        action: RemoteAction,
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
    ///
    /// The script is generated from the binary's own argument parser, so it
    /// always matches the verbs this build actually accepts. It contacts no
    /// server and reads no config, which is what makes it safe to run from a
    /// shell startup file.
    ///
    /// Install it the way your shell prefers, for example:
    ///
    ///   phux completion zsh  > ~/.zfunc/_phux   (~/.zfunc must be on $fpath)
    ///   phux completion bash > ~/.local/share/bash-completion/completions/phux
    ///   phux completion fish > ~/.config/fish/completions/phux.fish
    ///
    /// Regenerate after upgrading phux; a stale script completes verbs the
    /// installed binary no longer has.
    Completion {
        /// Shell dialect to generate for.
        #[arg(value_name = "SHELL")]
        shell: clap_complete::Shell,
    },

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

        /// Override the UDS path probed for a running server.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
    },

    /// Manage git worktrees and the sessions bound to them.
    ///
    /// Each worktree binds to one session whose name is derived from the
    /// worktree's directory basename. The derivation is a pure function of
    /// the path, so the binding is computed on demand and can never go
    /// stale — phux stores no worktree state and the server knows no git.
    #[command(subcommand)]
    Worktree(WorktreeAction),
}

/// `phux remote <action>` — CRUD over the client-side server registry.
#[derive(Debug, Subcommand)]
pub(crate) enum RemoteAction {
    /// Register a remote server, or replace an entry with the same name.
    Add {
        /// Local label. This is what `phux attach <name>` resolves.
        name: String,

        /// `quic://HOST:PORT`, `wss://HOST:PORT`, or `ssh://HOST`.
        ///
        /// `ssh://` needs no pairing — it rides your existing ssh trust and
        /// re-execs `ssh -t HOST phux attach`. The other two need a token
        /// and a certificate pin.
        endpoint: String,

        /// Absolute path to a file holding the pairing token minted by
        /// `phux pair` on the remote host.
        #[arg(long, value_name = "PATH")]
        token_file: Option<std::path::PathBuf>,

        /// The remote's TLS certificate SHA-256 fingerprint, as printed by
        /// `phux pair`. Required for `quic://` and `wss://`.
        #[arg(long, value_name = "FP")]
        cert_fingerprint: Option<String>,

        /// Session to attach on arrival. Omitted: the remote server's own
        /// last-attach memory decides.
        #[arg(long, value_name = "NAME")]
        session: Option<String>,
    },

    /// List registered remotes.
    List {
        /// Emit JSON on stdout instead of the table.
        #[arg(long)]
        json: bool,
    },

    /// Remove a registered remote. Its token file is left in place.
    Remove {
        /// Registered name.
        name: String,
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
        #[arg(long, value_name = "HOST:PORT")]
        quic: Option<String>,

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

        /// Override the UDS path the supervised server binds.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,

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
    List {
        /// Path inside the repository or worktree to list from.
        #[arg(default_value = ".")]
        path: std::path::PathBuf,

        /// Emit a stable JSON document instead of human text.
        #[arg(long)]
        json: bool,

        /// Override the UDS path consulted for the `bound` column.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
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

        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,

        /// Attach to the new session instead of creating it headlessly.
        #[arg(long)]
        attach: bool,

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

        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,

        /// Attach to the session instead of only reporting its name.
        #[arg(long)]
        attach: bool,
    },

    /// Remove a worktree, killing the session bound to it first.
    ///
    /// The session is killed before git runs, because git refuses to remove
    /// a worktree whose files are held open and a shell sitting in that
    /// directory holds it open. Refuses the worktree you are standing in.
    Remove {
        /// Worktree path, branch, or derived session name.
        target: String,

        /// Pass --force to git, removing a worktree with local changes.
        #[arg(long)]
        force: bool,

        /// Path inside the repository the worktree belongs to.
        #[arg(long, default_value = ".", value_name = "PATH")]
        repo: std::path::PathBuf,

        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
    },
}

/// `phux tag <action>` — list and edit a Terminal's L3 tags.
#[derive(Debug, Subcommand)]
pub(crate) enum TagAction {
    /// List the tags on each Terminal a selector resolves to.
    Ls {
        /// Target selector (session, `session:window`, `@id`, `.`, `#tag`).
        target: String,
    },

    /// Add one or more tags to each Terminal a selector resolves to.
    Add {
        /// Target selector.
        target: String,
        /// Tags to add (the leading `#` is optional).
        #[arg(required = true)]
        tags: Vec<String>,
    },

    /// Remove one or more tags from each Terminal a selector resolves to.
    Rm {
        /// Target selector.
        target: String,
        /// Tags to remove (the leading `#` is optional).
        #[arg(required = true)]
        tags: Vec<String>,
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
        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,

        /// Write the archive to a path instead of stdout.
        #[arg(long, short = 'o', value_name = "PATH")]
        output: Option<std::path::PathBuf>,
    },

    /// Restore missing sessions from a workspace archive.
    Restore {
        /// JSON archive path, or '-' to read from stdin.
        archive: std::path::PathBuf,

        /// Override the UDS path.
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
    },
}

/// `phux satellite <action>` — local satellite registry lifecycle.
#[derive(Debug, Subcommand)]
pub(crate) enum SatelliteAction {
    /// List configured satellites.
    #[command(visible_alias = "ls")]
    List {
        /// Emit a stable JSON document instead of human text.
        #[arg(long)]
        json: bool,
    },

    /// Add or update a satellite endpoint in `config.toml`.
    ///
    /// Updating replaces the whole entry, so repeat `--token-file` /
    /// `--cert-fingerprint` when re-adding a name or the auth material
    /// is cleared.
    Add {
        /// Hub-local satellite name.
        name: String,

        /// Endpoint URI, e.g. `<ssh://devbox>`, `<quic://host:8788>`, or
        /// `<wss://host:8787>`.
        endpoint: String,

        /// Register the satellite but leave it disabled.
        #[arg(long)]
        disabled: bool,

        /// Path to a file holding the pairing bearer token for this
        /// satellite, minted by running `phux pair` on the satellite host.
        /// The file holds one hex token and should be
        /// owner-only (0600); only the path lands in `config.toml` — the
        /// token itself is never written to config or printed.
        #[arg(long, value_name = "PATH")]
        token_file: Option<std::path::PathBuf>,

        /// SHA-256 fingerprint of the satellite server's TLS certificate,
        /// as printed by `phux pair` on the satellite host. Pins the
        /// certificate for routable endpoints; not a secret.
        #[arg(long, value_name = "FINGERPRINT")]
        cert_fingerprint: Option<String>,

        /// Emit a stable JSON document instead of human text.
        #[arg(long)]
        json: bool,
    },

    /// Remove a configured satellite by name.
    #[command(visible_alias = "rm")]
    Remove {
        /// Hub-local satellite name.
        name: String,

        /// Emit a stable JSON document instead of human text.
        #[arg(long)]
        json: bool,
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
/// skipping any unrelated frames the server interleaves (SPEC §5).
pub(crate) async fn command_on(
    conn: &mut Connection,
    request_id: u32,
    command: WireCommand,
) -> Result<CommandResult, AttachError> {
    conn.send(&FrameKind::Command {
        request_id,
        command,
    })
    .await?;
    loop {
        match conn.recv().await? {
            FrameKind::CommandResult {
                request_id: got,
                result,
            } if got == request_id => return Ok(result),
            _ => {}
        }
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
pub(crate) fn report_no_server(err: &AttachError, socket_path: &Path, verb: &str) -> ExitCode {
    match err {
        AttachError::Io(io_err)
            if matches!(
                io_err.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound,
            ) =>
        {
            eprintln!("phux: no server running at {}", socket_path.display());
        }
        AttachError::Disconnected => {
            eprintln!("phux: server closed the connection during {verb}");
        }
        other => eprintln!("phux: {verb} failed: {other}"),
    }
    ExitCode::FAILURE
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
pub(crate) async fn resolve_target(
    socket_path: &Path,
    selector: &crate::selector::Selector,
    verb: &str,
) -> Result<phux_protocol::ids::TerminalId, ExitCode> {
    let snapshot = phux_client::state::get_state(socket_path)
        .await
        .map_err(|err| report_no_server(&err, socket_path, verb))?;
    let candidates = resolve_targets(socket_path, selector, &snapshot).await;
    crate::selector::pick_target_pane(&candidates, &snapshot.focused_pane).ok_or_else(|| {
        eprintln!("phux: no such target");
        ExitCode::FAILURE
    })
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
    match err {
        AttachError::Io(io_err)
            if matches!(
                io_err.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound,
            ) =>
        {
            eprintln!(
                "phux: no server at {}. Start one with: phux server --session {session}",
                socket_path.display(),
            );
        }
        AttachError::Refused(message) => {
            eprintln!("phux: server refused attach: {message}");
        }
        AttachError::NotATty => {
            eprintln!("phux: attach requires an interactive terminal (stdin is not a TTY).",);
        }
        other => {
            eprintln!("phux: attach failed: {other}");
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::commands::parse_selector;
    use crate::selector::{Selector, WindowRef};

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
    }
}
