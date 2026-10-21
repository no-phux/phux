//! Hub outbound link supervisor (ADR-0007, ADR-0038).
//!
//! Under `phux server --hub` each enabled [`HubEntry`] gets one supervisor
//! (`run_link`) that dials the satellite and authenticates like a remote
//! consumer: TLS 1.3 pinned by fingerprint plus the pairing bearer token
//! (QUIC preamble or WS `Authorization` header), re-read every attempt so
//! rotation needs no restart.
//!
//! `ssh://` spawns the system `ssh` (or `$PHUX_SSH`) running the remote
//! `phux stdio-bridge`; SSH authenticates the channel, so no token or pin is
//! used (ADR-0038 addendum). The child runs with `BatchMode=yes` and a
//! shell-free argv; its exit is the drop signal and its stderr is drained
//! into a bounded tail.
//!
//! **Fail closed.** [`plan_link`] refuses a routable endpoint without both a
//! token file and a pin, and plaintext `ws://` to routable hosts. A refusal
//! is [`LinkStatus::Refused`] and is never retried.
//!
//! Lost links redial with capped exponential backoff; the streak resets only
//! after `LINK_STABLE_AFTER` of uptime. While up, the supervisor drives the
//! satellite's `RelaySession`; while down it fails queued requests fast with
//! `SatelliteUnreachable`.
//!
//! `inbound://` (ADR-0182) dials nothing: its supervisor's transport waits
//! for the satellite to dial in and be adopted (`hub::adopt`), then runs
//! the same relay session over a stream it opens. The dialing satellite owns
//! the backoff, so the hub takes the next connection without delay.
//!
//! Liveness: QUIC has transport keepalive/idle, SSH uses
//! `ServerAliveInterval`/`CountMax` derived from the same constants, and WS
//! pings on `LINK_KEEPALIVE_INTERVAL` and drops after `LINK_IDLE_TIMEOUT` of
//! silence. Writes are bounded by `LINK_SEND_TIMEOUT`; the keepalive tick
//! also prunes abandoned relayed commands.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use phux_dial::{CertTrust, QuicDial, WsDial, WsTarget};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapProfileKind, ClientCapabilities,
    ServerFeatureExtSet,
};
use phux_protocol::ids::SatelliteHost;
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::framing::FramingError;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::{HubEntry, SatelliteTarget};

mod framing;
mod negotiate;
pub mod net;
mod net_conn;
mod net_io;
mod plan;
pub mod session;
mod status;
mod writer;

use framing::{
    encode_frame_too_large, framing_loss_reason, read_length_prefixed, read_tail, ssh_exit_reason,
    ws_idle_error,
};
use negotiate::negotiate_link;
#[cfg(test)]
use negotiate::{hub_link_capabilities, validate_link_hello_ok};
pub(crate) use net::{NetLinkConn, NetLinkReader, NetLinkTransport, NetLinkWriter};
use plan::read_link_token;
pub use plan::{DialSpec, plan_link, ssh_argv};
pub(crate) use session::run_link;
pub use status::{Backoff, HubLinkStatuses, LinkRefusal, LinkStatus};
use writer::{LinkWrite, QueuedLinkWrite, drive_link_writer, queue_link_write, send_bounded};

/// First redial delay after a failure or a lost connection.
const BACKOFF_BASE: Duration = Duration::from_millis(500);

/// Ceiling for the exponential redial delay.
const BACKOFF_CAP: Duration = Duration::from_secs(30);

/// Housekeeping tick (keepalive and pruning), matching the QUIC dialer's
/// `keep_alive_interval`.
const LINK_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);

/// Inbound-idle limit for WS links, matching QUIC's `max_idle_timeout`.
const LINK_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Stall bound on any single write toward the satellite, so a partitioned
/// peer cannot wedge the supervisor loop.
const LINK_SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// Capacity of the established link's ordered writer; saturation closes the
/// link rather than dropping a registered request.
const LINK_WRITE_QUEUE: usize = 64;
const LINK_WRITE_QUEUE_BYTES: usize = 32 * 1024 * 1024;

/// Retry cadence for subscribers with retained frames.
const RELAY_DELIVERY_RETRY_INTERVAL: Duration = Duration::from_millis(25);

/// Uptime before the failure streak is forgotten. SSH auth failures surface
/// just after spawn, so resetting on establishment would hammer the remote
/// sshd at the base delay.
const LINK_STABLE_AFTER: Duration = Duration::from_secs(30);

/// Retained tail of the ssh child's stderr, which carries the remote
/// command's fd 2 for the link's life.
const SSH_STDERR_TAIL_MAX: usize = 8 * 1024;

/// The transport seam the supervisor dials through ([`NetLinkTransport`]
/// in production, scripted in tests). Errors are log strings.
pub(crate) trait LinkTransport {
    /// The established-connection handle.
    type Conn: LinkConn;

