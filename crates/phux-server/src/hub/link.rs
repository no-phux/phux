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

/// What the planner decided to dial. The token stays a path, re-read on
/// every attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialSpec {
    /// Dial a QUIC listener (`quic://host:port`).
    Quic {
        /// Hostname, IPv4, or bracketed IPv6 literal to resolve.
        host: String,
        /// UDP port.
        port: u16,
        /// Certificate trust: pinned for routable, skip for loopback dev.
        trust: CertTrust,
        /// Pairing-token file, read per attempt.
        token_file: Option<PathBuf>,
    },
    /// Dial a WebSocket listener (`ws://` loopback dev or `wss://`).
    Ws {
        /// The full endpoint URL as configured.
        url: String,
        /// Certificate trust (`wss://` only).
        trust: CertTrust,
        /// Pairing-token file, read per attempt.
        token_file: Option<PathBuf>,
    },
    /// Spawn `ssh` bridging to the satellite's UDS via `phux stdio-bridge`.
    /// No auth material: SSH authenticates the channel.
    Ssh {
        /// Login user (`-l`), if configured.
        user: Option<String>,
        /// Destination host (bare — IPv6 without brackets).
        host: String,
        /// SSH port (`-p`), if configured.
        port: Option<u16>,
    },
}

impl DialSpec {
    /// The token file this spec dials with (`None` for SSH).
    #[must_use]
    pub fn token_file(&self) -> Option<&Path> {
        match self {
            Self::Quic { token_file, .. } | Self::Ws { token_file, .. } => token_file.as_deref(),
            Self::Ssh { .. } => None,
        }
    }
}

impl core::fmt::Display for DialSpec {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Quic { host, port, .. } => write!(f, "quic://{host}:{port}"),
            Self::Ws { url, .. } => f.write_str(url),
            Self::Ssh { user, host, port } => {
                f.write_str("ssh://")?;
                if let Some(user) = user {
                    write!(f, "{user}@")?;
                }
                if host.contains(':') {
                    write!(f, "[{host}]")?;
                } else {
                    f.write_str(host)?;
                }
                if let Some(port) = port {
                    write!(f, ":{port}")?;
                }
                Ok(())
            }
        }
    }
}

/// Build the argv (excluding the program) for one SSH-stdio dial.
///
/// Shell-free, host/user charset-validated at parse time, and `--` before
/// the host anyway. `BatchMode=yes` fails fast without a key, `-T` refuses a
/// remote PTY, and `ClearAllForwardings=yes` ignores config forwardings.
/// `ServerAliveInterval`/`ServerAliveCountMax` implement the link's
/// keepalive/idle contract at the SSH layer.
#[must_use]
pub fn ssh_argv(user: Option<&str>, host: &str, port: Option<u16>) -> Vec<String> {
    // Derived so the SSH liveness window matches the WS/QUIC one.
    let alive_interval = LINK_KEEPALIVE_INTERVAL.as_secs().max(1);
    let alive_count = (LINK_IDLE_TIMEOUT.as_secs() / alive_interval).max(1);
    let mut argv = vec![
        "-o".to_owned(),
        "BatchMode=yes".to_owned(),
        "-o".to_owned(),
        "ClearAllForwardings=yes".to_owned(),
        "-o".to_owned(),
        format!("ServerAliveInterval={alive_interval}"),
        "-o".to_owned(),
        format!("ServerAliveCountMax={alive_count}"),
        "-T".to_owned(),
    ];
    if let Some(port) = port {
        argv.push("-p".to_owned());
        argv.push(port.to_string());
    }
    if let Some(user) = user {
        argv.push("-l".to_owned());
        argv.push(user.to_owned());
    }
    argv.push("--".to_owned());
    argv.push(host.to_owned());
    argv.push("phux".to_owned());
    argv.push("stdio-bridge".to_owned());
    argv
}

/// Why the planner refused to dial a satellite (fail closed, ADR-0038).
/// A configuration error; never retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkRefusal {
    /// Plaintext `ws://` to a routable host (no TLS, so no pin possible).
    PlaintextRoutable {
        /// The configured endpoint URL.
        url: String,
    },
    /// Routable endpoint with no `token-file` on the registry entry.
    MissingToken {
        /// The configured endpoint.
        endpoint: String,
    },
    /// Routable endpoint with no `cert-fingerprint` pin on the entry.
    MissingFingerprint {
        /// The configured endpoint.
        endpoint: String,
    },
    /// The endpoint failed the dialer's stricter URL parse.
    Malformed {
        /// The configured endpoint.
        endpoint: String,
        /// Why it did not parse.
        reason: String,
    },
}

impl core::fmt::Display for LinkRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PlaintextRoutable { url } => write!(
                f,
                "{url}: refusing plaintext ws:// to a routable host; use wss:// with `phux pair` \
                 credentials"
            ),
            Self::MissingToken { endpoint } => write!(
                f,
                "{endpoint}: refusing to dial a routable satellite without a token-file; run \
                 `phux pair` on the satellite host and register the token file with \
                 `phux host add --role satellite`"
            ),
            Self::MissingFingerprint { endpoint } => write!(
                f,
                "{endpoint}: refusing to dial a routable satellite without a cert-fingerprint \
                 pin; run `phux pair` on the satellite host and register the printed fingerprint"
            ),
            Self::Malformed { endpoint, reason } => {
                write!(f, "{endpoint}: malformed endpoint: {reason}")
            }
        }
    }
}

/// Per-satellite connection state, published by the link supervisor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkStatus {
    /// Fail-closed refusal; never dialed until the registry entry changes.
    Refused {
        /// Human-readable refusal, from [`LinkRefusal`]'s `Display`.
        reason: String,
    },
    /// A dial attempt is in flight.
    Connecting {
        /// 1-based attempt number since the last successful connection.
        attempt: u32,
    },
    /// The link is established and authenticated.
    Connected,
    /// The last attempt failed or the link dropped; redial after `retry_in`.
    Backoff {
        /// 1-based number of the attempt that just failed.
        attempt: u32,
        /// Delay before the next dial.
        retry_in: Duration,
        /// Why the attempt failed or the connection dropped.
        last_error: String,
    },
}

impl core::fmt::Display for LinkStatus {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Refused { reason } => write!(f, "refused (fail closed): {reason}"),
            Self::Connecting { attempt } => write!(f, "connecting (attempt {attempt})"),
            Self::Connected => f.write_str("connected"),
            Self::Backoff {
                attempt,
                retry_in,
                last_error,
            } => write!(
                f,
                "backoff {}ms after attempt {attempt}: {last_error}",
                retry_in.as_millis()
            ),
        }
    }
}

/// Shared per-satellite [`LinkStatus`] map; the mutex is never held across
/// an await.
#[derive(Debug, Clone, Default)]
pub struct HubLinkStatuses {
    inner: Arc<Mutex<BTreeMap<SatelliteHost, LinkStatus>>>,
}

impl HubLinkStatuses {
    /// Publish `status` for `host`.
    pub fn set(&self, host: &SatelliteHost, status: LinkStatus) {
        self.lock().insert(host.clone(), status);
    }

    /// The current status for `host`, if its supervisor has reported yet.
    #[must_use]
    pub fn get(&self, host: &SatelliteHost) -> Option<LinkStatus> {
        self.lock().get(host).cloned()
    }

    /// Forget `host` (its link was stopped).
    pub fn remove(&self, host: &SatelliteHost) {
        self.lock().remove(host);
    }

    /// Snapshot every satellite's status, in deterministic name order.
    #[must_use]
    pub fn snapshot(&self) -> BTreeMap<SatelliteHost, LinkStatus> {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<SatelliteHost, LinkStatus>> {
        // Poison only means a panic mid-insert; the map stays consistent.
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Capped exponential backoff: `base * 2^failures`, saturating at `cap`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backoff {
    base: Duration,
    cap: Duration,
    failures: u32,
}

impl Backoff {
    /// A fresh backoff starting at `base` and never exceeding `cap`.
    #[must_use]
    pub const fn new(base: Duration, cap: Duration) -> Self {
        Self {
            base,
            cap,
            failures: 0,
        }
    }

    /// Consecutive failures recorded since the last [`Self::reset`].
    #[must_use]
    pub const fn failures(&self) -> u32 {
        self.failures
    }

    /// Record a failure and return the delay before the next attempt.
    pub fn next_delay(&mut self) -> Duration {
        // Clamp the exponent so the shift cannot overflow.
        let exponent = self.failures.min(20);
        let delay = self
            .base
            .saturating_mul(2u32.saturating_pow(exponent))
            .min(self.cap);
        self.failures = self.failures.saturating_add(1);
        delay
    }

    /// Forget the failure streak after a stable connection.
    pub const fn reset(&mut self) {
        self.failures = 0;
    }

    /// Settle the streak after a lost connection and return the 1-based
    /// attempt number. Only a connection up for `LINK_STABLE_AFTER` clears
    /// the streak.
    pub fn settle_after_loss(&mut self, up_for: Duration) -> u32 {
        if up_for >= LINK_STABLE_AFTER {
            self.reset();
        }
        self.failures.saturating_add(1)
    }
}

/// Decide how (or whether) to dial one hub-table entry. Pure.
///
/// The fail-closed gate (ADR-0038): routable endpoints need a token file and
/// a pin; plaintext `ws://` is loopback-only; loopback keeps the dev
/// carve-out. `ssh://` needs neither, and ignores any configured.
/// # Errors
///
/// A [`LinkRefusal`] naming the configuration gap.
pub fn plan_link(entry: &HubEntry) -> Result<DialSpec, LinkRefusal> {
    match &entry.target {
        SatelliteTarget::Ssh { user, host, port } => Ok(DialSpec::Ssh {
            user: user.clone(),
            host: host.clone(),
            port: *port,
        }),
        SatelliteTarget::Quic { host, port } => {
            let trust = plan_trust(host_is_loopback(host), host, entry)?;
            Ok(DialSpec::Quic {
                host: host.clone(),
                port: *port,
                trust,
                token_file: entry.token_file.clone(),
            })
        }
        SatelliteTarget::Ws { url } => {
            let target = parse_ws(url)?;
            if !target.is_loopback() {
                return Err(LinkRefusal::PlaintextRoutable { url: url.clone() });
            }
            Ok(DialSpec::Ws {
                url: url.clone(),
                // No TLS handshake on ws://; the trust value is inert.
                trust: CertTrust::SkipVerify,
                token_file: entry.token_file.clone(),
            })
        }
        SatelliteTarget::Wss { url } => {
            let target = parse_ws(url)?;
            let trust = plan_trust(target.is_loopback(), url, entry)?;
            Ok(DialSpec::Ws {
                url: url.clone(),
                trust,
                token_file: entry.token_file.clone(),
            })
        }
    }
}

/// Trust for TLS transports: routable requires pin and token; loopback pins
/// when a fingerprint is configured and skips verification otherwise.
fn plan_trust(loopback: bool, endpoint: &str, entry: &HubEntry) -> Result<CertTrust, LinkRefusal> {
    if loopback {
        return Ok(entry
            .cert_fingerprint
            .clone()
            .map_or(CertTrust::SkipVerify, CertTrust::Pinned));
    }
    let Some(fingerprint) = entry.cert_fingerprint.clone() else {
        return Err(LinkRefusal::MissingFingerprint {
            endpoint: endpoint.to_owned(),
        });
    };
    if entry.token_file.is_none() {
        return Err(LinkRefusal::MissingToken {
            endpoint: endpoint.to_owned(),
        });
    }
    Ok(CertTrust::Pinned(fingerprint))
}

fn parse_ws(url: &str) -> Result<WsTarget, LinkRefusal> {
    WsTarget::parse(url).map_err(|err| LinkRefusal::Malformed {
        endpoint: url.to_owned(),
        reason: err.to_string(),
    })
}

/// Whether a host token names loopback (`localhost`, IPv4, or IPv6).
fn host_is_loopback(host: &str) -> bool {
    let bare = host.trim_matches(['[', ']']);
    bare.eq_ignore_ascii_case("localhost")
        || bare
            .parse::<std::net::IpAddr>()
            .is_ok_and(|addr| addr.is_loopback())
}

/// Read and validate the pairing token: first non-empty line, hex. Errors
/// count as a failed attempt, so a fixed file is picked up on redial.
fn read_link_token(path: Option<&Path>) -> Result<Option<String>, String> {
    let Some(path) = path else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(path)
        .map_err(|err| format!("read token file {}: {err}", path.display()))?;
    let Some(token) = raw.lines().map(str::trim).find(|line| !line.is_empty()) else {
        return Err(format!("token file {} is empty", path.display()));
    };
    if hex::decode(token).is_err() {
        return Err(format!(
            "token file {} does not hold a valid hex pairing token",
            path.display()
        ));
    }
    Ok(Some(token.to_owned()))
}

/// The transport seam the supervisor dials through ([`NetLinkTransport`]
/// in production, scripted in tests). Errors are log strings.
pub(crate) trait LinkTransport {
    /// The established-connection handle.
    type Conn: LinkConn;

    /// Dial `spec`, authenticating with `token` when present.
    async fn connect(&self, spec: &DialSpec, token: Option<String>) -> Result<Self::Conn, String>;
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

/// Supervise one satellite link: plan, dial, relay, redial, until `cancel`
/// or every relay handle is dropped. A refused link still drains its
/// mailbox with typed errors.
#[allow(
    clippy::future_not_send,
    reason = "ADR-0014: hub link supervisors run on the server's LocalSet; the transport seam is generic so tests can inject !Send scripted transports"
)]
pub(crate) async fn run_link<T: LinkTransport>(
    host: SatelliteHost,
    entry: HubEntry,
    transport: T,
    statuses: HubLinkStatuses,
    mailbox: super::relay::RelayMailbox,
    cancel: CancellationToken,
) {
    let super::relay::RelayMailbox {
        requests: relay_rx,
        unsubscribes: unsub_rx,
        journal,
        operations,
    } = mailbox;
    let mut inbox = LinkInbox {
        host: &host,
        relay_rx,
        unsub_rx,
        cancel: &cancel,
    };
    let spec = match plan_link(&entry) {
        Ok(spec) => spec,
        Err(refusal) => return refuse_link(&statuses, &refusal, &mut inbox).await,
    };

    let mut backoff = Backoff::new(BACKOFF_BASE, BACKOFF_CAP);
    loop {
        let attempt = backoff.failures().saturating_add(1);
        statuses.set(&host, LinkStatus::Connecting { attempt });
        let connect = async {
            // Re-read the token every attempt (rotation).
            let token = read_link_token(spec.token_file())?;
            transport.connect(&spec, token).await
        };
        // Fail relay requests fast while the dial is in flight.
        let Some(outcome) = inbox.drain_until(connect, "link is connecting").await else {
            return;
        };

        let (failed_attempt, last_error) = match outcome {
            Ok(conn) => {
                info!(satellite = %host, target = %spec, "hub link established");
                statuses.set(&host, LinkStatus::Connected);
                let connected_at = tokio::time::Instant::now();
                let session = run_relay_session(
                    &host,
                    conn,
                    &mut inbox.relay_rx,
                    &mut inbox.unsub_rx,
                    &cancel,
                    journal.as_ref(),
                    &operations,
                );
                match session.await {
                    Some(reason) => {
                        warn!(
                            satellite = %host,
                            reason = %reason,
                            "hub link lost; scheduling redial"
                        );
                        // A fast death is a failed attempt (`settle_after_loss`).
                        (backoff.settle_after_loss(connected_at.elapsed()), reason)
                    }
                    // Cancelled or every handle dropped: supervisor done.
                    None => return,
                }
            }
            Err(error) => {
                warn!(
                    satellite = %host,
                    target = %spec,
                    attempt,
                    error = %error,
                    "hub link attempt failed"
                );
                (attempt, error)
            }
        };

        let retry_in = backoff.next_delay();
        statuses.set(
            &host,
            LinkStatus::Backoff {
                attempt: failed_attempt,
                retry_in,
                last_error,
            },
        );
        let backoff_sleep = tokio::time::sleep(retry_in);
        let slept = inbox
            .drain_until(backoff_sleep, "link is backing off before redial")
            .await;
        if slept.is_none() {
            return;
        }
    }
}

/// A link whose plan was refused is never dialed: publish the refusal,
/// then fail relay requests fast until the supervisor exits.
#[allow(
    clippy::future_not_send,
    reason = "ADR-0014: runs on the server's LocalSet inside run_link"
)]
async fn refuse_link(
    statuses: &HubLinkStatuses,
    refusal: &impl std::fmt::Display,
    inbox: &mut LinkInbox<'_>,
) {
    warn!(
        satellite = %inbox.host,
        refusal = %refusal,
        "hub link refused (fail closed); not dialing"
    );
    statuses.set(
        inbox.host,
        LinkStatus::Refused {
            reason: refusal.to_string(),
        },
    );
    let _never = inbox
        .drain_until(
            std::future::pending::<()>(),
            "link refused (fail closed); fix the registry entry",
        )
        .await;
}

