//! Server runtime: tokio current-thread executor + Unix-domain-socket
//! listener (`phux-byc.3`).
//!
//! This module wires the minimum surface needed to host clients:
//!
//! * Construct a single-threaded tokio runtime
//!   (`tokio::runtime::Builder::new_current_thread`) per ADR-0003 (one server
//!   per user, one event loop).
//! * Bind a `SOCK_STREAM` Unix domain socket at a resolved path under
//!   `$XDG_RUNTIME_DIR` (falling back to `/tmp/phux-$UID/`), as described in
//!   `docs/spec/proto.md` §4 (Transport).
//! * Accept connections and spawn a per-client task on a
//!   [`tokio::task::LocalSet`] (per ADR-0014) that reads length-prefixed
//!   frames (`docs/spec/proto.md` §5), echoes `PING` with `PONG` (`docs/spec/proto.md` §7.4),
//!   and handles `ATTACH` / `DETACH` by talking to the per-terminal
//!   `TerminalActor`s (`phux-byc.8`). The
//!   remaining catalog (`INPUT_KEY`, etc.) is recorded against the
//!   terminal's input log but the PTY write side lands in `phux-byc.5`.
//! * Unlink the socket file on clean shutdown and refuse to start over an
//!   already-live socket.
//!
//! Frame types come from `phux_protocol::wire` (ADR-0008): the protocol crate
//! is the single source of truth for what bytes go on the wire.
#![allow(
    clippy::future_not_send,
    reason = "single-threaded tokio runtime per ADR-0003; Send/Sync not required"
)]

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use phux_protocol::wire::frame::{ErrorCode, FrameKind};
use tokio::net::UnixListener;
use tokio::runtime::Builder;
use tokio::task::LocalSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};

use crate::state::{Outbound, SharedState};

pub mod attach;
pub mod client;
pub mod commands;
pub mod input_lane;
mod resume;
mod upgrade;
mod upload;

pub(crate) use attach::*;
pub(crate) use client::*;
pub use commands::*;

/// Timeout for the "is the socket still live?" liveness probe used when an
/// existing socket file is encountered during bind.
pub(crate) const STALE_PROBE_TIMEOUT: Duration = Duration::from_millis(50);

/// A boxed, type-erased per-transport accept loop future. The accept loops over
/// the (heterogeneous) UDS / WebSocket / QUIC listeners share one `Output` but
/// have distinct concrete future types; boxing lets [`futures_util::future::select_all`]
/// drive them as one homogeneous set. The lifetime ties each future to the
/// listener it borrows for the duration of the `run_until` block.
type AcceptLoopFuture<'a> = std::pin::Pin<Box<dyn Future<Output = Result<(), ServerError>> + 'a>>;

/// Configuration for [`ServerRuntime`].
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Filesystem path to bind the Unix domain socket at.
    pub socket_path: PathBuf,
    /// Optional session name to pre-seed in the registry before clients
    /// connect. When `Some(name)`, the server creates a session by that
    /// name with one window and one pane during startup (`phux-byc.4`).
    ///
    /// Tests use this to launch a server whose registry already contains
    /// a known session to attach to without first issuing a `COMMAND` (the
    /// `COMMAND` message is not implemented yet).
    pub pre_seeded_session: Option<String>,
    /// When `true`, the pre-seeded session's initial pane spawns the
    /// resolved default shell ([`Self::shell`]) inside a real
    /// PTY (see [`seed_session_with_pty`] / [`crate::terminal_actor::TerminalActor::new_with_default_shell`]).
    /// When `false`, the pre-seeded session's pane has a no-PTY actor —
    /// the actor exists for snapshot/input plumbing but no child process
    /// runs and no bytes flow.
    ///
    /// The PTY path is what the `phux server` binary subcommand needs to
    /// actually be useful to a human attacher; tests and example code
    /// keep the default (no-PTY) so they can exercise the registry/wire
    /// surface without forking shells.
    pub seed_with_pty: bool,
    /// When `Some` and [`Self::seed_with_pty`] is `true`, the pre-seeded
    /// pane spawns this command instead of the user's default shell.
    /// Mostly useful for integration tests that need a deterministic
    /// PTY-backed actor (e.g. `cat`, which echoes input → output for a
    /// crisp wire round-trip assertion).
    ///
    /// Ignored when `seed_with_pty` is `false`. `None` (the default)
    /// falls back to [`crate::terminal_actor::default_shell_command`]
    /// over the resolved default shell ([`Self::shell`]).
    pub seed_command: Option<portable_pty::CommandBuilder>,
    /// Lines of scrollback retained per pane (`defaults.history-limit`,
    /// SPEC DESIGN.md §4.2). Threaded into every `TerminalActor`'s
    /// scrollback cap at construction — both the pre-seeded session and
    /// any session created later via `AttachTarget::CreateIfMissing` or
    /// `SPAWN_TERMINAL`. The binary populates this from
    /// `phux_config`'s `defaults.history-limit`;
    /// [`Self::with_default_socket`] uses the schema default.
    pub history_limit: u32,
    /// How a freshly-spawned pane chooses its working directory
    /// (`defaults.cwd-inheritance`, SPEC DESIGN.md). Threaded into
    /// shared state so `SPAWN_TERMINAL` resolves the new pane's CWD when
    /// the wire frame leaves `cwd` unset:
    /// [`phux_config::CwdInheritance::InheritFocused`] reads the spawning
    /// client's focused pane's live PTY working directory via a kernel
    /// query ([`crate::cwd_query`]); the other modes pick `$HOME` or the
    /// session root. The binary populates this from
    /// `phux_config`'s `defaults.cwd-inheritance`;
    /// [`Self::with_default_socket`] uses the schema default.
    pub cwd_inheritance: phux_config::CwdInheritance,
    /// `TERM` advertised to the inner program of every server-spawned pane
    /// (`defaults.term`, phux-ign). Threaded into shared state so the seed
    /// session, attach-time `CreateIfMissing`, and `SPAWN_TERMINAL` apply
    /// it as the PTY's `TERM` baseline. A per-spawn `SPAWN_TERMINAL.env`
    /// entry for `TERM` overrides it. The binary populates this from
    /// `phux_config`'s `defaults.term`; [`Self::with_default_socket`] uses
    /// the schema default (`xterm-256color`).
    pub term: String,
    /// Resolved default shell for server-spawned panes (phux-i0e8.4.1):
    /// `defaults.shell` when configured, else `$SHELL`, else `/bin/sh` —
    /// see [`crate::terminal_actor::resolve_shell`]. Threaded into shared
    /// state so the seed session, attach-time `CreateIfMissing`,
    /// `SESSION_CREATE_KEY`, and a command-less `SPAWN_TERMINAL` all run
    /// it. A wire `command` always wins over this default. The binary
    /// populates this from its single config load;
    /// [`Self::with_default_socket`] resolves with no configured value
    /// (`$SHELL`, else `/bin/sh`).
    pub shell: String,
    /// How a Terminal viewed by clients of differing sizes resolves its one
    /// authoritative PTY geometry (`defaults.window-size`, phux-nk07).
    /// Threaded into shared state so `handle_viewport_resize` applies the
    /// policy across every subscriber's viewport. The binary populates this
    /// from `phux_config`'s `defaults.window-size`; [`Self::with_default_socket`]
    /// uses the schema default ([`phux_config::WindowSize::Smallest`]).
    pub window_size: phux_config::WindowSize,
    /// Optional policy extension bundle. When `None`, the server uses the
    /// default permissive policy (allow everything, audit nothing).
    pub policy_bundle: Option<crate::policy::PolicyBundle>,
    /// Event-hook catalog (`docs/consumers/tui.md` §9, phux-r82.1): config
    /// `[[hooks.<name>]]` entries plus enabled plugin manifests'
    /// `[[events]]`, resolved by [`crate::hooks::HookCatalog::from_config`].
    /// When non-empty the runtime spawns the hook dispatcher at startup and
    /// registers its handle in shared state; when empty (the default) no
    /// dispatcher task exists and firing events is a no-op.
    pub hook_catalog: crate::hooks::HookCatalog,
    /// Opt-in lifetime for an **ephemeral** server: exit once no client
    /// connection has been open for this long, whether or not panes are
    /// still alive (`phux server --exit-after-idle <SECS>`, ADR-0063).
    ///
    /// `None` (the default) is the tmux contract: the server lives until its
    /// last pane is reaped ([`crate::state::ServerState::has_served_client`]
    /// gates that). That default is deliberately untouched — a human's
    /// multiplexer must not vanish because they walked away. This field is
    /// for the other caller: a harness that bootstraps a server per run on a
    /// private socket and has no way to guarantee its own cleanup step runs.
    ///
    /// The clock is armed at construction and re-armed whenever the last
    /// connection closes, so it covers both "never had a client" and "last
    /// client left". Expiry cancels the root token — the identical path
    /// Ctrl-C takes, so pane teardown is the graceful one.
    pub exit_after_idle: Option<Duration>,
}

/// The server's effective opt-in runtime flags, captured from the running
/// [`ServerRuntime`] configuration at startup (phux-v45.10).
///
/// A graceful upgrade (ADR-0032) re-execs the current binary; the new image
/// must be started with the same transport, connector, and federation surface
/// the old one was serving, or `--listen` / `--quic` / `--webtransport` /
/// `--connect` / `--hub` silently vanish across `phux server upgrade`. These
/// are derived from the runtime's own fields — the values the builder methods
/// ([`ServerRuntime::listen_ws`], [`ServerRuntime::listen_quic`],
/// [`ServerRuntime::listen_webtransport`], [`ServerRuntime::connectors`],
/// [`ServerRuntime::hub`]) actually applied — not from a stashed copy of the
/// original argv, so config-derived
/// state stays consistent with what the server is really running.
///
/// Environment-derived fallbacks (`PHUX_WS_ADDR`, `PHUX_QUIC_ADDR`,
/// `PHUX_WT_ADDR`) are deliberately *not* captured here: the environment
/// survives the `execve`, so the resumed image re-derives them with the same
/// precedence (explicit flag wins over environment) as the original start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeFlags {
    /// WebSocket listen address from `phux server --listen <ADDR>`
    /// ([`ServerRuntime::listen_ws`]). Re-emitted as `--listen` on resume.
    pub ws_addr: Option<SocketAddr>,
    /// QUIC listen address from `phux server --quic <ADDR>`
    /// ([`ServerRuntime::listen_quic`]). Re-emitted as `--quic` on resume.
    pub quic_addr: Option<SocketAddr>,
    /// WebTransport listen address from `phux server --webtransport <ADDR>`
    /// ([`ServerRuntime::listen_webtransport`]). Re-emitted as
    /// `--webtransport` on resume. Not feature-gated: a phux-server built
    /// without the `webtransport` feature ignores the address at the builder
    /// (with a warning), so this stays `None` there and nothing is
    /// re-emitted.
    pub wt_addr: Option<SocketAddr>,
    /// Ad-hoc relay endpoint from `phux server --connect HOST:PORT`.
    /// Re-emitted on resume; configured `[[connector]]` entries are re-read
    /// from disk by the new image.
    pub connect: Option<String>,
    /// Federation-hub mode from `phux server --hub` ([`ServerRuntime::hub`]).
    /// Re-emitted as `--hub` on resume; the resumed image re-reads and
    /// re-validates the `[[satellites]]` registry from config, exactly like a
    /// fresh `--hub` start.
    pub hub: bool,
    /// Ephemeral-server lifetime from `phux server --exit-after-idle <SECS>`
    /// ([`ServerConfig::exit_after_idle`]). Re-emitted as `--exit-after-idle`
    /// on resume, rounded to whole seconds because that is the flag's unit.
    ///
    /// Captured for the same reason as the listen addresses: a graceful
    /// upgrade of a harness server that silently dropped its lifetime would
    /// turn a bounded daemon into an immortal one, which is precisely the
    /// bug being fixed. Sub-second values (reachable only through the
    /// library API, which tests use) round up to one second rather than to
    /// zero — a resumed server must not become *more* eager to exit than the
    /// one it replaced, and never `--exit-after-idle 0`.
    pub exit_after_idle: Option<Duration>,
}

impl ServerConfig {
    /// Build a config with `socket_path` resolved via [`default_socket_path`]
    /// and no pre-seeded session.
    #[must_use]
    pub fn with_default_socket() -> Self {
        Self {
            socket_path: default_socket_path(),
            pre_seeded_session: None,
            seed_with_pty: false,
            seed_command: None,
            history_limit: phux_config::DefaultsCfg::default().history_limit,
            cwd_inheritance: phux_config::CwdInheritance::default(),
            term: phux_config::DefaultsCfg::default().term,
            shell: crate::terminal_actor::resolve_shell(None),
            window_size: phux_config::WindowSize::default(),
            policy_bundle: None,
            hook_catalog: crate::hooks::HookCatalog::default(),
            exit_after_idle: None,
        }
    }
}

/// Floor on the idle watchdog's re-check interval.
///
/// The watchdog normally sleeps exactly as long as the remaining idle
/// budget, so the steady state is one timer per idle window, not a poll
/// loop. The floor only matters when that remaining budget has already
/// reached zero but the deadline check has not yet fired (a connection that
/// opened and closed between the two), where sleeping `Duration::ZERO`
/// would spin the runtime. 50ms is far below any plausible
/// `--exit-after-idle` and far above a scheduler tick.
const IDLE_WATCH_MIN_INTERVAL: Duration = Duration::from_millis(50);

