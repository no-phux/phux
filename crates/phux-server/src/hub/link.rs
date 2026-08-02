//! Hub outbound link supervisor (phux-v45.3/v45.9, ADR-0007/ADR-0038).
//!
//! Under `phux server --hub`, each enabled [`HubEntry`] gets one link
//! supervisor task (`run_link`, crate-internal) that dials the satellite through the
//! shared `phux-dial` stack and authenticates **exactly like a remote
//! consumer** (ADR-0038): TLS 1.3 with the satellite's leaf certificate
//! pinned by SHA-256 fingerprint, plus the pairing bearer token — a
//! length-prefixed stream preamble on QUIC, an `Authorization: Bearer`
//! header on WebSocket. The token is re-read from its file on every
//! attempt, so rotating it never needs a hub restart.
//!
//! **SSH-stdio** (`ssh://`, phux-v45.9) is the third dial path: the hub
//! spawns the system `ssh` binary (override with `$PHUX_SSH`) running
//! the remote `phux stdio-bridge` verb, which splices its stdin/stdout
//! to the satellite server's local Unix socket. SSH itself
//! authenticates and encrypts the channel, and the remote bridge
//! inherits the satellite UDS's local (owner-only) trust, so **no
//! bearer preamble is sent** and a registry `token-file` /
//! `cert-fingerprint` on an `ssh://` entry is ignored (there is no TLS
//! channel to pin) — see the ADR-0038 addendum. The child is spawned
//! with `BatchMode=yes` (never an interactive prompt) and argv built
//! from charset-validated parts (no shell, `--` before the host);
//! "connected" means the ssh child is running, and the bridged stream
//! carries the same length-prefixed frames as every other link — the
//! relay session pumps it exactly like QUIC/WS. The authoritative
//! liveness signal is the child's exit (surfaced as a `recv_frame`
//! EOF), which redials like a dropped connection; the child's stderr
//! is drained into a bounded tail for the failure reason (never
//! buffered without limit).
//!
//! **Fail closed.** [`plan_link`] refuses to dial a routable endpoint
//! whose entry lacks a token file or fingerprint pin (and refuses
//! plaintext `ws://` to routable hosts outright), mirroring
//! `phux attach --quic/--ws`. Loopback endpoints keep the loopback dev
//! carve-out. A refused link is never dialed and never retried — the
//! refusal is a configuration error, surfaced as
//! [`LinkStatus::Refused`] and fixed by `phux satellite add`. Malformed
//! `ssh://` endpoints fail earlier still, at hub-table validation.
//!
//! A lost or failed link is re-dialed with capped exponential backoff.
//! The failure streak resets only after a connection has stayed up for
//! `LINK_STABLE_AFTER` — a connection that dies young (an ssh child
//! whose spawn succeeded but whose auth was refused) keeps growing the
//! backoff exactly like a dial that never connected. Per-satellite
//! state is published to [`HubLinkStatuses`], the shared handle a
//! future `LIST` aggregation (phux-v45.5) reads.
//!
//! While a link is up, the supervisor drives that satellite's
//! `super::relay::RelaySession` (phux-v45.4): consumer requests arrive
//! on the per-satellite relay mailbox and are framed onto the connection;
//! inbound frames resolve relayed replies and fan re-tagged streams out
//! to proxy-subscribed consumers. While the link is *down* (dialing,
//! backoff, fail-closed refusal) the supervisor keeps draining the
//! mailbox, failing every request fast with a typed
//! `SatelliteUnreachable` — a consumer never hangs on a dead satellite.
//!
//! A dead satellite that *looks* up is handled too: every link enforces a
//! keepalive / idle contract. QUIC gets it from the transport (`phux-dial`
//! sets `keep_alive_interval` + `max_idle_timeout`; expiry surfaces as a
//! read error). SSH-stdio gets it from the SSH layer: the hub dials with
//! `ServerAliveInterval`/`ServerAliveCountMax` derived from the same two
//! constants (see [`ssh_argv`]), so a partitioned peer makes the ssh
//! child exit — the ordinary drop signal — and nothing rides the bridged
//! stream. WebSocket has no transport-level idle detection, so the
//! supervisor originates pings on `LINK_KEEPALIVE_INTERVAL` and tears
//! the link down when nothing (not even a pong) has arrived within
//! `LINK_IDLE_TIMEOUT` — a silent partition becomes an ordinary
//! disconnect: session teardown, typed errors, redial. Wire writes are
//! bounded by `LINK_SEND_TIMEOUT` so a peer with full socket buffers
//! cannot wedge the supervisor loop (for ssh links the bound applies to
//! the child's stdin pipe), and the keepalive tick doubles as the sweep
//! that prunes relayed commands whose consumer stopped waiting.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use phux_dial::{CertTrust, QuicDial, WsDial, WsTarget};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapProfileKind, ClientCapabilities,
};
use phux_protocol::ids::SatelliteHost;
use phux_protocol::wire::frame::FrameKind;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::{HubEntry, HubTable, SatelliteTarget};

/// First redial delay after a failure or a lost connection.
const BACKOFF_BASE: Duration = Duration::from_millis(500);

/// Ceiling for the exponential redial delay.
const BACKOFF_CAP: Duration = Duration::from_secs(30);

/// Period of the relay session's housekeeping tick: drive the transport
/// keepalive ([`LinkConn::keepalive`]) and prune abandoned pending
/// commands. Mirrors the QUIC dialer's `keep_alive_interval`
/// (`phux-dial`), so both transports probe on the same cadence.
const LINK_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);

