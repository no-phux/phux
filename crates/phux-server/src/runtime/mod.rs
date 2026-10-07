//! Server runtime: accept loops, per-client tasks, and terminal actors.
//!
//! One current-thread tokio executor (ADR-0003) drives a
//! [`tokio::task::LocalSet`] (ADR-0014). The runtime binds the UDS
//! (`docs/spec/proto.md` §4) plus any configured remote listeners, and
//! unlinks the socket it bound on shutdown.
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
use crate::upgrade::blob::StateBlob;
use phux_protocol::wire::{
    ListenerDisabledReason, RemoteListenerSlot, RemoteListenerTransport, RemoteListenersReport,
};

pub mod attach;
pub mod client;
mod command_tasks;
pub mod commands;
mod committed_close;
mod directory;
mod ephemeral_listener;
pub mod idempotent_create;
pub mod input_lane;
pub mod keyed_ops;
pub mod operation_dedupe;
mod path_search;
mod process_env;
/// Shared per-generation state both pane output pumps enforce.
mod pump;
pub mod resource_commands;
mod resume;
pub mod revocation;
#[cfg(test)]
mod revocation_conformance;
mod upgrade;
mod upload;
mod voice;
mod whoami;

#[cfg(test)]
mod approval_matrix;
mod approvals;
mod dispatch_guard;
#[cfg(test)]
mod scope_matrix;
#[cfg(test)]
mod test_support;
mod workload_auth;

pub(crate) use attach::*;
pub(crate) use client::*;
pub use commands::*;
pub use process_env::ServerEnv;

/// Timeout for the "is the socket still live?" liveness probe used when an
/// existing socket file is encountered during bind.
pub(crate) const STALE_PROBE_TIMEOUT: Duration = Duration::from_millis(50);

/// A boxed accept loop, so heterogeneous listeners can share one
/// [`futures_util::future::select_all`] set.
type AcceptLoopFuture<'a> = std::pin::Pin<Box<dyn Future<Output = Result<(), ServerError>> + 'a>>;

/// Configuration for [`ServerRuntime`].
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Filesystem path to bind the Unix domain socket at.
    pub socket_path: PathBuf,
    /// Session to create (one window, one pane) at startup, so a client can
    /// attach without first issuing a command.
    pub pre_seeded_session: Option<String>,
    /// Whether the pre-seeded pane runs a real PTY child. `false` (tests,
    /// examples) gives a no-PTY actor that only serves snapshot/input
    /// plumbing.
    pub seed_with_pty: bool,
    /// Program for the pre-seeded PTY pane instead of [`Self::shell`].
    /// Ignored unless [`Self::seed_with_pty`].
    pub seed_command: Option<portable_pty::CommandBuilder>,
    /// Per-pane scrollback bounds (`defaults.history-limit` /
    /// `defaults.history-bytes`, ADR-0094).
    pub scrollback: phux_config::ScrollbackLimits,
    /// Bytes of agent event records each agent session retains
    /// (`defaults.agent-log-bytes`, ADR-0103 §4).
    pub agent_log_bytes: u32,
    /// Events the event journal retains for cursor replay
    /// (`defaults.event-journal-entries`, ADR-0123).
    pub event_journal_entries: u32,
    /// Estimated encoded bytes the event journal retains
    /// (`defaults.event-journal-bytes`); whichever bound hits first evicts.
    pub event_journal_bytes: u32,
    /// Retain-on-exit settings (`defaults.retain-on-exit*`, ADR-0124).
    pub retain: crate::state::RetainPolicy,
    /// How a new pane picks its working directory when the spawn leaves
    /// `cwd` unset (`defaults.cwd-inheritance`).
    pub cwd_inheritance: phux_config::CwdInheritance,
    /// `TERM` baseline for every server-spawned pane (`defaults.term`); a
    /// per-spawn `env` entry overrides it.
    pub term: String,
    /// Default shell for command-less spawns: `defaults.shell`, else
    /// `$SHELL`, else `/bin/sh` ([`crate::terminal_actor::resolve_shell`]).
    pub shell: String,
    /// Run [`Self::shell`] in login mode for command-less spawns. Set only
    /// when a service manager started the server, whose minimal environment
    /// never ran a login shell (ADR-0073); re-running profile init under a
    /// human-started server is not idempotent.
    pub login_shell: bool,
    /// How a Terminal shared by clients of differing sizes picks its PTY
    /// geometry (`defaults.window-size`).
    pub window_size: phux_config::WindowSize,
    /// `[voice]`: the transcriber behind `TRANSCRIBE`.
    pub voice: phux_config::VoiceCfg,
    /// `limits.metadata-value-bytes` (ADR-0129): largest L3 metadata value
    /// stored at one key.
    pub metadata_value_bytes: u32,
    /// `defaults.approval-ttl-secs` (ADR-0128): how long a held `SIGNAL`
    /// action waits for a decision before it expires.
    pub approval_ttl_secs: u32,
    /// `defaults.approval-max-pending` (ADR-0128): how many actions one
    /// connection may hold at once; one more is `RESOURCE_EXHAUSTED`.
    pub approval_max_pending: u32,
    /// `defaults.approval-max-pending-total` (ADR-0128): how many actions
    /// the whole server may hold at once.
    pub approval_max_pending_total: u32,
    /// HELLO authorization engine override (ADR-0072). `None` lets
    /// [`Self::policy_mode`] choose; tests and embedders inject one here.
    pub policy_engine: Option<std::sync::Arc<dyn crate::policy::PolicyEngine>>,
    /// `[policy] mode` (`docs/spec/workload-auth.md` §8). `None` is the
    /// transitional posture (every connection holds the owner's grant).
    pub policy_mode: Option<phux_config::PolicyMode>,
    /// Event-hook catalog; empty means no dispatcher task runs.
    pub hook_catalog: crate::hooks::HookCatalog,
    /// Exit once no client connection has been open for this long
    /// (`--exit-after-idle`, ADR-0063). `None` keeps the tmux contract: live
    /// until the last pane is reaped.
    pub exit_after_idle: Option<Duration>,
    /// The `PHUX_*` process configuration (listener addresses, TLS pair,
    /// credential store, workload mode). Only the `phux server` entry point
    /// fills it from the environment ([`ServerEnv::from_process`]); the
    /// default sets nothing, so an in-process server never inherits the
    /// launching shell's (a production pane exports several).
    pub env: ServerEnv,
}

/// The opt-in flags the running server applied.
///
/// Re-emitted on the resume argv so a graceful upgrade (ADR-0032) keeps the
/// same surface. Taken from the builder fields, not argv; environment
/// fallbacks (`PHUX_*_ADDR`) survive `execve` on their own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeFlags {
    /// `--listen` address.
    pub ws_addr: Option<SocketAddr>,
    /// `--quic` address.
    pub quic_addr: Option<SocketAddr>,
    /// `--webtransport` address; always `None` without the feature.
    pub wt_addr: Option<SocketAddr>,
    /// `--connect HOST:PORT`; `[[connector]]` entries are re-read from disk.
    pub connect: Option<String>,
    /// `--hub`; the satellite registry is re-read and re-validated.
    pub hub: bool,
    /// `--exit-after-idle`, rounded up to whole seconds (never 0), so a
    /// resumed server is never more eager to exit than its predecessor.
    pub exit_after_idle: Option<Duration>,
    /// Installed executable the next upgrade pins
    /// (`PHUX_UPGRADE_SOURCE_EXE`). `Some` only on `--resume`: the variable
    /// leaks into panes and must not steer an unrelated cold start.
    pub upgrade_source_exe: Option<PathBuf>,
    /// `--autosave PATH` (ADR-0150); a resumed server keeps saving, but
    /// does not restore.
    pub autosave: Option<PathBuf>,
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
            scrollback: phux_config::DefaultsCfg::default().scrollback_limits(),
            agent_log_bytes: phux_config::DEFAULT_AGENT_LOG_BYTES,
            event_journal_entries: phux_config::DEFAULT_EVENT_JOURNAL_ENTRIES,
            event_journal_bytes: phux_config::DEFAULT_EVENT_JOURNAL_BYTES,
            retain: crate::state::RetainPolicy::default(),
            cwd_inheritance: phux_config::CwdInheritance::default(),
            term: phux_config::DefaultsCfg::default().term,
            shell: crate::terminal_actor::resolve_shell(None),
            login_shell: false,
            window_size: phux_config::WindowSize::default(),
            voice: phux_config::VoiceCfg::default(),
            metadata_value_bytes: phux_config::DEFAULT_METADATA_VALUE_BYTES,
            approval_ttl_secs: phux_config::DEFAULT_APPROVAL_TTL_SECS,
            approval_max_pending: phux_config::DEFAULT_APPROVAL_MAX_PENDING,
            approval_max_pending_total: phux_config::DEFAULT_APPROVAL_MAX_PENDING_TOTAL,
            policy_engine: None,
            policy_mode: None,
            hook_catalog: crate::hooks::HookCatalog::default(),
            exit_after_idle: None,
            env: ServerEnv::default(),
        }
    }
}

/// Floor on the idle watchdog's re-check interval, so an already-expired
/// budget never spins the runtime with zero-length sleeps.
const IDLE_WATCH_MIN_INTERVAL: Duration = Duration::from_millis(50);