/// Exit the server once it has been unattended for `idle_limit` (ADR-0063,
/// `phux server --exit-after-idle`).
///
/// Spawned on the `LocalSet` only when the operator opted in. "Unattended"
/// is *zero open client connections*, not *no attached clients*: one-shot
/// control verbs never enter `ServerState::attached`, so a harness that only
/// ever drives the server with `phux send-keys` would otherwise be reaped
/// mid-script. The clock is armed from `SharedState::new`, so a server
/// nobody ever dialed exits on the same deadline as one whose last client
/// left.
///
/// Expiry cancels the **root** token rather than exiting the process: that
/// is the same signal Ctrl-C and the last-pane self-exit deliver, so panes
/// tear down through the one graceful path (`TerminalActor::shutdown_pty`
/// SIGHUPs the pane's process group and reaps the child) and the socket is
/// unlinked on the way out.
fn spawn_idle_exit_watchdog(
    state: SharedState,
    idle_limit: Duration,
    root_token: CancellationToken,
) {
    tokio::task::spawn_local(async move {
        loop {
            // Sleep exactly as long as the current idle budget allows. While
            // a client is connected `idle_since` is `None` and there is no
            // deadline to compute, so re-check after a full interval — the
            // connection cannot close without the count going through zero,
            // which re-arms the clock we will then read.
            let remaining = state.with(|s| {
                s.idle_since().map_or(idle_limit, |since| {
                    idle_limit.saturating_sub(since.elapsed())
                })
            });
            let nap = remaining.max(IDLE_WATCH_MIN_INTERVAL);
            tokio::select! {
                () = root_token.cancelled() => return,
                () = tokio::time::sleep(nap) => {}
            }
            // Re-read rather than trusting the sleep: a client may have
            // connected (and even disconnected) while we napped, in which
            // case the clock was re-armed and the deadline moved.
            let expired = state.with(|s| {
                s.idle_since()
                    .is_some_and(|since| since.elapsed() >= idle_limit)
            });
            if expired {
                if !root_token.is_cancelled() {
                    info!(
                        idle_limit_secs = idle_limit.as_secs_f64(),
                        "unattended for the configured idle limit; server exiting"
                    );
                    root_token.cancel();
                }
                return;
            }
        }
    });
}

/// Resolve the default Unix-domain-socket path.
///
/// Precedence (matches the MCP adapter's `resolve`, so the daemon, the CLI
/// verbs, and the MCP bridge all agree on one socket):
/// 1. `$PHUX_SOCKET` if set — an explicit `--socket` flag still overrides it
///    at the call sites that take one;
/// 2. `$XDG_RUNTIME_DIR/phux/phux.sock` if `XDG_RUNTIME_DIR` is set;
/// 3. `/tmp/phux-$UID/phux.sock` otherwise.
#[must_use]
pub fn default_socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("PHUX_SOCKET") {
        return PathBuf::from(path);
    }
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        let mut p = PathBuf::from(dir);
        p.push("phux");
        p.push("phux.sock");
        return p;
    }
    // SAFETY-free: `getuid` is a `libc` call we'd rather not depend on here.
    // Read the effective UID from `/proc` is Linux-only; instead use the
    // `USER` env var as a stable, portable fallback when crafting the path.
    // The exact directory name is cosmetic — it only needs to be unique per
    // user.
    let uid_segment = std::env::var("UID")
        .ok()
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "default".to_owned());
    let mut p = PathBuf::from("/tmp");
    p.push(format!("phux-{uid_segment}"));
    p.push("phux.sock");
    p
}

/// Maximum byte length of a Unix-domain-socket path on this platform.
///
/// `bind(2)` and `connect(2)` copy the path into `sockaddr_un.sun_path`,
/// a fixed-size buffer that must also hold a trailing NUL: 108 bytes on
/// Linux, 104 on macOS and the BSDs, so the usable maximum is one less.
/// Documented conservative constants are used rather than sizing
/// `libc::sockaddr_un` because `libc` is a macOS-gated dependency here.
pub const MAX_SOCKET_PATH_LEN: usize = if cfg!(target_os = "linux") { 107 } else { 103 };

/// Check that `path` fits in a `sockaddr_un` on this platform.
///
/// A path longer than [`MAX_SOCKET_PATH_LEN`] can never be bound *or*
/// connected to, so both the server's bind path and the CLI's
/// connect/auto-spawn path validate up front and surface
/// [`ServerError::SocketPathTooLong`] — which names the platform limit,
/// the offending path, and its byte length — instead of the kernel's
/// opaque "path must be shorter than `SUN_LEN`".
pub fn validate_socket_path_len(path: &Path) -> Result<(), ServerError> {
    let len = path.as_os_str().len();
    if len > MAX_SOCKET_PATH_LEN {
        return Err(ServerError::SocketPathTooLong {
            path: path.to_path_buf(),
            len,
        });
    }
    Ok(())
}