/// Hub-side inbound-idle limit for WS links, mirroring the QUIC dialer's
/// `max_idle_timeout` (`phux-dial`). A healthy but quiet link stays under
/// it because every keepalive ping solicits a pong; only a partitioned or
/// wedged satellite goes silent this long.
const LINK_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Stall bound on any single wire write toward the satellite (frames and
/// keepalive pings). Against a partitioned peer with full socket buffers
/// an unbounded write would pend forever and wedge the whole supervisor
/// loop — inbound dispatch and the keepalive tick included.
const LINK_SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a connection must stay up before the failure streak is
/// forgotten. SSH auth/connect failures surface *after* the spawn
/// succeeds (the child exits in well under a second), so resetting the
/// backoff on mere establishment would redial a persistently failing
/// `ssh://` satellite at the base delay forever — hammering the remote
/// sshd — instead of backing off toward the cap like a QUIC/WS dial
/// that fails in `connect`.
const LINK_STABLE_AFTER: Duration = Duration::from_secs(30);

/// Retained tail of the ssh child's stderr, in bytes. ssh forwards the
/// *remote command's* stderr into this pipe for the life of the link,
/// so the hub must drain it into a bounded buffer — an uncapped
/// `read_to_end` would let one chatty (or hostile) satellite grow hub
/// memory without bound.
const SSH_STDERR_TAIL_MAX: usize = 8 * 1024;

/// What the planner decided to dial for one satellite, auth material
/// resolved into the shared `phux-dial` vocabulary.
///
/// The pairing token stays a *path* here — the supervisor re-reads the
/// file on every attempt (ADR-0038 rotation: update the file, reconnect).
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
    /// Spawn the system `ssh` binary bridging to the satellite's UDS via
    /// the remote `phux stdio-bridge` verb (phux-v45.9). Carries no auth
    /// material: SSH authenticates the channel, and the bridge inherits
    /// the satellite UDS's local trust (ADR-0038 addendum).
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
    /// The token file this spec dials with, if any. Always `None` for
    /// SSH-stdio: the channel is SSH-authenticated, not token-bearing.
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

/// Build the argv (excluding the program itself) for one SSH-stdio dial.
///
/// Pure and shell-free: the returned vector is handed to
/// `tokio::process::Command::args`, never a shell, and the host/user
/// parts were charset-allowlisted at endpoint parse time (no leading
/// `-`, no whitespace or metacharacters). `--` still precedes the host
/// as defense in depth against option injection. `BatchMode=yes` makes
/// a missing/failed key a fast, non-interactive error; `-T` refuses a
/// remote PTY (a PTY would translate the byte stream);
/// `ClearAllForwardings=yes` keeps `ssh_config` forwardings from
/// piggybacking on hub links.
///
/// **Keepalive / idle contract** (`docs/architecture/transport.md`): the
/// link-level liveness the relay demands of every transport lives at the
/// SSH layer for ssh links, mirroring how QUIC gets it from quinn rather
/// than from hub pings. `ServerAliveInterval` probes on the
/// `LINK_KEEPALIVE_INTERVAL` cadence and `ServerAliveCountMax` sizes
/// the window to `LINK_IDLE_TIMEOUT`, so a silent partition makes ssh
/// exit — and a child exit is the ordinary link-drop signal
/// (`LinkConn::recv_frame` EOF). Nothing rides the bridged phux
/// stream: the bridge stays byte-transparent, and
/// `LinkConn::keepalive` is a no-op for ssh exactly as for QUIC.
#[must_use]
pub fn ssh_argv(user: Option<&str>, host: &str, port: Option<u16>) -> Vec<String> {
    // Derived, not hardcoded, so the SSH-layer contract cannot drift
    // from the WS/QUIC one: probe every LINK_KEEPALIVE_INTERVAL, declare
    // the peer dead after LINK_IDLE_TIMEOUT of silence.
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
///
/// A refusal is a *configuration* error: the supervisor publishes it as
/// [`LinkStatus::Refused`] and never dials — no retry loop can fix a
/// missing credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkRefusal {
    /// Plaintext `ws://` to a routable host: no TLS means no fingerprint
    /// pin is even possible. Mirrors the attach CLI's refusal.
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
    /// The endpoint survived hub-table validation but not the dialer's
    /// stricter URL parse.
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
                 `phux satellite add`"
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
///
/// Held behind [`HubLinkStatuses`] so a future `LIST` aggregation
/// (phux-v45.4+) can report every satellite's reachability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkStatus {
    /// Fail-closed refusal ([`LinkRefusal`]); the link was never dialed
    /// and will not be retried until the registry entry changes.
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
    /// The last attempt failed (or an established link dropped); the
    /// supervisor redials after `retry_in`.
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

/// Shared, cheaply-cloneable map of per-satellite [`LinkStatus`].
///
/// One handle lives in server shared state (set at hub startup); each
/// link supervisor holds a clone and publishes its transitions. The
/// `std::sync::Mutex` is held only for map reads/writes — never across
/// an await point.
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

    /// Snapshot every satellite's status, in deterministic name order.
    #[must_use]
    pub fn snapshot(&self) -> BTreeMap<SatelliteHost, LinkStatus> {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<SatelliteHost, LinkStatus>> {
        // A poisoned map only means another supervisor panicked mid-insert;
        // the map itself (Clone + insert) cannot be left inconsistent.
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
        // 2^32 * base overflows Duration long before `failures` wraps;
        // clamp the exponent so the shift itself cannot overflow.
        let exponent = self.failures.min(20);
        let delay = self
            .base
            .saturating_mul(2u32.saturating_pow(exponent))
            .min(self.cap);
        self.failures = self.failures.saturating_add(1);
        delay
    }

    /// Forget the failure streak after a connection that proved stable
    /// (see `LINK_STABLE_AFTER`).
    pub const fn reset(&mut self) {
        self.failures = 0;
    }

    /// Settle the streak after a lost connection and return the 1-based
    /// attempt number to report the loss as. Only a connection that
    /// stayed up at least `LINK_STABLE_AFTER` clears the failure
    /// streak; one that died young counts like a dial that never
    /// connected (an ssh child whose spawn succeeded but whose auth was
    /// refused exits fast — resetting on mere establishment would
    /// redial it at the base delay forever, hammering the remote sshd).
    pub fn settle_after_loss(&mut self, up_for: Duration) -> u32 {
        if up_for >= LINK_STABLE_AFTER {
            self.reset();
        }
        self.failures.saturating_add(1)
    }
}

/// Decide how (or whether) to dial one hub-table entry. Pure — no I/O.
///
/// This is the fail-closed gate (ADR-0038): routable endpoints without
/// both a token file and a fingerprint pin are refused, and plaintext
/// `ws://` is loopback-only. Loopback endpoints keep the dev carve-out
/// (skip-verify TLS, optional token), matching `phux attach --quic/--ws`.
/// `ssh://` endpoints need no credential material — SSH authenticates
/// the channel and the remote bridge inherits the satellite UDS's local
/// trust (ADR-0038 addendum) — so any configured `token-file` /
/// `cert-fingerprint` on such an entry is deliberately not carried into
/// the spec.
///
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
                // ws:// never performs a TLS handshake; the trust value is
                // inert but kept honest (a pin would never be checked).
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

/// The shared routable-vs-loopback trust rule for TLS transports
/// (QUIC and `wss://`): routable requires pin **and** token; loopback
/// pins when a fingerprint is configured and skips verification
/// otherwise.
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

/// Whether a `quic://` host token names the loopback interface —
/// `localhost`, a loopback IPv4, or a (possibly bracketed) loopback IPv6.
fn host_is_loopback(host: &str) -> bool {
    let bare = host.trim_matches(['[', ']']);
    bare.eq_ignore_ascii_case("localhost")
        || bare
            .parse::<std::net::IpAddr>()
            .is_ok_and(|addr| addr.is_loopback())
}

/// Read and validate the pairing token for an attempt.
///
/// One hex token, line-oriented (ADR-0038: the same shape as the server's
/// token store — the first non-empty line wins, trailing newline
/// tolerated). Returns the *hex* string; the QUIC path decodes it into
/// raw preamble bytes, the WebSocket path sends it verbatim in the
/// `Authorization` header, matching the attach CLI.
///
/// # Errors
///
/// A human-readable reason (missing/unreadable file, empty file, or a
/// token that is not valid hex). The supervisor treats this like a failed
/// attempt — fixed token files are picked up on the next redial.
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

/// The transport seam the supervisor dials through.
///
/// Production is [`NetLinkTransport`] (the shared `phux-dial` stack);
/// tests inject a scripted transport so backoff and status transitions
/// are exercised without any network. Errors are plain strings — they
/// only ever land in logs and [`LinkStatus::Backoff::last_error`].
pub(crate) trait LinkTransport {
    /// The established-connection handle.
    type Conn: LinkConn;

    /// Dial `spec`, authenticating with `token` (validated hex) when
    /// present.
    async fn connect(&self, spec: &DialSpec, token: Option<String>) -> Result<Self::Conn, String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NegotiatedBootstrap {
    profile: BootstrapProfile,
    limits: BootstrapLimits,
}
/// An established hub link: a duplex of complete encoded phux frames
/// (length prefix included, the `FrameKind::encode`/`decode` unit) the
/// relay session (phux-v45.4) pumps in both directions.
pub(crate) trait LinkConn {
    /// Put one complete encoded frame on the wire.
    async fn send_frame(&mut self, frame: &[u8]) -> Result<(), String>;

    /// Receive the next complete frame. `Ok(None)` is a clean close by
    /// the satellite; `Err` carries a human-readable loss reason.
    ///
    /// Must be cancel-safe: the supervisor polls it inside a `select!`
    /// against the relay mailbox and the cancellation token, recreating
    /// the future each iteration.
    async fn recv_frame(&mut self) -> Result<Option<Vec<u8>>, String>;

    /// Exact bounds selected before this connection was returned.
    fn bootstrap_limits(&self) -> BootstrapLimits;

    /// Exact synchronization profile selected before this connection returned.
    fn bootstrap_profile(&self) -> BootstrapProfile;

    /// Transport-level liveness probe, driven by the supervisor's
    /// [`LINK_KEEPALIVE_INTERVAL`] tick while the link is up. WS links
    /// originate a ping and enforce the hub-side inbound-idle limit
    /// ([`LINK_IDLE_TIMEOUT`]), mirroring the idle contract the QUIC
    /// dialer configures at the transport layer; QUIC links are a no-op
    /// (quinn surfaces idle expiry as a [`Self::recv_frame`] error).
    /// `Err` carries the reason the link must be torn down.
    async fn keepalive(&mut self) -> Result<(), String>;
}

/// Supervise one satellite link: plan, dial, relay, redial.
///
/// Runs until `cancel` fires or every [`super::relay::RelayHandle`] for
/// this satellite is dropped. Fail-closed refusals publish
/// [`LinkStatus::Refused`] and never dial, but keep draining the relay
/// mailbox so consumers get typed fail-fast errors instead of silence.
#[allow(
    clippy::future_not_send,
    reason = "ADR-0014: hub link supervisors run on the server's LocalSet; the transport seam is generic so tests can inject !Send scripted transports"
)]
#[allow(
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    reason = "one linear supervisor loop: plan -> dial (drain) -> relay -> backoff (drain); every drain arm repeats the same three-way select and splitting them scatters the lifecycle"
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
        requests: mut relay_rx,
        unsubscribes: mut unsub_rx,
    } = mailbox;
    let spec = match plan_link(&entry) {
        Ok(spec) => spec,
        Err(refusal) => {
            warn!(
                satellite = %host,
                refusal = %refusal,
                "hub link refused (fail closed); not dialing"
            );
            statuses.set(
                &host,
                LinkStatus::Refused {
                    reason: refusal.to_string(),
                },
            );
            // Never dialed, never will be until the registry changes:
            // fail every relay request fast until shutdown. Unsubscribes
            // are drained and discarded — no session exists, so there is
            // no registry to withdraw from.
            loop {
                tokio::select! {
                    () = cancel.cancelled() => return,
                    request = relay_rx.recv() => match request {
                        Some(request) => super::relay::fail_fast(
                            request,
                            &host,
                            "link refused (fail closed); fix the registry entry",
                        ),
                        None => return,
                    },
                    unsubscribe = unsub_rx.recv() => if unsubscribe.is_none() { return },
                }
            }
        }
    };

    let mut backoff = Backoff::new(BACKOFF_BASE, BACKOFF_CAP);
    loop {
        let attempt = backoff.failures().saturating_add(1);
        statuses.set(&host, LinkStatus::Connecting { attempt });
        let connect = async {
            // Re-read the token every attempt (ADR-0038 rotation: update
            // the hub's token file, reconnect). The file is a one-line
            // local read; blocking the current-thread runtime for it is
            // deliberate simplicity, matching the config-load paths.
            let token = read_link_token(spec.token_file())?;
            transport.connect(&spec, token).await
        };
        tokio::pin!(connect);
        // Drain relay requests while the dial is in flight: a consumer
        // targeting a not-yet-connected satellite fails fast, not late.
        let outcome = loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                request = relay_rx.recv() => match request {
                    Some(request) => super::relay::fail_fast(request, &host, "link is connecting"),
                    None => return,
                },
                unsubscribe = unsub_rx.recv() => if unsubscribe.is_none() { return },
                outcome = &mut connect => break outcome,
            }
        };

        let (failed_attempt, last_error) = match outcome {
            Ok(conn) => {
                info!(satellite = %host, target = %spec, "hub link established");
                statuses.set(&host, LinkStatus::Connected);
                let connected_at = tokio::time::Instant::now();
                match run_relay_session(&host, conn, &mut relay_rx, &mut unsub_rx, &cancel).await {
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
        let sleep = tokio::time::sleep(retry_in);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                request = relay_rx.recv() => match request {
                    Some(request) => super::relay::fail_fast(request, &host, "link is backing off before redial"),
                    None => return,
                },
                unsubscribe = unsub_rx.recv() => if unsubscribe.is_none() { return },
                () = &mut sleep => break,
            }
        }
    }
}