/// A link supervisor's relay mailbox and cancel token: what it serves
/// between connections, and hands to [`run_relay_session`] during one.
struct LinkInbox<'a> {
    host: &'a SatelliteHost,
    relay_rx: tokio::sync::mpsc::Receiver<super::relay::RelayRequest>,
    unsub_rx: tokio::sync::mpsc::UnboundedReceiver<super::relay::Unsubscribe>,
    cancel: &'a CancellationToken,
}

impl LinkInbox<'_> {
    /// Await `until` while no link is up to carry relay traffic: every
    /// relay request fails fast with `why`, and unsubscribes are dropped
    /// (there is no registry to withdraw from). `None` means the supervisor
    /// should exit: `cancel` fired, or every relay or unsubscribe handle was
    /// dropped.
    #[allow(
        clippy::future_not_send,
        reason = "ADR-0014: runs on the server's LocalSet inside run_link"
    )]
    async fn drain_until<F: std::future::Future>(
        &mut self,
        until: F,
        why: &str,
    ) -> Option<F::Output> {
        tokio::pin!(until);
        loop {
            tokio::select! {
                () = self.cancel.cancelled() => return None,
                request = self.relay_rx.recv() => {
                    super::relay::fail_fast(request?, self.host, why);
                }
                unsubscribe = self.unsub_rx.recv() => {
                    let _dropped: super::relay::Unsubscribe = unsubscribe?;
                }
                output = &mut until => return Some(output),
            }
        }
    }
}

/// Drive one established connection's relay session. `Some(reason)` means
/// the connection was lost (redial); `None` means exit. Session teardown
/// runs on every path.
#[allow(
    clippy::future_not_send,
    reason = "ADR-0014: runs on the server's LocalSet inside run_link"
)]
async fn run_relay_session<C: LinkConn>(
    host: &SatelliteHost,
    conn: C,
    relay_rx: &mut tokio::sync::mpsc::Receiver<super::relay::RelayRequest>,
    unsub_rx: &mut tokio::sync::mpsc::UnboundedReceiver<super::relay::Unsubscribe>,
    cancel: &CancellationToken,
    journal: Option<&crate::state::SharedState>,
    operations: &super::operation_fence::OperationFence,
) -> Option<String> {
    let mut session = match negotiated_relay_session(host, &conn) {
        Ok(session) => session,
        Err(error) => return Some(error),
    };
    session.set_journal(journal.cloned());
    // ADR-0127: publish what this satellite advertised for dispatch.
    if let Some(journal) = journal {
        let features = session.satellite_features();
        journal.with_mut(|s| s.set_satellite_features(host.clone(), features));
    }
    session.set_operation_fence(operations.clone());
    let (mut reader, writer) = conn.into_parts();
    let (write_tx, write_rx) = tokio::sync::mpsc::channel(LINK_WRITE_QUEUE);
    let queued_write_bytes = Rc::new(Cell::new(0usize));
    let (write_error_tx, mut write_error_rx) = tokio::sync::mpsc::channel(1);
    let _writer_task = AbortOnDrop(tokio::task::spawn_local(drive_link_writer(
        writer,
        write_rx,
        write_error_tx,
    )));
    // First housekeeping tick one interval out.
    let mut keepalive = tokio::time::interval_at(
        tokio::time::Instant::now() + LINK_KEEPALIVE_INTERVAL,
        LINK_KEEPALIVE_INTERVAL,
    );
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut delivery_retry = tokio::time::interval(RELAY_DELIVERY_RETRY_INTERVAL);
    delivery_retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let (lost, reason) = loop {
        tokio::select! {
            () = cancel.cancelled() => break (false, "hub is shutting down".to_owned()),
            error = write_error_rx.recv() => {
                break (true, error.unwrap_or_else(|| "satellite writer stopped".to_owned()));
            }
            request = relay_rx.recv() => {
                let Some(request) = request else {
                    break (false, "relay handles dropped".to_owned());
                };
                let frames = prepare_relay_request(&mut session, request);
                if let Err(error) = queue_link_write(&write_tx, &queued_write_bytes, LinkWrite::Frames(frames)) {
                    break (true, error);
                }
            }
            unsubscribe = unsub_rx.recv() => {
                // Apply the withdrawal and detach upstream where the last
                // proxy left.
                let Some(unsubscribe) = unsubscribe else {
                    break (false, "relay handles dropped".to_owned());
                };
                let frames = session.handle_unsubscribe(unsubscribe);
                if let Err(error) = queue_link_write(&write_tx, &queued_write_bytes, LinkWrite::Frames(frames)) {
                    break (true, error);
                }
            }
            inbound = reader.recv_frame() => {
                if let Err(error) = handle_relay_inbound(
                    &mut reader,
                    inbound,
                    &mut session,
                    &write_tx,
                    &queued_write_bytes,
                ).await {
                    break (true, error);
                }
            },
            _ = keepalive.tick() => {
                if let Err(error) = maintain_relay_session(&mut session) {
                    break (true, error);
                }
                if let Err(error) = queue_link_write(&write_tx, &queued_write_bytes, LinkWrite::Keepalive) {
                    break (true, error);
                }
            }
            _ = delivery_retry.tick() => {
                session.flush_pending_snapshots();
                let detaches = session.take_delivery_detaches();
                if let Err(error) = queue_link_write(&write_tx, &queued_write_bytes, LinkWrite::Frames(detaches)) {
                    break (true, error);
                }
            }
        }
    };
    drop(write_tx);
    session.teardown(&reason);
    lost.then_some(reason)
}

fn negotiated_relay_session<C: LinkConn>(
    host: &SatelliteHost,
    conn: &C,
) -> Result<super::relay::RelaySession, String> {
    let profile = conn.bootstrap_profile()?;
    let limits = conn.bootstrap_limits()?;
    let features = conn.server_features()?;
    let features_ext = conn.server_features_ext()?;
    info!(satellite = %host, ?profile, "hub relay using negotiated bootstrap profile");
    let mut session =
        super::relay::RelaySession::new_negotiated(host.clone(), limits, profile, features);
    session.set_satellite_features_ext(features_ext);
    session.set_incarnation(conn.satellite_incarnation());
    Ok(session)
}

#[allow(
    clippy::future_not_send,
    reason = "called only by the LocalSet-bound hub session with its Rc-local bounded writer"
)]
async fn handle_relay_inbound<R: LinkReader>(
    reader: &mut R,
    inbound: Result<Option<Vec<u8>>, String>,
    session: &mut super::relay::RelaySession,
    write_tx: &tokio::sync::mpsc::Sender<QueuedLinkWrite>,
    queued_write_bytes: &Rc<Cell<usize>>,
) -> Result<(), String> {
    match inbound {
        Ok(Some(frame)) => {
            session.handle_inbound(&frame)?;
            queue_link_write(
                write_tx,
                queued_write_bytes,
                LinkWrite::Frames(session.take_delivery_detaches()),
            )
        }
        Ok(None) => Err("connection closed by satellite".to_owned()),
        Err(error) => {
            if let Some(violation) = reader.take_framing_violation() {
                send_framing_goodbye(write_tx, queued_write_bytes, violation).await;
            }
            Err(error)
        }
    }
}

#[allow(
    clippy::future_not_send,
    reason = "called only by the LocalSet-bound hub session with its Rc-local bounded writer"
)]
async fn send_framing_goodbye(
    write_tx: &tokio::sync::mpsc::Sender<QueuedLinkWrite>,
    queued_write_bytes: &Rc<Cell<usize>>,
    violation: FramingError,
) {
    let (sent, received) = tokio::sync::oneshot::channel();
    if queue_link_write(
        write_tx,
        queued_write_bytes,
        LinkWrite::Final(encode_frame_too_large(violation), sent),
    )
    .is_ok()
    {
        let _ = tokio::time::timeout(LINK_SEND_TIMEOUT, received).await;
    }
}

/// Bound reply retention and retry slow snapshots before probing the transport.
#[allow(
    clippy::future_not_send,
    reason = "ADR-0014: runs on the server's LocalSet inside run_relay_session"
)]
fn maintain_relay_session(session: &mut super::relay::RelaySession) -> Result<(), String> {
    session.check_detach_deadlines()?;
    session.prune_abandoned();
    Ok(())
}