/// Cancel the root token once no client connection has been open for
/// `idle_limit` (ADR-0063). "Unattended" means zero open connections, not
/// zero attached clients, so a harness driving only one-shot verbs is not
/// reaped mid-script. Cancelling the root token takes the same graceful
/// teardown path as Ctrl-C.
fn spawn_idle_exit_watchdog(
    state: SharedState,
    idle_limit: Duration,
    root_token: CancellationToken,
) {
    tokio::task::spawn_local(async move {
        loop {
            // While a client is connected `idle_since` is `None`; re-check
            // after a full interval.
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
            // Re-read: a client may have come and gone while we slept.
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

pub use phux_config::socket::default_socket_path;

/// Maximum byte length of a Unix-domain-socket path: `sun_path` (108 bytes on
/// Linux, 104 elsewhere) minus the trailing NUL.
pub const MAX_SOCKET_PATH_LEN: usize = if cfg!(target_os = "linux") { 107 } else { 103 };

/// Check that `path` fits in a `sockaddr_un`, so bind and connect fail with a
/// readable [`ServerError::SocketPathTooLong`] instead of the kernel's
/// opaque `SUN_LEN` error.
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

    /// Hub mode was requested but the satellite registry did not validate.
    #[error("hub: {0}")]
    Hub(#[from] crate::hub::HubTableError),
    /// Outbound connector configuration was unsafe or malformed.
    #[error("connector: {0}")]
    Connector(#[from] crate::connector::ConnectorError),

    /// Workload mode (`PHUX_WORKLOAD_MTLS`) beside a remote entry point that
    /// cannot require a workload client certificate (ADR-0116).
    #[error(
        "PHUX_WORKLOAD_MTLS is set, but {surface} cannot require a workload client certificate, so the server refuses to start; {remedy}"
    )]
    WorkloadModeUncovered {
        /// The entry point that cannot be covered.
        surface: &'static str,
        /// How to start: remove that entry point, or leave workload mode.
        remedy: &'static str,
    },

    /// A `[policy]` mode the rest of the configuration contradicts
    /// (`docs/spec/workload-auth.md` §8).
    #[error("policy: {0}")]
    Policy(#[from] crate::policy::PostureError),

    /// The `paired` posture with missing, malformed, or unsafe workload
    /// authority material (`docs/spec/workload-auth.md` §8).
    #[error("policy: paired mode needs usable workload authority material: {0}")]
    PolicyMaterial(#[source] crate::workload::WorkloadError),

    /// The upgrade blob decoded, but rebuilding the session tree failed;
    /// resume fails closed rather than serve a partial tree.
    #[error("resume rebuild: {0}")]
    Rebuild(#[from] crate::state::RebuildError),

    /// The token store that authorizes bridged connector consumers could not
    /// be loaded; connectors fail closed.
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
    /// WebSocket listen address; `None` falls back to [`ServerEnv::ws_addr`].
    ws_addr: Option<SocketAddr>,
    /// QUIC listen address; `None` falls back to [`ServerEnv::quic_addr`].
    quic_addr: Option<SocketAddr>,
    /// WebTransport listen address; `None` falls back to
    /// [`ServerEnv::wt_addr`].
    #[cfg(feature = "webtransport")]
    wt_addr: Option<SocketAddr>,
    /// Inherited handoff-blob descriptor for `--resume` (ADR-0032).
    resume_fd: Option<RawFd>,
    /// Upgrade handoff taken from the environment by [`Self::resume`].
    inherited_upgrade: upgrade::InheritedUpgradeEnv,
    /// Federation-hub mode; only a hub reads [`Self::satellites`].
    hub: bool,
    /// `[[satellites]]` registry, validated at startup in hub mode.
    satellites: Vec<phux_config::SatelliteConfigEntry>,
    /// Where a hub re-reads `[[satellites]]` on a config-reload doorbell.
    satellite_source: Option<crate::hub::SatelliteSource>,
    /// Outbound relay connector entries.
    connectors: Vec<phux_config::ConnectorConfigEntry>,
    /// Raw `--connect` value, kept for the upgrade argv.
    connect_override: Option<String>,
    /// Overlay-address source for the auto-bound listener (ADR-0081). Never
    /// called on the startup path; see [`serve_auto_overlay_listeners`].
    overlay_detect: fn() -> Vec<std::net::IpAddr>,
    /// `--autosave`: the crash-safe workspace archive (ADR-0150).
    autosave: Option<crate::autosave::Autosave>,
    /// Told once the startup listeners are bound; see [`Self::on_listening`].
    on_listening: ListeningHook,
}

/// The callback behind [`ServerRuntime::on_listening`].
type ListeningCallback = Box<dyn FnOnce(&RemoteListenersReport) + Send>;

/// An optional [`ListeningCallback`]; a newtype so [`ServerRuntime`] keeps
/// its `Debug`.
struct ListeningHook(Option<ListeningCallback>);

impl std::fmt::Debug for ListeningHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "ListeningHook(Some)"
        } else {
            "ListeningHook(None)"
        })
    }
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
            inherited_upgrade: upgrade::InheritedUpgradeEnv::none(),
            hub: false,
            satellites: Vec::new(),
            satellite_source: None,
            connectors: Vec::new(),
            connect_override: None,
            overlay_detect: phux_config::overlay::detect,
            autosave: None,
            on_listening: ListeningHook(None),
        }
    }

    /// Call `hook` once the UDS socket and every configured remote listener
    /// have bound (each remote one may have failed; the report says which),
    /// before the session tree is seeded. A startup that fails, a socket bind
    /// above all, never calls it, so a "listening" line printed from here is
    /// never a lie. The auto-bound overlay listener (ADR-0081) binds later
    /// and is not in the report.
    #[must_use]
    pub fn on_listening(
        mut self,
        hook: impl FnOnce(&RemoteListenersReport) + Send + 'static,
    ) -> Self {
        self.on_listening = ListeningHook(Some(Box::new(hook)));
        self
    }

    /// Keep a crash-safe workspace archive (ADR-0150): restore it once on a
    /// cold start, then rewrite it atomically as the workspace changes.
    #[must_use]
    pub fn autosave(mut self, autosave: crate::autosave::Autosave) -> Self {
        self.autosave = Some(autosave);
        self
    }

    /// Override the overlay-address source for the auto-bound listener
    /// (ADR-0081), so tests control detection timing
    /// (`tests/lifecycle/overlay_startup.rs`).
    #[must_use]
    pub const fn overlay_detect(mut self, detect: fn() -> Vec<std::net::IpAddr>) -> Self {
        self.overlay_detect = detect;
        self
    }

    /// Resume from a graceful upgrade (ADR-0032): read the handoff state blob
    /// from inherited descriptor `fd`, adopt the inherited listener, and
    /// rebuild the session tree rather than starting fresh.
    ///
    /// Also consumes the `PHUX_UPGRADE_*` handoff variables and removes them
    /// from the process environment, so call it before starting threads.
    #[must_use]
    pub fn resume(mut self, fd: RawFd) -> Self {
        self.resume_fd = Some(fd);
        self.inherited_upgrade = upgrade::InheritedUpgradeEnv::take_from_env();
        self
    }

    /// A cold start: honor no inherited upgrade handoff, and remove any
    /// `PHUX_UPGRADE_*` variables from the process environment so hooks and
    /// every other child of this server cannot inherit them either. The
    /// counterpart of [`Self::resume`]; call it at the same point, before
    /// starting threads.
    #[must_use]
    pub fn discard_inherited_upgrade(self) -> Self {
        let _ignored = upgrade::InheritedUpgradeEnv::take_from_env();
        self
    }

    /// Also accept WebSocket connections on `addr`, overriding
    /// `PHUX_WS_ADDR`. Loopback is plaintext; a routable address gets TLS and
    /// bearer auth (ADR-0031).
    #[must_use]
    pub const fn listen_ws(mut self, addr: SocketAddr) -> Self {
        self.ws_addr = Some(addr);
        self
    }

    /// Also accept QUIC connections on `addr`, overriding `PHUX_QUIC_ADDR`.
    /// A routable address requires a paired bearer token (ADR-0031).
    #[must_use]
    pub const fn listen_quic(mut self, addr: SocketAddr) -> Self {
        self.quic_addr = Some(addr);
        self
    }

    /// Also accept WebTransport connections on `addr`, overriding
    /// `PHUX_WT_ADDR`. A routable address requires a paired bearer token in
    /// the `CONNECT` request (ADR-0031).
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

    /// Run as a federation hub (ADR-0007), validating `satellites` into the
    /// [`crate::hub::HubTable`] at startup; a bad entry fails with
    /// [`ServerError::Hub`].
    #[must_use]
    pub fn hub(mut self, satellites: Vec<phux_config::SatelliteConfigEntry>) -> Self {
        self.hub = true;
        self.satellites = satellites;
        self
    }

    /// Let a hub pick up registry edits live: on each
    /// `phux.config.reload/v1` doorbell it re-reads `[[satellites]]` from
    /// `source` and dials added entries, stops removed ones, and redials
    /// changed ones, leaving every other link and pane alone. Ignored off-hub.
    #[must_use]
    pub fn hub_reload(mut self, source: crate::hub::SatelliteSource) -> Self {
        self.satellite_source = Some(source);
        self
    }

    /// Supervise outbound relay connector entries. `connect_override` is the
    /// raw `--connect` value, kept for the upgrade argv.
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

    /// Build a current-thread runtime and block on [`Self::run_async`].
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

    /// Run the server until `shutdown` resolves, on a [`LocalSet`] driven by
    /// the current runtime (ADR-0014: pane actors are `!Send`).
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: server runs on a LocalSet; per-pane actors are !Send"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "startup binds every transport once; splitting hides the accept-set assembly"
    )]
    pub async fn run_async<F>(self, shutdown: F) -> Result<(), ServerError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let socket_path = self.cfg.socket_path.clone();

        let state = SharedState::new();

        let hub_table = install_hub_table(&state, self.hub, &self.satellites)?;

        // Connector plans and the policy posture are startup gates: bad
        // configuration fails before anything is bound.
        let connector_specs = crate::connector::plan_connectors(&self.connectors)?;
        // Each opt-in address: the flag wins over the environment.
        let env = &self.cfg.env;
        let ws_addr = self.ws_addr.or(env.ws_addr);
        let quic_addr = self.quic_addr.or(env.quic_addr);
        #[cfg(feature = "webtransport")]
        let webtransport_addr = self.wt_addr.or(env.wt_addr);
        #[cfg(feature = "webtransport")]
        let webtransport = webtransport_addr.is_some();
        #[cfg(not(feature = "webtransport"))]
        let webtransport = false;
        let (posture, posture_engine) = startup_policy(
            &self.cfg,
            ws_addr,
            quic_addr,
            webtransport,
            !self.connectors.is_empty(),
        )?;
        let connector_consumer_tokens = load_connector_consumer_tokens(&connector_specs, env)?;

        let resume_blob = read_resume_blob(self.resume_fd, &self.inherited_upgrade)?;
        let listener = adopt_or_bind_listener(
            &socket_path,
            resume_blob.as_ref().map(|blob| blob.listener_fd),
        )
        .await?;

        // Remember which inode we bound so shutdown only unlinks our socket.
        let bound_socket = socket_identity(&socket_path);

        let runtime_flags = self.runtime_flags();
        let autosave = self.autosave;
        let on_listening = self.on_listening;
        state.with_mut(|s| {
            s.set_upgrade_context(listener.as_raw_fd(), socket_path.clone(), runtime_flags);
        });

        mirror_config_into_state(&self.cfg, &socket_path, &state);
        install_policy_posture(&state, posture, posture_engine);

        let pre_seeded = self.cfg.pre_seeded_session.clone();
        let seed_with_pty = self.cfg.seed_with_pty;
        let seed_command = self.cfg.seed_command.clone();
        let scrollback = self.cfg.scrollback;
        let overlay_detect = self.overlay_detect;
        let hook_catalog = self.cfg.hook_catalog.clone();
        let hook_socket_path = socket_path.clone();
        let autosave_socket_path = socket_path.clone();
        let exit_after_idle = self.cfg.exit_after_idle;
        let satellite_source = self.satellite_source.clone();
        // Input routing runs on its own OS thread (ADR-0044) so keystrokes
        // are not queued behind output broadcast; its `Drop` joins it.
        let input_lane = input_lane::spawn_input_lane(state.clone())?;
        let input_lane_handle = input_lane.handle();
        let local = LocalSet::new();
        // Parent of every per-client and per-pane token; `shutdown` is
        // folded into it.
        let root_token = CancellationToken::new();
        let result = local
            .run_until(async move {
                spawn_shutdown_folder(shutdown, &root_token);
                arm_idle_exit(&state, exit_after_idle, &root_token);
                install_hook_dispatcher(&state, hook_catalog, hook_socket_path);
                spawn_hub_links(&state, hub_table.as_ref(), satellite_source, &root_token);
                // Live revocation (workload-auth §7).
                revocation::spawn_revocation_watcher(&state, &root_token);
                // Off the runtime thread: a large upload directory is a
                // directory walk.
                let upload_env = state.with(crate::state::ServerState::server_env);
                drop(tokio::task::spawn_blocking(move || {
                    upload::sweep_stale_partials_at_startup(&upload_env);
                }));
                spawn_connector_supervisors(
                    connector_specs,
                    connector_consumer_tokens.as_ref(),
                    &state,
                    &input_lane_handle,
                    &root_token,
                );

                // Configured listeners bind before the session tree exists, and
                // nothing slow may run between "a pane exists" and "the accept
                // loop runs": a live pane must never race an unreachable server.
                let configured = ConfiguredListeners::bind(ws_addr, quic_addr, &state).await;
                #[cfg(feature = "webtransport")]
                let webtransport_listener = webtransport_addr.and_then(|addr| {
                    let env = state.with(crate::state::ServerState::server_env);
                    let (listener, slot) = build_wt_listener(addr, &env);
                    state.with_mut(|s| s.record_remote_listener(slot));
                    listener
                });
                if let ListeningHook(Some(hook)) = on_listening {
                    hook(&state.with(|s| s.remote_listeners().clone()));
                }

                let cold_start = resume_blob.is_none();
                if let Some(blob) = resume_blob {
                    resume_session_tree(&state, &blob, &root_token)?;
                } else if let Some(name) = pre_seeded.as_deref() {
                    seed_initial_session(
                        &state,
                        name,
                        seed_with_pty,
                        seed_command,
                        scrollback,
                        &root_token,
                    );
                }
                // After the tree exists, so a restore lands beside the seed;
                // the saver dials the socket bound above.
                if let Some(autosave) = autosave {
                    crate::autosave::spawn(
                        autosave,
                        &state,
                        &root_token,
                        autosave_socket_path,
                        cold_start,
                    );
                }
                // Overlay detection shells out, so it runs inside the accept
                // set (see `serve_auto_overlay_listeners`); only the cheap gate
                // is evaluated here.
                let auto_overlay_ports = configured.unclaimed_overlay_ports();
                let auto_overlay_gate = auto_overlay_allowed(auto_overlay_ports, &state);

                let mut accepts =
                    configured.accept_loops(&listener, &state, &root_token, &input_lane_handle);
                #[cfg(feature = "webtransport")]
                if let Some(wt) = &webtransport_listener {
                    accepts.push(Box::pin(accept_loop(
                        wt,
                        state.clone(),
                        root_token.clone(),
                        Some(input_lane_handle.clone()),
                    )));
                }
                accepts.push(Box::pin(serve_auto_overlay_listeners(
                    auto_overlay_gate,
                    auto_overlay_ports,
                    overlay_detect,
                    state.clone(),
                    root_token.clone(),
                    input_lane_handle.clone(),
                )));
                drive_accept_loops(accepts, &root_token).await
            })
            .await;

        // Drop the LocalSet first: its tasks may hold `InputLaneHandle`
        // clones, and `InputLane::drop` joins a thread that only exits once
        // every handle is gone. The reverse order deadlocks.
        drop(local);
        drop(input_lane);

        // Unlinking a socket another server now owns would strand that
        // server unreachable by path.
        unlink_socket_if_ours(&socket_path, bound_socket);

        result
    }

    fn runtime_flags(&self) -> RuntimeFlags {
        RuntimeFlags {
            ws_addr: self.ws_addr,
            quic_addr: self.quic_addr,
            #[cfg(feature = "webtransport")]
            wt_addr: self.wt_addr,
            #[cfg(not(feature = "webtransport"))]
            wt_addr: None,
            hub: self.hub,
            connect: self.connect_override.clone(),
            exit_after_idle: self.cfg.exit_after_idle,
            upgrade_source_exe: self.inherited_upgrade.source_exe.clone(),
            autosave: self
                .autosave
                .as_ref()
                .map(|autosave| autosave.path().to_path_buf()),
        }
    }
}

/// Validate the satellite registry before any I/O (hub mode only) and mirror
/// the table into state; the link supervisors are spawned from the result.
fn install_hub_table(
    state: &SharedState,
    hub: bool,
    satellites: &[phux_config::SatelliteConfigEntry],
) -> Result<Option<crate::hub::HubTable>, ServerError> {
    let table = crate::hub::resolve_hub_table(hub, satellites)?;
    if let Some(table) = &table {
        info!(
            satellites = table.len(),
            "hub mode: satellite registry validated"
        );
        for (host, entry) in table.iter() {
            info!(satellite = %host, target = %entry.target, "hub satellite registered");
        }
        state.with_mut(|s| s.set_hub_table(table.clone()));
    }
    Ok(table)
}

/// Load the consumer-token store the outbound connector supervisors
/// authenticate with. Token contents stay on disk and are re-read by each
/// supervisor attempt; only the store's presence is a startup gate.
fn load_connector_consumer_tokens(
    specs: &[crate::connector::ConnectorSpec],
    env: &ServerEnv,
) -> Result<Option<std::sync::Arc<crate::auth::ReloadingTokenStore>>, ServerError> {
    if specs.is_empty() {
        return Ok(None);
    }
    let path = env.tokens_path();
    if let Err(refusal) = phux_config::production::refuse_dev_on_production_state(&path) {
        return Err(ServerError::ConnectorTokenStore {
            path,
            source: crate::auth::AuthError::ProductionStore(refusal),
        });
    }
    let store = crate::auth::ReloadingTokenStore::load(path.clone()).map_err(|source| {
        ServerError::ConnectorTokenStore {
            path: path.clone(),
            source,
        }
    })?;
    Ok(Some(std::sync::Arc::new(store)))
}

/// Read the upgrade handoff blob (ADR-0032) and drop the previous image's
/// executable snapshot once the blob proves this is a resume.
fn read_resume_blob(
    resume_fd: Option<RawFd>,
    inherited: &upgrade::InheritedUpgradeEnv,
) -> Result<Option<StateBlob>, ServerError> {
    let Some(fd) = resume_fd else {
        return Ok(None);
    };
    let blob = resume::read_blob_from_fd(fd)?;
    upgrade::cleanup_executable_snapshot(inherited.snapshot_dir.as_deref());
    Ok(Some(blob))
}

