//! `phux server`, `phux relay`, and `phux service`.

use crate::commands::{JsonOpt, relay};
use usage::{Args, Subcommands};

/// Run a server in the foreground
///
/// Binds a Unix domain socket, pre-seeds a session whose initial
/// pane spawns the user's `$SHELL` inside a real PTY, and serves
/// `ATTACH` requests until Ctrl-C.
#[allow(
    clippy::struct_excessive_bools,
    reason = "one independent CLI flag per bool, as the verb declares them"
)]
#[derive(Debug, Args)]
pub(crate) struct ServerArgs {
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
    pub(crate) ensure: bool,

    /// Emit the stable coordinator-availability result document.
    /// On failure, stdout stays empty and stderr carries one JSON error
    /// object. Requires `--ensure`.
    #[usage(flatten)]
    pub(crate) json: JsonOpt,

    /// Name of the pre-seeded session. Matches what
    /// `phux attach <name>` will request.
    #[usage(long, default = "default")]
    pub(crate) session: String,

    /// Start with no pre-seeded session. `phux new --empty` starts a
    /// server this way when none is running, so the only session is the
    /// empty one it asked for.
    #[usage(long, hide, conflicts("--seed-command"))]
    pub(crate) no_seed: bool,

    /// Also accept WebSocket clients on this `HOST:PORT` (the UDS stays
    /// on). Needs `[policy] mode = "paired"`, without which the server
    /// refuses to start: every connection then presents an enrolled
    /// client certificate over TLS, and a routable address (e.g.
    /// `0.0.0.0:8787`) also requires a `phux pair` token.
    /// Overrides `$PHUX_WS_ADDR`.
    #[usage(long, value_name = "HOST:PORT")]
    pub(crate) listen: Option<std::net::SocketAddr>,

    /// Also accept QUIC clients on this `HOST:PORT` (the UDS stays on).
    /// Needs `[policy] mode = "paired"`: every connection presents an
    /// enrolled client certificate. QUIC is always TLS 1.3-encrypted; a
    /// routable address also requires a `phux pair` token sent as the
    /// stream's opening preamble. Overrides `$PHUX_QUIC_ADDR`.
    #[usage(long, value_name = "HOST:PORT")]
    pub(crate) quic: Option<std::net::SocketAddr>,

    /// Also accept WebTransport (HTTP/3 over QUIC) clients on this
    /// `HOST:PORT` (the UDS stays on) — the browser's door to QUIC-class
    /// transport; the browser client dials it, falling back to WebSocket.
    /// Needs `[policy] mode = "paired"`, under which a session is
    /// admitted only through an end-to-end TLS session presenting an
    /// enrolled certificate, which a browser cannot open yet. Always TLS
    /// 1.3-encrypted; a routable address also requires a `phux pair`
    /// token carried in the CONNECT request (`Authorization: Bearer`
    /// from native consumers, `?token=<hex>` on the session URL from
    /// browsers). Overrides `$PHUX_WT_ADDR`.
    #[usage(long, value_name = "HOST:PORT")]
    pub(crate) webtransport: Option<std::net::SocketAddr>,

    /// Dial one relay outbound on `HOST:PORT`. If a matching
    /// `[[connector]]` entry exists, its token file and certificate pin
    /// are used; otherwise only a loopback endpoint is accepted for
    /// unauthenticated development. Needs `[policy] mode = "paired"`.
    /// Without this flag, every configured connector is supervised
    /// independently.
    #[usage(long, value_name = "HOST:PORT")]
    pub(crate) connect: Option<String>,

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
    pub(crate) hub: bool,

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
    pub(crate) exit_after_idle: Option<u64>,

    /// Deprecated: the server now keeps its sessions across a restart
    /// itself, and `phux restore` relaunches what a restart ended. The
    /// flag still works, as an export schedule: the server keeps the
    /// archive `phux archive export` writes at PATH, rewriting it
    /// atomically (temp file, fsync, rename) a few seconds after a
    /// change and at least once a minute. On a fresh start it imports
    /// the sessions PATH has and the server lacks, starting none of
    /// them, as `phux archive import` does. A development build refuses
    /// a production path.
    #[usage(long, value_name = "PATH", conflicts("--ensure"))]
    pub(crate) autosave: Option<std::path::PathBuf>,

    /// Detach from the controlling terminal via `setsid(2)` before
    /// binding. Set by the auto-spawn path so the server outlives
    /// the launching client's terminal; a foreground `phux server`
    /// run by hand leaves this off so Ctrl-C still works.
    #[usage(long, hide, conflicts("--ensure"))]
    pub(crate) daemonize: bool,

    /// Run this command (via `$SHELL -c`) as the pre-seeded session's
    /// initial program instead of a bare shell. The naked-`phux`
    /// auto-spawn path passes `defaults.spawn-on-attach` here;
    /// `phux new` deliberately does not, so an
    /// explicitly-created session still gets a shell.
    #[usage(long, hide, conflicts("--ensure"))]
    pub(crate) seed_command: Option<String>,

    /// Graceful-upgrade resume: read the handoff state blob
    /// from this inherited descriptor, adopt the inherited listener, and
    /// rebuild the live session tree instead of starting fresh. Set by
    /// the upgrade orchestrator's re-exec; never passed by hand.
    #[usage(long, hide, conflicts("--ensure"))]
    pub(crate) resume: Option<std::os::fd::RawFd>,
}

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
#[derive(Debug, Args)]
pub(crate) struct RelayArgs {
    #[usage(subcommand)]
    pub(crate) action: relay::RelayAction,
}

/// Keep a server running across logout and reboot
///
/// Generates this host's native per-user service unit — a `launchd`
/// `LaunchAgent` on macOS, a systemd user unit on Linux — with the
/// server's environment baked in, so a rebooted host comes back with a
/// server instead of waiting for someone to log in and start one.
/// A restarted server has no terminals: every pane's process died with
/// the host. `install --restore` brings back session names, layout, and
/// cwd, not running processes.
#[derive(Debug, Args)]
pub(crate) struct ServiceArgs {
    #[usage(subcommand)]
    pub(crate) action: ServiceAction,
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
        /// token. Prefer this over `--listen` where UDP is open. A remote
        /// listener needs `[policy] mode = "paired"`: with no mode set,
        /// install writes it; `local` is refused.
        // Parsed as `SocketAddr` so a bad address fails here, not at the
        // supervised server's first start.
        #[usage(long, value_name = "HOST:PORT")]
        quic: Option<std::net::SocketAddr>,

        /// Accept WebSocket clients on this `HOST:PORT`. The fallback for
        /// networks that block UDP.
        #[usage(long, value_name = "HOST:PORT")]
        listen: Option<String>,

        /// Keep a session archive beside the server. The server runs with
        /// `--autosave`, exporting the archive atomically a few seconds after
        /// its sessions change, and the unit exports once more on stop. The
        /// server keeps its sessions across a restart either way; after one,
        /// `phux restore` relaunches the processes it ended.
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