fn prepare_relay_request(
    session: &mut super::relay::RelaySession,
    request: super::relay::RelayRequest,
) -> Vec<Vec<u8>> {
    let mut frames = session.prepare_request(&request);
    if let Some(frame) = session.handle_request_checked(request) {
        frames.push(frame);
    }
    frames
}

enum LinkWrite {
    Frames(Vec<Vec<u8>>),
    Keepalive,
    Final(Vec<u8>, tokio::sync::oneshot::Sender<()>),
}

struct QueuedLinkWrite {
    write: Option<LinkWrite>,
    bytes: usize,
    total: Rc<Cell<usize>>,
}

impl QueuedLinkWrite {
    const fn take_write(&mut self) -> Option<LinkWrite> {
        self.write.take()
    }
}

impl Drop for QueuedLinkWrite {
    fn drop(&mut self) {
        self.total.set(self.total.get().saturating_sub(self.bytes));
    }
}

impl LinkWrite {
    fn encoded_bytes(&self) -> usize {
        match self {
            Self::Frames(frames) => frames.iter().map(Vec::len).sum(),
            Self::Keepalive => 0,
            Self::Final(frame, _) => frame.len(),
        }
    }
}

fn queue_link_write(
    tx: &tokio::sync::mpsc::Sender<QueuedLinkWrite>,
    queued_bytes: &Rc<Cell<usize>>,
    write: LinkWrite,
) -> Result<(), String> {
    if matches!(&write, LinkWrite::Frames(frames) if frames.is_empty()) {
        return Ok(());
    }
    let bytes = write.encoded_bytes();
    let next_bytes = queued_bytes.get().saturating_add(bytes);
    if next_bytes > LINK_WRITE_QUEUE_BYTES {
        return Err(format!(
            "satellite ordered writer queue exceeded {LINK_WRITE_QUEUE_BYTES} bytes; closing link without dropping accepted requests"
        ));
    }
    queued_bytes.set(next_bytes);
    tx.try_send(QueuedLinkWrite {
        write: Some(write),
        bytes,
        total: Rc::clone(queued_bytes),
    })
    .map_err(|error| match error {
        tokio::sync::mpsc::error::TrySendError::Full(_) => {
            "satellite ordered writer queue saturated; closing link without dropping accepted requests".to_owned()
        }
        tokio::sync::mpsc::error::TrySendError::Closed(_) => {
            "satellite ordered writer stopped".to_owned()
        }
    })
}

#[allow(
    clippy::future_not_send,
    reason = "the hub writer runs on ADR-0014's current-thread LocalSet and its byte accounting is Rc-local"
)]
async fn drive_link_writer<W: LinkWriter>(
    mut writer: W,
    mut rx: tokio::sync::mpsc::Receiver<QueuedLinkWrite>,
    errors: tokio::sync::mpsc::Sender<String>,
) {
    while let Some(mut queued) = rx.recv().await {
        // Hold `queued` until the write completes so its bytes stay charged.
        let Some(write) = queued.take_write() else {
            continue;
        };
        let result = match write {
            LinkWrite::Frames(frames) => {
                let mut result = Ok(());
                for frame in frames {
                    if let Err(error) = send_bounded(&mut writer, &frame).await {
                        result = Err(error);
                        break;
                    }
                }
                result
            }
            LinkWrite::Keepalive => tokio::time::timeout(LINK_SEND_TIMEOUT, writer.keepalive())
                .await
                .unwrap_or_else(|_| {
                    Err(format!(
                        "keepalive write to satellite stalled for {}s",
                        LINK_SEND_TIMEOUT.as_secs()
                    ))
                }),
            LinkWrite::Final(frame, sent) => {
                let result = send_bounded(&mut writer, &frame).await;
                if result.is_ok() {
                    let _ = sent.send(());
                }
                result
            }
        };
        if let Err(error) = result {
            let _ = errors.send(error).await;
            return;
        }
    }
}

/// Put one frame on the wire, bounded by [`LINK_SEND_TIMEOUT`].
#[allow(
    clippy::future_not_send,
    reason = "ADR-0014: runs on the server's LocalSet inside run_relay_session"
)]
async fn send_bounded<C: LinkWriter>(conn: &mut C, frame: &[u8]) -> Result<(), String> {
    tokio::time::timeout(LINK_SEND_TIMEOUT, conn.send_frame(frame))
        .await
        .unwrap_or_else(|_elapsed| {
            Err(format!(
                "write to satellite stalled for {}s",
                LINK_SEND_TIMEOUT.as_secs()
            ))
        })
}

/// Complete the mandatory network-transport version handshake before the
/// relay session can send commands.
async fn negotiate_link<C: LinkConn + LinkReader + LinkWriter>(
    conn: &mut C,
) -> Result<NegotiatedBootstrap, String> {
    let offered = hub_link_capabilities();
    let mut encoded = bytes::BytesMut::new();
    FrameKind::Hello {
        client_name: "phux-hub".to_owned(),
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        client_caps: offered,
    }
    .encode(&mut encoded);
    send_bounded(conn, &encoded).await?;

    let response = tokio::time::timeout(LINK_SEND_TIMEOUT, conn.recv_frame())
        .await
        .map_err(|_elapsed| {
            format!(
                "satellite did not answer HELLO within {}s",
                LINK_SEND_TIMEOUT.as_secs()
            )
        })??
        .ok_or_else(|| "satellite closed before HELLO_OK".to_owned())?;
    let (frame, rest) = FrameKind::decode(&response)
        .map_err(|error| format!("decode satellite HELLO response: {error:?}"))?;
    if !rest.is_empty() {
        return Err("satellite HELLO response contained trailing bytes".to_owned());
    }
    match frame {
        FrameKind::HelloOk {
            protocol_major,
            protocol_minor,
            protocol_patch,
            server_caps,
            selected_profile,
            bootstrap_limits,
            server_id,
            ..
        } => {
            validate_link_hello_ok(
                &offered,
                protocol_major,
                protocol_minor,
                protocol_patch,
                selected_profile,
                bootstrap_limits,
            )?;
            Ok(NegotiatedBootstrap {
                profile: selected_profile,
                limits: bootstrap_limits,
                server_features: server_caps.features,
                server_features_ext: server_caps.features_ext,
                server_id: <[u8; 16]>::try_from(server_id.as_slice()).ok(),
            })
        }
        FrameKind::Error { code, message, .. } => {
            Err(format!("satellite rejected HELLO: {code:?}: {message}"))
        }
        other => Err(format!("expected HELLO_OK from satellite, got {other:?}")),
    }
}

/// What the hub offers a satellite: defaults plus L3 (for the relayed
/// `LIST_DIRECTORY`) and the two mirrored agent-metadata keys (ADR-0136).
fn hub_link_capabilities() -> ClientCapabilities {
    ClientCapabilities::default().with_layers(phux_protocol::caps::LayerSet::with(&[
        phux_protocol::caps::Layer::L3,
    ]))
}

fn validate_link_hello_ok(
    offered: &ClientCapabilities,
    protocol_major: u16,
    protocol_minor: u16,
    protocol_patch: u16,
    selected_profile: BootstrapProfile,
    selected_limits: BootstrapLimits,
) -> Result<(), String> {
    if (protocol_major, protocol_minor, protocol_patch)
        != (
            PROTOCOL_VERSION.major,
            PROTOCOL_VERSION.minor,
            PROTOCOL_VERSION.patch,
        )
    {
        return Err(format!(
            "satellite HELLO_OK selected unsupported protocol {protocol_major}.{protocol_minor}.{protocol_patch}",
        ));
    }
    let profile_is_offered = match selected_profile {
        BootstrapProfile::NativeState { codec, features } => {
            offered
                .bootstrap
                .profiles
                .contains(BootstrapProfileKind::NativeState)
                && offered.bootstrap.native_codecs.contains(codec)
                && features.supports_native()
                && offered.bootstrap.native_features.intersect(features) == features
        }
        BootstrapProfile::SynthesizedVtRaw => offered
            .bootstrap
            .profiles
            .contains(BootstrapProfileKind::SynthesizedVtRaw),
        BootstrapProfile::SynthesizedVtStateSync => offered
            .bootstrap
            .profiles
            .contains(BootstrapProfileKind::SynthesizedVtStateSync),
        _ => false,
    };
    if !profile_is_offered {
        return Err(format!(
            "satellite HELLO_OK selected bootstrap profile outside the hub offer: {selected_profile:?}",
        ));
    }
    if offered.bootstrap.limits.intersect(selected_limits) != selected_limits {
        return Err(format!(
            "satellite HELLO_OK selected bootstrap limits outside the hub offer: chunk={} history_page={}",
            selected_limits.max_chunk_bytes(),
            selected_limits.max_history_page_bytes(),
        ));
    }
    Ok(())
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
}

impl HubLinks {
    /// No links yet; [`Self::start`] adds them to `relays`, the registry the
    /// consumer paths route through.
    pub(crate) fn new(
        relays: super::relay::HubRelays,
        cancel: &CancellationToken,
        ssh_program: std::ffi::OsString,
    ) -> Self {
        Self {
            statuses: HubLinkStatuses::default(),
            relays,
            cancel: cancel.clone(),
            transport: NetLinkTransport::new(ssh_program),
            links: BTreeMap::new(),
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
        self.relays.remove(host);
        self.statuses.remove(host);
    }
}

/// The production [`LinkTransport`]: `phux-dial` QUIC/WS plus SSH-stdio.
#[derive(Debug, Clone)]
pub(crate) struct NetLinkTransport {
    /// Program for SSH dials: `ssh`, or `$PHUX_SSH`.
    ssh_program: std::ffi::OsString,
}

impl NetLinkTransport {
    /// Build the production transport dialing SSH satellites with
    /// `ssh_program` (`ssh`, or `$PHUX_SSH` via the server's
    /// [`crate::runtime::ServerEnv`]).
    pub(crate) const fn new(ssh_program: std::ffi::OsString) -> Self {
        Self { ssh_program }
    }
}

/// An established production link.
#[derive(Debug)]
pub(crate) enum NetLinkConn {
    /// QUIC connection plus bidi stream, length-prefixed like UDS.
    Quic {
        /// Owns the UDP socket + I/O driver; must outlive the connection.
        _endpoint: quinn::Endpoint,
        /// The established connection, kept for a clean close.
        _connection: quinn::Connection,
        /// Opened bidi send half (auth preamble already written).
        send: quinn::SendStream,
        /// Opened bidi recv half.
        recv: quinn::RecvStream,
        /// Cancel-safe reassembly buffer.
        buf: bytes::BytesMut,
        /// Exact selection installed once before `connect` returns.
        negotiated: Option<NegotiatedBootstrap>,
    },
    /// WebSocket connection: one binary message is one complete frame.
    Ws {
        /// The established stream.
        ws: Box<phux_dial::ws::Ws>,
        /// Last inbound activity, feeding the WS idle limit.
        last_inbound: std::time::Instant,
        /// Exact selection installed once before `connect` returns.
        negotiated: Option<NegotiatedBootstrap>,
    },
    /// SSH-stdio child whose stdin/stdout carry the UDS framing verbatim;
    /// `kill_on_drop` reaps it on teardown.
    Ssh {
        /// The spawned ssh process; its exit is the link-loss signal.
        child: tokio::process::Child,
        /// Bridged write half — frames land on the remote server's UDS.
        stdin: tokio::process::ChildStdin,
        /// Bridged read half — the satellite's frames, length-prefixed.
        stdout: tokio::process::ChildStdout,
        /// Continuous stderr drainer. An unread fd 2 would fill its pipe and
        /// stall stdout on the shared SSH channel; keeps a bounded tail for
        /// the loss reason.
        stderr_reader: AbortOnDrop<Vec<u8>>,
        /// Cancel-safe reassembly buffer.
        buf: bytes::BytesMut,
        /// Exact selection installed once before `connect` returns.
        negotiated: Option<NegotiatedBootstrap>,
    },
}

pub(crate) enum NetLinkReader {
    Quic {
        _endpoint: quinn::Endpoint,
        recv: quinn::RecvStream,
        buf: bytes::BytesMut,
        framing_violation: Option<FramingError>,
    },
    Ws {
        stream: futures_util::stream::SplitStream<phux_dial::ws::Ws>,
        last_inbound: Arc<Mutex<std::time::Instant>>,
        framing_violation: Option<FramingError>,
    },
    Ssh {
        child: tokio::process::Child,
        stdout: tokio::process::ChildStdout,
        stderr_reader: AbortOnDrop<Vec<u8>>,
        buf: bytes::BytesMut,
        framing_violation: Option<FramingError>,
    },
}

pub(crate) enum NetLinkWriter {
    Quic {
        _connection: quinn::Connection,
        send: quinn::SendStream,
    },
    Ws {
        sink: futures_util::stream::SplitSink<
            phux_dial::ws::Ws,
            tokio_tungstenite::tungstenite::Message,
        >,
        last_inbound: Arc<Mutex<std::time::Instant>>,
    },
    Ssh {
        stdin: tokio::process::ChildStdin,
    },
}

impl NetLinkConn {
    fn install_negotiated(&mut self, selection: NegotiatedBootstrap) -> Result<(), String> {
        let slot = match self {
            Self::Quic { negotiated, .. }
            | Self::Ws { negotiated, .. }
            | Self::Ssh { negotiated, .. } => negotiated,
        };
        if slot.replace(selection).is_some() {
            return Err("satellite HELLO negotiation completed twice".to_owned());
        }
        Ok(())
    }

