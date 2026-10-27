//! Satellite uplinks: dialing out to a hub (ADR-0182, `docs/remote.md#linking-machines`).
//!
//! A server the hub cannot reach joins as a satellite by dialing the hub. On
//! the hub, `phux host ticket NAME` mints a [`JoinCode`]; on this server,
//! [`join`] enrolls a key it generates with that code's single-use ticket
//! over the enrollment ALPN and stores the issued identity owner-only. Each
//! `[[uplink]]` entry then gets one supervisor ([`Uplinks`]) that keeps a
//! QUIC connection to the hub under [`SATELLITE_ALPN`], presenting that
//! identity and pinning the hub's CA. The hub opens the streams. Each one is
//! an ordinary phux connection, spliced byte for byte to this server's own
//! owner socket, so the hub holds this server owner's authority, as an
//! `ssh://` hub does through `phux stdio-bridge`.
//!
//! A lost or refused connection is redialed with the hub links' capped
//! backoff, re-reading the identity files every attempt. The config-reload
//! doorbell re-reads `[[uplink]]` and starts, stops, or redials exactly the
//! uplinks that changed.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use phux_config::UplinkConfigEntry;
use phux_config::uplink::split_host_port;
use phux_dial::{CertTrust, QuicDial, TlsClientIdentity};
use phux_protocol::policy::SATELLITE_ALPN;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

mod enroll;

pub use enroll::{JOIN_PREFIX, JoinCode, Joined, join};

use crate::hub::link::Backoff;

const BACKOFF_BASE: Duration = Duration::from_millis(500);
const BACKOFF_CAP: Duration = Duration::from_secs(30);

/// How long joining may wait for the hub's enrollment answer.
const JOIN_DEADLINE: Duration = Duration::from_secs(15);

/// Streams the hub may hold open on one uplink at once: it opens one per
/// link session, so this only bounds a misbehaving hub.
const MAX_HUB_STREAMS: usize = 16;

/// The QUIC listener's `AUTH_FAILED` application close.
const AUTH_FAILED_CODE: u64 = 0x01;

/// Why an `[[uplink]]` entry cannot be supervised; refused before anything
/// binds.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("uplink {name:?} to {hub:?}: {reason}")]
pub struct UplinkError {
    /// The entry's satellite name.
    pub name: String,
    /// The entry's hub endpoint.
    pub hub: String,
    /// What is wrong with it.
    pub reason: String,
}

/// A validated uplink dial plan. The identity stays on disk, re-read every
/// attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UplinkSpec {
    name: String,
    hub: String,
    host: String,
    port: u16,
    server_name: String,
    ca: String,
    client_cert: PathBuf,
    client_key: PathBuf,
}

impl UplinkSpec {
    /// The key a reload diffs on: one supervisor per name and hub.
    fn key(&self) -> String {
        format!("{}@{}", self.name, self.hub)
    }
}

/// Validate every enabled entry the server dials (no DNS, no file I/O); a
/// mail-only entry has no link.
///
/// # Errors
///
/// The first entry that cannot be dialed safely.
pub fn plan_uplinks(entries: &[UplinkConfigEntry]) -> Result<Vec<UplinkSpec>, UplinkError> {
    entries
        .iter()
        .filter(|entry| entry.enabled && !entry.mail_only)
        .map(plan_uplink)
        .collect()
}

fn plan_uplink(entry: &UplinkConfigEntry) -> Result<UplinkSpec, UplinkError> {
    let refused = |reason: String| UplinkError {
        name: entry.name.clone(),
        hub: entry.hub.clone(),
        reason,
    };
    crate::workload::validate_satellite_name(&entry.name)
        .map_err(|err| refused(err.to_string()))?;
    let (host, port) = split_host_port(&entry.hub).map_err(refused)?;
    let ca = phux_config::known_authorities::canonical_authority(&entry.ca_fingerprint)
        .ok_or_else(|| refused("ca-fingerprint is not a sha256: fingerprint".to_owned()))?;
    if !entry.client_cert.is_absolute() || !entry.client_key.is_absolute() {
        return Err(refused(
            "client-cert and client-key must be absolute paths".to_owned(),
        ));
    }
    Ok(UplinkSpec {
        name: entry.name.clone(),
        hub: entry.hub.clone(),
        server_name: entry.server_name.clone().unwrap_or_else(|| host.clone()),
        host,
        port,
        ca,
        client_cert: entry.client_cert.clone(),
        client_key: entry.client_key.clone(),
    })
}

