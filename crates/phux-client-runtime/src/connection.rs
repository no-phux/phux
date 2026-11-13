//! The async connection driver: dial, framing, keepalive, the reconnect
//! ladder, and the pump that feeds the sans-IO control plane.
//!
//! One [`run_session`] future owns a session's life: it dials the [`Target`],
//! writes queued frames, feeds inbound ones, and walks the [`Ladder`] when
//! the transport drops (a fatal refusal ends the session). A resync redials
//! at once; a nudge cuts a backoff short. The control plane is shared
//! through a mutex, and no foreign code runs while it is held.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use phux_dial::TlsClientIdentity;
use tokio::sync::{Notify, watch};

use crate::control::ControlPlane;
use crate::dial::{DIAL_TIMEOUT, Dial, load_token, plan_quic_with_token, plan_ws};
use crate::reconnect::Ladder;
use crate::target::{AuthorityPin, Resolved, Transport as Lane};

/// The lane a session rides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// The local server's Unix-domain socket.
    Uds(PathBuf),
    /// A Linux Unix socket reached through a native Windows WSL stdio bridge.
    Wsl {
        /// Absolute Linux server socket path.
        socket: PathBuf,
        /// The Linux phux executable selected by the host.
        phux: PathBuf,
    },
    /// The protocol over `ssh DESTINATION REMOTE_PHUX stdio-bridge`.
    Ssh {
        /// The ssh destination, passed to ssh verbatim.
        destination: String,
        /// The `phux` to run on the host.
        remote_phux: String,
    },
    /// A `ws://` or `wss://` URL. Off loopback it needs the pin and token
    /// a registry entry carries; see [`Target::resolve`].
    Ws(String),
    /// A `HOST:PORT` QUIC authority. Off loopback it needs the pin and
    /// token a registry entry carries.
    Quic(String),
}

impl Transport {
    /// The lane's name as a log field.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Uds(_) => "uds",
            Self::Wsl { .. } => "wsl",
            Self::Ssh { .. } => "ssh",
            Self::Ws(_) => "ws",
            Self::Quic(_) => "quic",
        }
    }
}

/// Where a session connects and what it trusts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The lane.
    pub transport: Transport,
    /// The host's name, for wording.
    pub name: String,
    /// The SHA-256 leaf fingerprint to pin, or `None` for loopback.
    pub cert_fingerprint: Option<String>,
    /// The certificate-authority pin beside the leaf pin (ADR-0153).
    pub authority: AuthorityPin,
    /// Where the bearer token lives; read only at dial time.
    pub token_file: Option<PathBuf>,
    /// An in-memory bearer token supplied by an embedder such as a Keychain
    /// consumer. Takes precedence over `token_file` and is never logged.
    pub token: Option<String>,
    /// The TLS server name (SNI) a remote dial offers instead of the
    /// endpoint's host: a relay route when the endpoint is a relay
    /// (ADR-0149). `None` keeps the host-derived default.
    pub tls_server_name: Option<String>,
    /// The workload client certificate to present over TLS (ADR-0116):
    /// what a registry entry enrolled, else [`TlsClientIdentity::None`].
    /// Never read from the environment.
    pub client_identity: TlsClientIdentity,
    /// The program ssh asks for a password, 2FA code or key tap when no
    /// terminal can (`SSH_ASKPASS`): a desktop client's dialog. With one, an
    /// ssh target logs in through it before each dial; without one, ssh
    /// never prompts. Never read from the environment.
    pub ssh_askpass: Option<PathBuf>,
}

impl Target {
    /// A Linux server socket reached through `wsl.exe` on Windows.
    ///
    /// `phux` is the Linux executable explicitly selected by the host, never
    /// replaced by a fallback. `PHUX_WSL_DISTRO` optionally selects the
    /// distribution at dial time. Other operating systems refuse this transport.
    #[must_use]
    pub fn wsl(socket: PathBuf, phux: PathBuf) -> Self {
        let mut target = Self::uds(socket.clone());
        target.transport = Transport::Wsl { socket, phux };
        target
    }