/// Drive one established connection's relay session (phux-v45.4): pump
/// consumer requests onto the wire and inbound frames back to consumers.
///
/// Returns `Some(reason)` when the connection was lost (redial), `None`
/// when the supervisor should exit (cancellation, or every relay handle
/// dropped). Session teardown — failing in-flight commands and notifying
/// proxy subscribers with a typed error — runs on every exit path.
#[allow(
    clippy::future_not_send,
    reason = "ADR-0014: runs on the server's LocalSet inside run_link"
)]
async fn run_relay_session<C: LinkConn>(
    host: &SatelliteHost,
    mut conn: C,
    relay_rx: &mut tokio::sync::mpsc::Receiver<super::relay::RelayRequest>,
    unsub_rx: &mut tokio::sync::mpsc::UnboundedReceiver<super::relay::Unsubscribe>,
    cancel: &CancellationToken,
) -> Option<String> {
    let profile = conn.bootstrap_profile();
    info!(satellite = %host, ?profile, "hub relay using negotiated bootstrap profile");
    let mut session =
        super::relay::RelaySession::new_negotiated(host.clone(), conn.bootstrap_limits(), profile);
    // Housekeeping tick: transport keepalive + pending-map pruning. First
    // tick one interval out — the connection was live zero seconds ago.
    let mut keepalive = tokio::time::interval_at(
        tokio::time::Instant::now() + LINK_KEEPALIVE_INTERVAL,
        LINK_KEEPALIVE_INTERVAL,
    );
    keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let (lost, reason) = loop {
        tokio::select! {
            () = cancel.cancelled() => break (false, "hub is shutting down".to_owned()),
            request = relay_rx.recv() => {
                let Some(request) = request else {
                    break (false, "relay handles dropped".to_owned());
                };
                if let Some(frame) = session.handle_request_checked(request)
                    && let Err(error) = send_bounded(&mut conn, &frame).await
                {
                    break (true, error);
                }
            }
            unsubscribe = unsub_rx.recv() => {
                // Undroppable subscription teardown (phux-v45.11): apply
                // the withdrawal and tell the satellite to stop streaming
                // any terminal whose last proxy subscriber just left.
                let Some(unsubscribe) = unsubscribe else {
                    break (false, "relay handles dropped".to_owned());
                };
                let mut lost_reason = None;
                for frame in session.handle_unsubscribe(unsubscribe) {
                    if let Err(error) = send_bounded(&mut conn, &frame).await {
                        lost_reason = Some(error);
                        break;
                    }
                }
                if let Some(error) = lost_reason {
                    break (true, error);
                }
            }
            inbound = conn.recv_frame() => match inbound {
                Ok(Some(frame)) => {
                    if let Err(error) = session.handle_inbound(&frame) {
                        break (true, error);
                    }
                }
                Ok(None) => break (true, "connection closed by satellite".to_owned()),
                Err(error) => break (true, error),
            },
            _ = keepalive.tick() => {
                session.prune_abandoned();
                // Retry any attach snapshot a briefly-full consumer refused,
                // so it converges even with no further inbound frame for its
                // terminal to trigger the inline retry (phux-v45.12).
                session.flush_pending_snapshots();
                match tokio::time::timeout(LINK_SEND_TIMEOUT, conn.keepalive()).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => break (true, error),
                    Err(_) => break (true, format!(
                        "keepalive write to satellite stalled for {}s",
                        LINK_SEND_TIMEOUT.as_secs()
                    )),
                }
            }
        }
    };
    session.teardown(&reason);
    lost.then_some(reason)
}