    fn negotiated(&self) -> Result<NegotiatedBootstrap, String> {
        match self {
            Self::Quic { negotiated, .. }
            | Self::Ws { negotiated, .. }
            | Self::Ssh { negotiated, .. } => negotiated.ok_or_else(|| {
                "satellite connection returned before HELLO negotiation completed".to_owned()
            }),
        }
    }
}

impl LinkTransport for NetLinkTransport {
    type Conn = NetLinkConn;

    async fn connect(&self, spec: &DialSpec, token: Option<String>) -> Result<NetLinkConn, String> {
        let result: Result<NetLinkConn, String> = match spec {
            DialSpec::Quic {
                host, port, trust, ..
            } => {
                let bare = host.trim_matches(['[', ']']);
                let addr = tokio::net::lookup_host((bare, *port))
                    .await
                    .map_err(|err| format!("resolve {host}:{port}: {err}"))?
                    .next()
                    .ok_or_else(|| format!("resolve {host}:{port}: no addresses"))?;
                let token = token
                    .as_deref()
                    .map(phux_dial::quic::parse_token_hex)
                    .transpose()
                    .map_err(|err| err.to_string())?;
                let dial = QuicDial {
                    addr,
                    server_name: bare.to_owned(),
                    token,
                    trust: trust.clone(),
                    identity: None,
                    inner: None,
                };
                let (endpoint, connection, send, recv) = phux_dial::quic::dial(&dial)
                    .await
                    .map_err(|err| err.to_string())?;
                Ok(NetLinkConn::Quic {
                    _endpoint: endpoint,
                    _connection: connection,
                    send,
                    recv,
                    buf: bytes::BytesMut::with_capacity(8192),
                    negotiated: None,
                })
            }
            DialSpec::Ws { url, trust, .. } => {
                let dial = WsDial {
                    url: url.clone(),
                    token,
                    trust: trust.clone(),
                    tls_server_name: None,
                    identity: None,
                };
                let ws = phux_dial::ws::dial(&dial)
                    .await
                    .map_err(|err| err.to_string())?;
                Ok(NetLinkConn::Ws {
                    ws: Box::new(ws),
                    last_inbound: std::time::Instant::now(),
                    negotiated: None,
                })
            }
            DialSpec::Ssh { user, host, port } => {
                // SSH authenticates the channel; there is no token.
                debug_assert!(token.is_none(), "ssh dials carry no bearer token");
                let mut child = tokio::process::Command::new(&self.ssh_program)
                    .args(ssh_argv(user.as_deref(), host, *port))
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|err| {
                        format!("spawn {}: {err}", self.ssh_program.to_string_lossy())
                    })?;
                // Missing pipes are a failed dial, not a panic.
                let stdin = child.stdin.take().ok_or("ssh child has no stdin pipe")?;
                let stdout = child.stdout.take().ok_or("ssh child has no stdout pipe")?;
                let mut stderr = child.stderr.take().ok_or("ssh child has no stderr pipe")?;
                // Drain fd 2 for the link's life (see `stderr_reader`).
                let stderr_reader = AbortOnDrop(tokio::task::spawn_local(async move {
                    read_tail(&mut stderr, SSH_STDERR_TAIL_MAX).await
                }));
                Ok(NetLinkConn::Ssh {
                    child,
                    stdin,
                    stdout,
                    stderr_reader,
                    buf: bytes::BytesMut::with_capacity(8192),
                    negotiated: None,
                })
            }
        };
        let mut conn = result?;

        // HELLO/profile negotiation is mandatory on every transport,
        // including the SSH bridge.
        let selection = negotiate_link(&mut conn).await?;
        conn.install_negotiated(selection)?;
        Ok(conn)
    }
}

impl LinkConn for NetLinkConn {
    type Reader = NetLinkReader;
    type Writer = NetLinkWriter;

    fn into_parts(self) -> (Self::Reader, Self::Writer) {
        match self {
            Self::Quic {
                _endpoint: endpoint,
                _connection: connection,
                send,
                recv,
                buf,
                ..
            } => (
                NetLinkReader::Quic {
                    _endpoint: endpoint,
                    recv,
                    buf,
                    framing_violation: None,
                },
                NetLinkWriter::Quic {
                    _connection: connection,
                    send,
                },
            ),
            Self::Ws {
                ws, last_inbound, ..
            } => {
                let (sink, stream) = futures_util::StreamExt::split(*ws);
                let last_inbound = Arc::new(Mutex::new(last_inbound));
                (
                    NetLinkReader::Ws {
                        stream,
                        last_inbound: Arc::clone(&last_inbound),
                        framing_violation: None,
                    },
                    NetLinkWriter::Ws { sink, last_inbound },
                )
            }
            Self::Ssh {
                child,
                stdin,
                stdout,
                stderr_reader,
                buf,
                ..
            } => (
                NetLinkReader::Ssh {
                    child,
                    stdout,
                    stderr_reader,
                    buf,
                    framing_violation: None,
                },
                NetLinkWriter::Ssh { stdin },
            ),
        }
    }

    fn bootstrap_limits(&self) -> Result<BootstrapLimits, String> {
        self.negotiated().map(|selection| selection.limits)
    }

    fn bootstrap_profile(&self) -> Result<BootstrapProfile, String> {
        self.negotiated().map(|selection| selection.profile)
    }

    fn server_features(&self) -> Result<phux_protocol::caps::ServerFeatureSet, String> {
        self.negotiated().map(|selection| selection.server_features)
    }

    fn server_features_ext(&self) -> Result<ServerFeatureExtSet, String> {
        self.negotiated()
            .map(|selection| selection.server_features_ext)
    }

    fn satellite_incarnation(&self) -> Option<[u8; 16]> {
        self.negotiated()
            .ok()
            .and_then(|selection| selection.server_id)
    }
}

impl LinkWriter for NetLinkConn {
    async fn send_frame(&mut self, frame: &[u8]) -> Result<(), String> {
        match self {
            Self::Quic { send, .. } => send
                .write_all(frame)
                .await
                .map_err(|err| format!("write to satellite: {err}")),
            Self::Ws { ws, .. } => futures_util::SinkExt::send(
                ws.as_mut(),
                tokio_tungstenite::tungstenite::Message::Binary(frame.to_vec().into()),
            )
            .await
            .map_err(|err| format!("write to satellite: {err}")),
            // The child's stdin is the wire, bounded like any other.
            Self::Ssh { stdin, .. } => tokio::io::AsyncWriteExt::write_all(stdin, frame)
                .await
                .map_err(|err| format!("write to ssh transport: {err}")),
        }
    }

    async fn keepalive(&mut self) -> Result<(), String> {
        match self {
            Self::Quic { .. } | Self::Ssh { .. } => Ok(()),
            Self::Ws {
                ws, last_inbound, ..
            } => {
                if let Some(reason) = ws_idle_error(last_inbound.elapsed()) {
                    return Err(reason);
                }
                futures_util::SinkExt::send(
                    ws.as_mut(),
                    tokio_tungstenite::tungstenite::Message::Ping(Vec::new().into()),
                )
                .await
                .map_err(|err| format!("keepalive ping to satellite: {err}"))
            }
        }
    }
}

impl LinkReader for NetLinkConn {
    async fn recv_frame(&mut self) -> Result<Option<Vec<u8>>, String> {
        // A framing violation is carried out of the match (whose arms
        // borrow `self`) so the goodbye has one send path for every
        // transport.
        let violation = 'framing: {
            match self {
                Self::Quic { recv, buf, .. } => {
                    // Cancel-safe: bytes land in the persistent buffer.
                    loop {
                        match phux_protocol::wire::framing::split_frame(buf) {
                            Ok(Some(framed)) => return Ok(Some(framed.to_vec())),
                            Ok(None) => {}
                            Err(violation) => break 'framing violation,
                        }
                        let n = tokio::io::AsyncReadExt::read_buf(recv, buf)
                            .await
                            .map_err(|err| format!("read from satellite: {err}"))?;
                        if n == 0 {
                            if buf.is_empty() {
                                return Ok(None);
                            }
                            return Err("satellite closed the stream mid-frame".to_owned());
                        }
                    }
                }
                Self::Ws {
                    ws, last_inbound, ..
                } => loop {
                    match futures_util::StreamExt::next(ws.as_mut()).await {
                        None => return Ok(None),
                        Some(Ok(message)) => {
                            // Any inbound message counts as liveness.
                            *last_inbound = std::time::Instant::now();
                            match message {
                                tokio_tungstenite::tungstenite::Message::Close(_) => {
                                    return Ok(None);
                                }
                                tokio_tungstenite::tungstenite::Message::Binary(data) => {
                                    // One binary message is one frame (SPEC §5).
                                    if let Err(violation) =
                                        phux_protocol::wire::framing::check_frame(&data)
                                    {
                                        break 'framing violation;
                                    }
                                    return Ok(Some(data.to_vec()));
                                }
                                // Ping/pong: reading answers pings.
                                _ => {}
                            }
                        }
                        Some(Err(err)) => return Err(format!("connection error: {err}")),
                    }
                },
                Self::Ssh {
                    child,
                    stdout,
                    stderr_reader,
                    buf,
                    ..
                } => {
                    // Same reassembly as QUIC. stdout EOF means the link is
                    // gone; the exit status and stderr tail are the reason.
                    loop {
                        match phux_protocol::wire::framing::split_frame(buf) {
                            Ok(Some(framed)) => return Ok(Some(framed.to_vec())),
                            Ok(None) => {}
                            Err(violation) => break 'framing violation,
                        }
                        let n = tokio::io::AsyncReadExt::read_buf(stdout, buf)
                            .await
                            .map_err(|err| format!("read from ssh transport: {err}"))?;
                        if n == 0 {
                            let mut reason = ssh_exit_reason(child, stderr_reader).await;
                            if !buf.is_empty() {
                                reason = format!("{reason} (stream ended mid-frame)");
                            }
                            return Err(reason);
                        }
                    }
                }
            }
        };
        // Best-effort SPEC §5 goodbye; never masks the real loss reason.
        let _ = send_bounded(self, &encode_frame_too_large(violation)).await;
        Err(framing_loss_reason(violation))
    }
}

impl LinkWriter for NetLinkWriter {
    async fn send_frame(&mut self, frame: &[u8]) -> Result<(), String> {
        match self {
            Self::Quic { send, .. } => tokio::io::AsyncWriteExt::write_all(send, frame)
                .await
                .map_err(|err| format!("write to satellite: {err}")),
            Self::Ws { sink, .. } => futures_util::SinkExt::send(
                sink,
                tokio_tungstenite::tungstenite::Message::Binary(frame.to_vec().into()),
            )
            .await
            .map_err(|err| format!("write to satellite: {err}")),
            Self::Ssh { stdin } => tokio::io::AsyncWriteExt::write_all(stdin, frame)
                .await
                .map_err(|err| format!("write to ssh transport: {err}")),
        }
    }

    async fn keepalive(&mut self) -> Result<(), String> {
        match self {
            Self::Quic { .. } | Self::Ssh { .. } => Ok(()),
            Self::Ws {
                sink, last_inbound, ..
            } => {
                let idle_for = last_inbound
                    .lock()
                    .map_err(|_| "satellite liveness clock poisoned".to_owned())?
                    .elapsed();
                if let Some(reason) = ws_idle_error(idle_for) {
                    return Err(reason);
                }
                futures_util::SinkExt::send(
                    sink,
                    tokio_tungstenite::tungstenite::Message::Ping(Vec::new().into()),
                )
                .await
                .map_err(|err| format!("keepalive ping to satellite: {err}"))
            }
        }
    }
}