    /// A host's own server, reached over `ssh DESTINATION REMOTE_PHUX
    /// stdio-bridge`. ssh authenticates; no pin or token applies.
    #[must_use]
    pub fn ssh(destination: String, remote_phux: String) -> Self {
        let mut target = Self::uds(destination.clone());
        target.transport = Transport::Ssh {
            destination,
            remote_phux,
        };
        target
    }

    /// The local server at `path`.
    #[must_use]
    pub fn uds(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self {
            name: path.display().to_string(),
            transport: Transport::Uds(path),
            cert_fingerprint: None,
            authority: AuthorityPin::default(),
            token_file: None,
            token: None,
            tls_server_name: None,
            client_identity: TlsClientIdentity::None,
            ssh_askpass: None,
        }
    }

    /// A loopback WebSocket server, plaintext and unpaired.
    #[must_use]
    pub fn ws(url: impl Into<String>) -> Self {
        let url = url.into();
        Self {
            name: url.clone(),
            transport: Transport::Ws(url),
            cert_fingerprint: None,
            authority: AuthorityPin::default(),
            token_file: None,
            token: None,
            tls_server_name: None,
            client_identity: TlsClientIdentity::None,
            ssh_askpass: None,
        }
    }

    /// A loopback QUIC listener, unpaired.
    #[must_use]
    pub fn quic(authority: impl Into<String>) -> Self {
        let authority = authority.into();
        Self {
            name: authority.clone(),
            transport: Transport::Quic(authority),
            cert_fingerprint: None,
            authority: AuthorityPin::default(),
            token_file: None,
            token: None,
            tls_server_name: None,
            client_identity: TlsClientIdentity::None,
            ssh_askpass: None,
        }
    }

    /// A registered host: `[USER@]HOST[:PORT]` resolved against the CLI's
    /// `[[remote]]` registry, which supplies the pin and token file.
    pub fn resolve(raw: &str, config_path: Option<&Path>) -> Result<Self, String> {
        crate::target::resolve(raw, config_path).map(Self::from)
    }

    /// Plan this target's dial under the CLI's trust rules (see
    /// [`crate::dial`]): a routable host needs its pin, and its token is
    /// read now, from [`Self::token`] or else the token file.
    ///
    /// # Errors
    ///
    /// The operator-facing reason the plan was refused (a missing pin or
    /// token, an unresolvable host). No retry changes it.
    pub async fn plan(&self) -> Result<Dial, String> {
        match &self.transport {
            Transport::Uds(path) => Ok(Dial::Uds(path.clone())),
            Transport::Wsl { socket, phux } => wsl::plan(socket, phux),
            Transport::Ssh {
                destination,
                remote_phux,
            } => Ok(Dial::Ssh {
                destination: destination.clone(),
                remote_phux: remote_phux.clone(),
                askpass: self.ssh_askpass.clone(),
            }),
            Transport::Ws(url) => {
                let resolved = self.remote(url.clone(), Lane::Ws(url.clone()));
                let token = match self.token.clone() {
                    Some(token) => Some(token),
                    None => load_token(&resolved)?,
                };
                plan_ws(&resolved, url, token).map(Dial::Ws)
            }
            Transport::Quic(authority) => {
                let resolved =
                    self.remote(format!("quic://{authority}"), Lane::Quic(authority.clone()));
                plan_quic_with_token(&resolved, authority, self.token.clone())
                    .await
                    .map(Dial::Quic)
            }
        }
    }

    /// The registry entry a WebSocket or QUIC dial is planned from.
    pub(crate) fn resolved(&self) -> Option<Resolved> {
        match &self.transport {
            Transport::Uds(_) | Transport::Wsl { .. } | Transport::Ssh { .. } => None,
            Transport::Ws(url) => Some(self.remote(url.clone(), Lane::Ws(url.clone()))),
            Transport::Quic(authority) => {
                Some(self.remote(format!("quic://{authority}"), Lane::Quic(authority.clone())))
            }
        }
    }