/// Adopt the listener inherited across a graceful upgrade (ADR-0032), or bind
/// a fresh socket when this is a cold start.
async fn adopt_or_bind_listener(
    socket_path: &Path,
    inherited_fd: Option<RawFd>,
) -> Result<crate::transport::UdsListener, ServerError> {
    if let Some(fd) = inherited_fd {
        let listener = resume::adopt_uds_listener(fd)?;
        info!(
            path = %socket_path.display(),
            "phux-server resumed; adopted the inherited UDS listener"
        );
        return Ok(listener);
    }
    validate_socket_path_len(socket_path)?;
    // Before anything that could unlink a stale production socket.
    phux_config::socket::refuse_dev_on_production(socket_path).map_err(|refusal| {
        ServerError::Bind(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            refusal,
        ))
    })?;
    prepare_socket_dir(socket_path)?;
    handle_existing_socket(socket_path).await?;
    let listener = UnixListener::bind(socket_path).map_err(ServerError::Bind)?;
    secure_socket_file(socket_path)?;
    let listener = crate::transport::UdsListener::new(listener);
    info!(path = %socket_path.display(), "phux-server listening on UDS");
    Ok(listener)
}

/// Mirror the configured defaults into shared state, so every pane spawn
/// site resolves them from one place.
fn mirror_config_into_state(cfg: &ServerConfig, socket_path: &Path, state: &SharedState) {
    // A zero TTL would expire every hold before anyone could see it.
    let approval_ttl = Duration::from_secs(u64::from(cfg.approval_ttl_secs.max(1)));
    state.with_mut(|s| {
        s.set_pre_seeded_session(cfg.pre_seeded_session.clone());
        // Only the PTY mode is mirrored: a `CreateIfMissing` session runs its
        // own wire command, never the pre-seeded session's `seed_command`.
        s.set_attach_create_pty(cfg.seed_with_pty, None);
        // Injected as `PHUX_SOCKET` into every pane.
        s.set_server_socket_path(socket_path.to_path_buf());
        s.set_scrollback_limits(cfg.scrollback);
        s.set_agent_log_bytes(cfg.agent_log_bytes);
        s.set_event_journal_bounds(
            usize::try_from(cfg.event_journal_entries).unwrap_or(usize::MAX),
            usize::try_from(cfg.event_journal_bytes).unwrap_or(usize::MAX),
        );
        s.set_retain_policy(cfg.retain);
        s.set_cwd_inheritance(cfg.cwd_inheritance);
        s.set_term(cfg.term.clone());
        s.set_shell(cfg.shell.clone());
        s.set_login_shell(cfg.login_shell);
        s.set_window_size(cfg.window_size);
        s.set_voice(cfg.voice.clone());
        s.set_metadata_value_bytes(cfg.metadata_value_bytes);
        s.set_approval_limits(
            approval_ttl,
            cfg.approval_max_pending,
            cfg.approval_max_pending_total,
        );
        if let Some(engine) = cfg.policy_engine.clone() {
            s.set_policy_engine(engine);
        }
        s.set_server_env(cfg.env.clone());
    });
}

/// The engine a posture calls for; `None` keeps the state's current engine.
type PostureEngine = Option<std::sync::Arc<dyn crate::policy::PolicyEngine>>;

/// Resolve the startup posture from `[policy] mode`, `PHUX_WORKLOAD_MTLS`,
/// and the configured entry points, refuse the contradictions
/// (`docs/spec/workload-auth.md` §8), and build the engine it calls for.
fn startup_policy(
    cfg: &ServerConfig,
    ws_addr: Option<SocketAddr>,
    quic_addr: Option<SocketAddr>,
    webtransport: bool,
    connectors: bool,
) -> Result<(crate::policy::PolicyPosture, PostureEngine), ServerError> {
    let remote = remote_listener_configured(ws_addr, quic_addr, webtransport, connectors);
    let posture =
        crate::policy::PolicyPosture::resolve(cfg.policy_mode, cfg.env.workload_mtls, remote)?;
    workload_auth::refuse_uncovered_surfaces(
        posture.requires_workload_mtls(),
        connectors,
        webtransport,
    )?;
    let engine = posture_policy_engine(cfg, posture)?;
    Ok((posture, engine))
}

/// The engine the posture calls for, or `None` when the state's default
/// ([`crate::policy::PermissivePolicy`]) already is it or the embedder
/// injected one through [`ServerConfig::policy_engine`].
///
/// `paired` loads the workload authority here, before anything binds, so
/// missing, malformed, or unsafe material refuses to start
/// (`docs/spec/workload-auth.md` §8).
fn posture_policy_engine(
    cfg: &ServerConfig,
    posture: crate::policy::PolicyPosture,
) -> Result<Option<std::sync::Arc<dyn crate::policy::PolicyEngine>>, ServerError> {
    use crate::policy::{PolicyPosture, ScopedPolicy};

    if cfg.policy_engine.is_some() {
        return Ok(None);
    }
    let engine: std::sync::Arc<dyn crate::policy::PolicyEngine> = match posture {
        PolicyPosture::Transitional { .. } => return Ok(None),
        PolicyPosture::Local => std::sync::Arc::new(ScopedPolicy::local()),
        PolicyPosture::Paired => {
            let authority = workload_auth::WorkloadAuth::configured(&cfg.env)
                .map_err(ServerError::PolicyMaterial)?;
            std::sync::Arc::new(ScopedPolicy::paired(authority.registry))
        }
    };
    Ok(Some(engine))
}

/// Whether a remote entry point is configured: a WebSocket, QUIC, or
/// WebTransport listener (flag or environment) or a relay connector. The
/// `local` posture refuses to start beside one, and the transitional posture
/// warns about it (`docs/spec/workload-auth.md` §8).
const fn remote_listener_configured(
    ws_addr: Option<SocketAddr>,
    quic_addr: Option<SocketAddr>,
    webtransport: bool,
    connectors: bool,
) -> bool {
    ws_addr.is_some() || quic_addr.is_some() || webtransport || connectors
}

/// Mirror the posture into shared state: whether TLS listeners require a
/// workload certificate, and the engine that mints each connection's grant.
fn install_policy_posture(
    state: &SharedState,
    posture: crate::policy::PolicyPosture,
    engine: Option<std::sync::Arc<dyn crate::policy::PolicyEngine>>,
) {
    state.with_mut(|s| {
        s.set_policy_posture(posture);
        if let Some(engine) = engine {
            s.set_policy_engine(engine);
        }
    });
    if posture.warns_remote_owner_grant() {
        warn!(
            target: crate::policy::POLICY_TARGET,
            "no [policy] mode is configured and a remote listener or relay connector is: \
             remote consumers hold the owner's full grant (transitional posture, \
             docs/spec/workload-auth.md section 8); set [policy] mode = \"paired\" to \
             require enrolled workload certificates and enforce their scope ceilings"
        );
    }
}

/// Whether the auto-bound overlay listener may run: some overlay port is
/// unclaimed, its gate is open, and the posture admits a remote door, which
/// `[policy] mode = "local"` never does (`docs/spec/workload-auth.md` §8).
fn auto_overlay_allowed(ports: AutoOverlayPorts, state: &SharedState) -> bool {
    let local_only = state
        .with(crate::state::ServerState::policy_posture)
        .is_local();
    ports.any()
        && !local_only
        && auto_overlay_gate_open(
            state.with(|s| s.server_env().no_auto_listen),
            phux_config::instance::is_default_profile(),
        )
}

/// Fold the external shutdown future into the root token. `spawn_local` (not
/// `tokio::spawn`) because the runtime is current-thread with no worker pool.
fn spawn_shutdown_folder<F>(shutdown: F, root_token: &CancellationToken)
where
    F: Future<Output = ()> + 'static,
{
    let token = root_token.clone();
    tokio::task::spawn_local(async move {
        shutdown.await;
        debug!("shutdown future resolved; cancelling root token");
        token.cancel();
    });
}

/// Arm the idle-exit watchdog (ADR-0063) before the seed/resume paths, so a
/// server nobody ever dials still exits.
fn arm_idle_exit(
    state: &SharedState,
    exit_after_idle: Option<Duration>,
    root_token: &CancellationToken,
) {
    let Some(idle_limit) = exit_after_idle else {
        return;
    };
    info!(
        idle_limit_secs = idle_limit.as_secs_f64(),
        "ephemeral server: will exit when unattended for the idle limit"
    );
    spawn_idle_exit_watchdog(state.clone(), idle_limit, root_token.clone());
}

/// Spawn the hook dispatcher before the seed/resume paths, so a pre-seeded
/// pane fires `after-new-pane` too. No hooks, no dispatcher.
fn install_hook_dispatcher(
    state: &SharedState,
    catalog: crate::hooks::HookCatalog,
    socket_path: PathBuf,
) {
    if catalog.is_empty() {
        return;
    }
    let dispatcher = crate::hooks::spawn_hook_dispatcher(
        catalog,
        Some(socket_path),
        Some(phux_plugin::run_log::default_path()),
    );
    state.with_mut(|s| s.set_hook_dispatcher(dispatcher));
}

/// Spawn one link supervisor per validated satellite (ADR-0038) and mirror
/// the relay registry and supervisors into shared state, where a registry
/// reload finds them.
fn spawn_hub_links(
    state: &SharedState,
    hub_table: Option<&crate::hub::HubTable>,
    source: Option<crate::hub::SatelliteSource>,
    root_token: &CancellationToken,
) {
    let Some(table) = hub_table else {
        return;
    };
    let relays = crate::hub::relay::HubRelays::default();
    let ssh_program = state.with(|s| s.server_env().ssh_program());
    let mut links = crate::hub::link::HubLinks::new(relays.clone(), root_token, ssh_program);
    for (host, entry) in table.iter() {
        links.start(host, entry, state);
    }
    state.with_mut(|s| {
        s.set_hub_relays(relays);
        s.set_hub_links(links, source);
    });
}

/// Supervise the planned outbound connectors. Nothing to supervise without a
/// consumer-token store, which only exists when connectors were configured.
fn spawn_connector_supervisors(
    specs: Vec<crate::connector::ConnectorSpec>,
    consumer_tokens: Option<&std::sync::Arc<crate::auth::ReloadingTokenStore>>,
    state: &SharedState,
    input_lane: &input_lane::InputLaneHandle,
    root_token: &CancellationToken,
) {
    let Some(tokens) = consumer_tokens else {
        return;
    };
    crate::connector::spawn_connectors(specs, tokens, state, input_lane, root_token);
}

/// Rebuild the session tree from the upgrade blob (ADR-0032), all or
/// nothing, before any accept loop starts.
fn resume_session_tree(
    state: &SharedState,
    blob: &StateBlob,
    root_token: &CancellationToken,
) -> Result<(), ServerError> {
    // Each pane is wired exactly as a fresh spawn is (event sink, agent
    // detector), so a resumed pane keeps its events and agent-state feed.
    let rebuilt = state.with_mut(|s| {
        s.rebuild_from_blob(blob, |pane, actor| {
            commands::wire_pane_actor(state, pane, actor)
        })
    })?;
    let mut pane_events: std::collections::HashMap<_, _> = rebuilt
        .wired_panes
        .into_iter()
        .map(|(pane, wiring)| {
            let wire = state.with_mut(|s| s.intern_terminal_wire(pane));
            (pane, wiring.start(state, &wire))
        })
        .collect();
    for (resource, exit_notify) in rebuilt.exit_watchers {
        spawn_terminal_exit_watcher(
            state.clone(),
            resource,
            Some(exit_notify),
            root_token.clone(),
            pane_events.remove(&resource),
        );
    }
    // A pane with no exit to watch (no PTY) still drains its events.
    for (pane, events) in pane_events {
        spawn_terminal_exit_watcher(state.clone(), pane, None, root_token.clone(), Some(events));
    }
    // ADR-0124 §6: retained exited panes close with `SERVER_SHUTDOWN`.
    state.with_mut(|s| s.close_upgrade_retained(blob));
    info!(
        sessions = blob.sessions.len(),
        panes = blob.panes.len(),
        agent_sessions = blob.agent_sessions.len(),
        "resumed session tree from upgrade blob"
    );
    Ok(())
}

/// A fresh start pre-seeds its single session instead of resuming one.
fn seed_initial_session(
    state: &SharedState,
    name: &str,
    seed_with_pty: bool,
    seed_command: Option<portable_pty::CommandBuilder>,
    scrollback: phux_config::ScrollbackLimits,
    root_token: &CancellationToken,
) {
    let seeded = if seed_with_pty {
        seed_session_with_pty(
            state,
            name,
            seed_pane_command(state, seed_command),
            scrollback,
            root_token,
        )
    } else {
        seed_session_with_actor(state, name, scrollback, root_token)
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

/// The pre-seeded pane's command (configured, else the default shell), with
/// the server-wide `TERM` applied.
fn seed_pane_command(
    state: &SharedState,
    configured: Option<portable_pty::CommandBuilder>,
) -> portable_pty::CommandBuilder {
    let mut cmd = configured.unwrap_or_else(|| {
        let (shell, login_shell) = state.with(|s| (s.shell().to_owned(), s.login_shell()));
        crate::terminal_actor::default_shell_command(&shell, login_shell)
    });
    let term = state.with(|s| s.term().to_owned());
    crate::terminal_actor::apply_term(&mut cmd, &term);
    cmd
}

/// The explicitly configured additive listeners, alongside the addresses they
/// were asked for: the auto-bound overlay listener may claim only the ports
/// no explicit address took.
struct ConfiguredListeners {
    ws_addr: Option<SocketAddr>,
    quic_addr: Option<SocketAddr>,
    remote: RemoteListeners,
}

impl ConfiguredListeners {
    /// Bind the resolved opt-in addresses that were asked for.
    async fn bind(
        ws_addr: Option<SocketAddr>,
        quic_addr: Option<SocketAddr>,
        state: &SharedState,
    ) -> Self {
        Self {
            ws_addr,
            quic_addr,
            remote: RemoteListeners::bind(ws_addr, quic_addr, state).await,
        }
    }

    /// The overlay ports no explicitly configured address claimed.
    const fn unclaimed_overlay_ports(&self) -> AutoOverlayPorts {
        AutoOverlayPorts {
            ws: self.ws_addr.is_none(),
            quic: self.quic_addr.is_none(),
        }
    }

    /// The always-on UDS loop plus the additive remote ones.
    fn accept_loops<'a>(
        &'a self,
        uds: &'a crate::transport::UdsListener,
        state: &SharedState,
        root_token: &CancellationToken,
        input_lane: &input_lane::InputLaneHandle,
    ) -> Vec<AcceptLoopFuture<'a>> {
        let mut accepts: Vec<AcceptLoopFuture<'a>> = vec![Box::pin(accept_loop(
            uds,
            state.clone(),
            root_token.clone(),
            Some(input_lane.clone()),
        ))];
        accepts.extend(self.remote.accept_loops(state, root_token, input_lane));
        accepts
    }
}