/// Put one frame on the wire with [`LINK_SEND_TIMEOUT`] as the stall
/// bound: a partitioned peer whose socket buffers filled up would
/// otherwise pend the write forever and wedge the supervisor loop.
#[allow(
    clippy::future_not_send,
    reason = "ADR-0014: runs on the server's LocalSet inside run_relay_session"
)]
async fn send_bounded<C: LinkConn>(conn: &mut C, frame: &[u8]) -> Result<(), String> {
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
async fn negotiate_link<C: LinkConn>(conn: &mut C) -> Result<NegotiatedBootstrap, String> {
    let offered = ClientCapabilities::default();
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
            selected_profile,
            bootstrap_limits,
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
            })
        }
        FrameKind::Error { code, message, .. } => {
            Err(format!("satellite rejected HELLO: {code:?}: {message}"))
        }
        other => Err(format!("expected HELLO_OK from satellite, got {other:?}")),
    }
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

/// Spawn one [`run_link`] supervisor per hub-table entry onto the current
/// `LocalSet`, all children of `cancel`, registering each satellite's
/// [`super::relay::RelayHandle`] in `relays`.
///
/// Called from the server runtime's hub bring-up; `statuses` and `relays`
/// are the same handles mirrored into shared state for command routing
/// and future `LIST` aggregation.
pub(crate) fn spawn_links(
    table: &HubTable,
    statuses: &HubLinkStatuses,
    relays: &super::relay::HubRelays,
    cancel: &CancellationToken,
) {
    let transport = NetLinkTransport::from_env();
    for (host, entry) in table.iter() {
        let (handle, mailbox) = super::relay::RelayHandle::new(host.clone());
        relays.insert(handle);
        tokio::task::spawn_local(run_link(
            host.clone(),
            entry.clone(),
            transport.clone(),
            statuses.clone(),
            mailbox,
            cancel.child_token(),
        ));
    }
}