/// Errors surfaced by [`ServerRuntime`].
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// The Unix domain socket could not be bound.
    #[error("failed to bind unix socket: {0}")]
    Bind(#[source] io::Error),

    /// The socket path cannot fit in a `sockaddr_un`, so neither a server
    /// bind nor a client connect could ever succeed on it.
    #[error(
        "socket path {path} is {len} bytes, but unix domain socket paths on this platform are limited to {MAX_SOCKET_PATH_LEN} bytes; pick a shorter path (e.g. under /tmp) via PHUX_SOCKET or --socket"
    )]
    SocketPathTooLong {
        /// The over-long socket path.
        path: PathBuf,
        /// Byte length of `path`.
        len: usize,
    },

    /// Another server appears to be live at this socket path. The path is
    /// returned so callers can present a useful diagnostic.
    #[error("socket {0} is already in use by a live server")]
    SocketBusy(PathBuf),

    /// The parent directory of the socket path could not be prepared.
    #[error("failed to prepare socket directory {path}: {source}")]
    PrepareDir {
        /// Directory that could not be created or had wrong permissions.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// An I/O error not otherwise classified.
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    /// The handoff state blob could not be read or decoded on `--resume`.
    #[error("resume: {0}")]
    Resume(#[from] crate::upgrade::blob::BlobError),

    /// Failed to build the tokio runtime.
    #[error("failed to build tokio runtime: {0}")]
    Runtime(#[source] io::Error),

    /// Hub mode was requested but the satellite registry did not validate
    /// (phux-v45.1). A hub with a half-parsed table would silently drop
    /// satellites, so startup fails instead.
    #[error("hub: {0}")]
    Hub(#[from] crate::hub::HubTableError),
    /// Outbound connector configuration was unsafe or malformed.
    #[error("connector: {0}")]
    Connector(#[from] crate::connector::ConnectorError),

    /// The server token store needed to authorize bridged consumers could not
    /// be loaded. A connector must fail closed rather than admit consumers
    /// without the server's own authorization.
    #[error("connector consumer token store {path}: {source}")]
    ConnectorTokenStore {
        /// Token-store path.
        path: PathBuf,
        /// Parse or I/O failure.
        #[source]
        source: crate::auth::AuthError,
    },
}

/// Server runtime owning the listener loop and per-client task scaffolding.
#[derive(Debug)]
pub struct ServerRuntime {
    cfg: ServerConfig,
    /// Optional WebSocket listen address (in addition to the always-on UDS).
    /// `None` falls back to the `PHUX_WS_ADDR` environment variable. The
    /// `phux server --listen <ADDR>` flag populates this; binding off-loopback
    /// auto-engages TLS + token auth (see [`build_ws_listener`]).
    ws_addr: Option<SocketAddr>,
    /// Optional QUIC listen address (in addition to the always-on UDS).
    /// `None` falls back to the `PHUX_QUIC_ADDR` environment variable. The
    /// `phux server --quic <ADDR>` flag populates this; QUIC is always TLS-
    /// encrypted, and binding off-loopback requires a paired bearer token
    /// (see [`build_quic_listener`]).
    quic_addr: Option<SocketAddr>,
    /// Optional WebTransport listen address (in addition to the always-on
    /// UDS). `None` falls back to the `PHUX_WT_ADDR` environment variable.
    /// The `phux server --webtransport <ADDR>` flag populates this;
    /// WebTransport is always TLS-encrypted (HTTP/3 over QUIC), and binding
    /// off-loopback requires a paired bearer token (see
    /// [`build_wt_listener`]).
    #[cfg(feature = "webtransport")]
    wt_addr: Option<SocketAddr>,
    /// Graceful-upgrade resume descriptor (ADR-0032). When `Some`, the runtime
    /// reads the handoff state blob from this inherited fd, adopts the
    /// inherited listener, and rebuilds the session tree instead of binding a
    /// fresh socket and seeding an empty state. Set by `phux server --resume`.
    resume_fd: Option<RawFd>,
    /// Whether this server runs as a federation hub (phux-v45.1, ADR-0007).
    /// Off by default; `phux server --hub` populates this. Only a hub
    /// consumes [`Self::satellites`] — a non-hub server ignores the registry
    /// entirely.
    hub: bool,
    /// The satellite registry from `config.toml` (`[[satellites]]`), as
    /// loaded by the binary. Read only when [`Self::hub`] is set, at which
    /// point every enabled entry's endpoint is validated into the runtime
    /// [`crate::hub::HubTable`]; a validation failure fails startup.
    satellites: Vec<phux_config::SatelliteConfigEntry>,
    /// Outbound relay entries selected by the binary from `[[connector]]`
    /// configuration (or one `--connect` override).
    connectors: Vec<phux_config::ConnectorConfigEntry>,
    /// Raw `--connect HOST:PORT` override, retained only so graceful upgrade
    /// can reconstruct the same CLI surface.
    connect_override: Option<String>,
}

impl ServerRuntime {
    /// Create a runtime ready to be `run`. Does not perform I/O.
    #[must_use]
    pub const fn new(cfg: ServerConfig) -> Self {
        Self {
            cfg,
            ws_addr: None,
            quic_addr: None,
            #[cfg(feature = "webtransport")]
            wt_addr: None,
            resume_fd: None,
            hub: false,
            satellites: Vec::new(),
            connectors: Vec::new(),
            connect_override: None,
        }
    }

    /// Resume from a graceful upgrade (ADR-0032): read the handoff state blob
    /// from inherited descriptor `fd`, adopt the inherited listener, and
    /// rebuild the session tree rather than starting fresh.
    #[must_use]
    pub const fn resume(mut self, fd: RawFd) -> Self {
        self.resume_fd = Some(fd);
        self
    }

    /// Also accept WebSocket connections on `addr` (the UDS stays on).
    ///
    /// Overrides the `PHUX_WS_ADDR` environment variable. A loopback address
    /// is plaintext + unauthenticated (the local browser-dev path); any
    /// routable address auto-provisions TLS and requires a paired bearer
    /// token (ADR-0031).
    #[must_use]
    pub const fn listen_ws(mut self, addr: SocketAddr) -> Self {
        self.ws_addr = Some(addr);
        self
    }

    /// Also accept QUIC connections on `addr` (the UDS stays on).
    ///
    /// Overrides the `PHUX_QUIC_ADDR` environment variable. QUIC is always
    /// TLS 1.3-encrypted (the protocol mandates it); a loopback address skips
    /// token auth (local dev), while any routable address requires a paired
    /// bearer token sent as the stream's opening preamble (ADR-0031 parity
    /// with `wss://`).
    #[must_use]
    pub const fn listen_quic(mut self, addr: SocketAddr) -> Self {
        self.quic_addr = Some(addr);
        self
    }

    /// Also accept WebTransport connections on `addr` (the UDS stays on).
    ///
    /// Overrides the `PHUX_WT_ADDR` environment variable. WebTransport is
    /// HTTP/3 over QUIC — always TLS 1.3-encrypted — and is the browser's
    /// door to QUIC-class transport (`phux-web` dials it, falling back to
    /// WebSocket). A loopback address skips token auth (local dev), while
    /// any routable address requires a paired bearer token carried in the
    /// `CONNECT` request (ADR-0031 parity with `wss://`).
    #[cfg(feature = "webtransport")]
    #[must_use]
    pub const fn listen_webtransport(mut self, addr: SocketAddr) -> Self {
        self.wt_addr = Some(addr);
        self
    }

    /// Built without the `webtransport` feature: the listen address is
    /// ignored (with a warning) so callers keep one call site.
    #[cfg(not(feature = "webtransport"))]
    #[must_use]
    pub fn listen_webtransport(self, addr: SocketAddr) -> Self {
        warn!(
            %addr,
            "phux-server was built without the `webtransport` feature; ignoring the WebTransport listen address"
        );
        self
    }

    /// Run as a federation hub (phux-v45.1, ADR-0007): consume `satellites`
    /// (the `[[satellites]]` registry from `config.toml`) at startup,
    /// validating every enabled entry's endpoint into the runtime
    /// [`crate::hub::HubTable`]. A malformed enabled endpoint or a duplicate
    /// satellite name fails startup with [`ServerError::Hub`].
    ///
    /// Without this call the registry is never read: non-hub servers ignore
    /// `[[satellites]]` entirely. No dialing or routing happens yet — this
    /// is the validated table only (dialing is phux-v45.3, routing
    /// phux-v45.4).
    #[must_use]
    pub fn hub(mut self, satellites: Vec<phux_config::SatelliteConfigEntry>) -> Self {
        self.hub = true;
        self.satellites = satellites;
        self
    }

    /// Supervise outbound relay connector entries.
    ///
    /// `connect_override` is the original ad-hoc CLI endpoint, if any, and is
    /// retained for graceful-upgrade argv reconstruction.
    #[must_use]
    pub fn connectors(
        mut self,
        entries: Vec<phux_config::ConnectorConfigEntry>,
        connect_override: Option<String>,
    ) -> Self {
        self.connectors = entries;
        self.connect_override = connect_override;
        self
    }

    /// Run the server until `shutdown` resolves.
    ///
    /// Builds a `new_current_thread` tokio runtime internally and blocks on
    /// [`Self::run_async`].
    pub fn run<F>(self, shutdown: F) -> Result<(), ServerError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let rt = Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(ServerError::Runtime)?;
        rt.block_on(self.run_async(shutdown))
    }

    /// Async variant for tests and embedders that already own a runtime.
    ///
    /// Per ADR-0014, the accept loop and every per-client task run on a
    /// [`tokio::task::LocalSet`] driven by the current async context.
    /// `!Send` futures are legal — and required — because pane actors
    /// own a [`libghostty_vt::Terminal`], which carries no `Send`/`Sync`
    /// impls.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: server runs on a LocalSet; per-pane actors are !Send"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "linear server bring-up: resolve config, set up the listener (fresh bind or resume-adopt), mirror config into shared state, then drive the LocalSet accept loop — one straight-line sequence."
    )]
    pub async fn run_async<F>(self, shutdown: F) -> Result<(), ServerError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let socket_path = self.cfg.socket_path.clone();

        // Build shared state. The state is the merge point for multi-
        // client input and the routing table for fanout (see
        // `state.rs`). Cloning the `SharedState` is cheap (`Arc::clone`).
        let state = SharedState::new();

        // Hub mode (phux-v45.1, ADR-0007): validate the satellite registry
        // into the runtime hub table before any I/O, so a malformed registry
        // fails fast — before the socket is bound. Non-hub servers skip the
        // registry entirely (`resolve_hub_table` returns `Ok(None)`). The
        // table is kept locally too: the outbound link supervisors
        // (phux-v45.3) are spawned from it inside the LocalSet below.
        let hub_table = crate::hub::resolve_hub_table(self.hub, &self.satellites)?;
        if let Some(table) = &hub_table {
            info!(
                satellites = table.len(),
                "hub mode: satellite registry validated"
            );
            for (host, entry) in table.iter() {
                info!(satellite = %host, target = %entry.target, "hub satellite registered");
            }
            state.with_mut(|s| s.set_hub_table(table.clone()));
        }

        // Connector planning is a startup gate, not a retry condition:
        // malformed endpoints and routable entries missing a pin/token fail
        // before the UDS is bound. Token contents stay on disk and are re-read
        // by each supervisor attempt.
        let connector_specs = crate::connector::plan_connectors(&self.connectors)?;
        let connector_consumer_tokens = if connector_specs.is_empty() {
            None
        } else {
            let path = std::env::var_os("PHUX_WS_TOKENS")
                .map_or_else(crate::auth::default_token_store_path, PathBuf::from);
            let store = crate::auth::TokenStore::load(&path).map_err(|source| {
                ServerError::ConnectorTokenStore {
                    path: path.clone(),
                    source,
                }
            })?;
            Some(std::sync::Arc::new(store))
        };

        // Graceful upgrade (ADR-0032): when resuming, read the handoff blob and
        // adopt the inherited listener instead of binding a fresh socket. The
        // session tree is rebuilt from the blob inside the LocalSet below.
        let resume_blob = match self.resume_fd {
            Some(fd) => Some(resume::read_blob_from_fd(fd)?),
            None => None,
        };

        let listener = if let Some(blob) = &resume_blob {
            let listener = resume::adopt_uds_listener(blob.listener_fd)?;
            info!(
                path = %socket_path.display(),
                "phux-server resumed; adopted the inherited UDS listener"
            );
            listener
        } else {
            // phux-iwuc: fail fast on a path that cannot fit in a
            // `sockaddr_un` — `bind(2)` would only reject it later with an
            // opaque `SUN_LEN` error. The resume branch above adopts an
            // already-bound listener, so only a fresh bind needs the gate.
            validate_socket_path_len(&socket_path)?;
            prepare_socket_dir(&socket_path)?;
            handle_existing_socket(&socket_path).await?;
            let listener = UnixListener::bind(&socket_path).map_err(ServerError::Bind)?;
            secure_socket_file(&socket_path)?;
            let listener = crate::transport::UdsListener::new(listener);
            info!(path = %socket_path.display(), "phux-server listening on UDS");
            listener
        };

        // Capture the upgrade context (ADR-0032): the listening socket's fd +
        // path + effective runtime flags, so `handle_upgrade` can build the
        // handoff blob and re-pass `--socket` / `--listen` / `--quic` /
        // `--webtransport` / `--hub` to the re-exec'd image (phux-v45.10).
        // The flags come from the runtime's own configuration — what the
        // builder methods applied — not a stashed copy of the original argv.
        let runtime_flags = RuntimeFlags {
            ws_addr: self.ws_addr,
            quic_addr: self.quic_addr,
            #[cfg(feature = "webtransport")]
            wt_addr: self.wt_addr,
            #[cfg(not(feature = "webtransport"))]
            wt_addr: None,
            hub: self.hub,
            connect: self.connect_override.clone(),
            exit_after_idle: self.cfg.exit_after_idle,
        };
        state.with_mut(|s| {
            s.set_upgrade_context(listener.as_raw_fd(), socket_path.clone(), runtime_flags);
        });

        // The LocalSet hosts per-client tasks and per-pane actors —
        // both `!Send`. `LocalSet::run_until` drives the set to the
        // future's completion; tasks spawned via `spawn_local` from
        // inside the future are polled on the same thread.
        let pre_seeded = self.cfg.pre_seeded_session.clone();
        let seed_with_pty = self.cfg.seed_with_pty;
        let seed_command = self.cfg.seed_command.clone();
        // Mirror the PTY *mode* into shared state so `handle_attach`'s
        // `AttachTarget::CreateIfMissing` branch (phux-k61.3) spawns new
        // sessions' seed panes with PTYs when the server runs with them.
        //
        // phux-07y: the seed *command* is deliberately NOT mirrored as
        // the CreateIfMissing override. `seed_command` is the pre-seeded
        // session's program (e.g. `defaults.spawn-on-attach`, the thing
        // naked `phux` opens with); a CreateIfMissing-created session —
        // `phux new`, `phux new -- vim` — must instead honor its own
        // wire `command` (or fall back to `default_shell_command`), not
        // inherit naked-`phux`'s launcher. So the override stays `None`.
        state.with_mut(|s| s.set_attach_create_pty(seed_with_pty, None));
        // Mirror the listening socket path into shared state so every pane
        // spawn site injects it as `PHUX_SOCKET` (phux-cufw) — an in-pane
        // `phux` then targets this server even off the default socket path.
        state.with_mut(|s| s.set_server_socket_path(socket_path.clone()));
        // Mirror `defaults.history-limit` into shared state so the
        // attach-time creation path (`CreateIfMissing`) and
        // `SPAWN_TERMINAL` build their panes with the configured cap.
        let history_limit = self.cfg.history_limit;
        state.with_mut(|s| s.set_history_limit(history_limit));
        // Mirror `defaults.cwd-inheritance` into shared state so the
        // `SPAWN_TERMINAL` handler resolves a new pane's working directory
        // from the configured policy.
        let cwd_inheritance = self.cfg.cwd_inheritance;
        state.with_mut(|s| s.set_cwd_inheritance(cwd_inheritance));
        // Mirror `defaults.term` into shared state so the seed session,
        // attach-time `CreateIfMissing`, and `SPAWN_TERMINAL` apply the
        // configured `TERM` baseline.
        let term = self.cfg.term.clone();
        state.with_mut(|s| s.set_term(term));
        // Mirror the resolved default shell (`defaults.shell` → `$SHELL` →
        // `/bin/sh`, phux-i0e8.4.1) into shared state so every
        // command-less spawn path runs the configured shell.
        let shell = self.cfg.shell.clone();
        state.with_mut(|s| s.set_shell(shell));
        // Mirror `defaults.window-size` into shared state so
        // `handle_viewport_resize` resolves a shared Terminal's geometry from
        // the configured multi-client policy (phux-nk07).
        let window_size = self.cfg.window_size;
        state.with_mut(|s| s.set_window_size(window_size));
        // WebSocket listen address: the `--listen` flag (via `listen_ws`)
        // wins; otherwise fall back to `PHUX_WS_ADDR` inside the accept setup.
        let ws_addr_override = self.ws_addr;
        // QUIC listen address: the `--quic` flag (via `listen_quic`) wins;
        // otherwise fall back to `PHUX_QUIC_ADDR` inside the accept setup.
        let quic_addr_override = self.quic_addr;
        // WebTransport listen address: the `--webtransport` flag (via
        // `listen_webtransport`) wins; otherwise fall back to `PHUX_WT_ADDR`
        // inside the accept setup.
        #[cfg(feature = "webtransport")]
        let webtransport_addr_override = self.wt_addr;
        // Wire policy bundle from config into shared state.
        if let Some(bundle) = self.cfg.policy_bundle.clone() {
            state.with_mut(|s| s.set_policy_bundle(bundle));
        }
        // Event-hook catalog (phux-r82.1): moved into the LocalSet block
        // below, where the dispatcher task can `spawn_local`. The listening
        // socket path rides along so hook children get `PHUX_SOCKET` too
        // (phux-d4rf), matching the pane-spawn injection above (phux-cufw).
        let hook_catalog = self.cfg.hook_catalog.clone();
        let hook_socket_path = socket_path.clone();
        // Ephemeral-server lifetime (ADR-0063). Captured here and consumed
        // inside the LocalSet below, where the watchdog can `spawn_local`
        // alongside the accept loops it races.
        let exit_after_idle = self.cfg.exit_after_idle;
        // Dedicated input lane (phux-51n6.2, ADR-0044): a separate OS thread
        // that runs the input routing stage off the main runtime, so a
        // keystroke's lease/subscription gating and mailbox delivery preempt a
        // large output-broadcast tick instead of waiting for the current-thread
        // runtime to yield. `input_lane` is held here for the server's
        // lifetime; its `Drop` joins the thread on the way out. Its handle is
        // moved into the accept loops below and cloned per client.
        let input_lane = input_lane::spawn_input_lane(state.clone())?;
        let input_lane_handle = input_lane.handle();
        let local = LocalSet::new();
        // Hierarchical cancellation: a single root token is the parent
        // of every per-client / per-pane child. The external `shutdown`
        // future is folded into this token by a small task spawned on
        // the LocalSet (see below). On `root_token.cancel()`:
        //   * `accept_loop` returns from its select! → its per-client
        //     `JoinSet` drops → in-flight client tasks abort.
        //   * Every `TerminalActor`'s child token fires → actors exit
        //     cleanly via their own `select!` (shutdown_pty runs).
        let root_token = CancellationToken::new();
        let result = local
            .run_until(async move {
                // Fold the external shutdown future into the root
                // token. `spawn_local` (not `tokio::spawn`) because
                // the runtime is current-thread with no worker pool.
                {
                    let token = root_token.clone();
                    tokio::task::spawn_local(async move {
                        shutdown.await;
                        debug!("shutdown future resolved; cancelling root token");
                        token.cancel();
                    });
                }

                // Ephemeral-server lifetime (ADR-0063). Armed before the
                // seed/resume paths so the "nobody ever connected" case is
                // covered from the earliest possible instant — that is the
                // leak shape: a harness bootstraps a daemon, dies, and the
                // daemon it never dialed holds a live PTY forever.
                if let Some(idle_limit) = exit_after_idle {
                    info!(
                        idle_limit_secs = idle_limit.as_secs_f64(),
                        "ephemeral server: will exit when unattended for the idle limit"
                    );
                    spawn_idle_exit_watchdog(state.clone(), idle_limit, root_token.clone());
                }

                // Event-hook dispatcher (docs/consumers/tui.md §9,
                // phux-r82.1). Spawned BEFORE the seed/resume paths so a
                // pre-seeded session's pane fires `after-new-pane` too.
                // Skipped entirely when nothing is configured: firing an
                // event with no dispatcher registered is a no-op.
                if !hook_catalog.is_empty() {
                    let dispatcher =
                        crate::hooks::spawn_hook_dispatcher(hook_catalog, Some(hook_socket_path));
                    state.with_mut(|s| s.set_hook_dispatcher(dispatcher));
                }

                // Hub outbound dialer (phux-v45.3, ADR-0038): one link
                // supervisor per validated satellite, spawned on the
                // LocalSet as children of the root token. Each dials,
                // authenticates like a remote consumer, and maintains the
                // connection with capped exponential backoff; fail-closed
                // refusals (routable endpoint without token/pin) surface as
                // a `Refused` status without dialing. The status handle is
                // mirrored into shared state for future LIST aggregation,
                // and the frame-relay registry (phux-v45.4) alongside it so
                // command/input dispatch can route satellite-tagged
                // terminal ids over the established links.
                if let Some(table) = &hub_table {
                    let statuses = crate::hub::link::HubLinkStatuses::default();
                    let relays = crate::hub::relay::HubRelays::default();
                    state.with_mut(|s| {
                        s.set_hub_link_statuses(statuses.clone());
                        s.set_hub_relays(relays.clone());
                    });
                    crate::hub::link::spawn_links(table, &statuses, &relays, &root_token);
                }

                if let Some(tokens) = &connector_consumer_tokens {
                    crate::connector::spawn_connectors(
                        connector_specs,
                        tokens,
                        &state,
                        &input_lane_handle,
                        &root_token,
                    );
                }

                // Graceful-upgrade resume (ADR-0032): rebuild the whole
                // session tree from the handoff blob, re-adopting each pane's
                // inherited PTY. Runs inside the LocalSet so the rebuilt pane
                // actors `spawn_local` onto the same thread. A fresh start
                // pre-seeds its single session instead (the `else` branch).
                if let Some(blob) = resume_blob {
                    match state.with_mut(|s| s.rebuild_from_blob(&blob)) {
                        Ok(exit_watchers) => {
                            for (pane, exit_notify) in exit_watchers {
                                spawn_terminal_exit_watcher(
                                    state.clone(),
                                    pane,
                                    Some(exit_notify),
                                    root_token.clone(),
                                );
                            }
                            info!(
                                sessions = blob.sessions.len(),
                                panes = blob.panes.len(),
                                "resumed session tree from upgrade blob"
                            );
                        }
                        Err(err) => {
                            error!(error = %err, "failed to rebuild state from upgrade blob");
                        }
                    }
                } else if let Some(name) = pre_seeded.as_deref() {
                    let seeded = if seed_with_pty {
                        // Fall back to the resolved default shell
                        // (`defaults.shell` → `$SHELL` → `/bin/sh`,
                        // phux-i0e8.4.1) mirrored into state above.
                        let mut cmd = seed_command.unwrap_or_else(|| {
                            let shell = state.with(|s| s.shell().to_owned());
                            crate::terminal_actor::default_shell_command(&shell)
                        });
                        // Apply the configured `defaults.term` over the
                        // builder's baseline so the seed pane advertises the
                        // server-wide `TERM` (phux-ign).
                        let term = state.with(|s| s.term().to_owned());
                        crate::terminal_actor::apply_term(&mut cmd, &term);
                        seed_session_with_pty(&state, name, cmd, history_limit, &root_token)
                    } else {
                        seed_session_with_actor(&state, name, history_limit, &root_token)
                    };
                    if let Err(err) = seeded {
                        warn!(
                            session = name,
                            error = %err,
                            "failed to spawn pane actor for pre-seeded session",
                        );
                    } else {
                        debug!(
                            session = name,
                            pty = seed_with_pty,
                            "pre-seeded session in registry"
                        );
                    }
                }
                // Optionally also accept WebSocket connections (phux-486.4) so
                // browser consumers (`phux-web`) can speak the identical wire.
                // Opt-in via `phux server --listen <ADDR>` or the `PHUX_WS_ADDR`
                // environment variable (e.g. "127.0.0.1:8787"); UDS is always
                // on. The flag wins when both are set.
                let ws_addr = ws_addr_override.or_else(|| env_socket_addr("PHUX_WS_ADDR"));
                let ws_listener = match ws_addr {
                    Some(addr) => build_ws_listener(addr).await,
                    None => None,
                };
                // Optionally also accept QUIC connections (phux-y8v6, ADR-0007).
                // Opt-in via `phux server --quic <ADDR>` or `PHUX_QUIC_ADDR`;
                // QUIC carries the identical frames over a TLS 1.3 stream.
                let quic_addr = quic_addr_override.or_else(|| env_socket_addr("PHUX_QUIC_ADDR"));
                let quic_listener = quic_addr.and_then(build_quic_listener);
                // Optionally also accept WebTransport connections (phux-0wmf):
                // HTTP/3 over QUIC, the browser's QUIC-class door (`phux-web`
                // dials it, falling back to WebSocket). Opt-in via
                // `phux server --webtransport <ADDR>` or `PHUX_WT_ADDR`.
                #[cfg(feature = "webtransport")]
                let webtransport_listener = webtransport_addr_override
                    .or_else(|| env_socket_addr("PHUX_WT_ADDR"))
                    .and_then(build_wt_listener);

                // UDS is always on; WS and QUIC are additive. Each transport's
                // accept loop runs until the root token cancels; whichever
                // returns first ends the server (on shutdown all observe the
                // cancellation). `select_all` drives them concurrently without
                // the combinatorial `select!` nesting the optional transports
                // would otherwise need.
                let mut accepts: Vec<AcceptLoopFuture<'_>> = vec![Box::pin(accept_loop(
                    &listener,
                    state.clone(),
                    root_token.clone(),
                    Some(input_lane_handle.clone()),
                ))];
                if let Some(ws) = &ws_listener {
                    accepts.push(Box::pin(accept_loop(
                        ws,
                        state.clone(),
                        root_token.clone(),
                        Some(input_lane_handle.clone()),
                    )));
                }
                if let Some(quic) = &quic_listener {
                    accepts.push(Box::pin(accept_loop(
                        quic,
                        state.clone(),
                        root_token.clone(),
                        Some(input_lane_handle.clone()),
                    )));
                }
                #[cfg(feature = "webtransport")]
                if let Some(wt) = &webtransport_listener {
                    accepts.push(Box::pin(accept_loop(
                        wt,
                        state.clone(),
                        root_token.clone(),
                        Some(input_lane_handle.clone()),
                    )));
                }
                let (result, _index, _remaining) = futures_util::future::select_all(accepts).await;
                result
            })
            .await;

        // `run_until` returns the instant the accept-loop future is ready — on
        // shutdown that is the tick where `accept_loop` observed the cancelled
        // root token and dropped its per-client `JoinSet`. Dropping that
        // `JoinSet` only *aborts* the client tasks; their futures still live on
        // this `LocalSet` and are not dropped until the set advances or is
        // itself dropped. Each such future holds a cloned `InputLaneHandle`
        // (`task_input_lane`) that keeps the lane channel open. So we must drop
        // the `LocalSet` FIRST — that destroys every remaining client-task
        // future and releases its handle clone — and only then drop the lane
        // owner. Reversing this order deadlocks: `InputLane::drop` joins the
        // lane thread, whose `blocking_recv` never returns `None` while a
        // client-task clone keeps the channel open (ADR-0044).
        drop(local);
        drop(input_lane);

        // Always try to unlink the socket on the way out; ignore NotFound.
        if let Err(err) = std::fs::remove_file(&socket_path)
            && err.kind() != io::ErrorKind::NotFound
        {
            warn!(path = %socket_path.display(), error = %err, "failed to unlink socket");
        }

        result
    }
}

