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
use crate::dial::DIAL_TIMEOUT;
use crate::reconnect::Ladder;
use crate::target::{AuthorityPin, Resolved, Transport as Lane};

/// The lane a session rides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// The local server's Unix-domain socket.
    Uds(PathBuf),
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
}

impl Target {
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
        }
    }

    /// A registered host: `[USER@]HOST[:PORT]` resolved against the CLI's
    /// `[[remote]]` registry, which supplies the pin and token file.
    pub fn resolve(raw: &str, config_path: Option<&Path>) -> Result<Self, String> {
        crate::target::resolve(raw, config_path).map(Self::from)
    }

    /// The registry entry a WebSocket or QUIC dial is planned from.
    pub(crate) fn resolved(&self) -> Option<Resolved> {
        let (endpoint, transport) = match &self.transport {
            Transport::Uds(_) => return None,
            Transport::Ws(url) => (url.clone(), Lane::Ws(url.clone())),
            Transport::Quic(authority) => {
                (format!("quic://{authority}"), Lane::Quic(authority.clone()))
            }
        };
        Some(Resolved {
            name: self.name.clone(),
            endpoint,
            session: None,
            transport,
            token_file: self.token_file.clone(),
            cert_fingerprint: self.cert_fingerprint.clone(),
            authority: self.authority.clone(),
            tls_server_name: self.tls_server_name.clone(),
            client_identity: self.client_identity.clone(),
        })
    }
}

impl From<Resolved> for Target {
    fn from(resolved: Resolved) -> Self {
        let transport = match &resolved.transport {
            Lane::Quic(authority) => Transport::Quic(authority.clone()),
            Lane::Ws(url) => Transport::Ws(url.clone()),
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
mod io;

pub use driver::run_session;