    /// Dial `host` at `spec` (or adopt its inbound connection), authenticating
    /// with `token` when present.
    async fn connect(
        &self,
        host: &SatelliteHost,
        spec: &DialSpec,
        token: Option<String>,
    ) -> Result<Self::Conn, String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NegotiatedBootstrap {
    profile: BootstrapProfile,
    limits: BootstrapLimits,
    /// Features the satellite advertised in `HELLO_OK`.
    server_features: phux_protocol::caps::ServerFeatureSet,
    server_features_ext: ServerFeatureExtSet,
    /// The satellite's `HELLO_OK.server_id`, for the incarnation fence.
    server_id: Option<[u8; 16]>,
}
/// An established hub link: a duplex of complete encoded phux frames.
pub(crate) trait LinkConn {
    type Reader: LinkReader;
    type Writer: LinkWriter + 'static;

    fn into_parts(self) -> (Self::Reader, Self::Writer);

    /// Exact bounds selected before this connection was returned.
    fn bootstrap_limits(&self) -> Result<BootstrapLimits, String>;

    /// Exact synchronization profile selected before this connection returned.
    fn bootstrap_profile(&self) -> Result<BootstrapProfile, String>;

    /// Features the satellite advertised in its `HELLO_OK`.
    fn server_features(&self) -> Result<phux_protocol::caps::ServerFeatureSet, String>;

    /// Missing from older peers' `HELLO_OK`; treat absence as no support.
    fn server_features_ext(&self) -> Result<ServerFeatureExtSet, String> {
        Ok(ServerFeatureExtSet::new())
    }

    /// The satellite's `HELLO_OK.server_id` (L1 §9.1); `None` fences as one
    /// incarnation.
    fn satellite_incarnation(&self) -> Option<[u8; 16]> {
        None
    }
}

pub(crate) trait LinkReader {
    /// Receive the next complete frame.
    async fn recv_frame(&mut self) -> Result<Option<Vec<u8>>, String>;

    /// Take the structured framing violation associated with the last error.
    fn take_framing_violation(&mut self) -> Option<FramingError> {
        None
    }
}

pub(crate) trait LinkWriter {
    /// Put one complete encoded frame on the wire.
    async fn send_frame(&mut self, frame: &[u8]) -> Result<(), String>;

    /// Liveness probe on the keepalive tick. WS pings and enforces
    /// [`LINK_IDLE_TIMEOUT`]; QUIC and SSH are no-ops (their transports
    /// surface idle as a read error). `Err` tears the link down.
    async fn keepalive(&mut self) -> Result<(), String>;
}

/// The hub's running link supervisors, one cancel token per satellite, so a
/// registry reload can start, stop, or redial one link without touching any
/// other.
#[derive(Debug)]
pub(crate) struct HubLinks {
    statuses: HubLinkStatuses,
    relays: super::relay::HubRelays,
    /// Parent of every link's token: server shutdown stops them all.
    cancel: CancellationToken,
    transport: NetLinkTransport,
    links: BTreeMap<SatelliteHost, CancellationToken>,
    /// Where the QUIC listener hands satellites that dial in (ADR-0182).
    adopter: super::adopt::SatelliteAdopter,
}

impl HubLinks {
    /// No links yet; [`Self::start`] adds them to `relays`, the registry the
    /// consumer paths route through.
    pub(crate) fn new(
        relays: super::relay::HubRelays,
        cancel: &CancellationToken,
        ssh_program: std::ffi::OsString,
        adopter: super::adopt::SatelliteAdopter,
    ) -> Self {
        Self {
            statuses: HubLinkStatuses::default(),
            relays,
            cancel: cancel.clone(),
            transport: NetLinkTransport::new(ssh_program, adopter.clone()),
            links: BTreeMap::new(),
            adopter,
        }
    }

    /// Spawn [`run_link`] for `host` on the current `LocalSet` and register
    /// its relay handle, stopping any link `host` already had. `journal`
    /// re-stamps relayed events (ADR-0123).
    pub(crate) fn start(
        &mut self,
        host: &SatelliteHost,
        entry: &HubEntry,
        journal: &crate::state::SharedState,
    ) {
        self.stop(host);
        if matches!(entry.target, SatelliteTarget::Inbound) {
            self.adopter.register(host);
        }
        let (handle, mut mailbox) = super::relay::RelayHandle::new(host.clone());
        mailbox.journal = Some(journal.clone());
        self.relays.insert(handle);
        let cancel = self.cancel.child_token();
        self.links.insert(host.clone(), cancel.clone());
        tokio::task::spawn_local(run_link(
            host.clone(),
            entry.clone(),
            self.transport.clone(),
            self.statuses.clone(),
            mailbox,
            cancel,
        ));
    }

    /// Stop `host`'s link: new requests find no route, and the cancelled
    /// supervisor tears its relay session down, notifying its subscribers.
    pub(crate) fn stop(&mut self, host: &SatelliteHost) {
        if let Some(cancel) = self.links.remove(host) {
            cancel.cancel();
        }
        self.adopter.unregister(host);
        self.relays.remove(host);
        self.statuses.remove(host);
    }
}

/// A local task handle that aborts on drop (the stderr drainer).
#[derive(Debug)]
pub(crate) struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests;
