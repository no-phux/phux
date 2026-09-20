//! The async connection driver: dial, framing, keepalive, the reconnect
//! ladder, and the pump that feeds the sans-IO control plane.
//!
//! One [`run_session`] future owns a session's whole life: it dials the
//! [`Target`] over its lane, writes the frames the [`ControlPlane`] queues,
//! feeds it every inbound frame, and when the transport drops walks the
//! [`Ladder`] from `crate::reconnect`, with
//! [`is_fatal_refusal`](crate::reconnect::is_fatal_refusal) ending the
//! session terminally instead. A resync (a replica the kernel invalidated,
//! or a consumer's request) redials at once; a nudge cuts a backoff short
//! and probes a possibly-stale socket. The driver runs on whatever tokio
//! runtime polls it; [`crate::runtime`] gives it one background thread so
//! a synchronous binding drives the session with plain method calls.
//!
//! The control plane is shared with those callers through a mutex. The
//! driver never calls foreign code while holding it: the wake callback runs
//! after the lock is released.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::{Notify, watch};

use crate::control::ControlPlane;
use crate::dial::DIAL_TIMEOUT;
use crate::reconnect::Ladder;
use crate::target::{Resolved, Transport as Lane};

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
    /// Where the bearer token lives; read only at dial time.
    pub token_file: Option<PathBuf>,
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
            token_file: None,
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
            token_file: None,
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
            token_file: None,
        }
    }

    /// A registered host: `[USER@]HOST[:PORT]` resolved against the CLI's
    /// `[[remote]]` registry, which supplies the pin and token file.
    pub fn resolve(raw: &str, config_path: Option<&Path>) -> Result<Self, String> {
        crate::target::resolve(raw, config_path).map(Self::from)
    }

    /// The registry entry a WebSocket or QUIC dial is planned from.
    fn resolved(&self) -> Option<Resolved> {
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
            token_file: resolved.token_file,
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