/// The production [`LinkTransport`]: the shared `phux-dial` QUIC/WS stack
/// authenticating exactly like a remote consumer (ADR-0038), plus the
/// SSH-stdio child-process path for `ssh://` endpoints (phux-v45.9).
#[derive(Debug, Clone)]
pub(crate) struct NetLinkTransport {
    /// Program spawned for [`DialSpec::Ssh`] dials. `ssh` on `$PATH` by
    /// default; `$PHUX_SSH` overrides it (an OpenSSH-compatible wrapper,
    /// or a stub in tests).
    ssh_program: std::ffi::OsString,
}

impl NetLinkTransport {
    /// Build the production transport, honoring `$PHUX_SSH`.
    pub(crate) fn from_env() -> Self {
        Self {
            ssh_program: std::env::var_os("PHUX_SSH").unwrap_or_else(|| "ssh".into()),
        }
    }
}

/// An established production link, framed in both directions for the
/// relay session (phux-v45.4).
#[derive(Debug)]
pub(crate) enum NetLinkConn {
    /// QUIC connection with its endpoint driver and the opened bidi
    /// stream halves. Frames are length-prefixed on the byte stream,
    /// byte-for-byte the framing the UDS path uses (`docs/spec/proto.md` §5).
    Quic {
        /// Owns the UDP socket + I/O driver; must outlive the connection.
        _endpoint: quinn::Endpoint,
        /// The established connection, kept for a clean close.
        _connection: quinn::Connection,
        /// Opened bidi send half (auth preamble already written).
        send: quinn::SendStream,
        /// Opened bidi recv half.
        recv: quinn::RecvStream,
        /// Reassembly buffer: reads land here (cancel-safely) and complete
        /// length-prefixed frames are peeled off the front.
        buf: bytes::BytesMut,
        /// Exact selection installed once before `connect` returns.
        negotiated: Option<NegotiatedBootstrap>,
    },
    /// WebSocket connection: one binary message is one complete frame.
    Ws {
        /// The established stream.
        ws: Box<phux_dial::ws::Ws>,
        /// When the satellite last sent *anything* — data or control
        /// frames. WS has no transport-level idle detection (unlike the
        /// QUIC path), so this feeds the hub-side idle limit in
        /// [`LinkConn::keepalive`]; without it a silent partition would
        /// leave the link `Connected` forever.
        last_inbound: std::time::Instant,
        /// Exact selection installed once before `connect` returns.
        negotiated: Option<NegotiatedBootstrap>,
    },
    /// SSH-stdio child (phux-v45.9): the running `ssh` process whose
    /// stdin/stdout carry the bridged, length-prefixed frame stream —
    /// byte-for-byte the UDS framing, because the remote
    /// `phux stdio-bridge` is byte-transparent. The child is
    /// `kill_on_drop`, so tearing down the link (cancellation, redial)
    /// reaps the ssh process instead of orphaning it.
    Ssh {
        /// The spawned ssh process; its exit is the link-loss signal.
        child: tokio::process::Child,
        /// Bridged write half — frames land on the remote server's UDS.
        stdin: tokio::process::ChildStdin,
        /// Bridged read half — the satellite's frames, length-prefixed.
        stdout: tokio::process::ChildStdout,
        /// Background drainer of ssh's own diagnostics plus the remote
        /// command's stderr. It reads fd 2 *continuously* for the life of
        /// the link (not just at EOF): a stderr pipe left unread fills its
        /// ~64KiB OS buffer, blocks the remote's writes, and — because
        /// stdout rides the same ssh channel window — stalls inbound frame
        /// delivery with no loss signal (phux-v45.9). The task retains only
        /// a bounded tail (`SSH_STDERR_TAIL_MAX`) and yields it at EOF so
        /// `ssh_exit_reason` can compose the failure reason; it is aborted
        /// on drop when the link is torn down before EOF.
        stderr_reader: AbortOnDrop,
        /// Reassembly buffer: same cancel-safe frame peeling as the
        /// QUIC path.
        buf: bytes::BytesMut,
        /// Exact selection installed once before `connect` returns.
        negotiated: Option<NegotiatedBootstrap>,
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

    fn negotiated(&self) -> NegotiatedBootstrap {
        match self {
            Self::Quic { negotiated, .. }
            | Self::Ws { negotiated, .. }
            | Self::Ssh { negotiated, .. } => {
                negotiated.expect("production link constructors negotiate before returning")
            }
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
                // No token: SSH authenticates the channel and the remote
                // bridge inherits the satellite UDS's local trust
                // (ADR-0038 addendum). `token` is None here by
                // construction (plan_link never carries a token file into
                // an Ssh spec).
                debug_assert!(token.is_none(), "ssh dials carry no bearer token");
                let mut child = tokio::process::Command::new(&self.ssh_program)
                    .args(ssh_argv(user.as_deref(), host, *port))
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    // Reap the ssh process when the link is torn down
                    // (cancellation, redial) rather than orphaning it.
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|err| {
                        format!("spawn {}: {err}", self.ssh_program.to_string_lossy())
                    })?;
                // The three pipes exist because we just asked for them;
                // treat their absence as a failed dial, not a panic.
                let stdin = child.stdin.take().ok_or("ssh child has no stdin pipe")?;
                let stdout = child.stdout.take().ok_or("ssh child has no stdout pipe")?;
                let mut stderr = child.stderr.take().ok_or("ssh child has no stderr pipe")?;
                // Drain fd 2 for the whole life of the link so it can never
                // fill its pipe buffer and back-pressure the shared ssh
                // channel (phux-v45.9). `read_tail` keeps only the bounded
                // tail and returns it at EOF; `ssh_exit_reason` joins this
                // task to fold that tail into the loss reason. Spawned on
                // the supervisor's LocalSet (see `run_link`).
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

        // HELLO/profile negotiation is mandatory over QUIC, WebSocket, and
        // SSH-stdio alike. The remote UDS reached through the byte-transparent
        // SSH bridge has no same-uid protocol exemption.
        let selection = negotiate_link(&mut conn).await?;
        conn.install_negotiated(selection)?;
        Ok(conn)
    }
}

impl LinkConn for NetLinkConn {
    async fn send_frame(&mut self, frame: &[u8]) -> Result<(), String> {
        match self {
            Self::Quic { send, .. } => send
                .write_all(frame)
                .await
                .map_err(|err| format!("write to satellite: {err}")),
            Self::Ws { ws, .. } => futures_util::SinkExt::send(
                ws.as_mut(),
                tokio_tungstenite::tungstenite::Message::Binary(frame.to_vec()),
            )
            .await
            .map_err(|err| format!("write to satellite: {err}")),
            // The child stdin pipe is the wire: the caller's
            // `LINK_SEND_TIMEOUT` bound (send_bounded) applies here like
            // any other transport, so a wedged ssh child with a full pipe
            // cannot pend the supervisor forever.
            Self::Ssh { stdin, .. } => tokio::io::AsyncWriteExt::write_all(stdin, frame)
                .await
                .map_err(|err| format!("write to ssh transport: {err}")),
        }
    }