/// Bound WebSocket and QUIC listeners; each bind outcome is recorded for
/// `GET_STATE`.
struct RemoteListeners {
    ws: Option<crate::transport::WsListener>,
    quic: Option<crate::transport::quic::QuicListener>,
}

impl RemoteListeners {
    async fn bind(
        ws_addr: Option<SocketAddr>,
        quic_addr: Option<SocketAddr>,
        state: &SharedState,
    ) -> Self {
        let workload_mtls = state.with(crate::state::ServerState::workload_mtls_required);
        let env = state.with(crate::state::ServerState::server_env);
        let ws = match ws_addr {
            Some(addr) => {
                let (listener, slot) = build_ws_listener(addr, workload_mtls, &env).await;
                state.with_mut(|s| s.record_remote_listener(slot));
                listener
            }
            None => None,
        };
        let quic = quic_addr.and_then(|addr| {
            let (listener, slot) = build_quic_listener_for(addr, workload_mtls, &env);
            state.with_mut(|s| s.record_remote_listener(slot));
            listener
        });
        Self { ws, quic }
    }

    fn accept_loops<'a>(
        &'a self,
        state: &SharedState,
        root_token: &CancellationToken,
        input_lane: &input_lane::InputLaneHandle,
    ) -> Vec<AcceptLoopFuture<'a>> {
        let mut accepts: Vec<AcceptLoopFuture<'a>> = Vec::new();
        if let Some(ws) = &self.ws {
            accepts.push(Box::pin(accept_loop(
                ws,
                state.clone(),
                root_token.clone(),
                Some(input_lane.clone()),
            )));
        }
        if let Some(quic) = &self.quic {
            accepts.push(Box::pin(accept_loop(
                quic,
                state.clone(),
                root_token.clone(),
                Some(input_lane.clone()),
            )));
        }
        accepts
    }
}

/// Run every accept loop concurrently. The first fatal result cancels the
/// rest, and every loop is still joined so its clients can flush
/// `SERVER_SHUTDOWN`.
async fn drive_accept_loops(
    accepts: Vec<AcceptLoopFuture<'_>>,
    root_token: &CancellationToken,
) -> Result<(), ServerError> {
    let (mut result, _index, remaining) = futures_util::future::select_all(accepts).await;
    if result.is_err() {
        root_token.cancel();
    }
    for tail in futures_util::future::join_all(remaining).await {
        if result.is_ok() && tail.is_err() {
            result = tail;
        }
    }
    result
}

/// Default WebSocket port for the auto-configured overlay listener.
pub const DEFAULT_WS_PORT: u16 = 8787;

/// Default QUIC port for the auto-configured overlay listener.
pub const DEFAULT_QUIC_PORT: u16 = 8788;

/// Environment escape hatch disabling the overlay auto-listen entirely.
const DISABLE_AUTO_LISTEN_ENV: &str = "PHUX_NO_AUTO_LISTEN";

/// Whether the auto-bound overlay listener (ADR-0081) may run. It is safe to
/// default on because it binds only a detected overlay address (never
/// `0.0.0.0`), is TLS-only, and its token store admits nobody until
/// `phux pair`. `PHUX_NO_AUTO_LISTEN` disables it, and only the default
/// profile binds: ports are host-global, so a dev profile would race the
/// installed server (ADR-0080).
const fn auto_overlay_gate_open(disabled: bool, default_profile: bool) -> bool {
    !disabled && default_profile
}

/// The overlay IP to auto-bind. `detect` shells out to `tailscale`, so it
/// must not run at all when the gate is closed.
fn resolve_auto_overlay_ip(
    gate_open: bool,
    detect: impl FnOnce() -> Vec<std::net::IpAddr>,
) -> Option<std::net::IpAddr> {
    if !gate_open {
        return None;
    }
    detect().into_iter().next()
}

/// Which auto-bound overlay ports no explicit address claimed.
#[derive(Debug, Clone, Copy)]
struct AutoOverlayPorts {
    /// The WebSocket port ([`DEFAULT_WS_PORT`]) is unclaimed.
    ws: bool,
    /// The QUIC port ([`DEFAULT_QUIC_PORT`]) is unclaimed.
    quic: bool,
}

impl AutoOverlayPorts {
    /// With neither port free, detection has no consumer and must not run.
    const fn any(self) -> bool {
        self.ws || self.quic
    }
}

/// Detect the overlay address and serve the auto-bound listeners (ADR-0081)
/// as a peer of the other accept loops. Detection shells out to `tailscale`
/// (a wedged daemon costs ~2s), so it runs on a blocking thread after the
/// accept loops are live: a slow overlay delays this listener, never the
/// server (`tests/lifecycle/overlay_startup.rs`).
///
/// With nothing to bind it parks on cancellation, so the enclosing
/// `select_all` still reads a first completion as "an accept loop ended".
async fn serve_auto_overlay_listeners(
    gate_open: bool,
    ports: AutoOverlayPorts,
    detect: fn() -> Vec<std::net::IpAddr>,
    state: SharedState,
    root_token: CancellationToken,
    input_lane: input_lane::InputLaneHandle,
) -> Result<(), ServerError> {
    let detected = tokio::select! {
        () = root_token.cancelled() => return Ok(()),
        joined = tokio::task::spawn_blocking(move || resolve_auto_overlay_ip(gate_open, detect)) => {
            joined.unwrap_or_else(|err| {
                warn!(error = %err, "overlay detection task failed; no auto-bound remote listener");
                None
            })
        }
    };

    let remote = RemoteListeners::bind(
        detected
            .filter(|_| ports.ws)
            .map(|ip| SocketAddr::new(ip, DEFAULT_WS_PORT)),
        detected
            .filter(|_| ports.quic)
            .map(|ip| SocketAddr::new(ip, DEFAULT_QUIC_PORT)),
        &state,
    )
    .await;
    let accepts = remote.accept_loops(&state, &root_token, &input_lane);
    if accepts.is_empty() {
        debug!("no auto-bound overlay listener; nothing detected or nothing to bind");
        root_token.cancelled().await;
        return Ok(());
    }
    if state
        .with(crate::state::ServerState::policy_posture)
        .is_transitional()
    {
        warn!(
            target: crate::policy::POLICY_TARGET,
            "an overlay listener bound with no [policy] mode configured: remote consumers \
             hold the owner's full grant (transitional posture, docs/spec/workload-auth.md \
             section 8); set [policy] mode = \"paired\" to enforce workload scopes"
        );
    }

    drive_accept_loops(accepts, &root_token).await
}

/// The `(device, inode)` of the entry at `path`, if it can be stat'd.
fn socket_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::symlink_metadata(path)
        .ok()
        .map(|meta| (meta.dev(), meta.ino()))
}

/// Unlink `path` only when it still resolves to the socket this server bound.
/// Unprovable ownership leaves the entry: a stale file is reaped by the next
/// stale probe, but deleting a live server's socket strands it.
fn unlink_socket_if_ours(path: &Path, bound: Option<(u64, u64)>) {
    let Some(bound) = bound else {
        return;
    };
    match socket_identity(path) {
        Some(current) if current == bound => {
            if let Err(err) = std::fs::remove_file(path)
                && err.kind() != io::ErrorKind::NotFound
            {
                warn!(path = %path.display(), error = %err, "failed to unlink socket");
            }
        }
        Some(_) => {
            warn!(
                path = %path.display(),
                "socket path now belongs to another server; leaving it in place",
            );
        }
        None => {}
    }
}

/// A disabled listener slot for `GET_STATE`, paired with no listener.
const fn disabled<L>(
    transport: RemoteListenerTransport,
    addr: String,
    reason: ListenerDisabledReason,
) -> (Option<L>, RemoteListenerSlot) {
    (
        None,
        RemoteListenerSlot::disabled(transport, Some(addr), reason),
    )
}

/// `PHUX_WS_SECURE=1` forces the secure path on a loopback address, for
/// testing the remote path locally.
const fn secure_bind(addr: SocketAddr, env: &ServerEnv) -> bool {
    !addr.ip().is_loopback() || env.ws_secure
}

/// Build the optional WebSocket listener (ADR-0031). The bind address is the
/// toggle: loopback is plaintext and unauthenticated; a routable address (or
/// workload mTLS, which must not have a plaintext side door) gets TLS with an
/// auto-provisioned certificate plus bearer-token auth. Setup failure
/// disables only this listener; the slot records the outcome for
/// `GET_STATE`.
async fn build_ws_listener(
    addr: SocketAddr,
    workload_mtls: bool,
    env: &ServerEnv,
) -> (Option<crate::transport::WsListener>, RemoteListenerSlot) {
    const WSS: RemoteListenerTransport = RemoteListenerTransport::Wss;
    let addr_s = addr.to_string();
    let workload = match workload_auth::WorkloadAuth::for_posture(workload_mtls, env) {
        Ok(workload) => workload,
        Err(err) => {
            error!(error = %err, "configured workload mTLS unavailable; WebSocket disabled");
            return disabled(WSS, addr_s, ListenerDisabledReason::TlsSetupFailed);
        }
    };

    if !secure_bind(addr, env) && workload.is_none() {
        let origins = crate::transport::AllowedOrigins::parse(env.ws_allowed_origins.as_deref());
        return match crate::transport::WsListener::bind(addr, origins).await {
            Ok(ws) => {
                let bound = ws.local_addr().map_or(addr_s, |a| a.to_string());
                info!(addr = %bound, "WebSocket listening (plaintext, loopback)");
                (Some(ws), RemoteListenerSlot::bound(WSS, bound))
            }
            Err(err) => {
                warn!(addr = %addr, error = %err, "failed to bind WebSocket; UDS only");
                disabled(WSS, addr_s, ListenerDisabledReason::BindFailed)
            }
        };
    }

    let Some((cert_path, key_path)) = remote_certificate(addr, "wss", env) else {
        return disabled(WSS, addr_s, ListenerDisabledReason::CertProvisionFailed);
    };
    let acceptor = match crate::transport::tls::acceptor_from_pem_with_client_ca(
        &cert_path,
        &key_path,
        workload.as_ref().map(|auth| &auth.ca),
    ) {
        Ok(acceptor) => acceptor,
        Err(err) => {
            error!(error = %err, "TLS setup failed; WebSocket disabled");
            return disabled(WSS, addr_s, ListenerDisabledReason::TlsSetupFailed);
        }
    };
    let store = remote_token_store(&env.tokens_path(), "wss");
    let token_count = store.len();
    let workload_mtls = workload.is_some();
    match crate::transport::WsListener::bind_secure(
        addr,
        acceptor,
        std::sync::Arc::new(store),
        workload.map(|auth| auth.registry),
    )
    .await
    {
        Ok(ws) => {
            let bound = ws.local_addr().map_or(addr_s, |a| a.to_string());
            info!(addr = %bound, tokens = token_count, workload_mtls, "WebSocket listening with TLS + token auth");
            (Some(ws), RemoteListenerSlot::bound(WSS, bound))
        }
        Err(err) => {
            warn!(addr = %addr, error = %err, "failed to bind secure WebSocket; UDS only");
            disabled(WSS, addr_s, ListenerDisabledReason::BindFailed)
        }
    }
}

/// The credential store a secure remote listener gates admission on. Loaded
/// leniently: a store that will not load at boot admits nobody but reloads on
/// the next authentication, so it never takes the remote surface down until
/// a restart.
fn remote_token_store(tokens_path: &Path, transport: &str) -> crate::auth::ReloadingTokenStore {
    let (store, error) = crate::auth::ReloadingTokenStore::load_deferred(tokens_path.to_path_buf());
    if let Some(error) = error {
        warn!(
            error = %error,
            path = %tokens_path.display(),
            "credential store does not load; {transport} listens anyway and refuses every \
             authentication until it does -- run `phux doctor` for the remedy"
        );
    } else if store.is_empty() {
        warn!(
            path = %tokens_path.display(),
            "no pairing tokens; run `phux pair` -- it takes effect immediately, with no restart"
        );
    }
    store
}

/// Log, do not fail, when the certificate does not name the bound address
/// (ADR-0091). phux consumers pin the fingerprint and ignore the name, and
/// widening the SANs would change the fingerprint and un-pair every device;
/// `phux doctor` carries the remedy.
fn warn_if_cert_omits_bind(cert_path: &Path, advertised: &[String], transport: &str) {
    let uncovered = match crate::transport::tls::uncovered_names(cert_path, advertised) {
        Ok(uncovered) => uncovered,
        Err(err) => {
            // The acceptor build reports an unreadable certificate itself.
            debug!(error = %err, "could not check certificate name coverage");
            return;
        }
    };
    if !uncovered.is_empty() {
        warn!(
            transport,
            addresses = %uncovered.join(", "),
            cert = %cert_path.display(),
            "certificate does not name this listener's address; fingerprint-pinning \
             consumers are unaffected, but a client that validates the server name \
             will refuse the handshake -- run `phux doctor` for the remedy"
        );
    }
}