impl LinkReader for NetLinkReader {
    async fn recv_frame(&mut self) -> Result<Option<Vec<u8>>, String> {
        match self {
            Self::Quic {
                recv,
                buf,
                framing_violation,
                ..
            } => read_length_prefixed(recv, buf, framing_violation, "satellite").await,
            Self::Ws {
                stream,
                last_inbound,
                framing_violation,
            } => loop {
                match futures_util::StreamExt::next(stream).await {
                    None => return Ok(None),
                    Some(Ok(message)) => {
                        *last_inbound
                            .lock()
                            .map_err(|_| "satellite liveness clock poisoned".to_owned())? =
                            std::time::Instant::now();
                        match message {
                            tokio_tungstenite::tungstenite::Message::Close(_) => return Ok(None),
                            tokio_tungstenite::tungstenite::Message::Binary(data) => {
                                if let Err(violation) =
                                    phux_protocol::wire::framing::check_frame(&data)
                                {
                                    *framing_violation = Some(violation);
                                    return Err(framing_loss_reason(violation));
                                }
                                return Ok(Some(data.to_vec()));
                            }
                            _ => {}
                        }
                    }
                    Some(Err(err)) => return Err(format!("connection error: {err}")),
                }
            },
            Self::Ssh {
                child,
                stdout,
                stderr_reader,
                buf,
                framing_violation,
            } => {
                match read_length_prefixed(stdout, buf, framing_violation, "ssh transport").await {
                    Ok(None) => Err(ssh_exit_reason(child, stderr_reader).await),
                    other => other,
                }
            }
        }
    }

    fn take_framing_violation(&mut self) -> Option<FramingError> {
        match self {
            Self::Quic {
                framing_violation, ..
            }
            | Self::Ws {
                framing_violation, ..
            }
            | Self::Ssh {
                framing_violation, ..
            } => framing_violation.take(),
        }
    }
}

async fn read_length_prefixed<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    buf: &mut bytes::BytesMut,
    framing_violation: &mut Option<FramingError>,
    source: &str,
) -> Result<Option<Vec<u8>>, String> {
    loop {
        match phux_protocol::wire::framing::split_frame(buf) {
            Ok(Some(framed)) => return Ok(Some(framed.to_vec())),
            Ok(None) => {}
            Err(violation) => {
                *framing_violation = Some(violation);
                return Err(framing_loss_reason(violation));
            }
        }
        let n = tokio::io::AsyncReadExt::read_buf(reader, buf)
            .await
            .map_err(|err| format!("read from {source}: {err}"))?;
        if n == 0 {
            return if buf.is_empty() {
                Ok(None)
            } else {
                Err(format!("{source} closed the stream mid-frame"))
            };
        }
    }
}

/// The teardown reason once a WS link has been idle for
/// [`LINK_IDLE_TIMEOUT`].
fn ws_idle_error(idle_for: Duration) -> Option<String> {
    (idle_for >= LINK_IDLE_TIMEOUT).then(|| {
        format!(
            "satellite sent nothing for {}s (idle limit {}s); link presumed dead",
            idle_for.as_secs(),
            LINK_IDLE_TIMEOUT.as_secs()
        )
    })
}

/// How long an ssh child gets to exit after its stdout closes before the
/// hub kills it.
const SSH_EXIT_GRACE: Duration = Duration::from_secs(5);

/// A local task handle that aborts on drop (the stderr drainer).
#[derive(Debug)]
pub(crate) struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The loss reason for an ssh link at stdout EOF: exit status plus the
/// bounded stderr tail. Wait and join are bounded by [`SSH_EXIT_GRACE`];
/// a lingering child is killed.
async fn ssh_exit_reason(
    child: &mut tokio::process::Child,
    stderr_reader: &mut AbortOnDrop<Vec<u8>>,
) -> String {
    let gathered = tokio::time::timeout(SSH_EXIT_GRACE, async {
        // The drainer finishes at process death, so joining yields the
        // complete tail. `Pin::new` is sound: `JoinHandle` is `Unpin`.
        tokio::join!(child.wait(), std::pin::Pin::new(&mut stderr_reader.0))
    })
    .await;
    match gathered {
        Ok((status, diagnostics)) => {
            let diagnostics = diagnostics.unwrap_or_default();
            let tail = String::from_utf8_lossy(&diagnostics);
            let tail = tail.trim();
            match status {
                Ok(status) if tail.is_empty() => format!("ssh transport ended: {status}"),
                Ok(status) => format!("ssh transport ended: {status}: {tail}"),
                Err(err) => format!("wait for ssh transport: {err}"),
            }
        }
        Err(_elapsed) => {
            let _ = child.start_kill();
            format!(
                "ssh transport closed its stream but the child did not exit within {}s; killed",
                SSH_EXIT_GRACE.as_secs()
            )
        }
    }
}

/// Compose the loss reason for a satellite that broke SPEC §5 framing.
fn framing_loss_reason(violation: FramingError) -> String {
    format!("satellite sent a malformed frame: {violation}")
}

/// The SPEC §5 `ERROR { FRAME_TOO_LARGE }` goodbye, shared with the
/// server's client loop.
fn encode_frame_too_large(violation: FramingError) -> Vec<u8> {
    let mut encoded = bytes::BytesMut::new();
    phux_protocol::wire::framing::frame_too_large_error(violation).encode(&mut encoded);
    encoded.to_vec()
}