/// Seed `(session, window, pane)` and spawn a **no-PTY** `TerminalActor`
/// on the current `LocalSet`. Used by tests that pre-seed a session
/// to exercise the ATTACH path without spawning a real subprocess.
///
/// For the real server path (which will spawn `$SHELL` once a binary
/// entry point exists), see [`seed_session_with_pty`].
///
/// Public-ish (`pub(crate)`) so tests can drive it directly inside
/// their own `LocalSet`.
/// Build the optional WebSocket listener for `PHUX_WS_ADDR`, applying the
/// ADR-0031 remote-consumer policy. Returns `None` (WebSocket disabled, UDS
/// only) on any setup failure rather than failing the whole server.
///
/// **The bind address is the toggle, so there is no remote-mode setup friction:**
///
/// * **Loopback address → plaintext, unauthenticated.** The local browser-client
///   dev path; zero config.
/// * **Routable address → TLS + bearer-token auth, auto-provisioned.** Binding to
///   anything off-loopback is treated as exposing the server, so phux generates
///   and persists a self-signed certificate (if none is configured) and reads
///   the default token store — no openssl, no manual PEM. The operator just runs
///   `phux pair` to mint a device token. Plaintext never reaches a routable
///   address (ADR-0031 no-plaintext-remote invariant).
///
/// `PHUX_WS_SECURE=1` forces the secure path on a loopback address (for testing
/// the remote path locally). `PHUX_WS_TLS_CERT` + `PHUX_WS_TLS_KEY` override the
/// auto-generated certificate with an operator-supplied one; `PHUX_WS_TOKENS`
/// overrides the default token-store path.
async fn build_ws_listener(addr: SocketAddr) -> Option<crate::transport::WsListener> {
    let force_secure = std::env::var_os("PHUX_WS_SECURE").is_some_and(|v| !v.is_empty());
    let secure = !addr.ip().is_loopback() || force_secure;

    if !secure {
        return match crate::transport::WsListener::bind(addr).await {
            Ok(ws) => {
                let bound = ws.local_addr().map(|a| a.to_string()).unwrap_or_default();
                info!(addr = %bound, "WebSocket listening (plaintext, loopback)");
                Some(ws)
            }
            Err(err) => {
                warn!(addr = %addr, error = %err, "failed to bind WebSocket; UDS only");
                None
            }
        };
    }

    // Secure path. Operator-supplied cert overrides the auto-generated one;
    // otherwise provision a persisted self-signed cert at the default paths.
    let cert_env = std::env::var_os("PHUX_WS_TLS_CERT").map(PathBuf::from);
    let key_env = std::env::var_os("PHUX_WS_TLS_KEY").map(PathBuf::from);
    let operator_cert = cert_env.is_some() || key_env.is_some();
    let cert_path = cert_env.unwrap_or_else(crate::transport::tls::default_cert_path);
    let key_path = key_env.unwrap_or_else(crate::transport::tls::default_key_path);
    if !operator_cert
        && let Err(err) = crate::transport::tls::ensure_self_signed(&cert_path, &key_path)
    {
        error!(error = %err, "failed to provision self-signed certificate; WebSocket disabled");
        return None;
    }
    let acceptor = match crate::transport::tls::acceptor_from_pem(&cert_path, &key_path) {
        Ok(acceptor) => acceptor,
        Err(err) => {
            error!(error = %err, "TLS setup failed; WebSocket disabled");
            return None;
        }
    };

    let tokens_path = std::env::var_os("PHUX_WS_TOKENS")
        .map_or_else(crate::auth::default_token_store_path, PathBuf::from);
    let store = match crate::auth::TokenStore::load(&tokens_path) {
        Ok(store) => store,
        Err(err) => {
            error!(error = %err, path = %tokens_path.display(), "failed to load token store; WebSocket disabled");
            return None;
        }
    };
    if store.is_empty() {
        warn!(
            path = %tokens_path.display(),
            "no pairing tokens; every remote consumer is rejected until `phux pair`"
        );
    }
    let token_count = store.len();

    match crate::transport::WsListener::bind_secure(addr, acceptor, std::sync::Arc::new(store))
        .await
    {
        Ok(ws) => {
            let bound = ws.local_addr().map(|a| a.to_string()).unwrap_or_default();
            info!(addr = %bound, tokens = token_count, "WebSocket listening with TLS + token auth");
            Some(ws)
        }
        Err(err) => {
            warn!(addr = %addr, error = %err, "failed to bind secure WebSocket; UDS only");
            None
        }
    }
}

/// Parse a [`SocketAddr`] from environment variable `var`. Returns `None` (the
/// transport stays disabled) when the variable is unset or malformed, logging a
/// warning in the malformed case.
fn env_socket_addr(var: &str) -> Option<SocketAddr> {
    let raw = std::env::var(var).ok()?;
    match raw.parse::<SocketAddr>() {
        Ok(addr) => Some(addr),
        Err(err) => {
            warn!(var, addr = %raw, error = %err, "invalid socket address; transport disabled");
            None
        }
    }
}

/// Build the optional QUIC listener for `addr` (phux-y8v6, ADR-0007). Returns
/// `None` (QUIC disabled, other transports unaffected) on any setup failure
/// rather than failing the whole server.
///
/// QUIC is **always** TLS 1.3-encrypted, so a certificate is provisioned in
/// both modes — it shares the persisted self-signed cert and token store with
/// the `wss://` path (so a single `phux pair` token authorizes either), keyed
/// off the same `PHUX_WS_TLS_CERT` / `PHUX_WS_TLS_KEY` / `PHUX_WS_TOKENS`
/// overrides:
///
/// * **Loopback address → TLS, no token.** Local dev; the dialer sends no
///   preamble.
/// * **Routable address (or `PHUX_WS_SECURE=1`) → TLS + bearer-token preamble.**
///   Off-loopback is treated as exposing the server, so a paired token is
///   required exactly as for a remote WebSocket consumer (ADR-0031).
fn build_quic_listener(addr: SocketAddr) -> Option<crate::transport::quic::QuicListener> {
    let force_secure = std::env::var_os("PHUX_WS_SECURE").is_some_and(|v| !v.is_empty());
    let secure = !addr.ip().is_loopback() || force_secure;

    let cert_env = std::env::var_os("PHUX_WS_TLS_CERT").map(PathBuf::from);
    let key_env = std::env::var_os("PHUX_WS_TLS_KEY").map(PathBuf::from);
    let operator_cert = cert_env.is_some() || key_env.is_some();
    let cert_path = cert_env.unwrap_or_else(crate::transport::tls::default_cert_path);
    let key_path = key_env.unwrap_or_else(crate::transport::tls::default_key_path);
    if !operator_cert
        && let Err(err) = crate::transport::tls::ensure_self_signed(&cert_path, &key_path)
    {
        error!(error = %err, "failed to provision self-signed certificate; QUIC disabled");
        return None;
    }

    let tokens = if secure {
        let tokens_path = std::env::var_os("PHUX_WS_TOKENS")
            .map_or_else(crate::auth::default_token_store_path, PathBuf::from);
        let store = match crate::auth::TokenStore::load(&tokens_path) {
            Ok(store) => store,
            Err(err) => {
                error!(error = %err, path = %tokens_path.display(), "failed to load token store; QUIC disabled");
                return None;
            }
        };
        if store.is_empty() {
            warn!(
                path = %tokens_path.display(),
                "no pairing tokens; every remote QUIC consumer is rejected until `phux pair`"
            );
        }
        Some(std::sync::Arc::new(store))
    } else {
        None
    };
    let token_count = tokens.as_ref().map_or(0, |s| s.len());

    match crate::transport::quic::QuicListener::from_pem(addr, &cert_path, &key_path, tokens) {
        Ok(quic) => {
            let bound = quic.local_addr().map(|a| a.to_string()).unwrap_or_default();
            if secure {
                info!(addr = %bound, tokens = token_count, "QUIC listening with TLS + token auth");
            } else {
                info!(addr = %bound, "QUIC listening (TLS, loopback, unauthenticated)");
            }
            Some(quic)
        }
        Err(err) => {
            warn!(addr = %addr, error = %err, "failed to bind QUIC; UDS only");
            None
        }
    }
}