/// Certificate and key a TLS listener bound to `addr` presents: the
/// operator's (`PHUX_WS_TLS_CERT` / `PHUX_WS_TLS_KEY`) when set, otherwise
/// the shared self-signed pair, provisioned on first use and never
/// regenerated (ADR-0091), so every listener presents the one fingerprint
/// `phux pair` prints. `None`, after logging, when it cannot be provisioned.
fn remote_certificate(
    addr: SocketAddr,
    transport: &str,
    env: &ServerEnv,
) -> Option<(PathBuf, PathBuf)> {
    let operator_cert = env.tls_cert.is_some() || env.tls_key.is_some();
    let cert_path = env
        .tls_cert
        .clone()
        .unwrap_or_else(crate::transport::tls::default_cert_path);
    let key_path = env
        .tls_key
        .clone()
        .unwrap_or_else(crate::transport::tls::default_key_path);
    if let Err(refusal) = refuse_dev_on_production_credentials(&cert_path, &key_path, env) {
        error!(transport, "{refusal}; listener disabled");
        return None;
    }
    let advertised = crate::transport::tls::advertised_for_bind(addr);
    if !operator_cert
        && let Err(err) =
            crate::transport::tls::ensure_self_signed_for(&cert_path, &key_path, &advertised)
    {
        error!(error = %err, transport, "failed to provision self-signed certificate; listener disabled");
        return None;
    }
    warn_if_cert_omits_bind(&cert_path, &advertised, transport);
    Some((cert_path, key_path))
}

/// A development build never presents the production certificate or admits
/// against the production credential store, which an inherited
/// `PHUX_WS_TLS_*` / `PHUX_WS_TOKENS` (a production pane exports them) would
/// otherwise hand it. Every remote listener resolves its certificate through
/// [`remote_certificate`], so this gates them all.
fn refuse_dev_on_production_credentials(
    cert_path: &Path,
    key_path: &Path,
    env: &ServerEnv,
) -> Result<(), String> {
    crate::transport::tls::refuse_dev_on_production_tls(cert_path, key_path)
        .map_err(|err| err.to_string())?;
    phux_config::production::refuse_dev_on_production_state(&env.tokens_path())
}

/// Bearer-token store for a QUIC-class listener: required on a secure bind,
/// absent on loopback.
fn remote_tokens(
    secure: bool,
    transport: &str,
    env: &ServerEnv,
) -> Option<std::sync::Arc<crate::auth::ReloadingTokenStore>> {
    secure.then(|| std::sync::Arc::new(remote_token_store(&env.tokens_path(), transport)))
}

/// Build the optional QUIC listener for `addr` (ADR-0007). QUIC is always
/// TLS 1.3 and shares the certificate and token store with `wss://`; a
/// routable address (or `PHUX_WS_SECURE=1`) also requires a bearer-token
/// preamble (ADR-0031). Setup failure disables only this listener.
fn build_quic_listener_for(
    addr: SocketAddr,
    workload_mtls: bool,
    env: &ServerEnv,
) -> (
    Option<crate::transport::quic::QuicListener>,
    RemoteListenerSlot,
) {
    const QUIC: RemoteListenerTransport = RemoteListenerTransport::Quic;
    let secure = secure_bind(addr, env);
    let addr_s = addr.to_string();
    let Some((cert_path, key_path)) = remote_certificate(addr, "quic", env) else {
        return disabled(QUIC, addr_s, ListenerDisabledReason::CertProvisionFailed);
    };
    let tokens = remote_tokens(secure, "quic", env);
    let token_count = tokens.as_ref().map_or(0, |s| s.len());
    let workload_auth = match workload_auth::WorkloadAuth::for_posture(workload_mtls, env) {
        Ok(auth) => auth,
        Err(err) => {
            error!(error = %err, "configured workload mTLS unavailable; QUIC disabled");
            return disabled(QUIC, addr_s, ListenerDisabledReason::TlsSetupFailed);
        }
    };
    let (workload_ca, workload_registry) =
        workload_auth.map_or((None, None), |auth| (Some(auth.ca), Some(auth.registry)));
    match crate::transport::quic::QuicListener::from_pem_with_client_ca_and_registry(
        addr,
        &cert_path,
        &key_path,
        tokens,
        workload_ca.as_ref(),
        workload_registry,
    ) {
        Ok(quic) => {
            let bound = quic.local_addr().map_or(addr_s, |a| a.to_string());
            info!(
                addr = %bound,
                tokens = token_count,
                secure,
                workload_mtls = workload_ca.is_some(),
                "QUIC listening"
            );
            (Some(quic), RemoteListenerSlot::bound(QUIC, bound))
        }
        Err(err) => {
            warn!(addr = %addr, error = %err, "failed to bind QUIC; UDS only");
            disabled(QUIC, addr_s, ListenerDisabledReason::BindFailed)
        }
    }
}

/// Build the optional WebTransport listener for `addr`: HTTP/3 over QUIC,
/// with the same certificate, token, and loopback rules as QUIC. Browsers,
/// which cannot set headers, carry the token as `?token=` inside TLS.
#[cfg(feature = "webtransport")]
fn build_wt_listener(
    addr: SocketAddr,
    env: &ServerEnv,
) -> (
    Option<crate::transport::webtransport::WtListener>,
    RemoteListenerSlot,
) {
    const WT: RemoteListenerTransport = RemoteListenerTransport::Wt;
    let secure = secure_bind(addr, env);
    let addr_s = addr.to_string();
    let Some((cert_path, key_path)) = remote_certificate(addr, "webtransport", env) else {
        return disabled(WT, addr_s, ListenerDisabledReason::CertProvisionFailed);
    };
    let tokens = remote_tokens(secure, "webtransport", env);
    let token_count = tokens.as_ref().map_or(0, |s| s.len());
    match crate::transport::webtransport::WtListener::from_pem(addr, &cert_path, &key_path, tokens)
    {
        Ok(wt) => {
            let bound = wt.local_addr().map_or(addr_s, |a| a.to_string());
            info!(addr = %bound, tokens = token_count, secure, "WebTransport listening");
            (Some(wt), RemoteListenerSlot::bound(WT, bound))
        }
        Err(err) => {
            warn!(addr = %addr, error = %err, "failed to bind WebTransport; UDS only");
            disabled(WT, addr_s, ListenerDisabledReason::BindFailed)
        }
    }
}

/// The wire form of a server-local client id, saturating at `u32::MAX`.
pub(crate) fn wire_client(client_id: crate::state::ClientId) -> phux_protocol::ids::ClientId {
    phux_protocol::ids::ClientId::new(u32::try_from(client_id.0).unwrap_or(u32::MAX))
}