/// Check a hub endpoint the way a join code and an `[[uplink]]` entry read
/// it: `HOST:PORT`, IPv6 bracketed, no scheme.
///
/// # Errors
///
/// Wording naming the malformed endpoint.
pub fn validate_hub_endpoint(hub: &str) -> Result<(), String> {
    split_host_port(hub).map(drop)
}

async fn resolve(host: &str, port: u16) -> Result<SocketAddr, String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, port));
    }
    tokio::net::lookup_host((host, port))
        .await
        .map_err(|err| format!("resolve {host}:{port}: {err}"))?
        .next()
        .ok_or_else(|| format!("resolve {host}:{port}: no addresses"))
}

/// Re-reads `[[uplink]]` for a config-reload doorbell.
#[derive(Clone)]
pub struct UplinkSource(Arc<UplinkReader>);

type UplinkReader = dyn Fn() -> Result<Vec<UplinkConfigEntry>, String> + Send + Sync;

impl UplinkSource {
    /// Wrap the function a reload calls to re-read the entries.
    pub fn new(
        read: impl Fn() -> Result<Vec<UplinkConfigEntry>, String> + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(read))
    }
}

impl std::fmt::Debug for UplinkSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UplinkSource(..)")
    }
}

/// The running uplink supervisors, keyed by name and hub.
#[derive(Debug, Default)]
pub struct Uplinks {
    source: Option<UplinkSource>,
    /// This server's owner socket, every hub stream's destination.
    socket: Option<PathBuf>,
    /// Parent of every supervisor; `None` until the runtime arms them.
    cancel: Option<CancellationToken>,
    running: BTreeMap<String, (UplinkSpec, CancellationToken)>,
}

impl Uplinks {
    /// Arm supervision: streams splice to `socket`, supervisors stop with
    /// `cancel`, and a reload re-reads `source`.
    pub(crate) fn arm(
        &mut self,
        source: Option<UplinkSource>,
        socket: PathBuf,
        cancel: CancellationToken,
    ) {
        self.source = source;
        self.socket = Some(socket);
        self.cancel = Some(cancel);
    }

    /// Run exactly `specs`: start new ones, stop missing ones, and restart
    /// changed ones; an unchanged uplink keeps its connection. Returns how
    /// many were started and stopped.
    pub(crate) fn apply(&mut self, specs: Vec<UplinkSpec>) -> (usize, usize) {
        let (Some(socket), Some(parent)) = (self.socket.clone(), self.cancel.clone()) else {
            return (0, 0);
        };
        let next: BTreeMap<String, UplinkSpec> =
            specs.into_iter().map(|spec| (spec.key(), spec)).collect();
        let mut stopped = 0;
        self.running.retain(|key, (spec, cancel)| {
            let keep = next.get(key) == Some(spec);
            if !keep {
                cancel.cancel();
                stopped += 1;
            }
            keep
        });
        let mut started = 0;
        for (key, spec) in next {
            if self.running.contains_key(&key) {
                continue;
            }
            let cancel = parent.child_token();
            tokio::task::spawn_local(supervise(spec.clone(), socket.clone(), cancel.clone()));
            self.running.insert(key, (spec, cancel));
            started += 1;
        }
        (started, stopped)
    }
}

/// Re-read `[[uplink]]` and apply it (the config-reload doorbell). A source
/// that fails to read or validate keeps the running uplinks.
pub(crate) fn reload_uplinks(state: &crate::state::SharedState) {
    let Some(source) = state.with(|s| s.uplinks().source.clone()) else {
        return;
    };
    let planned =
        (source.0)().and_then(|entries| plan_uplinks(&entries).map_err(|err| err.to_string()));
    match planned {
        Ok(specs) => {
            let (started, stopped) = state.with_mut(|s| s.uplinks_mut().apply(specs));
            if started + stopped > 0 {
                info!(started, stopped, "uplinks reloaded");
            }
        }
        Err(error) => warn!(%error, "uplink reload failed; keeping the running uplinks"),
    }
}