/// Build the optional WebTransport listener for `addr` (phux-0wmf). Returns
/// `None` (WebTransport disabled, other transports unaffected) on any setup
/// failure rather than failing the whole server.
///
/// WebTransport is HTTP/3 over QUIC, so it is **always** TLS 1.3-encrypted;
/// a certificate is provisioned in both modes. It shares the persisted
/// self-signed cert and token store with the `wss://` and QUIC paths (one
/// `phux pair` token authorizes all three), keyed off the same
/// `PHUX_WS_TLS_CERT` / `PHUX_WS_TLS_KEY` / `PHUX_WS_TOKENS` overrides:
///
/// * **Loopback address → TLS, no token.** Local dev; the `CONNECT` carries
///   no token.
/// * **Routable address (or `PHUX_WS_SECURE=1`) → TLS + bearer token.**
///   Off-loopback is treated as exposing the server, so a paired token is
///   required exactly as for a remote WebSocket consumer (ADR-0031). Native
///   consumers send `Authorization: Bearer <hex>`; browsers — whose
///   `WebTransport` JS API cannot set headers — append `?token=<hex>` to the
///   session URL, still inside TLS.
#[cfg(feature = "webtransport")]
fn build_wt_listener(addr: SocketAddr) -> Option<crate::transport::webtransport::WtListener> {
    let force_secure = std::env::var_os("PHUX_WS_SECURE").is_some_and(|v| !v.is_empty());
    let secure = !addr.ip().is_loopback() || force_secure;

    let cert_env = std::env::var_os("PHUX_WS_TLS_CERT").map(PathBuf::from);
    let key_env = std::env::var_os("PHUX_WS_TLS_KEY").map(PathBuf::from);
    let operator_cert = cert_env.is_some() || key_env.is_some();
    let cert_path = cert_env.unwrap_or_else(crate::transport::tls::default_cert_path);
    let key_path = key_env.unwrap_or_else(crate::transport::tls::default_key_path);
    if !operator_cert
        && let Err(err) = crate::transport::tls::ensure_self_signed(&cert_path, &key_path)
    {
        error!(error = %err, "failed to provision self-signed certificate; WebTransport disabled");
        return None;
    }

    let tokens = if secure {
        let tokens_path = std::env::var_os("PHUX_WS_TOKENS")
            .map_or_else(crate::auth::default_token_store_path, PathBuf::from);
        let store = match crate::auth::TokenStore::load(&tokens_path) {
            Ok(store) => store,
            Err(err) => {
                error!(error = %err, path = %tokens_path.display(), "failed to load token store; WebTransport disabled");
                return None;
            }
        };
        if store.is_empty() {
            warn!(
                path = %tokens_path.display(),
                "no pairing tokens; every remote WebTransport consumer is rejected until `phux pair`"
            );
        }
        Some(std::sync::Arc::new(store))
    } else {
        None
    };
    let token_count = tokens.as_ref().map_or(0, |s| s.len());

    match crate::transport::webtransport::WtListener::from_pem(addr, &cert_path, &key_path, tokens)
    {
        Ok(wt) => {
            let bound = wt.local_addr().map(|a| a.to_string()).unwrap_or_default();
            if secure {
                info!(addr = %bound, tokens = token_count, "WebTransport listening with TLS + token auth");
            } else {
                info!(addr = %bound, "WebTransport listening (TLS, loopback, unauthenticated)");
            }
            Some(wt)
        }
        Err(err) => {
            warn!(addr = %addr, error = %err, "failed to bind WebTransport; UDS only");
            None
        }
    }
}