/// Queue an `ERROR` frame on `out_tx`.
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
    use phux_protocol::caps::ClientCapabilities;
    use phux_protocol::wire::frame::{AttachTarget, ViewportInfo};
    use tokio::task::JoinSet;

    /// A store that will not load still binds, admitting nobody.
    #[test]
    fn a_legacy_store_still_yields_a_bindable_listener_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remote-tokens");
        std::fs::write(&path, format!("# old\n{}\n", "ab".repeat(32))).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let store = remote_token_store(&path, "wss");
        assert!(
            store.is_empty(),
            "a store that will not load still binds, admitting nobody"
        );
    }

    /// Register one Terminal in `state` backed by a running no-PTY actor.
    /// Returns its core id, its wire id, and its engine token.
    fn register_seeded_pane(
        state: &mut crate::state::ServerState,
        window: phux_core::ids::WindowId,
    ) -> (
        phux_core::ids::ResourceId,
        phux_protocol::ids::ResourceId,
        CancellationToken,
    ) {
        let pane = state.registry_mut().new_terminal(window).expect("pane");
        let bundle =
            crate::terminal_actor::TerminalActor::new_with_seed(20, 5, b"seeded").expect("actor");
        let token = bundle.token;
        tokio::task::spawn_local(bundle.actor.run());
        let wire = state.register_resource_handle(pane, bundle.handle, token.clone());
        (pane, wire, token)
    }

    /// An old image holding a pane retained after it exited with status 5
    /// and a running pane that asked to be retained. Returns the image,
    /// both panes' wire ids (exited, running), and their engine tokens.
    fn old_image_with_retained_panes() -> (
        crate::state::ServerState,
        phux_protocol::ids::ResourceId,
        phux_protocol::ids::ResourceId,
        [CancellationToken; 2],
    ) {
        let mut old = crate::state::ServerState::new();
        let sid = old.registry_mut().new_session("main".to_owned());
        let wid = old.registry_mut().new_window(sid).expect("window");
        let (exited, exited_wire, exited_token) = register_seeded_pane(&mut old, wid);
        let (running, running_wire, running_token) = register_seeded_pane(&mut old, wid);
        let _ = old.build_session_snapshot(sid);
        old.note_retain_request(running, 60);
        old.note_retain_request(exited, 60);
        let _retention = old
            .retain_exited(exited, phux_core::process::ExitOutcome::exited(5), 1)
            .expect("retained");
        (
            old,
            exited_wire,
            running_wire,
            [exited_token, running_token],
        )
    }

    /// The next `RESOURCE_CLOSED` for `wire` on `rx`: its reason and exit
    /// status.
    async fn next_close_of(
        rx: &mut tokio::sync::mpsc::Receiver<Outbound>,
        wire: &phux_protocol::ids::ResourceId,
    ) -> (phux_protocol::wire::frame::CloseReason, Option<i32>) {
        use phux_protocol::wire::frame::FrameKind;
        loop {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
                .await
                .expect("RESOURCE_CLOSED arrives");
            match frame {
                Some(Outbound::Frame(FrameKind::ResourceClosed {
                    terminal_id,
                    reason,
                    exit_status,
                    ..
                })) if &terminal_id == wire => return (reason, exit_status),
                Some(_) => {}
                None => panic!("the mailbox closed before RESOURCE_CLOSED"),
            }
        }
    }

    /// ADR-0124 §6: a retained exited pane crosses an upgrade without a PTY
    /// and the resumed image closes it with `SERVER_SHUTDOWN`.
    #[tokio::test(flavor = "current_thread")]
    async fn graceful_upgrade_drops_retained_resources_with_server_shutdown() {
        use phux_protocol::wire::frame::CloseReason;
        let local = tokio::task::LocalSet::new();
        local
            .run_until(Box::pin(async {
                let (old, wire, running_wire, tokens) = old_image_with_retained_panes();
                let blob = old.build_upgrade_blob(7).await;
                let crossed = blob
                    .panes
                    .iter()
                    .find(|p| Some(p.wire_id) == wire.local_id())
                    .expect("the pane is in the blob");
                assert_eq!(
                    crossed.retained_exit.map(|exit| exit.exit_status),
                    Some(Some(5)),
                    "marked, with its exit, for the new image to close"
                );
                let live_blob = blob
                    .panes
                    .iter()
                    .find(|p| Some(p.wire_id) == running_wire.local_id())
                    .expect("the running pane is in the blob");
                assert_eq!(
                    (live_blob.retained_exit, live_blob.retain_secs),
                    (None, Some(60)),
                    "a running pane's retention request crosses"
                );
                assert_eq!(
                    (crossed.master_fd, crossed.child_pid),
                    (None, None),
                    "no PTY crosses for a retained pane"
                );

                let resumed = SharedState::new();
                let root = CancellationToken::new();
                resume_session_tree(&resumed, &blob, &root).expect("resume");
                let pane = resumed
                    .with(|s| s.terminal_from_wire(&wire))
                    .expect("rebuilt, to be closed");
                let running_pane = resumed
                    .with(|s| s.terminal_from_wire(&running_wire))
                    .expect("the running pane is rebuilt");
                assert_eq!(
                    resumed.with(|s| s.retain_request(running_pane)),
                    Some(60),
                    "its retention request survived the upgrade"
                );
                assert!(
                    resumed
                        .with_mut(|s| s.retain_exited(
                            running_pane,
                            phux_core::process::ExitOutcome::exited(1),
                            2
                        ))
                        .is_some(),
                    "so it is still retained at exit"
                );
                let (tx, mut rx) = tokio::sync::mpsc::channel(8);
                resumed.with_mut(|s| {
                    let client = s.new_client_id();
                    s.subscribe_terminal(client, pane, Some(tx));
                });
                assert_eq!(
                    next_close_of(&mut rx, &wire).await,
                    (CloseReason::ServerShutdown, Some(5)),
                    "closed as a shutdown, carrying the exit it was retained with"
                );
                assert!(
                    resumed.with(|s| s.registry().resource(pane).is_none()),
                    "the resumed image reaped it"
                );
                for token in &tokens {
                    token.cancel();
                }
            }))
            .await;
    }

    fn resume_handoff_blob(
        listener_fd: std::os::fd::RawFd,
        sessions: Vec<crate::upgrade::blob::SessionBlob>,
        windows: Vec<crate::upgrade::blob::WindowBlob>,
        panes: Vec<crate::upgrade::blob::PaneBlob>,
    ) -> crate::upgrade::blob::StateBlob {
        crate::upgrade::blob::StateBlob {
            version: crate::upgrade::blob::BLOB_VERSION,
            listener_fd,
            counters: crate::upgrade::blob::Counters {
                next_session_wire_id: 10,
                next_terminal_wire_id: 10,
                next_window_wire_id: 10,
                next_touch_timestamp: 1,
                server_instance: None,
            },
            sessions,
            windows,
            panes,
            agent_sessions: Vec::new(),
        }
    }

    fn write_resume_fd(blob: &crate::upgrade::blob::StateBlob) -> std::os::fd::RawFd {
        use std::io::Write;
        use std::os::fd::IntoRawFd;
        let mut file = tempfile::tempfile().expect("tempfile");
        file.write_all(&blob.to_bytes().expect("serialize"))
            .expect("write blob");
        file.into_raw_fd()
    }

    fn bind_resume_listener(dir: &tempfile::TempDir) -> (std::path::PathBuf, std::os::fd::RawFd) {
        use std::os::fd::IntoRawFd;
        let path = dir.path().join("resume.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        (path, listener.into_raw_fd())
    }

    /// A well-formed empty handoff rebuilds, then the adopted listener accepts.
    #[tokio::test(flavor = "current_thread")]
    async fn transactional_resume_serves_on_the_adopted_listener() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let (path, listener_fd) = bind_resume_listener(&dir);
                let blob = resume_handoff_blob(listener_fd, Vec::new(), Vec::new(), Vec::new());
                let resume_fd = write_resume_fd(&blob);
                let cfg = ServerConfig {
                    socket_path: path.clone(),
                    ..ServerConfig::with_default_socket()
                };
                let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
                let server = tokio::task::spawn_local(async move {
                    ServerRuntime::new(cfg)
                        .resume(resume_fd)
                        .overlay_detect(Vec::new)
                        .run_async(async move {
                            let _ = shutdown_rx.await;
                        })
                        .await
                });

                let mut connected = false;
                for _ in 0..50 {
                    if tokio::net::UnixStream::connect(&path).await.is_ok() {
                        connected = true;
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                assert!(
                    connected,
                    "a successful resume must accept on the adopted listener"
                );
                let _ = shutdown_tx.send(());
                server
                    .await
                    .expect("server task")
                    .expect("resumed server shuts down cleanly");
            })
            .await;
    }

    /// A dangling topology must not start accept loops: `run_async` returns
    /// the rebuild error and the adopted listener is gone.
    #[tokio::test(flavor = "current_thread")]
    async fn dangling_blob_refuses_to_serve_after_resume() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let (path, listener_fd) = bind_resume_listener(&dir);
                let blob = resume_handoff_blob(
                    listener_fd,
                    vec![crate::upgrade::blob::SessionBlob {
                        wire_id: 1,
                        name: "main".to_owned(),
                        window_wire_ids: vec![99],
                        active_window: None,
                        created_at_unix_nanos: 0,
                        last_touched: None,
                        root: None,
                        keep_empty: false,
                    }],
                    Vec::new(),
                    Vec::new(),
                );
                let resume_fd = write_resume_fd(&blob);
                let cfg = ServerConfig {
                    socket_path: path.clone(),
                    ..ServerConfig::with_default_socket()
                };
                let err = ServerRuntime::new(cfg)
                    .resume(resume_fd)
                    .overlay_detect(Vec::new)
                    .run_async(std::future::pending())
                    .await
                    .expect_err("incomplete topology must fail closed");
                match err {
                    ServerError::Rebuild(crate::state::RebuildError::DanglingRef { kind, id }) => {
                        assert_eq!(kind, "window");
                        assert_eq!(id, 99);
                    }
                    other => panic!("expected dangling rebuild error, got {other:?}"),
                }
                assert!(
                    tokio::net::UnixStream::connect(&path).await.is_err(),
                    "rebuild failure must not keep the adopted listener serving"
                );
            })
            .await;
    }

    /// A closed gate must skip the `tailscale` shell-out, not just discard
    /// its answer.
    #[test]
    fn closed_gate_never_calls_detect() {
        let called = std::cell::Cell::new(false);
        let result = resolve_auto_overlay_ip(false, || {
            called.set(true);
            vec![std::net::IpAddr::from([100, 79, 155, 27])]
        });
        assert_eq!(result, None);
        assert!(
            !called.get(),
            "detect() must not run when the gate is closed"
        );
    }

    #[test]
    fn open_gate_calls_detect_and_uses_first_address() {
        let called = std::cell::Cell::new(false);
        let result = resolve_auto_overlay_ip(true, || {
            called.set(true);
            vec![
                std::net::IpAddr::from([100, 79, 155, 27]),
                std::net::IpAddr::from([100, 79, 155, 28]),
            ]
        });
        assert!(called.get(), "an open gate must run detection");
        assert_eq!(result, Some(std::net::IpAddr::from([100, 79, 155, 27])));
    }

    /// Only the default profile auto-binds (ports are host-global), and the
    /// disable switch wins over everything.
    #[test]
    fn auto_overlay_gate_matrix() {
        for (disabled, default_profile, open) in [
            (false, true, true),
            (false, false, false),
            (true, true, false),
            (true, false, false),
        ] {
            assert_eq!(
                auto_overlay_gate_open(disabled, default_profile),
                open,
                "disabled={disabled} default_profile={default_profile}"
            );
        }
    }

    #[test]
    fn exiting_server_unlinks_the_socket_it_bound() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("phux.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        let bound = socket_identity(&path);
        drop(listener);

        unlink_socket_if_ours(&path, bound);
        assert!(!path.exists());
    }

    /// A server whose socket path was taken over must not delete the
    /// winner's socket, or the winner becomes unreachable by path.
    #[test]
    fn exiting_server_leaves_a_socket_that_now_belongs_to_another_server() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("phux.sock");

        let first = std::os::unix::net::UnixListener::bind(&path).expect("bind A");
        let bound_by_a = socket_identity(&path);
        drop(first);

        // Rename rather than unlink so A's inode stays allocated: ext4 and
        // tmpfs would otherwise hand the same inode number straight to B.
        let parked = dir.path().join("phux.sock.parked");
        std::fs::rename(&path, &parked).expect("park A's inode");

        let _second = std::os::unix::net::UnixListener::bind(&path).expect("bind B");
        let bound_by_b = socket_identity(&path);
        assert_ne!(bound_by_a, bound_by_b, "the test needs distinct inodes");

        unlink_socket_if_ours(&path, bound_by_a);
        assert_eq!(
            socket_identity(&path),
            bound_by_b,
            "the winner's socket survives"
        );
    }

    #[test]
    fn a_server_that_never_established_identity_unlinks_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("phux.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");

        unlink_socket_if_ours(&path, None);

        assert!(path.exists(), "unprovable ownership must not delete");
    }

    /// Bound on waits for frames already handed to the mailbox; generous so
    /// a loaded machine does not produce false failures.
    const MAILBOX_DEADLINE: Duration = Duration::from_secs(30);

    #[test]
    fn socket_path_length_limit() {
        let at_limit = PathBuf::from(format!("/{}", "a".repeat(MAX_SOCKET_PATH_LEN - 1)));
        validate_socket_path_len(&at_limit).unwrap();

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

    /// Receivers for a [`stub_terminal`] handle; every channel stays open so
    /// sends succeed, and tests read the ones they observe.
    struct StubRx {
        snapshot: tokio::sync::mpsc::Receiver<crate::terminal_actor::SnapshotRequest>,
        resize: crate::terminal_actor::ResizeReceiver,
        encoded_input: tokio::sync::mpsc::Receiver<crate::terminal_actor::EncodedInputRequest>,
        consumer_attach: tokio::sync::mpsc::Receiver<crate::terminal_actor::ConsumerAttachRequest>,
        consumer_detach: tokio::sync::mpsc::Receiver<crate::terminal_actor::ConsumerDetachRequest>,
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        native_bootstrap:
            tokio::sync::mpsc::Receiver<crate::terminal_actor::NativeBootstrapRequest>,
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        native_publication:
            tokio::sync::mpsc::Receiver<crate::terminal_actor::NativePublicationRequest>,
        _keep: Vec<Box<dyn std::any::Any>>,
    }

    /// A Terminal resource handle with no actor behind it.
    fn stub_terminal() -> (crate::resource::ResourceHandle, StubRx) {
        use tokio::sync::mpsc::channel;
        let mut keep: Vec<Box<dyn std::any::Any>> = Vec::new();
        let (input, rx) = channel(8);
        keep.push(Box::new(rx));
        let (screen, rx) = channel(8);
        keep.push(Box::new(rx));
        let (pwd, rx) = channel(8);
        keep.push(Box::new(rx));
        let (process, rx) = channel(8);
        keep.push(Box::new(rx));
        let (consumer_ack, rx) = channel(8);
        keep.push(Box::new(rx));
        let (snapshot, snapshot_rx) = channel(8);
        let (resize, resize_rx) = crate::terminal_actor::ResizeSender::channel(8);
        let (encoded_input, encoded_rx) = channel(8);
        let (consumer_attach, attach_rx) = channel(8);
        let (consumer_detach, detach_rx) = channel(8);
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let (native_bootstrap, native_bootstrap_rx) = channel(8);
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        let (native_publication, native_publication_rx) = channel(8);
        let facet = crate::terminal_actor::TerminalHandle {
            input,
            encoded_input,
            snapshot,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_bootstrap,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_publication,
            screen,
            pwd,
            process,
            resize,
            ..crate::terminal_actor::TerminalHandle::detached_for_test(80, 24)
        };
        let handle = crate::resource::ResourceHandle {
            kind: crate::resource::ResourceKind::Terminal,
            parent: None,
            output: tokio::sync::broadcast::channel(8).0,
            consumer_attach,
            consumer_detach,
            consumer_ack,
            upgrade: channel(8).0,
            control: channel(8).0,
            facet: crate::resource::ResourceFacetHandle::Terminal(facet),
        };
        let rx = StubRx {
            snapshot: snapshot_rx,
            resize: resize_rx,
            encoded_input: encoded_rx,
            consumer_attach: attach_rx,
            consumer_detach: detach_rx,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_bootstrap: native_bootstrap_rx,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_publication: native_publication_rx,
            _keep: keep,
        };
        (handle, rx)
    }

    /// Register a [`stub_terminal`] for `pane` and return its receivers. The
    /// native channels are closed, so capture falls back to the VT path.
    fn register_stub(state: &SharedState, pane: phux_core::ids::ResourceId) -> StubRx {
        #[cfg_attr(
            not(all(feature = "native-engine", not(target_arch = "wasm32"))),
            allow(unused_mut)
        )]
        let (handle, mut rx) = stub_terminal();
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        {
            rx.native_bootstrap.close();
            rx.native_publication.close();
        }
        state.with_mut(|s| {
            let _ = s.register_resource_handle(pane, handle, CancellationToken::new());
        });
        rx
    }

    /// Attach `client_id` to `session` with default caps.
    fn attach_client(state: &SharedState, session: &str) -> ClientId {
        let client_id = state.with_mut(crate::state::ServerState::new_client_id);
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        state
            .with_mut(|s| s.attach_default_caps(client_id, session, tx))
            .expect("attach");
        client_id
    }

    /// Install a hook dispatcher whose events land on the returned receiver.
    fn capture_hooks(state: &SharedState) -> tokio::sync::mpsc::Receiver<crate::hooks::HookEvent> {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        state.with_mut(|s| s.set_hook_dispatcher(crate::hooks::HookDispatcher::from_sender(tx)));
        rx
    }

    async fn next_frame(out_rx: &mut tokio::sync::mpsc::Receiver<Outbound>) -> FrameKind {
        match tokio::time::timeout(MAILBOX_DEADLINE, out_rx.recv())
            .await
            .expect("outbound frame timed out")
            .expect("outbound closed")
        {
            Outbound::Frame(frame) => frame,
            other @ Outbound::TerminalError { .. } => {
                panic!("unexpected outbound sentinel: {other:?}")
            }
        }
    }

    const fn frame_name(frame: &FrameKind) -> &'static str {
        match frame {
            FrameKind::Attached { .. } => "ATTACHED",
            FrameKind::BootstrapBegin { .. } => "BEGIN",
            FrameKind::BootstrapChunk { .. } => "CHUNK",
            FrameKind::BootstrapReady { .. } => "READY",
            FrameKind::AttachReady { .. } => "ATTACH_READY",
            _ => "OTHER",
        }
    }

    async fn expect_frames(out_rx: &mut tokio::sync::mpsc::Receiver<Outbound>, expected: &[&str]) {
        for want in expected {
            let frame = next_frame(out_rx).await;
            assert_eq!(frame_name(&frame), *want, "got {frame:?}");
        }
    }

    /// Spawn `handle_attach` for `session` on the current `LocalSet`.
    fn spawn_attach(
        state: &SharedState,
        client_id: ClientId,
        attach_id: u32,
        session: &str,
        out_tx: &tokio::sync::mpsc::Sender<Outbound>,
        caps: ClientCapabilities,
        profile: phux_protocol::caps::BootstrapProfile,
    ) -> tokio::task::JoinHandle<()> {
        let state = state.clone();
        let out_tx = out_tx.clone();
        let target = AttachTarget::ByName(session.to_owned());
        tokio::task::spawn_local(async move {
            let token = CancellationToken::new();
            let mut output_pumps = JoinSet::new();
            handle_attach(
                &state,
                client_id,
                attach_id,
                target,
                ViewportInfo::new(80, 24),
                false,
                0,
                None,
                &out_tx,
                caps,
                profile,
                phux_protocol::caps::BootstrapLimits::default(),
                &token,
                &mut output_pumps,
                &token,
                false,
            )
            .await;
        })
    }

    fn state_sync_outcome(bytes: &[u8]) -> crate::terminal_actor::ConsumerAttachOutcome {
        crate::terminal_actor::ConsumerAttachOutcome {
            tick_managed: true,
            state_sync_bootstrap: Some(crate::terminal_actor::StateSyncBootstrap {
                snapshot: crate::grid::SnapshotBytes {
                    cols: 80,
                    rows: 24,
                    bytes: bytes.to_vec(),
                    scrollback: Vec::new(),
                },
                base_seq: 0,
            }),
        }
    }

    /// `VIEWPORT_RESIZE` updates the registry dims and asks the actor for a
    /// resize with a client resync; pixel reports resolve the cell size.
    /// Unattached clients are ignored.
    #[test]
    fn viewport_resize_updates_dims_and_queues_actor_resize() {
        let state = SharedState::new();
        let (sid, _wid, pid) = state.with_mut(|s| s.seed_session("test-session"));
        let mut rx = register_stub(&state, pid);
        let dims = || {
            state
                .with(|s| s.registry().terminal(pid).map(|p| p.dims))
                .expect("pane exists")
        };

        handle_viewport_resize(&state, ClientId(9999), &ViewportInfo::new(200, 60));
        assert_eq!(dims(), (80, 24), "unattached client must not mutate");
        assert!(rx.resize.try_recv().is_err());

        let client_id = attach_client(&state, "test-session");
        handle_viewport_resize(&state, client_id, &ViewportInfo::new(132, 50));
        assert_eq!(dims(), (132, 50));
        let observed = rx.resize.try_recv().expect("resize queued");
        assert_eq!((observed.cols, observed.rows), (132, 50));
        assert!(observed.resync_clients, "a live resize resyncs clients");
        assert_eq!(observed.cell_px, None, "no pixels, no invented cell size");
        assert!(rx.resize.try_recv().is_err(), "exactly one resize");

        // 1320x750 px over 132x50 cells -> 10x15 px cells.
        let viewport = ViewportInfo::new(132, 50).with_pixels(Some(1320), Some(750));
        handle_viewport_resize(&state, client_id, &viewport);
        assert_eq!(
            rx.resize.try_recv().expect("resize").cell_px,
            Some((10, 15))
        );
        assert_eq!(
            state.with(|s| s.attached().get(&client_id).map(|c| c.session)),
            Some(sid)
        );
    }

    /// ADR-0145: `RESIZE_TERMINAL` carries a cell size to the actor when it
    /// has one, and `None` (actor keeps its last cell size) when it has none
    /// or a degenerate one.
    #[test]
    fn terminal_resize_forwards_cell_px_or_keeps_the_last() {
        let state = SharedState::new();
        let (_, _, pane) = state.with_mut(|s| s.seed_session("home"));
        let mut rx = register_stub(&state, pane);
        let client = attach_client(&state, "home");
        let wire = state.with_mut(|s| s.intern_terminal_wire(pane));

        handle_terminal_resize(&state, client, &wire, (100, 40), Some((9, 18)));
        let sized = rx.resize.try_recv().expect("resize queued");
        assert_eq!(
            (sized.cols, sized.rows, sized.cell_px),
            (100, 40, Some((9, 18)))
        );

        handle_terminal_resize(&state, client, &wire, (90, 30), None);
        assert_eq!(rx.resize.try_recv().expect("resize").cell_px, None);

        handle_terminal_resize(&state, client, &wire, (90, 30), Some((0, 18)));
        assert_eq!(rx.resize.try_recv().expect("resize").cell_px, None);
        assert_eq!(
            state.with(|s| s.registry().terminal(pane).unwrap().dims),
            (90, 30)
        );
    }

    /// ADR-0145 regression: a pane-sizing client (the TUI) attaches with no
    /// vote and sizes the pane once with its tile; detaching and re-attaching
    /// (a sidebar session switch) never resizes it. A legacy outer-window
    /// vote resized the pane to the full window first, then to the tile, and
    /// the departing vote shrank it to the headless size: three PTY sizes per
    /// switch, each tearing output written for the previous width.
    #[test]
    fn pane_sizing_attach_resizes_each_pane_at_most_once() {
        let state = SharedState::new();
        let (_, _, pane) = state.with_mut(|s| s.seed_session("home"));
        let mut rx = register_stub(&state, pane);
        let target = || {
            state.with_mut(|s| crate::state::AttachSnapshotPane {
                terminal_id: pane,
                handle: s.resource_handle(pane).unwrap().clone(),
                wire_terminal_id: s.intern_terminal_wire(pane),
            })
        };
        let no_vote = ViewportInfo::new(0, 0);
        let mut sizes = Vec::new();
        let mut drain = |rx: &mut StubRx| {
            while let Ok(request) = rx.resize.try_recv() {
                sizes.push((request.cols, request.rows));
            }
        };

        let client = attach_client(&state, "home");
        attach::apply_attach_viewport(&state, client, &[target()], no_vote);
        handle_terminal_resize(
            &state,
            client,
            &target().wire_terminal_id,
            (116, 37),
            Some((9, 18)),
        );
        drain(&mut rx);
        // A session switch: detach, then attach again the same way.
        state.with_mut(|s| s.detach(client));
        drain(&mut rx);
        let again = attach_client(&state, "home");
        attach::apply_attach_viewport(&state, again, &[target()], no_vote);
        drain(&mut rx);

        assert_eq!(sizes, [(116, 37)], "one tile resize, never the full window");
        assert_eq!(
            state.with(|s| s.registry().terminal(pane).unwrap().dims),
            (116, 37)
        );
    }

    /// ADR-0145: a vote-free detach that leaves a pane unwatched and below
    /// the usable minimum returns it to the headless size; a normally sized
    /// pane keeps its tile, so a GUI or TUI relaunch reflows nothing.
    #[test]
    fn vote_free_detach_resets_only_an_unusably_small_pane() {
        let state = SharedState::new();
        let (_, window, tiny) = state.with_mut(|s| s.seed_session("home"));
        let normal = state.with_mut(|s| s.registry_mut().new_terminal(window).unwrap());
        let mut tiny_rx = register_stub(&state, tiny);
        let mut normal_rx = register_stub(&state, normal);
        let client = attach_client(&state, "home");
        let (tiny_wire, normal_wire) =
            state.with_mut(|s| (s.intern_terminal_wire(tiny), s.intern_terminal_wire(normal)));
        handle_terminal_resize(&state, client, &tiny_wire, (1, 1), None);
        handle_terminal_resize(&state, client, &normal_wire, (116, 37), None);
        tiny_rx.resize.try_recv().expect("tile resize");
        normal_rx.resize.try_recv().expect("tile resize");

        state.with_mut(|s| s.detach(client));

        let reset = tiny_rx.resize.try_recv().expect("tiny pane reset");
        assert_eq!(
            (reset.cols, reset.rows),
            crate::state::HEADLESS_TERMINAL_DIMS
        );
        assert!(
            normal_rx.resize.try_recv().is_err(),
            "normal pane untouched"
        );
        assert_eq!(
            state.with(|s| s.registry().terminal(normal).unwrap().dims),
            (116, 37)
        );
    }

    #[test]
    fn viewport_resize_fans_out_only_to_live_subscribed_session_panes() {
        let state = SharedState::new();
        let (_, window, active) = state.with_mut(|s| s.seed_session("home"));
        let inactive = state.with_mut(|s| s.registry_mut().new_terminal(window).unwrap());
        let retained = state.with_mut(|s| s.registry_mut().new_terminal(window).unwrap());
        let (_, _, foreign) = state.with_mut(|s| s.seed_session("foreign"));
        let unsubscribed = state.with_mut(|s| s.registry_mut().new_terminal(window).unwrap());
        let mut receivers: Vec<_> = [active, inactive, retained, foreign, unsubscribed]
            .into_iter()
            .map(|pane| register_stub(&state, pane))
            .collect();
        let client = attach_client(&state, "home");
        state.with_mut(|s| {
            s.subscribe_terminal(client, foreign, None);
            s.unsubscribe_terminal(client, unsubscribed);
            s.restore_retained_exit(retained, phux_protocol::wire::info::ExitFacet::new(1, 1000));
        });
        let foreign_wire = state.with_mut(|s| s.intern_terminal_wire(foreign));
        handle_terminal_resize(&state, client, &foreign_wire, (137, 57), None);
        receivers[3]
            .resize
            .try_recv()
            .expect("foreign exact geometry");
        for cols in 1..=512 {
            handle_viewport_resize(&state, client, &ViewportInfo::new(cols, 53));
        }
        for index in [0, 1] {
            let request = receivers[index].resize.try_recv().expect("final geometry");
            assert_eq!((request.cols, request.rows), (512, 53));
        }
        for index in [2, 3, 4] {
            assert!(receivers[index].resize.try_recv().is_err());
        }
        assert_eq!(
            state.with(|s| s.registry().terminal(retained).unwrap().dims),
            (80, 24)
        );
        assert_eq!(
            state.with(|s| s.registry().terminal(unsubscribed).unwrap().dims),
            (80, 24)
        );
        assert_eq!(
            state.with(|s| s.registry().terminal(foreign).unwrap().dims),
            (137, 57)
        );
        state.with_mut(|s| s.detach(client));
        assert!(
            receivers[3].resize.try_recv().is_err(),
            "detach cannot reflow a foreign resource subscription"
        );
        assert!(
            receivers[4].resize.try_recv().is_err(),
            "detach cannot reflow an unsubscribed session pane"
        );
        assert_eq!(
            state.with(|s| s.registry().terminal(foreign).unwrap().dims),
            (137, 57)
        );
        assert_eq!(
            state.with(|s| s.registry().terminal(retained).unwrap().dims),
            (80, 24)
        );
    }

    #[test]
    fn viewport_resize_preserves_policy_and_zero_vote_protection() {
        use phux_config::WindowSize;
        let state = SharedState::new();
        let (_, _, pane) = state.with_mut(|s| s.seed_session("home"));
        let mut receiver = register_stub(&state, pane);
        let small = attach_client(&state, "home");
        let large = attach_client(&state, "home");
        state.with_mut(|s| s.set_client_viewport(small, ViewportInfo::new(70, 20)));
        for (policy, expected) in [
            (WindowSize::Smallest, (70, 20)),
            (WindowSize::Largest, (140, 50)),
            (WindowSize::Latest, (140, 50)),
        ] {
            state.with_mut(|s| s.set_window_size(policy));
            handle_viewport_resize(&state, large, &ViewportInfo::new(140, 50));
            let request = receiver.resize.try_recv().expect("policy geometry");
            assert_eq!((request.cols, request.rows), expected);
        }
        let wire = state.with_mut(|s| s.intern_terminal_wire(pane));
        handle_terminal_resize(&state, large, &wire, (99, 33), None);
        receiver.resize.try_recv().expect("exact resize");
        state.with_mut(|s| s.set_window_size(WindowSize::Smallest));
        handle_viewport_resize(&state, large, &ViewportInfo::new(0, 50));
        assert!(
            receiver.resize.try_recv().is_err(),
            "zero report cannot resize from another vote"
        );
        assert_eq!(
            state.with(|s| s.registry().terminal(pane).unwrap().dims),
            (99, 33)
        );
        state.with_mut(|s| s.set_window_size(WindowSize::Manual));
        handle_viewport_resize(&state, large, &ViewportInfo::new(200, 60));
        assert!(receiver.resize.try_recv().is_err());
        assert_eq!(
            state.with(|s| s.registry().terminal(pane).unwrap().dims),
            (99, 33)
        );
    }

    #[test]
    fn closed_resize_delivery_never_advances_registry_geometry() {
        let state = SharedState::new();
        let (_, _, pane) = state.with_mut(|s| s.seed_session("home"));
        drop(register_stub(&state, pane));
        let client = attach_client(&state, "home");
        let target = state.with_mut(|s| crate::state::AttachSnapshotPane {
            terminal_id: pane,
            handle: s.resource_handle(pane).unwrap().clone(),
            wire_terminal_id: s.intern_terminal_wire(pane),
        });
        handle_viewport_resize(&state, client, &ViewportInfo::new(140, 50));
        assert_eq!(
            state.with(|s| s.registry().terminal(pane).unwrap().dims),
            (80, 24)
        );
        handle_terminal_resize(&state, client, &target.wire_terminal_id, (99, 33), None);
        assert_eq!(
            state.with(|s| s.registry().terminal(pane).unwrap().dims),
            (80, 24)
        );
        attach::apply_attach_viewport(&state, client, &[target], ViewportInfo::new(100, 40));
        assert_eq!(
            state.with(|s| s.registry().terminal(pane).unwrap().dims),
            (80, 24)
        );
    }

    /// Each pane's snapshot request completes before the next is asked, so
    /// the connection-wide byte ceiling can only shrink as retained state
    /// grows.
    #[tokio::test(flavor = "current_thread")]
    async fn handle_attach_bounds_snapshot_sources_sequentially() {
        const N: usize = 4;
        let local = LocalSet::new();
        local
            .run_until(async {
                let state = SharedState::new();
                let (_sid, wid, first) = state.with_mut(|s| s.seed_session("multi"));
                let mut panes = vec![first];
                state.with_mut(|s| {
                    for _ in 1..N {
                        panes.push(s.registry_mut().new_terminal(wid).expect("new_pane"));
                    }
                });
                // No actor answers consumer registration: raw per-pane pumps.
                let mut stubs: Vec<StubRx> = panes
                    .iter()
                    .map(|&pid| {
                        let mut rx = register_stub(&state, pid);
                        rx.consumer_attach.close();
                        rx
                    })
                    .collect();

                let (out_tx, mut out_rx) =
                    tokio::sync::mpsc::channel::<Outbound>(crate::state::DEFAULT_CLIENT_MAILBOX);
                let client_id = state.with_mut(crate::state::ServerState::new_client_id);
                let attach_task = spawn_attach(
                    &state,
                    client_id,
                    1,
                    "multi",
                    &out_tx,
                    ClientCapabilities::default(),
                    phux_protocol::caps::BootstrapProfile::SynthesizedVtRaw,
                );

                let mut previous_max_bytes = usize::MAX;
                for (i, stub) in stubs.iter_mut().enumerate() {
                    let req = tokio::time::timeout(MAILBOX_DEADLINE, stub.snapshot.recv())
                        .await
                        .unwrap_or_else(|_| panic!("snapshot request {i} never arrived"))
                        .expect("snapshot channel closed");
                    assert!(req.max_bytes <= previous_max_bytes);
                    previous_max_bytes = req.max_bytes;
                    let payload = crate::grid::SnapshotBytes {
                        cols: 80,
                        rows: 24,
                        bytes: format!("snap-{i}").into_bytes(),
                        scrollback: Vec::new(),
                    };
                    req.reply
                        .send(Ok((payload, u64::try_from(i).unwrap())))
                        .expect("attach still waiting for snapshot");
                }
                expect_frames(&mut out_rx, &["ATTACHED"]).await;
                let mut counts = std::collections::BTreeMap::new();
                for _ in 0..(N * 3) {
                    *counts
                        .entry(frame_name(&next_frame(&mut out_rx).await))
                        .or_insert(0) += 1;
                }
                assert_eq!(
                    counts,
                    [("BEGIN", N), ("CHUNK", N), ("READY", N)]
                        .into_iter()
                        .collect()
                );
                expect_frames(&mut out_rx, &["ATTACH_READY"]).await;
                attach_task.await.expect("attach task panicked");
            })
            .await;
    }

    /// ATTACH registers a per-consumer state-sync lifecycle (gated until
    /// `ATTACH_READY`), a replacement ATTACH retires the prior consumer
    /// before emitting anything, and teardown detaches the consumer.
    #[tokio::test(flavor = "current_thread")]
    async fn attach_registers_and_detach_unregisters_consumer_lifecycle() {
        let local = LocalSet::new();
        local
            .run_until(async {
                let state = SharedState::new();
                let (_sid, _wid, pid) = state.with_mut(|s| s.seed_session("lifecycle"));
                let mut rx = register_stub(&state, pid);
                let (out_tx, mut out_rx) =
                    tokio::sync::mpsc::channel::<Outbound>(crate::state::DEFAULT_CLIENT_MAILBOX);
                let client_id = state.with_mut(crate::state::ServerState::new_client_id);
                let caps = ClientCapabilities::default()
                    .with_output_mode(phux_protocol::caps::OutputMode::StateSync);
                let profile = phux_protocol::caps::BootstrapProfile::SynthesizedVtStateSync;
                let bootstrap = ["BEGIN", "CHUNK", "READY", "ATTACH_READY"];

                let attach_task =
                    spawn_attach(&state, client_id, 1, "lifecycle", &out_tx, caps, profile);
                let attach_req = tokio::time::timeout(MAILBOX_DEADLINE, rx.consumer_attach.recv())
                    .await
                    .expect("ConsumerAttachRequest never arrived")
                    .expect("consumer_attach channel closed");
                assert_eq!(attach_req.client_id, wire_client(client_id));
                assert!(attach_req.wire_terminal_id >= 1);
                let live_gate = attach_req.live_gate.clone();
                assert!(!*live_gate.borrow(), "live output waits for ATTACH_READY");
                attach_req
                    .reply
                    .send(Ok(state_sync_outcome(b"snap")))
                    .expect("send attach reply");
                expect_frames(&mut out_rx, &["ATTACHED"]).await;
                attach_task.await.expect("attach task panicked");
                expect_frames(&mut out_rx, &bootstrap).await;

                let second =
                    spawn_attach(&state, client_id, 2, "lifecycle", &out_tx, caps, profile);
                let retired = tokio::time::timeout(MAILBOX_DEADLINE, rx.consumer_detach.recv())
                    .await
                    .expect("replacement did not retire the prior consumer")
                    .expect("consumer detach channel closed");
                assert_eq!(retired.client_id, wire_client(client_id));
                assert!(
                    out_rx.try_recv().is_err(),
                    "replacement ATTACHED must wait until prior emission is retired",
                );
                retired.reply.send(()).expect("ack retirement");
                let replacement = tokio::time::timeout(MAILBOX_DEADLINE, rx.consumer_attach.recv())
                    .await
                    .expect("replacement ConsumerAttachRequest did not arrive")
                    .expect("consumer attach channel closed");
                let replacement_gate = replacement.live_gate.clone();
                assert!(!*replacement_gate.borrow());
                replacement
                    .reply
                    .send(Ok(state_sync_outcome(b"replacement")))
                    .expect("ack replacement consumer");
                assert!(matches!(
                    next_frame(&mut out_rx).await,
                    FrameKind::Attached { attach_id: 2, .. }
                ));
                expect_frames(&mut out_rx, &bootstrap).await;
                second.await.expect("replacement attach task panicked");
                assert!(*replacement_gate.borrow());
                assert!(
                    *live_gate.borrow(),
                    "aggregate completion releases live output"
                );

                detach_and_release_consumer_state(&state, client_id);
                let detach_req = tokio::time::timeout(MAILBOX_DEADLINE, rx.consumer_detach.recv())
                    .await
                    .expect("ConsumerDetachRequest never arrived")
                    .expect("consumer_detach channel closed");
                assert_eq!(detach_req.client_id, wire_client(client_id));
                assert!(state.with(|s| !s.attached().contains_key(&client_id)));
            })
            .await;
    }

    /// Seeding fires `after-new-pane`, and the exit watcher fires
    /// `pane-exit` with the exit code (docs/consumers/tui.md §9).
    #[test]
    fn pane_lifecycle_hooks_fire() {
        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        let local = LocalSet::new();
        local.block_on(&rt, async {
            let state = SharedState::new();
            let mut hooks = capture_hooks(&state);
            let token = CancellationToken::new();
            seed_session_with_actor(
                &state,
                "hooked",
                phux_config::ScrollbackLimits::new(100, phux_config::DEFAULT_HISTORY_BYTES),
                &token,
            )
            .expect("seed");
            let event = hooks.try_recv().expect("after-new-pane fired");
            assert_eq!(event.name, crate::hooks::AFTER_NEW_PANE);
            assert_eq!(
                event.context.get("session").map(String::as_str),
                Some("hooked")
            );
            assert!(event.context.contains_key("terminal-id"));

            let (_sid, _wid, pane) = state.with_mut(|s| s.seed_session("dying"));
            let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
            spawn_terminal_exit_watcher(state.clone(), pane, Some(exit_rx), token, None);
            exit_tx
                .send(phux_core::process::ExitOutcome::exited(3))
                .expect("exit notify");
            let event = tokio::time::timeout(MAILBOX_DEADLINE, hooks.recv())
                .await
                .expect("pane-exit hook timed out")
                .expect("hook channel closed");
            assert_eq!(event.name, crate::hooks::PANE_EXIT);
            assert_eq!(
                event.context.get("exit-code").map(String::as_str),
                Some("3")
            );
            assert!(event.context.contains_key("terminal-id"));
        });
    }

    /// A kill reaps its pane in its own commit (L1 §5.2), before the process
    /// ends its hangup grace: the watcher still fires `pane-exit` with the
    /// status the process ended with, and last-session self-exit waits for
    /// that process instead of cutting its grace short.
    #[test]
    fn a_pane_killed_before_its_exit_still_fires_pane_exit_and_holds_self_exit() {
        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        let local = LocalSet::new();
        local.block_on(&rt, async {
            let state = SharedState::new();
            let mut hooks = capture_hooks(&state);
            let root = CancellationToken::new();
            let (_sid, _wid, pane) = state.with_mut(|s| s.seed_session("doomed"));
            state.with_mut(crate::state::ServerState::arm_self_exit);
            let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
            spawn_terminal_exit_watcher(state.clone(), pane, Some(exit_rx), root.clone(), None);

            let committed = state.with_mut(|s| {
                committed_close::commit_close(
                    s,
                    &[pane],
                    phux_protocol::wire::frame::CloseReason::Killed,
                    crate::state::CloseAttribution::default(),
                )
            });
            assert_eq!(committed.len(), 1);
            committed_close::announce(&state, committed);
            assert!(
                state.with(|s| s.registry().session_count() == 0 && !s.self_exit_due()),
                "the session is gone at commit, but the process is still in its grace"
            );

            exit_tx
                .send(phux_core::process::ExitOutcome::exited(7))
                .expect("exit notify");
            let event = tokio::time::timeout(MAILBOX_DEADLINE, hooks.recv())
                .await
                .expect("pane-exit hook timed out")
                .expect("hook channel closed");
            assert_eq!(event.name, crate::hooks::PANE_EXIT);
            assert_eq!(
                event.context.get("exit-code").map(String::as_str),
                Some("7")
            );
            tokio::time::timeout(MAILBOX_DEADLINE, root.cancelled())
                .await
                .expect("the last watcher's end self-exits");
        });
    }

    /// A client's emulator reply never reaches the PTY (the canonical
    /// terminal already answered), and `focus-changed` fires only for an
    /// authorized focus-gained event.
    #[test]
    fn terminal_replies_are_discarded_and_focus_hooks_fire() {
        use phux_protocol::input::focus::FocusEvent;

        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        let local = LocalSet::new();
        local.block_on(&rt, async {
            let state = SharedState::new();
            let mut hooks = capture_hooks(&state);
            let (_sid, _wid, pane) = state.with_mut(|s| s.seed_session("focus"));
            let mut rx = register_stub(&state, pane);
            let wire_terminal_id = state.with_mut(|s| s.intern_terminal_wire(pane));
            let client_id = attach_client(&state, "focus");

            let reply = bytes::Bytes::from_static(b"\x1b[5;1R");
            handle_terminal_reply(client_id, &wire_terminal_id, &reply);
            assert!(
                rx.encoded_input.try_recv().is_err(),
                "reply must be discarded"
            );

            let focus = |client, event| {
                handle_terminal_input(
                    &state,
                    client,
                    &wire_terminal_id,
                    crate::state::TerminalInput::Focus(event),
                    "INPUT_FOCUS",
                );
            };
            focus(client_id, FocusEvent::Gained);
            let event = hooks.try_recv().expect("focus-changed fired");
            assert_eq!(event.name, crate::hooks::FOCUS_CHANGED);
            assert!(event.context.contains_key("terminal-id"));
            assert!(event.context.contains_key("client-id"));

            focus(client_id, FocusEvent::Lost);
            assert!(hooks.try_recv().is_err(), "focus lost fires nothing");
            focus(ClientId(4242), FocusEvent::Gained);
            assert!(
                hooks.try_recv().is_err(),
                "a gated focus frame fires nothing"
            );
        });
    }

    /// Teardown fires `client-detached` with the session name, but only for
    /// a client that attached.
    #[test]
    fn detach_fires_client_detached_hook_only_for_attached_clients() {
        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        let local = LocalSet::new();
        local.block_on(&rt, async {
            let state = SharedState::new();
            let mut hooks = capture_hooks(&state);
            let _ = state.with_mut(|s| s.seed_session("leaving"));

            let stranger = state.with_mut(crate::state::ServerState::new_client_id);
            detach_and_release_consumer_state(&state, stranger);
            assert!(hooks.try_recv().is_err());

            let client_id = attach_client(&state, "leaving");
            detach_and_release_consumer_state(&state, client_id);
            let event = hooks.try_recv().expect("client-detached fired");
            assert_eq!(event.name, crate::hooks::CLIENT_DETACHED);
            assert_eq!(
                event.context.get("session").map(String::as_str),
                Some("leaving")
            );
        });
    }

    /// A native replacement bootstrap that fails after a resync closes the
    /// connection with `CODEC_UNAVAILABLE` and rolls back the consumer.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    #[tokio::test(flavor = "current_thread")]
    #[allow(
        clippy::too_many_lines,
        reason = "one scripted actor conversation from attach through fatal resync"
    )]
    async fn native_resync_capture_failure_closes_connection_and_rolls_back_consumer() {
        use phux_protocol::caps::{
            BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, EngineCodec,
            EngineFeatureSet,
        };

        use crate::terminal_actor::{
            ConsumerAttachOutcome, NativeBootstrapReply, PaneOutput, ResyncReason,
        };

        let local = LocalSet::new();
        local
            .run_until(async {
                let state = SharedState::new();
                let (_sid, _wid, terminal) = state.with_mut(|s| s.seed_session("native-fatal"));
                let (handle, mut rx) = stub_terminal();
                let output_tx = handle.output.clone();
                state.with_mut(|s| {
                    let _ = s.register_resource_handle(terminal, handle, CancellationToken::new());
                });

                let client_id = state.with_mut(crate::state::ServerState::new_client_id);
                let (out_tx, mut out_rx) =
                    tokio::sync::mpsc::channel::<Outbound>(crate::state::DEFAULT_CLIENT_MAILBOX);
                let connection_token = CancellationToken::new();
                let task_token = connection_token.clone();
                let state_for_task = state.clone();
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
                        None,
                        &out_tx,
                        ClientCapabilities::default(),
                        BootstrapProfile::NativeState {
                            codec: EngineCodec::LibghosttySnapshotV1,
                            features: EngineFeatureSet::required_native(),
                        },
                        BootstrapLimits::default(),
                        &root_token,
                        &mut output_pumps,
                        &task_token,
                        false,
                    )
                    .await;
                    task_token.cancelled().await;
                    abort_output_pumps(&mut output_pumps, client_id, "fatal-native-resync").await;
                });

                let registration =
                    tokio::time::timeout(MAILBOX_DEADLINE, rx.consumer_attach.recv())
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

                let initial = tokio::time::timeout(MAILBOX_DEADLINE, rx.native_bootstrap.recv())
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
                                    codec: EngineCodec::LibghosttySnapshotV1,
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
                        retained_bytes: b"opaque-checkpoint".len(),
                        base_seq: 0,
                        publication_cursor: [9; 32],
                    }))
                    .expect("initial native reply");
                let publication =
                    tokio::time::timeout(MAILBOX_DEADLINE, rx.native_publication.recv())
                        .await
                        .expect("initial native publication timed out")
                        .expect("native publication sender closed");
                assert_eq!(publication.cursor, [9; 32]);
                publication
                    .reply
                    .send(Ok(crate::terminal_actor::NativePublicationReply {
                        replay: Vec::new(),
                        live: output_tx.subscribe(),
                    }))
                    .expect("initial native publication reply");
                expect_frames(
                    &mut out_rx,
                    &["ATTACHED", "BEGIN", "CHUNK", "READY", "ATTACH_READY"],
                )
                .await;

                output_tx
                    .send(PaneOutput::Live {
                        seq: 1,
                        bytes: bytes::Bytes::from_static(b"live"),
                        at: std::time::Instant::now(),
                    })
                    .expect("live receiver");
                assert!(matches!(
                    next_frame(&mut out_rx).await,
                    FrameKind::ResourceOutput { seq: 1, .. }
                ));

                output_tx
                    .send(PaneOutput::Resync {
                        cols: 80,
                        rows: 24,
                        bytes: bytes::Bytes::new(),
                        reason: ResyncReason::OutboundGap,
                        audience: crate::terminal_actor::ResyncAudience::Everyone,
                        base_seq: 1,
                    })
                    .expect("resync receiver");
                assert!(matches!(
                    next_frame(&mut out_rx).await,
                    FrameKind::BootstrapTombstone {
                        last_valid_seq: 1,
                        ..
                    }
                ));
                let failed = tokio::time::timeout(MAILBOX_DEADLINE, rx.native_bootstrap.recv())
                    .await
                    .expect("replacement native request timed out")
                    .expect("native request sender closed");
                failed
                    .reply
                    .send(Err(crate::native_state::NativeStateError::OutOfMemory))
                    .expect("replacement failure reply");

                assert!(matches!(
                    next_frame(&mut out_rx).await,
                    FrameKind::Error {
                        code: ErrorCode::CodecUnavailable,
                        ..
                    }
                ));
                tokio::time::timeout(MAILBOX_DEADLINE, connection_token.cancelled())
                    .await
                    .expect("connection was not cancelled");
                tokio::time::timeout(MAILBOX_DEADLINE, rx.consumer_detach.recv())
                    .await
                    .expect("consumer rollback timed out")
                    .expect("consumer detach sender closed");
                assert!(
                    state.with(|s| !s.attached().contains_key(&client_id)),
                    "fatal native replacement left the aggregate consumer attached"
                );
                attach_task.await.expect("attach task");
            })
            .await;
    }
}