/// Keep one uplink up until `cancel`.
#[allow(
    clippy::future_not_send,
    reason = "ADR-0014: uplink supervisors run on the server's LocalSet"
)]
async fn supervise(spec: UplinkSpec, socket: PathBuf, cancel: CancellationToken) {
    let mut backoff = Backoff::new(BACKOFF_BASE, BACKOFF_CAP);
    loop {
        let attempt = backoff.failures().saturating_add(1);
        let dialed = async {
            let addr = resolve(&spec.host, spec.port).await?;
            let dial = QuicDial {
                addr,
                server_name: spec.server_name.clone(),
                token: None,
                trust: CertTrust::Authority {
                    ca: spec.ca.clone(),
                },
                identity: Some(TlsClientIdentity::PemFiles {
                    certificate: spec.client_cert.clone(),
                    private_key: spec.client_key.clone(),
                }),
                inner: None,
            };
            phux_dial::quic::connect_with_alpn(&dial, SATELLITE_ALPN)
                .await
                .map_err(|err| err.to_string())
        };
        let outcome = tokio::select! {
            () = cancel.cancelled() => return,
            outcome = dialed => outcome,
        };
        let (failed_attempt, error) = match outcome {
            Ok((endpoint, connection)) => {
                info!(satellite = %spec.name, hub = %spec.hub, attempt, "uplink to the hub established");
                let up_at = tokio::time::Instant::now();
                let lost = serve(&connection, &socket, &cancel).await;
                drop(endpoint);
                let Some(lost) = lost else {
                    return;
                };
                (backoff.settle_after_loss(up_at.elapsed()), lost)
            }
            Err(error) => (attempt, error),
        };
        warn!(
            satellite = %spec.name,
            hub = %spec.hub,
            attempt = failed_attempt,
            %error,
            "uplink lost; scheduling redial"
        );
        let delay = backoff.next_delay();
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(delay) => {}
        }
    }
}

/// Serve the hub's streams until the connection ends (`Some(why)`) or
/// `cancel` fires (`None`, after closing it).
async fn serve(
    connection: &quinn::Connection,
    socket: &Path,
    cancel: &CancellationToken,
) -> Option<String> {
    let open = Arc::new(AtomicUsize::new(0));
    loop {
        let accepted = tokio::select! {
            () = cancel.cancelled() => {
                connection.close(0_u32.into(), b"satellite stopping");
                return None;
            }
            accepted = connection.accept_bi() => accepted,
        };
        let (mut send, mut recv) = match accepted {
            Ok(pair) => pair,
            Err(err) => return Some(loss_reason(&err)),
        };
        if open.load(Ordering::SeqCst) >= MAX_HUB_STREAMS {
            debug!("uplink: the hub opened more streams than allowed; refusing one");
            let _ = send.reset(0_u32.into());
            let _ = recv.stop(0_u32.into());
            continue;
        }
        open.fetch_add(1, Ordering::SeqCst);
        let open = Arc::clone(&open);
        let socket = socket.to_path_buf();
        tokio::spawn(async move {
            splice(send, recv, &socket).await;
            open.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

/// Splice one hub stream to the owner socket, byte for byte, until either
/// side ends.
async fn splice(send: quinn::SendStream, recv: quinn::RecvStream, socket: &Path) {
    let mut uds = match tokio::net::UnixStream::connect(socket).await {
        Ok(uds) => uds,
        Err(err) => {
            warn!(socket = %socket.display(), %err, "uplink: cannot reach this server's own socket");
            return;
        }
    };
    let mut hub = tokio::io::join(recv, send);
    if let Err(err) = tokio::io::copy_bidirectional(&mut hub, &mut uds).await {
        debug!(%err, "uplink stream ended");
    }
}

fn loss_reason(err: &quinn::ConnectionError) -> String {
    match err {
        quinn::ConnectionError::ApplicationClosed(close)
            if close.error_code.into_inner() == AUTH_FAILED_CODE =>
        {
            "the hub refused this satellite: its credential is revoked or expired, or the \
             hub has no enabled inbound:// [[satellites]] entry for it"
                .to_owned()
        }
        other => other.to_string(),
    }
}

/// Percent-encode everything outside RFC 3986 `unreserved` and `:`, `[`,
/// `]` (which an endpoint keeps readable).
#[cfg(test)]
mod tests;
