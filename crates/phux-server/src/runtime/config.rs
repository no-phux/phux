//! Server configuration, public runtime flags, and startup errors.

use crate::runtime::ServerEnv;
use phux_config::socket::default_socket_path;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Configuration for [`crate::runtime::ServerRuntime`].
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
    pub seed_command: Option<crate::runtime::apps::terminal::launch::LaunchCommand>,
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
    /// `$SHELL`, else `/bin/sh` ([`phux_config::shell::resolve_shell`]).
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
    /// `[policy] mode` (`docs/remote.md#who-can-connect`). `None` is `local`
    /// unless a remote entry point or authority location is configured,
    /// which refuses to start (ADR-0179); with an injected
    /// [`Self::policy_engine`] it leaves that engine in charge.
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
    /// Where the organization store lives (ADR-0162). `phux server` uses
    /// [`crate::OrgStoreLocation::for_socket`]; in-process servers default
    /// to memory so no test opens a shared store.
    pub org_store: crate::OrgStoreLocation,
    /// The work store this server answers its satellites' mail and claims
    /// from (`docs/agents.md#mail-across-a-hub`). `phux server` uses this
    /// user's; in-process servers default to none, so no test opens a shared
    /// store, and a satellite's mail is refused.
    pub work_store: Option<Box<phux_work::Client>>,
    /// Direct guardian executable override for isolated fixtures; otherwise
    /// the running server executable, never a PATH-resolved installed phux.
    pub native_host_executable: Option<PathBuf>,
    /// Plugin `[[startup]]` sidecars. `None` supervises nothing (in-process
    /// tests and embedders). `phux server` fills it from the config file.
    /// Boxed so a `ServerConfig` captured by an accept-loop future stays
    /// under the large-future lint. `None` supervises nothing.
    pub plugin_startup: Option<Box<crate::plugin_startup::PluginStartupConfig>>,
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
    /// `--hub`; the satellite registry is re-read and re-validated. Also set
    /// at upgrade time when a reload doorbell promoted the server to a hub.
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
            shell: phux_config::shell::resolve_shell(None),
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
            org_store: crate::OrgStoreLocation::Memory,
            work_store: None,
            native_host_executable: None,
            plugin_startup: None,
        }
    }
}

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

/// Errors surfaced by [`crate::runtime::ServerRuntime`].
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
    /// An `[[uplink]]` entry was unsafe or malformed (ADR-0182).
    #[error("{0}")]
    Uplink(#[from] crate::uplink::UplinkError),

    /// A `[policy]` mode the rest of the configuration contradicts
    /// (`docs/remote.md#who-can-connect`).
    #[error("policy: {0}")]
    Policy(#[from] crate::policy::PostureError),

    /// The `paired` posture with missing, malformed, or unsafe workload
    /// authority material (`docs/remote.md#who-can-connect`).
    #[error("policy: paired mode needs usable workload authority material: {0}")]
    PolicyMaterial(#[source] crate::workload::WorkloadError),

    /// The upgrade blob decoded, but rebuilding the session tree failed;
    /// resume fails closed rather than serve a partial tree.
    #[error("resume rebuild: {0}")]
    Rebuild(#[from] crate::state::RebuildError),

    /// The organization store could not be opened, read, or written; the
    /// server refuses to start rather than guess at its records.
    #[error("organization store: {0}")]
    OrgStore(#[from] crate::org_store::StoreError),

    /// The stored records break an organization invariant.
    #[error("organization records: {0}")]
    Organization(#[from] phux_core::InvariantViolation),

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