/// Drain `reader` to EOF (or error), keeping only the last `max` bytes.
async fn read_tail<R>(reader: &mut R, max: usize) -> Vec<u8>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut tail = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => return tail,
            Ok(n) => {
                tail.extend_from_slice(&chunk[..n]);
                if tail.len() > max {
                    tail.drain(..tail.len() - max);
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

    use super::*;

    /// Revoking the link's token on the satellite ends the link live and
    /// refuses the redial (`docs/spec/workload-auth.md` §7).
    #[tokio::test(flavor = "current_thread")]
    async fn revoking_the_links_token_on_the_satellite_drops_the_link() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let tokens = dir.path().join("satellite-tokens");
                let secret = [0x5a; crate::auth::TOKEN_LEN];
                crate::auth::write_test_credential(&tokens, &secret);
                let store = std::sync::Arc::new(
                    crate::auth::ReloadingTokenStore::load(tokens.clone()).expect("store"),
                );
                let listener = crate::transport::WsListener::loopback_with_tokens(store)
                    .await
                    .expect("bind satellite");
                let port = listener.local_addr().expect("addr").port();
                let satellite = crate::state::SharedState::new();
                let satellite_root = CancellationToken::new();
                let accept_state = satellite.clone();
                let accept_root = satellite_root.clone();
                tokio::task::spawn_local(async move {
                    crate::runtime::client::accept_loop(&listener, accept_state, accept_root, None)
                        .await
                });
                crate::runtime::revocation::spawn_revocation_watcher(&satellite, &satellite_root);

                let token_file = dir.path().join("link.token");
                std::fs::write(&token_file, hex::encode(secret)).expect("write link token");
                let statuses = HubLinkStatuses::default();
                let cancel = CancellationToken::new();
                let host = host();
                let (_relay, relay_rx) = relay_pair(&host);
                tokio::task::spawn_local(run_link(
                    host.clone(),
                    entry(
                        &format!("ws://127.0.0.1:{port}"),
                        Some(token_file.to_str().expect("utf8 path")),
                        None,
                    ),
                    NetLinkTransport::new("ssh".into()),
                    statuses.clone(),
                    relay_rx,
                    cancel.child_token(),
                ));
                wait_for_status(&statuses, &host, |s| *s == LinkStatus::Connected).await;

                crate::auth::revoke_credential(&tokens, "test-credential").expect("revoke");
                wait_for_status(&statuses, &host, |s| {
                    matches!(s, LinkStatus::Backoff { .. })
                })
                .await;
                // The redial presents the revoked token and is refused.
                tokio::time::sleep(Duration::from_millis(1500)).await;
                assert_ne!(
                    statuses.get(&host),
                    Some(LinkStatus::Connected),
                    "a revoked link token must not reconnect"
                );

                cancel.cancel();
                satellite_root.cancel();
            })
            .await;
    }

    fn entry(endpoint: &str, token_file: Option<&str>, cert_fingerprint: Option<&str>) -> HubEntry {
        HubEntry {
            target: super::super::parse_endpoint(endpoint).expect("valid endpoint"),
            token_file: token_file.map(PathBuf::from),
            cert_fingerprint: cert_fingerprint.map(str::to_owned),
        }
    }

    // --- dial-target selection ----------------------------------------

    #[test]
    fn quic_plan_keeps_host_port_and_pins_routable() {
        let spec = plan_link(&entry(
            "quic://devbox:8788",
            Some("/secrets/devbox.token"),
            Some("AB:CD"),
        ))
        .expect("dialable");
        assert_eq!(
            spec,
            DialSpec::Quic {
                host: "devbox".to_owned(),
                port: 8788,
                trust: CertTrust::Pinned("AB:CD".to_owned()),
                token_file: Some(PathBuf::from("/secrets/devbox.token")),
            }
        );
    }

    #[test]
    fn wss_plan_keeps_url_and_pins_routable() {
        let spec = plan_link(&entry(
            "wss://sandbox:8787/phux",
            Some("/secrets/sandbox.token"),
            Some("ab:cd"),
        ))
        .expect("dialable");
        assert_eq!(
            spec,
            DialSpec::Ws {
                url: "wss://sandbox:8787/phux".to_owned(),
                trust: CertTrust::Pinned("ab:cd".to_owned()),
                token_file: Some(PathBuf::from("/secrets/sandbox.token")),
            }
        );
    }

    #[test]
    fn loopback_plans_skip_verify_without_a_pin() {
        for endpoint in [
            "quic://127.0.0.1:8788",
            "quic://[::1]:8788",
            "quic://localhost:8788",
            "ws://127.0.0.1:8787",
            "wss://localhost:8787",
        ] {
            let spec = plan_link(&entry(endpoint, None, None)).expect("loopback carve-out");
            let trust = match spec {
                DialSpec::Quic { trust, .. } | DialSpec::Ws { trust, .. } => trust,
                DialSpec::Ssh { .. } => unreachable!("no ssh endpoints in this matrix"),
            };
            assert_eq!(trust, CertTrust::SkipVerify, "{endpoint}");
        }
    }

    #[test]
    fn loopback_with_a_pin_still_pins() {
        let spec = plan_link(&entry("wss://127.0.0.1:8787", None, Some("AB"))).expect("dialable");
        assert!(matches!(spec, DialSpec::Ws { trust: CertTrust::Pinned(pin), .. } if pin == "AB"));
    }

    // --- fail-closed matrix ---------------------------------------------

    #[test]
    fn routable_endpoints_fail_closed_without_token_or_pin() {
        // (endpoint, token, pin) -> expected refusal
        let matrix: &[(&str, Option<&str>, Option<&str>)] = &[
            ("quic://devbox:8788", None, None),
            ("quic://devbox:8788", Some("/t"), None),
            ("quic://devbox:8788", None, Some("AB")),
            ("wss://devbox:8787", None, None),
            ("wss://devbox:8787", Some("/t"), None),
            ("wss://devbox:8787", None, Some("AB")),
        ];
        for (endpoint, token, pin) in matrix {
            let refusal = plan_link(&entry(endpoint, *token, *pin)).expect_err("must fail closed");
            match (token, pin) {
                (_, None) => assert!(
                    matches!(refusal, LinkRefusal::MissingFingerprint { .. }),
                    "{endpoint} token={token:?} pin={pin:?}: {refusal:?}"
                ),
                (None, Some(_)) => assert!(
                    matches!(refusal, LinkRefusal::MissingToken { .. }),
                    "{endpoint} token={token:?} pin={pin:?}: {refusal:?}"
                ),
                (Some(_), Some(_)) => unreachable!("dialable rows are not in the matrix"),
            }
        }
    }

    #[test]
    fn plaintext_ws_to_routable_host_is_refused_even_with_credentials() {
        let refusal = plan_link(&entry("ws://devbox:8787", Some("/t"), Some("AB")))
            .expect_err("plaintext routable");
        assert!(matches!(refusal, LinkRefusal::PlaintextRoutable { .. }));
    }

    // --- ssh-stdio planning and argv (phux-v45.9) ------------------------

    #[test]
    fn ssh_plan_needs_no_credentials_and_carries_none() {
        // Dialable with no auth material.
        let spec = plan_link(&entry("ssh://me@devbox:2222", None, None)).expect("dialable");
        assert_eq!(
            spec,
            DialSpec::Ssh {
                user: Some("me".to_owned()),
                host: "devbox".to_owned(),
                port: Some(2222),
            }
        );
        assert_eq!(spec.token_file(), None);

        // A configured token/pin on an ssh entry is ignored.
        let spec =
            plan_link(&entry("ssh://devbox", Some("/t"), Some("AB"))).expect("still dialable");
        assert_eq!(spec.token_file(), None, "{spec:?}");
    }

    #[test]
    fn ssh_argv_matrix_is_shell_free_and_option_injection_proof() {
        assert_eq!(
            ssh_argv(None, "devbox", None),
            vec![
                "-o",
                "BatchMode=yes",
                "-o",
                "ClearAllForwardings=yes",
                "-o",
                "ServerAliveInterval=10",
                "-o",
                "ServerAliveCountMax=3",
                "-T",
                "--",
                "devbox",
                "phux",
                "stdio-bridge",
            ]
        );
        assert_eq!(
            ssh_argv(Some("me"), "devbox", Some(2222)),
            vec![
                "-o",
                "BatchMode=yes",
                "-o",
                "ClearAllForwardings=yes",
                "-o",
                "ServerAliveInterval=10",
                "-o",
                "ServerAliveCountMax=3",
                "-T",
                "-p",
                "2222",
                "-l",
                "me",
                "--",
                "devbox",
                "phux",
                "stdio-bridge",
            ]
        );
        // interval * count == LINK_IDLE_TIMEOUT.
        assert_eq!(
            LINK_KEEPALIVE_INTERVAL * 3,
            LINK_IDLE_TIMEOUT,
            "keepalive constants drifted; re-derive ServerAlive* expectations"
        );
        // The host always follows `--`.
        let argv = ssh_argv(Some("me"), "devbox", Some(22));
        let dashdash = argv.iter().position(|a| a == "--").expect("has --");
        assert_eq!(argv[dashdash + 1], "devbox");
    }

    #[test]
    fn quic_loopback_literals_are_recognized() {
        assert!(host_is_loopback("127.0.0.1"));
        assert!(host_is_loopback("[::1]"));
        assert!(host_is_loopback("LOCALHOST"));
        assert!(!host_is_loopback("devbox"));
        assert!(!host_is_loopback("10.0.0.7"));
        assert!(!host_is_loopback("[2001:db8::1]"));
    }

    // --- token file -----------------------------------------------------

    #[test]
    fn token_file_first_nonempty_line_wins_and_hex_is_enforced() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("satellite.token");

        std::fs::write(&path, "deadbeef\n").expect("write");
        assert_eq!(
            read_link_token(Some(&path)).expect("valid"),
            Some("deadbeef".to_owned())
        );

        std::fs::write(&path, "\n  \ndeadbeef\nc0ffee\n").expect("write");
        assert_eq!(
            read_link_token(Some(&path)).expect("line-oriented"),
            Some("deadbeef".to_owned())
        );

        std::fs::write(&path, "not hex!\n").expect("write");
        let err = read_link_token(Some(&path)).expect_err("not hex");
        assert!(err.contains("valid hex"), "{err}");

        std::fs::write(&path, "\n\n").expect("write");
        let err = read_link_token(Some(&path)).expect_err("empty");
        assert!(err.contains("empty"), "{err}");

        let missing = dir.path().join("nope.token");
        let err = read_link_token(Some(&missing)).expect_err("missing");
        assert!(err.contains("read token file"), "{err}");

        assert_eq!(read_link_token(None).expect("no file configured"), None);
    }

    // --- stderr tail ------------------------------------------------------

    #[tokio::test]
    async fn read_tail_is_bounded_and_keeps_the_end() {
        // Only the final `max` bytes survive.
        let mut input = vec![b'x'; 1024 * 1024];
        input.extend_from_slice(b"END-MARKER");
        let mut reader = input.as_slice();
        let tail = read_tail(&mut reader, SSH_STDERR_TAIL_MAX).await;
        assert_eq!(tail.len(), SSH_STDERR_TAIL_MAX);
        assert!(tail.ends_with(b"END-MARKER"));

        // Short input passes through untouched.
        let mut reader = &b"auth refused\n"[..];
        assert_eq!(
            read_tail(&mut reader, SSH_STDERR_TAIL_MAX).await,
            b"auth refused\n"
        );
    }

    // --- backoff ----------------------------------------------------------

    #[test]
    fn backoff_doubles_from_base_and_caps() {
        let mut backoff = Backoff::new(Duration::from_millis(500), Duration::from_secs(30));
        let mut delays = Vec::new();
        for _ in 0..9 {
            delays.push(backoff.next_delay());
        }
        assert_eq!(
            delays,
            vec![
                Duration::from_millis(500),
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(16),
                Duration::from_secs(30),
                Duration::from_secs(30),
                Duration::from_secs(30),
            ]
        );
    }

    #[test]
    fn backoff_reset_returns_to_base() {
        let mut backoff = Backoff::new(Duration::from_millis(500), Duration::from_secs(30));
        let _ = backoff.next_delay();
        let _ = backoff.next_delay();
        backoff.reset();
        assert_eq!(backoff.failures(), 0);
        assert_eq!(backoff.next_delay(), Duration::from_millis(500));
    }

    #[test]
    fn backoff_never_overflows_at_large_failure_counts() {
        let mut backoff = Backoff::new(Duration::from_millis(500), Duration::from_secs(30));
        for _ in 0..10_000 {
            assert!(backoff.next_delay() <= Duration::from_secs(30));
        }
    }

    // --- supervisor (scripted transport, no network) ---------------------

    /// Scripted transport: pops the next result per connect call.
    #[derive(Clone)]
    struct ScriptTransport {
        script: Rc<RefCell<VecDeque<Result<ScriptConn, String>>>>,
        calls: Rc<Cell<u32>>,
        seen_tokens: Rc<RefCell<Vec<Option<String>>>>,
    }

    impl ScriptTransport {
        fn new(script: Vec<Result<ScriptConn, String>>) -> Self {
            Self {
                script: Rc::new(RefCell::new(script.into())),
                calls: Rc::new(Cell::new(0)),
                seen_tokens: Rc::new(RefCell::new(Vec::new())),
            }
        }
    }

    /// A scripted connection that drops with the reason sent on `closed`.
    struct ScriptConn {
        closed: Option<tokio::sync::mpsc::Receiver<String>>,
        keepalive_error: Option<String>,
    }

    struct ScriptReader {
        closed: Option<tokio::sync::mpsc::Receiver<String>>,
    }

    struct ScriptWriter {
        keepalive_error: Option<String>,
    }

    struct BlockedWriteConn {
        inbound: tokio::sync::mpsc::Receiver<Vec<u8>>,
        write_started: Arc<tokio::sync::Notify>,
        release_write: Arc<tokio::sync::Notify>,
        writer_dropped: Arc<std::sync::atomic::AtomicBool>,
    }

    struct BlockedWriteReader {
        inbound: tokio::sync::mpsc::Receiver<Vec<u8>>,
    }

    struct BlockedWriteWriter {
        write_started: Arc<tokio::sync::Notify>,
        release_write: Arc<tokio::sync::Notify>,
        dropped: Arc<std::sync::atomic::AtomicBool>,
    }

    impl ScriptConn {
        /// A connection that stays up for the whole test.
        const fn open_forever() -> Self {
            Self {
                closed: None,
                keepalive_error: None,
            }
        }

        /// A connection the test can drop by sending a reason.
        fn closable() -> (tokio::sync::mpsc::Sender<String>, Self) {
            let (tx, rx) = tokio::sync::mpsc::channel(1);
            (
                tx,
                Self {
                    closed: Some(rx),
                    keepalive_error: None,
                },
            )
        }

        /// Never closes, but its keepalive reports the link dead.
        fn keepalive_fails(reason: &str) -> Self {
            Self {
                closed: None,
                keepalive_error: Some(reason.to_owned()),
            }
        }
    }

    impl LinkTransport for ScriptTransport {
        type Conn = ScriptConn;

        #[allow(
            clippy::future_not_send,
            clippy::unused_async_trait_impl,
            reason = "test transport is Rc-scripted and driven on a LocalSet; its trait methods remain async to match production"
        )]
        async fn connect(
            &self,
            _spec: &DialSpec,
            token: Option<String>,
        ) -> Result<ScriptConn, String> {
            self.calls.set(self.calls.get() + 1);
            self.seen_tokens.borrow_mut().push(token);
            self.script
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| Err("script exhausted".to_owned()))
        }
    }

    #[allow(
        clippy::unused_async_trait_impl,
        reason = "the scripted test connection implements the production async transport trait without I/O"
    )]
    impl LinkConn for ScriptConn {
        type Reader = ScriptReader;
        type Writer = ScriptWriter;

        fn into_parts(self) -> (Self::Reader, Self::Writer) {
            (
                ScriptReader {
                    closed: self.closed,
                },
                ScriptWriter {
                    keepalive_error: self.keepalive_error,
                },
            )
        }

        fn bootstrap_limits(&self) -> Result<BootstrapLimits, String> {
            Ok(BootstrapLimits::default())
        }

        fn bootstrap_profile(&self) -> Result<BootstrapProfile, String> {
            Ok(BootstrapProfile::SynthesizedVtRaw)
        }

        fn server_features(&self) -> Result<phux_protocol::caps::ServerFeatureSet, String> {
            Ok(phux_protocol::caps::ServerFeatureSet::with(&[
                phux_protocol::caps::ServerFeature::ListDirectory,
            ]))
        }
    }

    impl LinkConn for BlockedWriteConn {
        type Reader = BlockedWriteReader;
        type Writer = BlockedWriteWriter;

        fn into_parts(self) -> (Self::Reader, Self::Writer) {
            (
                BlockedWriteReader {
                    inbound: self.inbound,
                },
                BlockedWriteWriter {
                    write_started: self.write_started,
                    release_write: self.release_write,
                    dropped: self.writer_dropped,
                },
            )
        }

        fn bootstrap_limits(&self) -> Result<BootstrapLimits, String> {
            Ok(BootstrapLimits::default())
        }

        fn bootstrap_profile(&self) -> Result<BootstrapProfile, String> {
            Ok(BootstrapProfile::SynthesizedVtRaw)
        }

        fn server_features(&self) -> Result<phux_protocol::caps::ServerFeatureSet, String> {
            Ok(phux_protocol::caps::ServerFeatureSet::with(&[
                phux_protocol::caps::ServerFeature::ListDirectory,
            ]))
        }
    }

    impl LinkReader for BlockedWriteReader {
        async fn recv_frame(&mut self) -> Result<Option<Vec<u8>>, String> {
            Ok(self.inbound.recv().await)
        }
    }

    #[allow(
        clippy::unused_async_trait_impl,
        reason = "test writer implements the production async transport trait"
    )]
    impl LinkWriter for BlockedWriteWriter {
        async fn send_frame(&mut self, _frame: &[u8]) -> Result<(), String> {
            self.write_started.notify_waiters();
            self.release_write.notified().await;
            Ok(())
        }

        async fn keepalive(&mut self) -> Result<(), String> {
            Ok(())
        }
    }

    impl Drop for BlockedWriteWriter {
        fn drop(&mut self) {
            self.dropped
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl LinkReader for ScriptReader {
        async fn recv_frame(&mut self) -> Result<Option<Vec<u8>>, String> {
            match &mut self.closed {
                Some(rx) => rx.recv().await.map_or(Ok(None), Err),
                None => std::future::pending().await,
            }
        }
    }

    #[allow(
        clippy::unused_async_trait_impl,
        reason = "scripted test writer implements the production async transport trait"
    )]
    impl LinkWriter for ScriptWriter {
        async fn send_frame(&mut self, _frame: &[u8]) -> Result<(), String> {
            Ok(())
        }

        async fn keepalive(&mut self) -> Result<(), String> {
            self.keepalive_error.clone().map_or(Ok(()), Err)
        }
    }

    #[test]
    fn active_write_bytes_remain_charged_against_the_aggregate_bound() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let total = Rc::new(Cell::new(0));
        let active_bytes = LINK_WRITE_QUEUE_BYTES / 2 + 1;
        queue_link_write(&tx, &total, LinkWrite::Frames(vec![vec![0; active_bytes]]))
            .expect("first write fits");
        let mut active = rx.try_recv().expect("queued write");
        let active_write = active.take_write().expect("active write");
        assert_eq!(total.get(), active_bytes, "active bytes stay reserved");

        let error = queue_link_write(&tx, &total, LinkWrite::Frames(vec![vec![0; active_bytes]]))
            .expect_err("active plus queued bytes exceed the aggregate bound");
        assert!(error.contains("exceeded"));

        drop(active_write);
        drop(active);
        assert_eq!(total.get(), 0);
    }

    #[tokio::test]
    async fn blocked_outbound_write_does_not_stall_inbound_fanout() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let host = host();
                let (relay, mailbox) = relay_pair(&host);
                let (inbound_tx, inbound_rx) = tokio::sync::mpsc::channel(1);
                let write_started = Arc::new(tokio::sync::Notify::new());
                let release_write = Arc::new(tokio::sync::Notify::new());
                let writer_dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let conn = BlockedWriteConn {
                    inbound: inbound_rx,
                    write_started: Arc::clone(&write_started),
                    release_write: Arc::clone(&release_write),
                    writer_dropped,
                };
                let cancel = CancellationToken::new();
                let session_host = host.clone();
                let session_cancel = cancel.clone();
                let session = tokio::task::spawn_local(async move {
                    let super::super::relay::RelayMailbox {
                        mut requests,
                        mut unsubscribes,
                        ..
                    } = mailbox;
                    run_relay_session(
                        &session_host,
                        conn,
                        &mut requests,
                        &mut unsubscribes,
                        &session_cancel,
                        None,
                        &super::super::operation_fence::OperationFence::default(),
                    )
                    .await
                });
                let (out_tx, mut out_rx) = tokio::sync::mpsc::channel(4);
                let started = write_started.notified();
                tokio::pin!(started);
                relay.subscribe(
                    super::super::relay::ProxySubscription {
                        terminal: 9,
                        client: crate::state::ClientId(7),
                        out_tx,
                        consumer_cancel: CancellationToken::new(),
                        seq: 0,
                        awaits_snapshot: false,
                        bootstrap_profile: None,
                        bootstrap_limits: None,
                    },
                    FrameKind::SubscribeEvents {
                        terminal: Some(phux_protocol::ResourceId::local(9)),
                        after_seq: None,
                    },
                );
                tokio::time::timeout(Duration::from_secs(1), &mut started)
                    .await
                    .expect("the ordered writer entered its blocked send");

                let mut encoded = bytes::BytesMut::new();
                FrameKind::Event {
                    terminal: Some(phux_protocol::ResourceId::local(9)),
                    event: phux_protocol::wire::frame::AgentEvent::CommandStarted,
                    stamp: None,
                }
                .encode(&mut encoded);
                inbound_tx
                    .send(encoded.to_vec())
                    .await
                    .expect("inject inbound event");
                assert!(matches!(
                    tokio::time::timeout(Duration::from_millis(250), out_rx.recv())
                        .await
                        .expect("inbound fanout must not wait for the write")
                        .expect("subscriber remains live"),
                    crate::mailbox::Outbound::Frame(FrameKind::Event { .. })
                ));

                cancel.cancel();
                release_write.notify_waiters();
                let _ = session.await;
            })
            .await;
    }

    #[tokio::test]
    async fn aborting_session_cancels_a_blocked_transport_writer() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let host = host();
                let (relay, mailbox) = relay_pair(&host);
                let (_inbound_tx, inbound_rx) = tokio::sync::mpsc::channel(1);
                let write_started = Arc::new(tokio::sync::Notify::new());
                let writer_dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let conn = BlockedWriteConn {
                    inbound: inbound_rx,
                    write_started: Arc::clone(&write_started),
                    release_write: Arc::new(tokio::sync::Notify::new()),
                    writer_dropped: Arc::clone(&writer_dropped),
                };
                let session_host = host.clone();
                let session = tokio::task::spawn_local(async move {
                    let super::super::relay::RelayMailbox {
                        mut requests,
                        mut unsubscribes,
                        ..
                    } = mailbox;
                    run_relay_session(
                        &session_host,
                        conn,
                        &mut requests,
                        &mut unsubscribes,
                        &CancellationToken::new(),
                        None,
                        &super::super::operation_fence::OperationFence::default(),
                    )
                    .await
                });
                let (out_tx, _out_rx) = tokio::sync::mpsc::channel(1);
                let started = write_started.notified();
                tokio::pin!(started);
                relay.subscribe(
                    super::super::relay::ProxySubscription {
                        terminal: 9,
                        client: crate::state::ClientId(7),
                        out_tx,
                        consumer_cancel: CancellationToken::new(),
                        seq: 0,
                        awaits_snapshot: false,
                        bootstrap_profile: None,
                        bootstrap_limits: None,
                    },
                    FrameKind::SubscribeEvents {
                        terminal: Some(phux_protocol::ResourceId::local(9)),
                        after_seq: None,
                    },
                );
                tokio::time::timeout(Duration::from_secs(1), &mut started)
                    .await
                    .expect("writer entered blocked send");
                session.abort();
                for _ in 0..10 {
                    tokio::task::yield_now().await;
                    if writer_dropped.load(std::sync::atomic::Ordering::SeqCst) {
                        return;
                    }
                }
                panic!("blocked transport writer survived session cancellation");
            })
            .await;
    }

    /// A live relay handle and mailbox; the handle must stay alive or the
    /// supervisor exits.
    fn relay_pair(
        host: &SatelliteHost,
    ) -> (
        super::super::relay::RelayHandle,
        super::super::relay::RelayMailbox,
    ) {
        super::super::relay::RelayHandle::new(host.clone())
    }

    fn loopback_entry() -> HubEntry {
        entry("ws://127.0.0.1:9", None, None)
    }

    fn host() -> SatelliteHost {
        SatelliteHost::new("devbox")
    }

    /// Poll `statuses` under paused time until `want` matches.
    async fn wait_for_status(
        statuses: &HubLinkStatuses,
        host: &SatelliteHost,
        want: impl Fn(&LinkStatus) -> bool,
    ) {
        for _ in 0..60_000 {
            if statuses.get(host).as_ref().is_some_and(&want) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("status never matched; last = {:?}", statuses.get(host));
    }

    #[tokio::test(start_paused = true)]
    async fn supervisor_backs_off_exponentially_then_connects() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let transport = ScriptTransport::new(vec![
                    Err("boom-1".to_owned()),
                    Err("boom-2".to_owned()),
                    Ok(ScriptConn::open_forever()),
                ]);
                let statuses = HubLinkStatuses::default();
                let cancel = CancellationToken::new();
                let host = host();
                let (_relay, relay_rx) = relay_pair(&host);
                tokio::task::spawn_local(run_link(
                    host.clone(),
                    loopback_entry(),
                    transport.clone(),
                    statuses.clone(),
                    relay_rx,
                    cancel.child_token(),
                ));

                wait_for_status(&statuses, &host, |s| {
                    *s == LinkStatus::Backoff {
                        attempt: 1,
                        retry_in: Duration::from_millis(500),
                        last_error: "boom-1".to_owned(),
                    }
                })
                .await;
                wait_for_status(&statuses, &host, |s| {
                    *s == LinkStatus::Backoff {
                        attempt: 2,
                        retry_in: Duration::from_secs(1),
                        last_error: "boom-2".to_owned(),
                    }
                })
                .await;
                wait_for_status(&statuses, &host, |s| *s == LinkStatus::Connected).await;
                assert_eq!(transport.calls.get(), 3);

                cancel.cancel();
            })
            .await;
    }

    #[tokio::test(start_paused = true)]
    async fn supervisor_redials_after_a_lost_connection_with_reset_backoff() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (close_tx, closable) = ScriptConn::closable();
                let transport = ScriptTransport::new(vec![
                    // Fail once so the backoff has a streak to reset.
                    Err("boom-1".to_owned()),
                    Ok(closable),
                    Ok(ScriptConn::open_forever()),
                ]);
                let statuses = HubLinkStatuses::default();
                let cancel = CancellationToken::new();
                let host = host();
                let (_relay, relay_rx) = relay_pair(&host);
                tokio::task::spawn_local(run_link(
                    host.clone(),
                    loopback_entry(),
                    transport.clone(),
                    statuses.clone(),
                    relay_rx,
                    cancel.child_token(),
                ));

                wait_for_status(&statuses, &host, |s| *s == LinkStatus::Connected).await;

                // A connection that proved stable resets the streak.
                tokio::time::sleep(LINK_STABLE_AFTER).await;
                close_tx
                    .send("satellite went away".to_owned())
                    .await
                    .expect("send");
                wait_for_status(&statuses, &host, |s| {
                    *s == LinkStatus::Backoff {
                        attempt: 1,
                        retry_in: Duration::from_millis(500),
                        last_error: "satellite went away".to_owned(),
                    }
                })
                .await;
                wait_for_status(&statuses, &host, |s| *s == LinkStatus::Connected).await;
                assert_eq!(transport.calls.get(), 3);

                cancel.cancel();
            })
            .await;
    }

    #[tokio::test(start_paused = true)]
    async fn supervisor_keeps_backing_off_when_connections_die_young() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // An established connection that dies at once (a refused
                // ssh auth) still grows the backoff.
                let dead = || {
                    let (tx, rx) = tokio::sync::mpsc::channel::<String>(1);
                    drop(tx);
                    Ok(ScriptConn {
                        closed: Some(rx),
                        keepalive_error: None,
                    })
                };
                let transport = ScriptTransport::new(vec![
                    dead(),
                    dead(),
                    dead(),
                    Ok(ScriptConn::open_forever()),
                ]);
                let statuses = HubLinkStatuses::default();
                let cancel = CancellationToken::new();
                let host = host();
                let (_relay, relay_rx) = relay_pair(&host);
                tokio::task::spawn_local(run_link(
                    host.clone(),
                    loopback_entry(),
                    transport.clone(),
                    statuses.clone(),
                    relay_rx,
                    cancel.child_token(),
                ));

                for (attempt, retry_in) in [
                    (1, Duration::from_millis(500)),
                    (2, Duration::from_secs(1)),
                    (3, Duration::from_secs(2)),
                ] {
                    wait_for_status(&statuses, &host, |s| {
                        *s == LinkStatus::Backoff {
                            attempt,
                            retry_in,
                            last_error: "connection closed by satellite".to_owned(),
                        }
                    })
                    .await;
                }
                wait_for_status(&statuses, &host, |s| *s == LinkStatus::Connected).await;
                assert_eq!(transport.calls.get(), 4);

                cancel.cancel();
            })
            .await;
    }

    #[tokio::test(start_paused = true)]
    async fn supervisor_fails_closed_without_dialing() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let transport = ScriptTransport::new(vec![Ok(ScriptConn::open_forever())]);
                let statuses = HubLinkStatuses::default();
                let host = host();
                // Routable QUIC with no auth material: refused.
                let (relay, relay_rx) = relay_pair(&host);
                drop(relay);
                run_link(
                    host.clone(),
                    entry("quic://devbox:8788", None, None),
                    transport.clone(),
                    statuses.clone(),
                    relay_rx,
                    CancellationToken::new(),
                )
                .await;
                assert_eq!(transport.calls.get(), 0, "fail closed means no dial");
                assert!(matches!(
                    statuses.get(&host),
                    Some(LinkStatus::Refused { reason }) if reason.contains("cert-fingerprint")
                ));
            })
            .await;
    }

    #[tokio::test(start_paused = true)]
    async fn refused_link_fails_relay_commands_fast() {
        use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode};

        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let transport = ScriptTransport::new(vec![]);
                let statuses = HubLinkStatuses::default();
                let host = host();
                let (relay, relay_rx) = relay_pair(&host);
                let cancel = CancellationToken::new();
                tokio::task::spawn_local(run_link(
                    host.clone(),
                    entry("quic://devbox:8788", None, None),
                    transport.clone(),
                    statuses.clone(),
                    relay_rx,
                    cancel.child_token(),
                ));

                // Commands through a refused link fail typed, no dial.
                let result =
                    tokio::time::timeout(Duration::from_secs(5), relay.command(Command::Upgrade))
                        .await
                        .expect("fail fast, not hang");
                assert!(matches!(
                    result,
                    CommandResult::Error {
                        code: ErrorCode::SatelliteUnreachable,
                        ..
                    }
                ));
                assert_eq!(transport.calls.get(), 0, "refused link never dials");
                cancel.cancel();
            })
            .await;
    }

    #[tokio::test(start_paused = true)]
    async fn supervisor_rereads_the_token_file_every_attempt() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let token_path = dir.path().join("sat.token");
                std::fs::write(&token_path, "deadbeef\n").expect("write");

                let (close_tx, closable) = ScriptConn::closable();
                let transport =
                    ScriptTransport::new(vec![Ok(closable), Ok(ScriptConn::open_forever())]);
                let statuses = HubLinkStatuses::default();
                let cancel = CancellationToken::new();
                let host = host();
                let (_relay, relay_rx) = relay_pair(&host);
                tokio::task::spawn_local(run_link(
                    host.clone(),
                    entry(
                        "wss://devbox:8787",
                        Some(token_path.to_str().expect("utf8 path")),
                        Some("AB:CD"),
                    ),
                    transport.clone(),
                    statuses.clone(),
                    relay_rx,
                    cancel.child_token(),
                ));

                wait_for_status(&statuses, &host, |s| *s == LinkStatus::Connected).await;

                // Rotate the token and drop the link: the redial uses the new
                // token. Wait for Backoff first so Connected is the new one.
                std::fs::write(&token_path, "c0ffee\n").expect("rotate");
                close_tx.send("rotated".to_owned()).await.expect("send");
                wait_for_status(&statuses, &host, |s| {
                    matches!(s, LinkStatus::Backoff { last_error, .. } if last_error == "rotated")
                })
                .await;
                wait_for_status(&statuses, &host, |s| *s == LinkStatus::Connected).await;

                assert_eq!(
                    *transport.seen_tokens.borrow(),
                    vec![Some("deadbeef".to_owned()), Some("c0ffee".to_owned())]
                );

                cancel.cancel();
            })
            .await;
    }

    #[tokio::test(start_paused = true)]
    async fn supervisor_tears_down_a_link_whose_keepalive_fails() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // A silent WS partition: readable forever, but keepalive
                // fails, so the link must be torn down and redialed.
                let transport = ScriptTransport::new(vec![
                    Ok(ScriptConn::keepalive_fails("satellite sent nothing")),
                    Ok(ScriptConn::open_forever()),
                ]);
                let statuses = HubLinkStatuses::default();
                let cancel = CancellationToken::new();
                let host = host();
                let (_relay, relay_rx) = relay_pair(&host);
                tokio::task::spawn_local(run_link(
                    host.clone(),
                    loopback_entry(),
                    transport.clone(),
                    statuses.clone(),
                    relay_rx,
                    cancel.child_token(),
                ));

                wait_for_status(&statuses, &host, |s| {
                    matches!(
                        s,
                        LinkStatus::Backoff { last_error, .. }
                            if last_error == "satellite sent nothing"
                    )
                })
                .await;
                wait_for_status(&statuses, &host, |s| *s == LinkStatus::Connected).await;
                assert_eq!(transport.calls.get(), 2);

                cancel.cancel();
            })
            .await;
    }

    #[test]
    fn ws_idle_error_trips_only_at_the_limit() {
        assert!(ws_idle_error(Duration::ZERO).is_none());
        assert!(
            ws_idle_error(
                LINK_IDLE_TIMEOUT
                    .checked_sub(Duration::from_secs(1))
                    .expect("idle timeout exceeds one second"),
            )
            .is_none()
        );
        let reason = ws_idle_error(LINK_IDLE_TIMEOUT).expect("at the limit");
        assert!(reason.contains("idle limit 30s"), "{reason}");
        assert!(ws_idle_error(LINK_IDLE_TIMEOUT * 2).is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn supervisor_stops_on_cancellation() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let transport = ScriptTransport::new(vec![Err("boom".to_owned())]);
                let statuses = HubLinkStatuses::default();
                let cancel = CancellationToken::new();
                let host = host();
                let (_relay, relay_rx) = relay_pair(&host);
                let task = tokio::task::spawn_local(run_link(
                    host.clone(),
                    loopback_entry(),
                    transport,
                    statuses.clone(),
                    relay_rx,
                    cancel.child_token(),
                ));
                wait_for_status(&statuses, &host, |s| {
                    matches!(s, LinkStatus::Backoff { .. })
                })
                .await;
                cancel.cancel();
                tokio::time::timeout(Duration::from_secs(5), task)
                    .await
                    .expect("supervisor exits on cancel")
                    .expect("no panic");
            })
            .await;
    }

    // --- loopback integration: the real transport over a real socket -----

    #[tokio::test]
    async fn net_link_preserves_satellite_extended_features_from_hello_ok() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let _hello = futures_util::StreamExt::next(&mut ws)
                .await
                .unwrap()
                .unwrap();
            let mut caps = phux_protocol::caps::ServerCapabilities::default();
            caps.features_ext =
                ServerFeatureExtSet::with(&[phux_protocol::caps::ServerFeatureExt::PathQuery]);
            let mut encoded = bytes::BytesMut::new();
            FrameKind::HelloOk {
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                server_caps: caps,
                server_id: vec![0; 16],
                selected_profile: BootstrapProfile::SynthesizedVtRaw,
                bootstrap_limits: BootstrapLimits::default(),
            }
            .encode(&mut encoded);
            futures_util::SinkExt::send(
                &mut ws,
                tokio_tungstenite::tungstenite::Message::Binary(encoded.to_vec().into()),
            )
            .await
            .unwrap();
        });
        let spec = DialSpec::Ws {
            url: format!("ws://127.0.0.1:{port}"),
            trust: CertTrust::SkipVerify,
            token_file: None,
        };
        let conn = NetLinkTransport::new("ssh".into())
            .connect(&spec, None)
            .await
            .unwrap();
        assert!(
            conn.server_features_ext()
                .unwrap()
                .contains(phux_protocol::caps::ServerFeatureExt::PathQuery)
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn net_transport_connects_to_a_loopback_ws_listener_and_notices_the_drop() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("bind loopback");
                let addr = listener.local_addr().expect("addr");
                let (drop_tx, drop_rx) = tokio::sync::oneshot::channel::<()>();
                let server = tokio::task::spawn_local(async move {
                    let (tcp, _) = listener.accept().await.expect("accept");
                    let mut ws = tokio_tungstenite::accept_async(tcp)
                        .await
                        .expect("ws handshake");
                    let hello = futures_util::StreamExt::next(&mut ws)
                        .await
                        .expect("HELLO message")
                        .expect("HELLO read");
                    let tokio_tungstenite::tungstenite::Message::Binary(hello) = hello else {
                        panic!("expected binary HELLO");
                    };
                    assert!(matches!(
                        FrameKind::decode(&hello).expect("decode HELLO").0,
                        FrameKind::Hello { .. }
                    ));
                    let hello_ok = synthesized_hello_ok();
                    futures_util::SinkExt::send(
                        &mut ws,
                        tokio_tungstenite::tungstenite::Message::Binary(hello_ok.into()),
                    )
                    .await
                    .expect("send HELLO_OK");
                    let _ = drop_rx.await;
                    drop(ws);
                    drop(listener);
                });

                let statuses = HubLinkStatuses::default();
                let cancel = CancellationToken::new();
                let host = host();
                let (_relay, relay_rx) = relay_pair(&host);
                tokio::task::spawn_local(run_link(
                    host.clone(),
                    entry(&format!("ws://127.0.0.1:{}", addr.port()), None, None),
                    NetLinkTransport::new("ssh".into()),
                    statuses.clone(),
                    relay_rx,
                    cancel.child_token(),
                ));

                wait_for_real_status(&statuses, &host, |s| *s == LinkStatus::Connected).await;

                drop_tx.send(()).expect("server alive");
                wait_for_real_status(&statuses, &host, |s| {
                    matches!(s, LinkStatus::Backoff { .. })
                })
                .await;

                cancel.cancel();
                server.await.expect("server task");
            })
            .await;
    }

    // --- ssh-stdio through the real transport against a stub `$PHUX_SSH`,
    // which records its argv; its exit is the drop signal. The bridge half
    // is covered by `crates/phux/tests/fleet/stdio_bridge_e2e.rs`.

    fn synthesized_hello_ok() -> Vec<u8> {
        let mut encoded = bytes::BytesMut::new();
        FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: phux_protocol::caps::ServerCapabilities::default(),
            server_id: vec![0; 16],
            selected_profile: BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: BootstrapLimits::default(),
        }
        .encode(&mut encoded);
        encoded.to_vec()
    }

    fn shell_octal(bytes: &[u8]) -> String {
        use std::fmt::Write as _;

        let mut escaped = String::with_capacity(bytes.len() * 5);
        for byte in bytes {
            write!(escaped, "\\0{byte:03o}").expect("write to String");
        }
        escaped
    }

    /// Write an executable stub script and return its path.
    fn write_stub(dir: &std::path::Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-ssh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write stub");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");
        path
    }

    fn ssh_entry() -> HubEntry {
        entry("ssh://me@devbox:2222", None, None)
    }

    #[tokio::test]
    async fn ssh_transport_spawns_the_planned_argv_and_treats_exit_as_a_drop() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let argv_file = dir.path().join("argv.txt");
                let stub = write_stub(
                    dir.path(),
                    &format!(
                        "printf '%s\\n' \"$@\" > {}\necho 'stub bridge refused' >&2\nexit 7",
                        argv_file.display()
                    ),
                );
                let transport = NetLinkTransport {
                    ssh_program: stub.into(),
                };
                let statuses = HubLinkStatuses::default();
                let cancel = CancellationToken::new();
                let host = host();
                let (_relay, relay_rx) = relay_pair(&host);
                tokio::task::spawn_local(run_link(
                    host.clone(),
                    ssh_entry(),
                    transport,
                    statuses.clone(),
                    relay_rx,
                    cancel.child_token(),
                ));

                wait_for_real_status(&statuses, &host, |s| {
                    matches!(
                        s,
                        LinkStatus::Backoff { last_error, .. }
                            if last_error.contains("stub bridge refused")
                                && last_error.contains('7')
                    )
                })
                .await;
                cancel.cancel();

                // The stub saw exactly the planner's argv, host after `--`.
                let recorded = std::fs::read_to_string(&argv_file).expect("argv recorded");
                let expected: Vec<String> = ssh_argv(Some("me"), "devbox", Some(2222));
                assert_eq!(
                    recorded.lines().collect::<Vec<_>>(),
                    expected.iter().map(String::as_str).collect::<Vec<_>>()
                );
            })
            .await;
    }

    #[tokio::test]
    async fn ssh_transport_drains_stderr_past_the_pipe_buffer_without_stalling() {
        // A remote writing more than a pipe buffer to fd 2 must not wedge
        // the link, and the tail keeps the end of the stream.
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                // ~200 KiB of stderr, a marker, and a nonzero exit.
                let stub = write_stub(
                    dir.path(),
                    "i=0\n\
                     while [ \"$i\" -lt 200 ]; do printf '%01024d' 0 >&2; i=$((i + 1)); done\n\
                     echo 'DRAINED-TO-EOF' >&2\n\
                     exit 3",
                );
                let transport = NetLinkTransport {
                    ssh_program: stub.into(),
                };
                let statuses = HubLinkStatuses::default();
                let cancel = CancellationToken::new();
                let host = host();
                let (_relay, relay_rx) = relay_pair(&host);
                tokio::task::spawn_local(run_link(
                    host.clone(),
                    ssh_entry(),
                    transport,
                    statuses.clone(),
                    relay_rx,
                    cancel.child_token(),
                ));

                wait_for_real_status(&statuses, &host, |s| {
                    matches!(
                        s,
                        LinkStatus::Backoff { last_error, .. }
                            if last_error.contains("DRAINED-TO-EOF")
                                && last_error.contains('3')
                    )
                })
                .await;
                cancel.cancel();
            })
            .await;
    }

    #[tokio::test]
    async fn ssh_transport_holds_a_live_child_as_connected() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                // Stays up reading stdin until the pipe closes or it is killed.
                let reply = shell_octal(&synthesized_hello_ok());
                let stub = write_stub(
                    dir.path(),
                    &format!("printf '%b' '{reply}'\nexec cat > /dev/null"),
                );
                let transport = NetLinkTransport {
                    ssh_program: stub.into(),
                };
                let statuses = HubLinkStatuses::default();
                let cancel = CancellationToken::new();
                let host = host();
                let (_relay, relay_rx) = relay_pair(&host);
                let task = tokio::task::spawn_local(run_link(
                    host.clone(),
                    ssh_entry(),
                    transport,
                    statuses.clone(),
                    relay_rx,
                    cancel.child_token(),
                ));

                wait_for_real_status(&statuses, &host, |s| *s == LinkStatus::Connected).await;
                cancel.cancel();
                tokio::time::timeout(Duration::from_secs(10), task)
                    .await
                    .expect("supervisor exits on cancel")
                    .expect("no panic");
            })
            .await;
    }

    #[tokio::test]
    async fn ssh_transport_missing_program_is_a_failed_attempt_not_a_panic() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let transport = NetLinkTransport {
                    ssh_program: "/nonexistent/phux-test-no-such-ssh".into(),
                };
                let statuses = HubLinkStatuses::default();
                let cancel = CancellationToken::new();
                let host = host();
                let (_relay, relay_rx) = relay_pair(&host);
                tokio::task::spawn_local(run_link(
                    host.clone(),
                    ssh_entry(),
                    transport,
                    statuses.clone(),
                    relay_rx,
                    cancel.child_token(),
                ));
                wait_for_real_status(&statuses, &host, |s| {
                    matches!(
                        s,
                        LinkStatus::Backoff { last_error, .. } if last_error.contains("spawn")
                    )
                })
                .await;
                cancel.cancel();
            })
            .await;
    }

    /// Real-time sibling of [`wait_for_status`] for the loopback test.
    async fn wait_for_real_status(
        statuses: &HubLinkStatuses,
        host: &SatelliteHost,
        want: impl Fn(&LinkStatus) -> bool,
    ) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if statuses.get(host).as_ref().is_some_and(&want) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("status never matched; last = {:?}", statuses.get(host));
    }
}