/// Queue an `ERROR` frame on `out_tx`. Used by attach failure paths.
pub(crate) async fn send_error(
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    code: ErrorCode,
    message: &str,
) {
    if out_tx
        .send(Outbound::Frame(FrameKind::Error {
            request_id: None,
            code,
            message: message.to_owned(),
        }))
        .await
        .is_err()
    {
        trace!(?code, "ERROR send dropped: writer gone");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Used only by the tests below — scoped here rather than at module level
    // so the lib's import set stays clean under `-D warnings`.
    use crate::state::ClientId;
    use crate::terminal_actor::ResizeRequest;
    use phux_protocol::caps::ClientCapabilities;
    use phux_protocol::wire::frame::{AttachTarget, ViewportInfo};
    use tokio::task::JoinSet;

    /// Ceiling for "this frame was already handed to the mailbox, so reading
    /// it back should be immediate" waits.
    ///
    /// Not load-bearing. Nothing below asserts a latency — the assertions are
    /// on frame *identity* and *order* — so the only job of the timeout is to
    /// turn a wedged handler into a bounded failure rather than a hung
    /// binary. Every one of these resolves in microseconds when the runtime
    /// gets a core.
    ///
    /// Deliberately far above the old 1-2s (phux-br1f): these tests share a
    /// machine with the whole suite, and a current-thread runtime that loses
    /// its core for a couple of seconds turns "the handler emitted ATTACHED"
    /// into "attached frame did not arrive". A suite that cries wolf gets
    /// ignored, which is how a real regression ships. A handler that genuinely
    /// never emits still fails, just 30s later, with the same message.
    const MAILBOX_DEADLINE: Duration = Duration::from_secs(30);

    #[test]
    fn socket_path_at_the_platform_limit_is_accepted() {
        // Exactly MAX_SOCKET_PATH_LEN bytes: a leading '/' plus the fill.
        let path = PathBuf::from(format!("/{}", "a".repeat(MAX_SOCKET_PATH_LEN - 1)));
        assert_eq!(path.as_os_str().len(), MAX_SOCKET_PATH_LEN);
        validate_socket_path_len(&path).unwrap();
    }

    #[test]
    fn socket_path_over_the_platform_limit_names_limit_and_length() {
        let len = MAX_SOCKET_PATH_LEN + 1;
        let path = PathBuf::from(format!("/{}", "a".repeat(len - 1)));
        let err = validate_socket_path_len(&path).unwrap_err();
        assert!(matches!(err, ServerError::SocketPathTooLong { .. }));
        let msg = err.to_string();
        assert!(
            msg.contains(&format!("{len} bytes")),
            "offending length missing from: {msg}"
        );
        assert!(
            msg.contains(&format!("{MAX_SOCKET_PATH_LEN} bytes")),
            "platform limit missing from: {msg}"
        );
        assert!(
            msg.contains("/tmp"),
            "shorter-path remedy missing from: {msg}"
        );
    }

    #[test]
    fn detach_aborts_raw_output_pumps_without_closing_writer_mailbox() {
        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        let local = LocalSet::new();
        local.block_on(&rt, async {
            let client_id = ClientId(7);
            let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<Outbound>(8);
            let (output_tx, _seed_rx) = tokio::sync::broadcast::channel::<bytes::Bytes>(8);
            let mut output_rx = output_tx.subscribe();
            let mut output_pumps = JoinSet::new();
            let terminal_id = phux_protocol::ids::TerminalId::local(42);

            let pump_out_tx = out_tx.clone();
            let pump_terminal_id = terminal_id.clone();
            output_pumps.spawn_local(async move {
                let mut seq: u64 = 0;
                while let Ok(bytes) = output_rx.recv().await {
                    seq = seq.wrapping_add(1);
                    if pump_out_tx
                        .send(Outbound::Frame(FrameKind::TerminalOutput {
                            terminal_id: pump_terminal_id.clone(),
                            stream_id: phux_protocol::ids::StreamId::new(1)
                                .expect("test stream id"),
                            bootstrap_id: phux_protocol::ids::BootstrapId::new(1)
                                .expect("test bootstrap id"),
                            seq,
                            bytes,
                        }))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });

            output_tx
                .send(bytes::Bytes::from_static(b"before-detach"))
                .unwrap();
            let first = tokio::time::timeout(MAILBOX_DEADLINE, out_rx.recv())
                .await
                .expect("first output timed out")
                .expect("writer mailbox closed");
            assert!(matches!(
                first,
                Outbound::Frame(FrameKind::TerminalOutput { seq: 1, .. })
            ));

            abort_output_pumps(&mut output_pumps, client_id, "test-detach").await;
            assert!(output_pumps.is_empty());

            // The writer mailbox remains usable after DETACH so the server
            // can still emit DETACHED or serve a later ATTACH on the same
            // connection, but the old per-pane pump no longer forwards bytes.
            assert!(
                out_tx
                    .send(Outbound::Frame(FrameKind::Detached))
                    .await
                    .is_ok()
            );
            assert!(
                output_tx
                    .send(bytes::Bytes::from_static(b"after-detach"))
                    .is_ok()
            );

            let detached = tokio::time::timeout(MAILBOX_DEADLINE, out_rx.recv())
                .await
                .expect("DETACHED timed out")
                .expect("writer mailbox closed");
            assert!(matches!(detached, Outbound::Frame(FrameKind::Detached)));
            tokio::task::yield_now().await;
            assert!(
                out_rx.try_recv().is_err(),
                "old output pump forwarded after detach"
            );
        });
    }

    /// `VIEWPORT_RESIZE` updates the focused pane's stored dims on the
    /// canonical `Registry`. byc.5's PTY-resize integration will read
    /// this state when it lands; today we just observe the mutation.
    #[test]
    fn viewport_resize_updates_focused_pane_dims() {
        use phux_core::ids::TerminalId as CoreTerminalId;

        let state = SharedState::new();
        // Seed a session with a pane, then attach a client. Mirrors what
        // `seed_session_with_actor` does on the real path, minus the
        // TerminalActor spawn (we're not exercising the actor here — just
        // the state-side dim update).
        let (sid, _wid, pid): (_, _, CoreTerminalId) =
            state.with_mut(|s| s.seed_session("test-session"));
        let client_id = state.with_mut(crate::state::ServerState::new_client_id);
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        state
            .with_mut(|s| s.attach_default_caps(client_id, "test-session", tx))
            .expect("attach");

        // Sanity: starts at 80x24 (default core::Pane::dims).
        let before = state
            .with(|s| s.registry.terminal(pid).map(|p| p.dims))
            .expect("pane exists");
        assert_eq!(before, (80, 24));

        let viewport = ViewportInfo::new(132, 50).with_pixels(Some(1320), Some(750));
        handle_viewport_resize(&state, client_id, &viewport);

        let after = state
            .with(|s| s.registry.terminal(pid).map(|p| p.dims))
            .expect("pane exists");
        assert_eq!(after, (132, 50));

        // Sanity: the session linkage didn't get clobbered.
        let attached_session = state.with(|s| s.attached.get(&client_id).map(|c| c.session));
        assert_eq!(attached_session, Some(sid));
    }

    /// `VIEWPORT_RESIZE` fans the new (cols, rows) tuple onto the
    /// `TerminalHandle::resize` channel byc.5 added. We inject a hand-
    /// built `TerminalHandle` (no real actor) so the test can observe the
    /// receiver side directly — this pins the wire from
    /// `handle_viewport_resize` into the actor without needing to
    /// stand up libghostty or a PTY pair.
    #[test]
    fn viewport_resize_sends_to_terminal_actor_resize_channel() {
        use crate::terminal_actor::TerminalHandle;
        use phux_core::ids::TerminalId as CoreTerminalId;
        use tokio::sync::{broadcast, mpsc};

        let state = SharedState::new();
        let (_sid, _wid, pid): (_, _, CoreTerminalId) =
            state.with_mut(|s| s.seed_session("test-session"));

        // Build a `TerminalHandle` directly. The actor side is not running;
        // we only care that `handle.resize.try_send` lands. The other
        // channels exist purely to satisfy the struct shape.
        let (input_tx, _input_rx) = mpsc::channel(8);
        let (snapshot_tx, _snapshot_rx) = mpsc::channel(8);
        let (screen_tx, _screen_rx) = mpsc::channel(8);
        let (pwd_tx, _pwd_rx) = mpsc::channel(8);
        let (output_tx, _output_rx_seed) =
            broadcast::channel::<crate::terminal_actor::PaneOutput>(8);
        let (resize_tx, mut resize_rx) = mpsc::channel::<ResizeRequest>(8);
        let (consumer_attach_tx, _consumer_attach_rx) = mpsc::channel(8);
        let (consumer_detach_tx, _consumer_detach_rx) = mpsc::channel(8);
        let (consumer_ack_tx, _consumer_ack_rx) = mpsc::channel(8);
        let (subscribe_to_events_tx, _subscribe_to_events_rx) = mpsc::channel(8);
        let (unsubscribe_from_events_tx, _unsubscribe_from_events_rx) = mpsc::channel(8);
        let handle = TerminalHandle {
            input: input_tx,
            encoded_input: mpsc::channel(8).0,
            input_snapshot: tokio::sync::watch::channel(
                crate::input::InputEncoderSnapshot::default(),
            )
            .1,
            snapshot: snapshot_tx,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_bootstrap: mpsc::channel(8).0,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_history: mpsc::channel(8).0,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_release: mpsc::channel(8).0,
            set_default_colors: mpsc::channel(8).0,
            screen: screen_tx,
            upgrade: mpsc::channel::<crate::terminal_actor::UpgradeHandleRequest>(8).0,
            pwd: pwd_tx,
            output: output_tx,
            resize: resize_tx,
            consumer_attach: consumer_attach_tx,
            consumer_detach: consumer_detach_tx,
            consumer_ack: consumer_ack_tx,
            subscribe_to_events: subscribe_to_events_tx,
            unsubscribe_from_events: unsubscribe_from_events_tx,
            control: mpsc::channel(8).0,
            cols: 80,
            rows: 24,
        };
        state.with_mut(|s| {
            // `register_terminal_handle` wants a CancellationToken; build
            // a fresh one. We don't keep a clone — no actor is running
            // for this test, so cancellation is moot.
            let token = CancellationToken::new();
            let _ = s.register_terminal_handle(pid, handle, token);
        });

        let client_id = state.with_mut(crate::state::ServerState::new_client_id);
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        state
            .with_mut(|s| s.attach_default_caps(client_id, "test-session", tx))
            .expect("attach");

        let viewport = ViewportInfo::new(132, 50);
        handle_viewport_resize(&state, client_id, &viewport);

        // The connector ran inside the same task; the channel must
        // already carry exactly one resize request.
        let observed = resize_rx
            .try_recv()
            .expect("resize request must be queued on the channel");
        assert_eq!(
            (observed.cols, observed.rows),
            (132, 50),
            "TerminalHandle::resize must receive the new viewport dims",
        );
        assert!(
            observed.resync_clients,
            "a live VIEWPORT_RESIZE must request a client resync (phux-8v1)",
        );
        assert_eq!(
            observed.cell_px, None,
            "a pixel-less viewport report must not invent a cell size",
        );
        assert!(
            resize_rx.try_recv().is_err(),
            "exactly one resize request should be queued — got more",
        );

        // A pixel-bearing report resolves the per-cell size and rides the
        // same request: 1320x750 px over 132x50 cells -> 10x15 px cells.
        let viewport = ViewportInfo::new(132, 50).with_pixels(Some(1320), Some(750));
        handle_viewport_resize(&state, client_id, &viewport);
        let observed = resize_rx
            .try_recv()
            .expect("second resize request must be queued on the channel");
        assert_eq!(
            observed.cell_px,
            Some((10, 15)),
            "the resolved cell pixel size must reach the actor",
        );
    }

    /// Source-allocation proof for the ATTACH per-pane snapshot sequence.
    ///
    /// Builds N hand-crafted `TerminalHandle`s and verifies each snapshot
    /// request is completed before the next one arrives. That ordering lets
    /// the aggregate host charge retained bytes and pass only the remaining
    /// connection-wide allocation ceiling to the next actor.
    #[tokio::test(flavor = "current_thread")]
    #[allow(
        clippy::too_many_lines,
        reason = "linear setup-then-act-then-assert test body; splitting would obscure the allocation proof"
    )]
    async fn handle_attach_bounds_snapshot_sources_sequentially() {
        use phux_core::ids::TerminalId as CoreTerminalId;
        use tokio::sync::{broadcast, mpsc};
        use tokio::task::LocalSet;

        use crate::grid::SnapshotBytes;
        use crate::terminal_actor::{SnapshotRequest, TerminalHandle};

        const N: usize = 4;

        let local = LocalSet::new();
        local
            .run_until(async {
                let state = SharedState::new();
                // Seed one session with one window and N panes.
                let (sid, wid, _first_pane) = state.with_mut(|s| s.seed_session("multi"));
                // `seed_session` made one pane already; we want N total.
                let mut terminal_ids: Vec<CoreTerminalId> = Vec::with_capacity(N);
                state.with_mut(|s| {
                    let session = s.registry.session(sid).cloned().expect("session");
                    let window = s
                        .registry
                        .window(session.windows[0])
                        .cloned()
                        .expect("window");
                    terminal_ids.push(window.panes[0]);
                    for _ in 1..N {
                        let pid = s.registry.new_terminal(wid).expect("new_pane");
                        terminal_ids.push(pid);
                    }
                });

                // Build N TerminalHandles; keep the snapshot receivers in the test.
                let mut snapshot_rxs: Vec<mpsc::Receiver<SnapshotRequest>> = Vec::with_capacity(N);
                for &pid in &terminal_ids {
                    let (input_tx, _input_rx) = mpsc::channel(8);
                    let (snapshot_tx, snapshot_rx) = mpsc::channel(8);
                    let (screen_tx, _screen_rx) = mpsc::channel(8);
                    let (pwd_tx, _pwd_rx) = mpsc::channel(8);
                    let (output_tx, _output_rx_seed) =
                        broadcast::channel::<crate::terminal_actor::PaneOutput>(8);
                    let (resize_tx, _resize_rx) = mpsc::channel::<ResizeRequest>(8);
                    let (consumer_attach_tx, _consumer_attach_rx) = mpsc::channel(8);
                    let (consumer_detach_tx, _consumer_detach_rx) = mpsc::channel(8);
                    let (consumer_ack_tx, _consumer_ack_rx) = mpsc::channel(8);
                    let (subscribe_to_events_tx, _subscribe_to_events_rx) = mpsc::channel(8);
                    let (unsubscribe_from_events_tx, _unsubscribe_from_events_rx) =
                        mpsc::channel(8);
                    let handle = TerminalHandle {
                        input: input_tx,
                        encoded_input: mpsc::channel(8).0,
                        input_snapshot: tokio::sync::watch::channel(
                            crate::input::InputEncoderSnapshot::default(),
                        )
                        .1,
                        snapshot: snapshot_tx,
                        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                        native_bootstrap: mpsc::channel(8).0,
                        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                        native_history: mpsc::channel(8).0,
                        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                        native_release: mpsc::channel(8).0,
                        set_default_colors: mpsc::channel(8).0,
                        screen: screen_tx,
                        upgrade: mpsc::channel::<crate::terminal_actor::UpgradeHandleRequest>(8).0,
                        pwd: pwd_tx,
                        output: output_tx,
                        resize: resize_tx,
                        consumer_attach: consumer_attach_tx,
                        consumer_detach: consumer_detach_tx,
                        consumer_ack: consumer_ack_tx,
                        subscribe_to_events: subscribe_to_events_tx,
                        unsubscribe_from_events: unsubscribe_from_events_tx,
                        control: mpsc::channel(8).0,
                        cols: 80,
                        rows: 24,
                    };
                    state.with_mut(|s| {
                        let _ = s.register_terminal_handle(pid, handle, CancellationToken::new());
                    });
                    snapshot_rxs.push(snapshot_rx);
                }

                // Outbound channel for the would-be writer task; we read
                // TERMINAL_SNAPSHOT frames out of `out_rx` to verify all N
                // shipped.
                let (out_tx, mut out_rx) =
                    mpsc::channel::<Outbound>(crate::state::DEFAULT_CLIENT_MAILBOX);
                let client_id = state.with_mut(crate::state::ServerState::new_client_id);

                // Spawn `handle_attach` on the LocalSet so the test
                // body can interleave with it.
                let state_for_task = state.clone();
                let test_root_token = CancellationToken::new();
                let attach_task = tokio::task::spawn_local(async move {
                    let mut output_pumps = JoinSet::new();
                    handle_attach(
                        &state_for_task,
                        client_id,
                        1,
                        AttachTarget::ByName("multi".to_owned()),
                        ViewportInfo::new(80, 24),
                        false,
                        0,
                        &out_tx,
                        ClientCapabilities::default(),
                        phux_protocol::caps::BootstrapProfile::SynthesizedVtRaw,
                        phux_protocol::caps::BootstrapLimits::default(),
                        &test_root_token,
                        &mut output_pumps,
                        &test_root_token,
                    )
                    .await;
                });

                // Each request must be completed before the next pane is
                // asked to allocate its source snapshot. Its byte ceiling can
                // therefore only shrink as retained aggregate state grows.
                let mut previous_max_bytes = usize::MAX;
                for (i, rx) in snapshot_rxs.iter_mut().enumerate() {
                    let req = tokio::time::timeout(MAILBOX_DEADLINE, rx.recv())
                        .await
                        .unwrap_or_else(|_| panic!("snapshot request {i} never arrived"))
                        .expect("snapshot channel closed");
                    assert!(req.max_bytes <= previous_max_bytes);
                    previous_max_bytes = req.max_bytes;
                    let payload = SnapshotBytes {
                        cols: 80,
                        rows: 24,
                        bytes: format!("snap-{i}").into_bytes(),
                        scrollback: Vec::new(),
                    };
                    req.reply
                        .send(Ok((payload, u64::try_from(i).unwrap())))
                        .expect("attach still waiting for snapshot");
                }
                // First the writer should see ATTACHED.
                let attached = tokio::time::timeout(MAILBOX_DEADLINE, out_rx.recv())
                    .await
                    .expect("attached frame did not arrive")
                    .expect("out_rx closed before attached");
                let Outbound::Frame(frame) = attached;
                assert!(
                    matches!(frame, FrameKind::Attached { .. }),
                    "expected Attached, got {frame:?}",
                );

                // Drain one BEGIN/CHUNK/READY sequence per pane.
                let mut begins = 0usize;
                let mut chunks = 0usize;
                let mut ready = 0usize;
                for _ in 0..(N * 3) {
                    let frame = tokio::time::timeout(MAILBOX_DEADLINE, out_rx.recv())
                        .await
                        .expect("pane bootstrap frame did not arrive")
                        .expect("out_rx closed before bootstrap completed");
                    match frame {
                        Outbound::Frame(FrameKind::BootstrapBegin { .. }) => begins += 1,
                        Outbound::Frame(FrameKind::BootstrapChunk { .. }) => chunks += 1,
                        Outbound::Frame(FrameKind::BootstrapReady { .. }) => ready += 1,
                        other => panic!("expected bootstrap frame, got {other:?}"),
                    }
                }
                assert_eq!((begins, chunks, ready), (N, N, N));
                let attach_ready = tokio::time::timeout(MAILBOX_DEADLINE, out_rx.recv())
                    .await
                    .expect("ATTACH_READY did not arrive")
                    .expect("out_rx closed before ATTACH_READY");
                assert!(matches!(
                    attach_ready,
                    Outbound::Frame(FrameKind::AttachReady { attach_id: 1 })
                ));

                attach_task.await.expect("attach task panicked");
            })
            .await;
    }

    /// phux-0q8: ATTACH wires the per-consumer state-sync lifecycle.
    /// `handle_attach` must send a `ConsumerAttachRequest` (carrying the
    /// resolved wire terminal id) and await its reply before streaming;
    /// the DETACH-class teardown helper must send a matching
    /// `ConsumerDetachRequest` so the actor frees the per-consumer
    /// `RenderState`. We inject a hand-built `TerminalHandle` and hold the
    /// consumer-lifecycle receivers so the test observes both ends without
    /// standing up a libghostty actor.
    #[tokio::test(flavor = "current_thread")]
    #[allow(
        clippy::too_many_lines,
        reason = "linear setup-attach-observe-detach-observe body; splitting would scatter the lifecycle proof"
    )]
    async fn attach_registers_and_detach_unregisters_consumer_lifecycle() {
        use phux_core::ids::TerminalId as CoreTerminalId;
        use tokio::sync::{broadcast, mpsc};
        use tokio::task::LocalSet;

        use crate::grid::SnapshotBytes;
        use crate::terminal_actor::{
            ConsumerAttachRequest, ConsumerDetachRequest, SnapshotRequest, TerminalHandle,
        };

        let local = LocalSet::new();
        local
            .run_until(async {
                let state = SharedState::new();
                let (_sid, _wid, pid): (_, _, CoreTerminalId) =
                    state.with_mut(|s| s.seed_session("lifecycle"));

                let (input_tx, _input_rx) = mpsc::channel(8);
                let (snapshot_tx, _snapshot_rx) = mpsc::channel::<SnapshotRequest>(8);
                let (screen_tx, _screen_rx) = mpsc::channel(8);
                let (pwd_tx, _pwd_rx) = mpsc::channel(8);
                let (output_tx, _output_rx_seed) =
                    broadcast::channel::<crate::terminal_actor::PaneOutput>(8);
                let (resize_tx, _resize_rx) = mpsc::channel::<ResizeRequest>(8);
                let (consumer_attach_tx, mut consumer_attach_rx) =
                    mpsc::channel::<ConsumerAttachRequest>(8);
                let (consumer_detach_tx, mut consumer_detach_rx) =
                    mpsc::channel::<ConsumerDetachRequest>(8);
                let (consumer_ack_tx, _consumer_ack_rx) = mpsc::channel(8);
                let (subscribe_to_events_tx, _subscribe_to_events_rx) = mpsc::channel(8);
                let (unsubscribe_from_events_tx, _unsubscribe_from_events_rx) = mpsc::channel(8);
                let handle = TerminalHandle {
                    input: input_tx,
                    encoded_input: mpsc::channel(8).0,
                    input_snapshot: tokio::sync::watch::channel(
                        crate::input::InputEncoderSnapshot::default(),
                    )
                    .1,
                    snapshot: snapshot_tx,
                    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                    native_bootstrap: mpsc::channel(8).0,
                    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                    native_history: mpsc::channel(8).0,
                    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                    native_release: mpsc::channel(8).0,
                    set_default_colors: mpsc::channel(8).0,
                    screen: screen_tx,
                    upgrade: mpsc::channel::<crate::terminal_actor::UpgradeHandleRequest>(8).0,
                    pwd: pwd_tx,
                    output: output_tx,
                    resize: resize_tx,
                    consumer_attach: consumer_attach_tx,
                    consumer_detach: consumer_detach_tx,
                    consumer_ack: consumer_ack_tx,
                    subscribe_to_events: subscribe_to_events_tx,
                    unsubscribe_from_events: unsubscribe_from_events_tx,
                    control: mpsc::channel(8).0,
                    cols: 80,
                    rows: 24,
                };
                state.with_mut(|s| {
                    let _ = s.register_terminal_handle(pid, handle, CancellationToken::new());
                });

                let (out_tx, mut out_rx) =
                    mpsc::channel::<Outbound>(crate::state::DEFAULT_CLIENT_MAILBOX);
                let client_id = state.with_mut(crate::state::ServerState::new_client_id);

                let state_for_task = state.clone();
                let token = CancellationToken::new();
                let first_out_tx = out_tx.clone();
                let attach_task = tokio::task::spawn_local(async move {
                    let mut output_pumps = JoinSet::new();
                    handle_attach(
                        &state_for_task,
                        client_id,
                        1,
                        AttachTarget::ByName("lifecycle".to_owned()),
                        ViewportInfo::new(80, 24),
                        false,
                        0,
                        &first_out_tx,
                        ClientCapabilities::default()
                            .with_output_mode(phux_protocol::caps::OutputMode::StateSync),
                        phux_protocol::caps::BootstrapProfile::SynthesizedVtStateSync,
                        phux_protocol::caps::BootstrapLimits::default(),
                        &token,
                        &mut output_pumps,
                        &token,
                    )
                    .await;
                });

                // The consumer-attach request must land, carrying the wire
                // terminal id. Reply Ok so `handle_attach` proceeds.
                let attach_req = tokio::time::timeout(MAILBOX_DEADLINE, consumer_attach_rx.recv())
                    .await
                    .expect("ConsumerAttachRequest never arrived — register not wired?")
                    .expect("consumer_attach channel closed");
                let attached_wire_client_id = attach_req.client_id;
                assert_eq!(
                    attached_wire_client_id,
                    phux_protocol::ids::ClientId::new(
                        u32::try_from(client_id.0).unwrap_or(u32::MAX)
                    ),
                    "consumer attach keyed by the wire client id",
                );
                assert!(
                    attach_req.wire_terminal_id >= 1,
                    "wire terminal id assigned"
                );
                let live_gate = attach_req.live_gate.clone();
                assert!(
                    !*live_gate.borrow(),
                    "live output must wait for ATTACH_READY"
                );
                // A state-sync consumer is actor-managed; its watch gate is
                // the only thing preventing pre-ATTACH_READY live deltas.
                attach_req
                    .reply
                    .send(Ok(crate::terminal_actor::ConsumerAttachOutcome {
                        tick_managed: true,
                        state_sync_bootstrap: Some(crate::terminal_actor::StateSyncBootstrap {
                            snapshot: SnapshotBytes {
                                cols: 80,
                                rows: 24,
                                bytes: b"snap".to_vec(),
                                scrollback: Vec::new(),
                            },
                            base_seq: 0,
                        }),
                    }))
                    .expect("send attach reply");
                // ATTACHED first.
                let attached = tokio::time::timeout(MAILBOX_DEADLINE, out_rx.recv())
                    .await
                    .expect("attached frame did not arrive")
                    .expect("out_rx closed");
                assert!(matches!(
                    attached,
                    Outbound::Frame(FrameKind::Attached { .. })
                ));

                attach_task.await.expect("attach task panicked");
                for expected in [
                    "BOOTSTRAP_BEGIN",
                    "BOOTSTRAP_CHUNK",
                    "BOOTSTRAP_READY",
                    "ATTACH_READY",
                ] {
                    let queued = tokio::time::timeout(MAILBOX_DEADLINE, out_rx.recv())
                        .await
                        .expect("first bootstrap frame did not arrive")
                        .expect("outbound closed during first bootstrap");
                    let Outbound::Frame(frame) = queued;
                    let observed = match frame {
                        FrameKind::BootstrapBegin { .. } => "BOOTSTRAP_BEGIN",
                        FrameKind::BootstrapChunk { .. } => "BOOTSTRAP_CHUNK",
                        FrameKind::BootstrapReady { .. } => "BOOTSTRAP_READY",
                        FrameKind::AttachReady { .. } => "ATTACH_READY",
                        other => panic!("unexpected first-bootstrap frame: {other:?}"),
                    };
                    assert_eq!(observed, expected);
                }

                // A replacement ATTACH to the same session must first retire
                // the prior actor-side state-sync emitter. It then allocates a
                // fresh stream/bootstrap generation without allocating a new
                // server ClientId or detaching the session.
                let second_state = state.clone();
                let second_out_tx = out_tx.clone();
                let second_token = CancellationToken::new();
                let second_attach = tokio::task::spawn_local(async move {
                    let mut output_pumps = JoinSet::new();
                    handle_attach(
                        &second_state,
                        client_id,
                        2,
                        AttachTarget::ByName("lifecycle".to_owned()),
                        ViewportInfo::new(80, 24),
                        false,
                        0,
                        &second_out_tx,
                        ClientCapabilities::default()
                            .with_output_mode(phux_protocol::caps::OutputMode::StateSync),
                        phux_protocol::caps::BootstrapProfile::SynthesizedVtStateSync,
                        phux_protocol::caps::BootstrapLimits::default(),
                        &second_token,
                        &mut output_pumps,
                        &second_token,
                    )
                    .await;
                });
                let retired = tokio::time::timeout(MAILBOX_DEADLINE, consumer_detach_rx.recv())
                    .await
                    .expect("replacement did not retire the prior consumer")
                    .expect("consumer detach channel closed");
                assert_eq!(retired.client_id, attached_wire_client_id);
                assert!(
                    out_rx.try_recv().is_err(),
                    "replacement ATTACHED must wait until prior live emission is retired",
                );
                retired
                    .reply
                    .send(())
                    .expect("ack prior consumer retirement");

                let replacement = tokio::time::timeout(MAILBOX_DEADLINE, consumer_attach_rx.recv())
                    .await
                    .expect("replacement ConsumerAttachRequest did not arrive")
                    .expect("consumer attach channel closed");
                let replacement_gate = replacement.live_gate.clone();
                assert!(!*replacement_gate.borrow());
                replacement
                    .reply
                    .send(Ok(crate::terminal_actor::ConsumerAttachOutcome {
                        tick_managed: true,
                        state_sync_bootstrap: Some(crate::terminal_actor::StateSyncBootstrap {
                            snapshot: SnapshotBytes {
                                cols: 80,
                                rows: 24,
                                bytes: b"replacement".to_vec(),
                                scrollback: Vec::new(),
                            },
                            base_seq: 0,
                        }),
                    }))
                    .expect("ack replacement consumer");
                let attached = tokio::time::timeout(MAILBOX_DEADLINE, out_rx.recv())
                    .await
                    .expect("replacement ATTACHED did not arrive")
                    .expect("outbound closed before replacement ATTACHED");
                assert!(matches!(
                    attached,
                    Outbound::Frame(FrameKind::Attached { attach_id: 2, .. })
                ));
                for expected in [
                    "BOOTSTRAP_BEGIN",
                    "BOOTSTRAP_CHUNK",
                    "BOOTSTRAP_READY",
                    "ATTACH_READY",
                ] {
                    let queued = tokio::time::timeout(MAILBOX_DEADLINE, out_rx.recv())
                        .await
                        .expect("replacement bootstrap frame did not arrive")
                        .expect("outbound closed during replacement bootstrap");
                    let Outbound::Frame(frame) = queued;
                    let observed = match frame {
                        FrameKind::BootstrapBegin { .. } => "BOOTSTRAP_BEGIN",
                        FrameKind::BootstrapChunk { .. } => "BOOTSTRAP_CHUNK",
                        FrameKind::BootstrapReady { .. } => "BOOTSTRAP_READY",
                        FrameKind::AttachReady { .. } => "ATTACH_READY",
                        other => panic!("unexpected replacement-bootstrap frame: {other:?}"),
                    };
                    assert_eq!(observed, expected);
                }
                second_attach
                    .await
                    .expect("replacement attach task panicked");
                assert!(*replacement_gate.borrow());
                assert!(
                    *live_gate.borrow(),
                    "aggregate completion must release live output"
                );

                // Now tear the client down. The helper must send a
                // ConsumerDetachRequest for the subscribed pane.
                detach_and_release_consumer_state(&state, client_id);
                let detach_req = tokio::time::timeout(MAILBOX_DEADLINE, consumer_detach_rx.recv())
                    .await
                    .expect("ConsumerDetachRequest never arrived — detach not wired?")
                    .expect("consumer_detach channel closed");
                assert_eq!(
                    detach_req.client_id,
                    phux_protocol::ids::ClientId::new(
                        u32::try_from(client_id.0).unwrap_or(u32::MAX)
                    ),
                    "consumer detach keyed by the same wire client id",
                );

                // And the client is gone from ServerState.
                assert!(
                    state.with(|s| !s.attached.contains_key(&client_id)),
                    "detach helper must remove the client from ServerState",
                );
            })
            .await;
    }

    /// docs/consumers/tui.md §9 (phux-r82.1): seeding a session fires the
    /// `after-new-pane` hook with the pane's wire id and the session name
    /// in context.
    #[test]
    fn seed_session_fires_after_new_pane_hook() {
        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        let local = LocalSet::new();
        local.block_on(&rt, async {
            let state = SharedState::new();
            let (tx, mut rx) = tokio::sync::mpsc::channel(8);
            state.with_mut(|s| {
                s.set_hook_dispatcher(crate::hooks::HookDispatcher::from_sender(tx));
            });
            let token = CancellationToken::new();
            seed_session_with_actor(&state, "hooked", 100, &token).expect("seed");
            let event = rx.try_recv().expect("after-new-pane fired");
            assert_eq!(event.name, crate::hooks::AFTER_NEW_PANE);
            assert_eq!(
                event.context.get("session").map(String::as_str),
                Some("hooked"),
            );
            assert!(
                event.context.contains_key("terminal-id"),
                "context must carry the pane's wire id",
            );
        });
    }

    /// docs/consumers/tui.md §9 (phux-r82.1): the per-pane exit watcher
    /// fires `pane-exit` with the reported exit code in context.
    #[test]
    fn exit_watcher_fires_pane_exit_hook_with_exit_code() {
        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        let local = LocalSet::new();
        local.block_on(&rt, async {
            let state = SharedState::new();
            let (hook_tx, mut hook_rx) = tokio::sync::mpsc::channel(8);
            state.with_mut(|s| {
                s.set_hook_dispatcher(crate::hooks::HookDispatcher::from_sender(hook_tx));
            });
            let (_sid, _wid, pane) = state.with_mut(|s| s.seed_session("dying"));
            let (exit_tx, exit_rx) = tokio::sync::oneshot::channel::<Option<i32>>();
            let token = CancellationToken::new();
            spawn_terminal_exit_watcher(state.clone(), pane, Some(exit_rx), token);
            exit_tx.send(Some(3)).expect("exit notify");
            let event = tokio::time::timeout(MAILBOX_DEADLINE, hook_rx.recv())
                .await
                .expect("pane-exit hook timed out")
                .expect("hook channel closed");
            assert_eq!(event.name, crate::hooks::PANE_EXIT);
            assert_eq!(
                event.context.get("exit-code").map(String::as_str),
                Some("3"),
            );
            assert!(event.context.contains_key("terminal-id"));
        });
    }

    /// Input authority routes opaque emulator replies byte-for-byte while
    /// rejecting an unsubscribed client. The same gate still fires the
    /// focus-changed hook only for an authorized focus-gained event.
    #[test]
    fn input_authority_routes_terminal_replies_and_focus_hooks() {
        use phux_protocol::input::focus::FocusEvent;
        use tokio::sync::{broadcast, mpsc};

        use crate::terminal_actor::TerminalHandle;

        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        let local = LocalSet::new();
        local.block_on(&rt, async {
            let state = SharedState::new();
            let (hook_tx, mut hook_rx) = tokio::sync::mpsc::channel(8);
            state.with_mut(|s| {
                s.set_hook_dispatcher(crate::hooks::HookDispatcher::from_sender(hook_tx));
            });
            let (_sid, _wid, pane) = state.with_mut(|s| s.seed_session("focus"));

            // Hand-built handle: only the input channel matters here.
            let (input_tx, _input_rx) = mpsc::channel(8);
            let (encoded_tx, mut encoded_rx) = mpsc::channel(8);
            let (output_tx, _output_rx_seed) =
                broadcast::channel::<crate::terminal_actor::PaneOutput>(8);
            let handle = TerminalHandle {
                input: input_tx,
                encoded_input: encoded_tx,
                input_snapshot: tokio::sync::watch::channel(
                    crate::input::InputEncoderSnapshot::default(),
                )
                .1,
                snapshot: mpsc::channel(8).0,
                #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                native_bootstrap: mpsc::channel(8).0,
                #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                native_history: mpsc::channel(8).0,
                #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                native_release: mpsc::channel(8).0,
                set_default_colors: mpsc::channel(8).0,
                screen: mpsc::channel(8).0,
                upgrade: mpsc::channel::<crate::terminal_actor::UpgradeHandleRequest>(8).0,
                pwd: mpsc::channel(8).0,
                output: output_tx,
                resize: mpsc::channel::<ResizeRequest>(8).0,
                consumer_attach: mpsc::channel(8).0,
                consumer_detach: mpsc::channel(8).0,
                consumer_ack: mpsc::channel(8).0,
                subscribe_to_events: mpsc::channel(8).0,
                unsubscribe_from_events: mpsc::channel(8).0,
                control: mpsc::channel(8).0,
                cols: 80,
                rows: 24,
            };
            let wire_terminal_id = state.with_mut(|s| {
                let _ = s.register_terminal_handle(pane, handle, CancellationToken::new());
                s.intern_terminal_wire(pane)
            });

            let client_id = state.with_mut(crate::state::ServerState::new_client_id);
            let (tx, _rx) = tokio::sync::mpsc::channel(8);
            state
                .with_mut(|s| s.attach_default_caps(client_id, "focus", tx))
                .expect("attach");
            let reply = bytes::Bytes::from_static(b"\0\x1b[?1;2c\xff");
            handle_terminal_reply(&state, client_id, &wire_terminal_id, reply.clone());
            let routed = encoded_rx
                .try_recv()
                .expect("authorized terminal reply reaches the PTY byte lane");
            assert_eq!(
                routed.bytes, reply,
                "opaque bytes, including NUL, are exact"
            );

            let stranger = ClientId(4242);
            handle_terminal_reply(&state, stranger, &wire_terminal_id, reply);
            assert!(
                encoded_rx.try_recv().is_err(),
                "an unsubscribed client cannot inject a terminal reply"
            );

            // Focus gained → hook fires.
            handle_terminal_input(
                &state,
                client_id,
                &wire_terminal_id,
                crate::state::TerminalInput::Focus(FocusEvent::Gained),
                "INPUT_FOCUS",
            );
            let event = hook_rx.try_recv().expect("focus-changed fired");
            assert_eq!(event.name, crate::hooks::FOCUS_CHANGED);
            assert!(event.context.contains_key("terminal-id"));
            assert!(event.context.contains_key("client-id"));

            // Focus lost → no hook.
            handle_terminal_input(
                &state,
                client_id,
                &wire_terminal_id,
                crate::state::TerminalInput::Focus(FocusEvent::Lost),
                "INPUT_FOCUS",
            );
            assert!(
                hook_rx.try_recv().is_err(),
                "focus lost must not fire focus-changed",
            );

            // Focus gained from a non-attached client is gated → no hook.
            handle_terminal_input(
                &state,
                stranger,
                &wire_terminal_id,
                crate::state::TerminalInput::Focus(FocusEvent::Gained),
                "INPUT_FOCUS",
            );
            assert!(
                hook_rx.try_recv().is_err(),
                "a gated focus frame must not fire focus-changed",
            );
        });
    }

    /// docs/consumers/tui.md §9 (phux-r82.1): tearing down an attached
    /// client fires `client-detached` with the session name; a connection
    /// that never attached fires nothing.
    #[test]
    fn detach_fires_client_detached_hook_only_for_attached_clients() {
        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        let local = LocalSet::new();
        local.block_on(&rt, async {
            let state = SharedState::new();
            let (hook_tx, mut hook_rx) = tokio::sync::mpsc::channel(8);
            state.with_mut(|s| {
                s.set_hook_dispatcher(crate::hooks::HookDispatcher::from_sender(hook_tx));
            });
            let (_sid, _wid, _pane) = state.with_mut(|s| s.seed_session("leaving"));

            // Never-attached connection: teardown fires nothing.
            let stranger = state.with_mut(crate::state::ServerState::new_client_id);
            detach_and_release_consumer_state(&state, stranger);
            assert!(
                hook_rx.try_recv().is_err(),
                "an unattached connection must not fire client-detached",
            );

            // Attached client: teardown fires client-detached.
            let client_id = state.with_mut(crate::state::ServerState::new_client_id);
            let (tx, _rx) = tokio::sync::mpsc::channel(8);
            state
                .with_mut(|s| s.attach_default_caps(client_id, "leaving", tx))
                .expect("attach");
            detach_and_release_consumer_state(&state, client_id);
            let event = hook_rx.try_recv().expect("client-detached fired");
            assert_eq!(event.name, crate::hooks::CLIENT_DETACHED);
            assert_eq!(
                event.context.get("session").map(String::as_str),
                Some("leaving"),
            );
        });
    }

    /// A `VIEWPORT_RESIZE` from a non-attached client is a benign no-op —
    /// the handler must not panic or mutate state.
    #[test]
    fn viewport_resize_from_unattached_client_is_noop() {
        let state = SharedState::new();
        let (_sid, _wid, pid) = state.with_mut(|s| s.seed_session("session"));
        let bogus_client = ClientId(9999);
        let before = state
            .with(|s| s.registry.terminal(pid).map(|p| p.dims))
            .expect("pane exists");
        handle_viewport_resize(&state, bogus_client, &ViewportInfo::new(200, 60));
        let after = state
            .with(|s| s.registry.terminal(pid).map(|p| p.dims))
            .expect("pane exists");
        assert_eq!(before, after, "no mutation expected for unattached client");
    }
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    #[tokio::test(flavor = "current_thread")]
    #[allow(
        clippy::too_many_lines,
        reason = "end-to-end fatal resync setup keeps the fake actor, publication, and cleanup assertions together"
    )]
    async fn native_resync_capture_failure_closes_connection_and_rolls_back_consumer() {
        use phux_core::ids::TerminalId as CoreTerminalId;
        use phux_protocol::caps::{
            BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, EngineCodec,
            EngineFeatureSet,
        };
        use tokio::sync::{broadcast, mpsc};
        use tokio::task::LocalSet;

        use crate::terminal_actor::{
            ConsumerAttachOutcome, NativeBootstrapReply, PaneOutput, ResyncReason, TerminalHandle,
        };

        let local = LocalSet::new();
        local
            .run_until(async {
                let state = SharedState::new();
                let (_sid, _wid, terminal): (_, _, CoreTerminalId) =
                    state.with_mut(|s| s.seed_session("native-fatal"));
                let (output_tx, _output_seed) = broadcast::channel::<PaneOutput>(8);
                let (native_bootstrap_tx, mut native_bootstrap_rx) = mpsc::channel(8);
                let (consumer_attach_tx, mut consumer_attach_rx) = mpsc::channel(8);
                let (consumer_detach_tx, mut consumer_detach_rx) = mpsc::channel(8);
                let handle = TerminalHandle {
                    input: mpsc::channel(8).0,
                    encoded_input: mpsc::channel(8).0,
                    input_snapshot: tokio::sync::watch::channel(
                        crate::input::InputEncoderSnapshot::default(),
                    )
                    .1,
                    snapshot: mpsc::channel(8).0,
                    native_bootstrap: native_bootstrap_tx,
                    native_history: mpsc::channel(8).0,
                    native_release: mpsc::channel(8).0,
                    set_default_colors: mpsc::channel(8).0,
                    screen: mpsc::channel(8).0,
                    upgrade: mpsc::channel(8).0,
                    pwd: mpsc::channel(8).0,
                    output: output_tx.clone(),
                    resize: mpsc::channel(8).0,
                    consumer_attach: consumer_attach_tx,
                    consumer_detach: consumer_detach_tx,
                    consumer_ack: mpsc::channel(8).0,
                    subscribe_to_events: mpsc::channel(8).0,
                    unsubscribe_from_events: mpsc::channel(8).0,
                    control: mpsc::channel(8).0,
                    cols: 80,
                    rows: 24,
                };
                state.with_mut(|s| {
                    let _ = s.register_terminal_handle(terminal, handle, CancellationToken::new());
                });

                let client_id = state.with_mut(crate::state::ServerState::new_client_id);
                let (out_tx, mut out_rx) =
                    mpsc::channel::<Outbound>(crate::state::DEFAULT_CLIENT_MAILBOX);
                let connection_token = CancellationToken::new();
                let task_token = connection_token.clone();
                let state_for_task = state.clone();
                let out_for_task = out_tx.clone();
                let attach_task = tokio::task::spawn_local(async move {
                    let root_token = CancellationToken::new();
                    let mut output_pumps = JoinSet::new();
                    handle_attach(
                        &state_for_task,
                        client_id,
                        1,
                        AttachTarget::ByName("native-fatal".to_owned()),
                        ViewportInfo::new(80, 24),
                        false,
                        0,
                        &out_for_task,
                        ClientCapabilities::default(),
                        BootstrapProfile::NativeState {
                            codec: EngineCodec::LibghosttyCheckpointV2,
                            features: EngineFeatureSet::required_native(),
                        },
                        BootstrapLimits::default(),
                        &root_token,
                        &mut output_pumps,
                        &task_token,
                    )
                    .await;
                    task_token.cancelled().await;
                    abort_output_pumps(&mut output_pumps, client_id, "fatal-native-resync").await;
                });

                let registration =
                    tokio::time::timeout(MAILBOX_DEADLINE, consumer_attach_rx.recv())
                        .await
                        .expect("consumer registration timed out")
                        .expect("consumer registration sender closed");
                registration
                    .reply
                    .send(Ok(ConsumerAttachOutcome {
                        tick_managed: false,
                        state_sync_bootstrap: None,
                    }))
                    .expect("consumer registration reply");

                let initial = tokio::time::timeout(MAILBOX_DEADLINE, native_bootstrap_rx.recv())
                    .await
                    .expect("initial native request timed out")
                    .expect("native request sender closed");
                let terminal_id = initial.terminal_id.clone();
                let stream_id = initial.stream_id;
                let bootstrap_id = initial.bootstrap_id;
                initial
                    .reply
                    .send(Ok(NativeBootstrapReply {
                        frames: vec![
                            FrameKind::BootstrapBegin {
                                terminal_id: terminal_id.clone(),
                                stream_id,
                                bootstrap_id,
                                profile: BootstrapStreamProfile::NativeState {
                                    codec: EngineCodec::LibghosttyCheckpointV2,
                                },
                                cols: 80,
                                rows: 24,
                                base_seq: 0,
                            },
                            FrameKind::BootstrapChunk {
                                terminal_id: terminal_id.clone(),
                                stream_id,
                                bootstrap_id,
                                chunk_seq: 0,
                                payload: bytes::Bytes::from_static(b"opaque-checkpoint"),
                            },
                            FrameKind::BootstrapReady {
                                terminal_id,
                                stream_id,
                                bootstrap_id,
                                history_cursor: None,
                            },
                        ],
                        base_seq: 0,
                    }))
                    .expect("initial native reply");

                for expected in ["ATTACHED", "BEGIN", "CHUNK", "READY", "ATTACH_READY"] {
                    let Outbound::Frame(frame) =
                        tokio::time::timeout(MAILBOX_DEADLINE, out_rx.recv())
                            .await
                            .expect("bootstrap frame timed out")
                            .expect("outbound closed");
                    let actual = match frame {
                        FrameKind::Attached { .. } => "ATTACHED",
                        FrameKind::BootstrapBegin { .. } => "BEGIN",
                        FrameKind::BootstrapChunk { .. } => "CHUNK",
                        FrameKind::BootstrapReady { .. } => "READY",
                        FrameKind::AttachReady { .. } => "ATTACH_READY",
                        other => panic!("unexpected initial frame: {other:?}"),
                    };
                    assert_eq!(actual, expected);
                }

                output_tx
                    .send(PaneOutput::Live {
                        seq: 1,
                        bytes: bytes::Bytes::from_static(b"live"),
                    })
                    .expect("live receiver");
                assert!(matches!(
                    tokio::time::timeout(MAILBOX_DEADLINE, out_rx.recv())
                        .await
                        .expect("live output timed out")
                        .expect("outbound closed"),
                    Outbound::Frame(FrameKind::TerminalOutput { seq: 1, .. })
                ));

                output_tx
                    .send(PaneOutput::Resync {
                        cols: 80,
                        rows: 24,
                        bytes: bytes::Bytes::new(),
                        reason: ResyncReason::OutboundGap,
                        base_seq: 1,
                    })
                    .expect("resync receiver");
                assert!(matches!(
                    tokio::time::timeout(MAILBOX_DEADLINE, out_rx.recv())
                        .await
                        .expect("tombstone timed out")
                        .expect("outbound closed"),
                    Outbound::Frame(FrameKind::BootstrapTombstone {
                        last_valid_seq: 1,
                        ..
                    })
                ));
                let failed = tokio::time::timeout(MAILBOX_DEADLINE, native_bootstrap_rx.recv())
                    .await
                    .expect("replacement native request timed out")
                    .expect("native request sender closed");
                failed
                    .reply
                    .send(Err(crate::native_state::NativeStateError::OutOfMemory))
                    .expect("replacement failure reply");

                assert!(matches!(
                    tokio::time::timeout(MAILBOX_DEADLINE, out_rx.recv())
                        .await
                        .expect("fatal error timed out")
                        .expect("outbound closed"),
                    Outbound::Frame(FrameKind::Error {
                        code: ErrorCode::CodecUnavailable,
                        ..
                    })
                ));
                tokio::time::timeout(MAILBOX_DEADLINE, connection_token.cancelled())
                    .await
                    .expect("connection was not cancelled");
                tokio::time::timeout(MAILBOX_DEADLINE, consumer_detach_rx.recv())
                    .await
                    .expect("consumer rollback timed out")
                    .expect("consumer detach sender closed");
                assert!(
                    state.with(|s| !s.attached.contains_key(&client_id)),
                    "fatal native replacement left the aggregate consumer attached"
                );
                attach_task.await.expect("attach task");
            })
            .await;
    }
}