    fn bootstrap_limits(&self) -> BootstrapLimits {
        self.negotiated().limits
    }

    fn bootstrap_profile(&self) -> BootstrapProfile {
        self.negotiated().profile
    }

    async fn recv_frame(&mut self) -> Result<Option<Vec<u8>>, String> {
        match self {
            Self::Quic { recv, buf, .. } => {
                // Cancel-safe reassembly: `read_buf` lands bytes in the
                // persistent buffer even if this future is dropped between
                // polls; complete frames are peeled off the front.
                loop {
                    if let Some(frame) = split_buffered_frame(buf)? {
                        return Ok(Some(frame));
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
                        // Anything the satellite sends — data or control —
                        // is liveness for the idle limit.
                        *last_inbound = std::time::Instant::now();
                        match message {
                            tokio_tungstenite::tungstenite::Message::Close(_) => {
                                return Ok(None);
                            }
                            tokio_tungstenite::tungstenite::Message::Binary(data) => {
                                return Ok(Some(data));
                            }
                            // Control frames (ping/pong): reading them is
                            // what answers pings; skip and keep reading.
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
                // Same cancel-safe reassembly as the QUIC path: the
                // bridged stream carries the identical length-prefixed
                // framing (the remote bridge splices the satellite's UDS
                // byte-for-byte). stdout EOF means the link is gone —
                // remote bridge ended, satellite server closed the UDS,
                // network died, or the key was refused — and the child's
                // exit status plus a bounded tail of its stderr become
                // the loss reason.
                loop {
                    if let Some(frame) = split_buffered_frame(buf)? {
                        return Ok(Some(frame));
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
    }

    async fn keepalive(&mut self) -> Result<(), String> {
        match self {
            // quinn already originates keepalives and enforces the idle
            // timeout at the transport layer (`phux-dial` sets
            // `keep_alive_interval` + `max_idle_timeout`); expiry surfaces
            // as a `recv_frame` error, so there is nothing to drive here.
            // The ssh child likewise *is* the transport: the dial argv
            // carries ServerAliveInterval/ServerAliveCountMax (see
            // `ssh_argv`), so SSH itself probes the peer and a silent
            // partition makes the child exit — surfaced as a
            // `recv_frame` EOF with the exit status and stderr as the
            // reason. The bridged stream stays byte-transparent — there
            // is no in-band phux ping to originate on either.
            Self::Quic { .. } | Self::Ssh { .. } => Ok(()),
            Self::Ws {
                ws, last_inbound, ..
            } => {
                if let Some(reason) = ws_idle_error(last_inbound.elapsed()) {
                    return Err(reason);
                }
                // Solicit a pong so a healthy quiet satellite keeps
                // resetting `last_inbound`; a partitioned one cannot.
                futures_util::SinkExt::send(
                    ws.as_mut(),
                    tokio_tungstenite::tungstenite::Message::Ping(Vec::new()),
                )
                .await
                .map_err(|err| format!("keepalive ping to satellite: {err}"))
            }
        }
    }
}

/// The teardown reason once a WS link has been inbound-idle for
/// [`LINK_IDLE_TIMEOUT`] or longer, or `None` while it is live.
fn ws_idle_error(idle_for: Duration) -> Option<String> {
    (idle_for >= LINK_IDLE_TIMEOUT).then(|| {
        format!(
            "satellite sent nothing for {}s (idle limit {}s); link presumed dead",
            idle_for.as_secs(),
            LINK_IDLE_TIMEOUT.as_secs()
        )
    })
}

/// How long an ssh child gets to exit after closing its bridged stdout
/// before the hub kills it. ssh normally exits immediately once the
/// remote command ends; a child that lingers past this (a wrapper
/// holding the process open) would otherwise pin the supervisor between
/// "stream is gone" and "redial".
const SSH_EXIT_GRACE: Duration = Duration::from_secs(5);

/// A local task handle that aborts its task on drop, so tearing down an
/// ssh link (cancellation, redial, connection drop before stdout EOF)
/// stops the background stderr drainer instead of leaking it past the
/// child's death.
#[derive(Debug)]
pub(crate) struct AbortOnDrop(tokio::task::JoinHandle<Vec<u8>>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Compose the loss reason for an ssh link whose bridged stdout hit EOF:
/// the child's exit status plus a bounded tail of its stderr.
///
/// stderr has been drained the whole time by the `stderr_reader` task
/// (so a chatty fd 2 never back-pressured the shared ssh channel); that
/// task's `read_tail` returns the bounded tail ([`SSH_STDERR_TAIL_MAX`])
/// once the pipe hits EOF at the child's death, and joining it here folds
/// that tail into the reason. Both the wait and the join are bounded by
/// [`SSH_EXIT_GRACE`]; a child that will not exit is killed (the redial
/// loop owns recovery, and the pipes are dropped with the connection
/// either way).
async fn ssh_exit_reason(
    child: &mut tokio::process::Child,
    stderr_reader: &mut AbortOnDrop,
) -> String {
    let gathered = tokio::time::timeout(SSH_EXIT_GRACE, async {
        // The child's exit is the authoritative loss signal; the drainer
        // finishes as fd 2 hits EOF at process death, so joining it yields
        // the complete bounded tail — not just what was drained before
        // stdout closed. `Pin::new` is sound: `JoinHandle` is `Unpin`.
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

/// Peel one complete length-prefixed frame (prefix included, the unit
/// `FrameKind::decode` expects) off the front of `buf`, or `None` when
/// the buffer holds only a partial frame.
fn split_buffered_frame(buf: &mut bytes::BytesMut) -> Result<Option<Vec<u8>>, String> {
    use phux_protocol::wire::frame::MAX_FRAME_LEN;
    const LENGTH_PREFIX: usize = 4;
    if buf.len() < LENGTH_PREFIX {
        return Ok(None);
    }
    let body_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if !(1..=MAX_FRAME_LEN).contains(&body_len) {
        return Err(format!(
            "satellite sent frame with out-of-range length {body_len}"
        ));
    }
    let total = LENGTH_PREFIX + body_len as usize;
    if buf.len() < total {
        return Ok(None);
    }
    Ok(Some(buf.split_to(total).to_vec()))
}

/// Drain `reader` to EOF (or error), retaining only the last `max`
/// bytes.
///
/// The bounded sibling of `read_to_end` for pipes that may carry an
/// unbounded stream (an ssh child's stderr forwards the remote
/// command's fd 2 for the whole life of the link). A read error ends
/// the drain and returns whatever tail was collected — the caller only
/// wants diagnostics, and the authoritative exit signal is
/// `child.wait()`, joined alongside.
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
        // Bare entry: dialable with zero auth material (SSH authenticates
        // the channel; ADR-0038 addendum).
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

        // Configured token/pin on an ssh entry is ignored, not carried:
        // there is no TLS channel to pin and no preamble to send.
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
        // The SSH-layer liveness window mirrors the WS idle contract:
        // interval * count == LINK_IDLE_TIMEOUT (see ssh_argv).
        assert_eq!(
            LINK_KEEPALIVE_INTERVAL * 3,
            LINK_IDLE_TIMEOUT,
            "keepalive constants drifted; re-derive ServerAlive* expectations"
        );
        // The host always sits after `--`, so even a hostile host token
        // (which endpoint parsing already rejects) could not be read as
        // an option — defense in depth.
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
        // A megabyte of remote stderr chatter must not accumulate on the
        // hub: only the final `max` bytes survive.
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

    /// A scripted connection: drops with the reason sent on `closed`
    /// (an mpsc so `recv_frame` stays cancel-safe), or never.
    struct ScriptConn {
        closed: Option<tokio::sync::mpsc::Receiver<String>>,
        keepalive_error: Option<String>,
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

        /// A connection that never closes on its own but whose keepalive
        /// probe reports the link dead (the WS idle-limit shape).
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
            reason = "test transport is Rc-scripted and driven on a LocalSet"
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

    impl LinkConn for ScriptConn {
        async fn send_frame(&mut self, _frame: &[u8]) -> Result<(), String> {
            Ok(())
        }

        fn bootstrap_limits(&self) -> BootstrapLimits {
            BootstrapLimits::default()
        }

        fn bootstrap_profile(&self) -> BootstrapProfile {
            BootstrapProfile::SynthesizedVtRaw
        }

        async fn recv_frame(&mut self) -> Result<Option<Vec<u8>>, String> {
            match &mut self.closed {
                Some(rx) => rx.recv().await.map_or(Ok(None), Err),
                None => std::future::pending().await,
            }
        }

        async fn keepalive(&mut self) -> Result<(), String> {
            self.keepalive_error.clone().map_or(Ok(()), Err)
        }
    }

    /// A live relay handle + mailbox pair for driving [`run_link`]; the
    /// handle must stay alive or the supervisor treats the relay as
    /// abandoned and exits.
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

    /// Poll `statuses` (under paused time) until `want` matches. The
    /// window is 60s of virtual time — comfortably past the keepalive
    /// interval and idle limit, which paused-time tests must outlast.
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

                // Let the connection prove stability (paused time makes
                // this instant), then drop the link: the redial must
                // start from the base delay again (the stable connection
                // reset the failure streak).
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
                // Models an ssh dial whose spawn succeeds but whose auth
                // is refused: the "connection" is established, then dies
                // immediately (dropped sender => recv_frame resolves at
                // once with a clean close). The failure streak must keep
                // growing — a fast death is a failed attempt, not a
                // success that resets the backoff to its base.
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
                // Routable QUIC endpoint with no auth material: refused.
                // Dropping the relay handle up front lets the supervisor's
                // fail-fast drain loop observe an abandoned relay and exit.
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

                // A command through the refused link resolves with a typed
                // error — no dial, no hang.
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

                // Rotate the token, drop the link: the redial must present
                // the new token without a hub restart (ADR-0038). Wait for
                // the drop to be observed (Backoff) before waiting for the
                // reconnect, or the first Connected status matches again.
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
                // First connection stays readable forever but its liveness
                // probe reports the link dead (a silent partition on a WS
                // link: no FIN/RST, no inbound traffic, idle limit trips).
                // The supervisor must tear it down and redial rather than
                // trust `Connected` forever.
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
        assert!(ws_idle_error(LINK_IDLE_TIMEOUT - Duration::from_secs(1)).is_none());
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
                        tokio_tungstenite::tungstenite::Message::Binary(hello_ok),
                    )
                    .await
                    .expect("send HELLO_OK");
                    // Hold the connection until the test asks us to drop it,
                    // then close listener + connection so the redial fails.
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
                    NetLinkTransport::from_env(),
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

    // --- ssh-stdio integration: the real transport over a stub program ---
    //
    // The environment cannot be assumed to have non-interactive `ssh
    // localhost` (keys, host trust, sshd), so these tests exercise the
    // REAL `NetLinkTransport` ssh path against a stub program — the same
    // `$PHUX_SSH` seam an operator uses for an OpenSSH wrapper. The stub
    // records the argv it was spawned with (proving the shell-free,
    // `--`-guarded command line end to end) and its exit is the drop
    // signal, exactly as an exiting ssh would be. The stdio *bridge* half
    // (bytes actually splicing to a UDS) is exercised in
    // `crates/phux/tests/stdio_bridge_e2e.rs` against the real binary.

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

                // The stub saw exactly the argv the planner built — one
                // argument per line, host after `--`, no shell expansion.
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
        // Regression (phux-v45.9): a remote that emits more than the OS
        // pipe buffer (~64KiB) to fd 2 must not wedge the link. If the hub
        // only drained stderr at stdout EOF, this child would block on its
        // stderr write at ~64KiB and never exit — stdout would never close,
        // recv_frame would block forever, and the link would sit
        // `Connected` with no loss signal. The background drainer keeps fd 2
        // flowing, so the child runs to completion and the bounded tail
        // still keeps the *end* of the stream (the marker + exit code).
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                // ~200KiB of fd-2 chatter (well past one pipe buffer), then
                // a marker and a nonzero exit. No stdout: it closes on exit.
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

                // Reaching Backoff at all proves no stall; the tail keeping
                // the end proves stderr was drained the whole way to EOF.
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
                // The stub bridges nothing but honestly models a live ssh:
                // it stays up reading stdin (which the hub holds open) and
                // exits only when the pipe closes or it is killed
                // (kill_on_drop at cancellation).
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