    /// This target as the registry entry for `endpoint` over `transport`.
    fn remote(&self, endpoint: String, transport: Lane) -> Resolved {
        Resolved {
            name: self.name.clone(),
            endpoint,
            session: None,
            transport,
            token_file: self.token_file.clone(),
            cert_fingerprint: self.cert_fingerprint.clone(),
            authority: self.authority.clone(),
            tls_server_name: self.tls_server_name.clone(),
            client_identity: self.client_identity.clone(),
        }
    }
}

impl From<Resolved> for Target {
    fn from(resolved: Resolved) -> Self {
        let transport = match &resolved.transport {
            Lane::Quic(authority) => Transport::Quic(authority.clone()),
            Lane::Ws(url) => Transport::Ws(url.clone()),
            Lane::Ssh {
                destination,
                remote_phux,
            } => Transport::Ssh {
                destination: destination.clone(),
                remote_phux: remote_phux.clone(),
            },
        };
        Self {
            transport,
            name: resolved.name,
            cert_fingerprint: resolved.cert_fingerprint,
            authority: resolved.authority,
            token_file: resolved.token_file,
            token: None,
            tls_server_name: resolved.tls_server_name,
            client_identity: resolved.client_identity,
            ssh_askpass: None,
        }
    }
}

/// The reconnect policy a session runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectOptions {
    /// The backoff ladder between attempts.
    pub ladder: Ladder,
    /// Attempts a session that never attached gets before it fails.
    pub initial_attempts: u32,
    /// Wall-clock cap on the never-attached phase, whichever comes first
    /// with `initial_attempts`; a consumer waiting for the attach derives
    /// its deadline from this.
    pub initial_budget: Duration,
    /// How long a nudge's liveness probe waits for any inbound traffic.
    pub probe_timeout: Duration,
    /// The bound on one dial.
    pub dial_timeout: Duration,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            ladder: Ladder::INTERACTIVE,
            initial_attempts: 5,
            initial_budget: Duration::from_secs(18),
            probe_timeout: Duration::from_secs(3),
            dial_timeout: DIAL_TIMEOUT,
        }
    }
}

/// The control plane, shared between the driver and the caller threads.
pub type Shared = Arc<Mutex<ControlPlane>>;

/// Lock the shared control plane, surviving a poisoned mutex.
pub fn lock(shared: &Shared) -> MutexGuard<'_, ControlPlane> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The signals a caller raises at the driver.
#[derive(Debug)]
pub struct Signals {
    /// Frames were queued on the control plane.
    pub outbound: Arc<Notify>,
    /// Drop the socket and redial for fresh snapshots.
    pub resync: watch::Receiver<u64>,
    /// Cut a backoff short; probe an attached socket.
    pub nudge: watch::Receiver<u64>,
    /// The network path changed: move a QUIC connection onto a fresh
    /// socket (connection migration), then probe it.
    pub rebind: watch::Receiver<u64>,
    /// End the session for good.
    pub close: watch::Receiver<bool>,
}

/// Called after the driver changes consumer-visible state, outside the
/// control-plane lock.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// How one connection ended, deciding the session loop's next move.
#[derive(Debug)]
enum ConnectionEnd {
    /// The consumer closed the session, or the server detached it at the
    /// consumer's request: clean stop.
    Closed,
    /// The server closed the socket or the transport failed.
    Dropped(Option<String>),
    /// A refusal no retry can satisfy.
    Refused(String),
    /// Fresh snapshots were asked for: reconnect at once.
    Resync,
}

mod driver;
mod establish;
pub(crate) mod io;
mod ssh;
mod wsl;

pub(crate) use establish::{DialFailure, establish};
pub use ssh::{
    ASKPASS_ENV, HOST_KEY_REFUSED, batch_control_options as ssh_batch_control_options,
    control_options as ssh_control_options, failure_hint as ssh_failure_hint, host_key_remedy,
    master_options as ssh_master_options, pre_auth_drop as ssh_pre_auth_drop,
    pre_auth_retry_delay as ssh_pre_auth_retry_delay, retry_login as ssh_retry_login,
    set_agent as set_ssh_agent,
};

pub use driver::run_session;
