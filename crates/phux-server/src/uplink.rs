//! Satellite uplinks: dialing out to a hub (ADR-0180, `workload-auth.md`
//! §8.3).
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
use phux_dial::{CertTrust, QuicDial, TlsClientIdentity};
use phux_protocol::policy::SATELLITE_ALPN;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

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

/// The prefix of every join code.
pub const JOIN_PREFIX: &str = "phux://satellite?";

/// What `phux host ticket` prints and `phux host join` takes.
///
/// Where the hub listens, the CA it is pinned by, the satellite name, and
/// the single-use enrollment ticket. The ticket is a secret; `Debug`
/// withholds it.
#[derive(Clone, PartialEq, Eq)]
pub struct JoinCode {
    /// The hub's QUIC listener, `HOST:PORT`.
    pub hub: String,
    /// The TLS server name to offer, when not `hub`'s host.
    pub server_name: Option<String>,
    /// The hub's workload CA, canonical `sha256:` form.
    pub ca: String,
    /// The satellite name the ticket enrolls.
    pub name: String,
    /// The ticket, 64 lowercase hex digits.
    pub ticket: String,
}

impl std::fmt::Debug for JoinCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JoinCode")
            .field("hub", &self.hub)
            .field("server_name", &self.server_name)
            .field("ca", &self.ca)
            .field("name", &self.name)
            .field("ticket", &"[withheld]")
            .finish()
    }
}

impl JoinCode {
    /// The code as one link: `phux://satellite?hub=…[&sni=…]&ca=…&name=…&enroll=…`.
    #[must_use]
    pub fn to_link(&self) -> String {
        let mut link = format!("{JOIN_PREFIX}hub={}", percent_encode(&self.hub));
        if let Some(sni) = &self.server_name {
            link.push_str("&sni=");
            link.push_str(&percent_encode(sni));
        }
        link.push_str("&ca=");
        link.push_str(&self.ca);
        link.push_str("&name=");
        link.push_str(&percent_encode(&self.name));
        link.push_str("&enroll=");
        link.push_str(&self.ticket);
        link
    }

    /// Parse a code [`Self::to_link`] printed. Every field but `sni` is
    /// required; unknown keys are ignored and a repeated key is refused.
    ///
    /// # Errors
    ///
    /// Wording naming the field at fault, never the ticket.
    pub fn parse(text: &str) -> Result<Self, String> {
        let trimmed = text.trim().trim_matches(|c| c == '\'' || c == '"');
        let query = trimmed
            .strip_prefix(JOIN_PREFIX)
            .ok_or_else(|| format!("a join code starts with `{JOIN_PREFIX}`"))?;
        let mut fields: BTreeMap<&str, String> = BTreeMap::new();
        for pair in query.split('&').filter(|pair| !pair.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            if !matches!(key, "hub" | "sni" | "ca" | "name" | "enroll") {
                continue;
            }
            if fields.insert(key, percent_decode(value)?).is_some() {
                return Err(format!("the join code repeats `{key}`"));
            }
        }
        let mut take = |key: &str| {
            fields
                .remove(key)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("the join code has no `{key}`"))
        };
        let hub = take("hub")?;
        split_host_port(&hub)?;
        let ca = take("ca")?;
        let ca = phux_config::known_authorities::canonical_authority(&ca)
            .ok_or_else(|| "the join code's `ca` is not a sha256: fingerprint".to_owned())?;
        let name = take("name")?;
        crate::workload::validate_satellite_name(&name).map_err(|err| err.to_string())?;
        let ticket = take("enroll")?;
        if ticket.len() != 64 || !ticket.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("the join code's ticket is not 64 hex digits".to_owned());
        }
        let server_name = fields.remove("sni").filter(|sni| !sni.is_empty());
        Ok(Self {
            hub,
            server_name,
            ca,
            name,
            ticket: ticket.to_ascii_lowercase(),
        })
    }
}

/// What [`join`] enrolled and stored.
#[derive(Debug, Clone)]
pub struct Joined {
    /// The `[[uplink]]` entry naming it, ready to write to `config.toml`.
    pub entry: UplinkConfigEntry,
    /// The credential id the hub recorded.
    pub credential: String,
}

/// Enroll a fresh key with `code`'s ticket and store it in `dir`.
///
/// The exchange runs over the hub's enrollment ALPN; the reply must be issued
/// by the pinned CA for this key. The pair is stored owner-only in `dir`,
/// which is created owner-only if missing.
///
/// # Errors
///
/// Wording for the operator: an unreachable hub, a refused ticket (consumed,
/// expired, revoked, or unknown), a reply that fails a check, or a storage
/// failure. Never the ticket or the key.
pub async fn join(code: &JoinCode, dir: &Path) -> Result<Joined, String> {
    let (host, port) = split_host_port(&code.hub)?;
    let addr = resolve(&host, port).await?;
    let request = crate::workload::ClientRequest::generate()
        .map_err(|err| format!("could not generate a key: {err}"))?;
    let csr = match crate::workload::ClientMaterial::from_pem(request.csr_pem().as_bytes()) {
        Ok(crate::workload::ClientMaterial::Request(der)) => der.as_ref().to_vec(),
        _ => return Err("could not encode the enrollment request".to_owned()),
    };
    let ticket = hex::decode(&code.ticket)
        .map_err(|_| "the join code's ticket is not 64 hex digits".to_owned())?;
    let dial = phux_dial::enroll::EnrollDial {
        addr,
        server_name: code.server_name.clone().unwrap_or(host),
        trust: CertTrust::Authority {
            ca: code.ca.clone(),
            leaf: None,
        },
        inner: None,
    };
    let chain = tokio::time::timeout(
        JOIN_DEADLINE,
        phux_dial::enroll::enroll(&dial, &phux_protocol::enroll::Request { ticket, csr }),
    )
    .await
    .map_err(|_| format!("the hub at {} did not answer in time", code.hub))?
    .map_err(|err| err.to_string())?;
    let issued = request.accept(&chain).map_err(|err| err.to_string())?;
    if issued.ca_fingerprint() != code.ca {
        return Err("the issuing CA is not the one the join code pins".to_owned());
    }
    ensure_private_dir(dir)?;
    let stem = format!(
        "uplink.{}.{}",
        file_safe(&code.name),
        issued
            .credential_id()
            .trim_start_matches("sha256:")
            .get(..16)
            .unwrap_or_default()
    );
    let client_cert = dir.join(format!("{stem}.pem"));
    let client_key = dir.join(format!("{stem}.key"));
    issued
        .store(&client_key, &client_cert)
        .map_err(|err| format!("could not store the satellite identity: {err}"))?;
    Ok(Joined {
        entry: UplinkConfigEntry {
            name: code.name.clone(),
            hub: code.hub.clone(),
            server_name: code.server_name.clone(),
            ca_fingerprint: code.ca.clone(),
            client_cert,
            client_key,
            enabled: true,
        },
        credential: issued.credential_id().to_owned(),
    })
}

fn ensure_private_dir(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt as _;
    if dir.is_dir() {
        return Ok(());
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|err| format!("could not create {}: {err}", dir.display()))
}

/// A satellite name as a file-name component: anything outside
/// `[A-Za-z0-9._-]` becomes `_`.
fn file_safe(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

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

/// Validate every enabled entry (no DNS, no file I/O).
///
/// # Errors
///
/// The first entry that cannot be dialed safely.
pub fn plan_uplinks(entries: &[UplinkConfigEntry]) -> Result<Vec<UplinkSpec>, UplinkError> {
    entries
        .iter()
        .filter(|entry| entry.enabled)
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

/// Split `HOST:PORT` (IPv6 bracketed), refusing an empty host or port 0.
fn split_host_port(hub: &str) -> Result<(String, u16), String> {
    let malformed = || format!("{hub:?} is not HOST:PORT");
    let (host, port) = match hub.strip_prefix('[') {
        Some(rest) => rest.split_once("]:").ok_or_else(malformed)?,
        None => hub.rsplit_once(':').ok_or_else(malformed)?,
    };
    let port = port.parse::<u16>().ok().filter(|port| *port != 0);
    match port {
        Some(port) if !host.is_empty() && !host.contains(['/', '[', ']']) => {
            Ok((host.to_owned(), port))
        }
        _ => Err(malformed()),
    }
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
                    leaf: None,
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
fn percent_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~' | b':' | b'[' | b']')
        {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(byte >> 4)]));
            out.push(char::from(HEX[usize::from(byte & 0x0F)]));
        }
    }
    out
}

fn percent_decode(value: &str) -> Result<String, String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let byte = value
                .get(index + 1..index + 3)
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                .ok_or_else(|| "the join code has a malformed %-escape".to_owned())?;
            out.push(byte);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "the join code is not UTF-8".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code() -> JoinCode {
        JoinCode {
            hub: "[::1]:8788".to_owned(),
            server_name: Some("hub.example".to_owned()),
            ca: format!("sha256:{}", "ab".repeat(32)),
            name: "sandbox one".to_owned(),
            ticket: "cd".repeat(32),
        }
    }

    #[test]
    fn a_join_code_round_trips_and_hides_its_ticket() {
        let code = code();
        let link = code.to_link();
        assert!(link.starts_with(JOIN_PREFIX), "{link}");
        assert!(link.contains("name=sandbox%20one"), "{link}");
        assert_eq!(JoinCode::parse(&format!("'{link}'\n")), Ok(code.clone()));
        assert!(!format!("{code:?}").contains(&code.ticket));
    }

    #[test]
    fn a_join_code_refuses_missing_or_bad_fields() {
        let link = code().to_link();
        for (from, to) in [
            ("hub=", "hob="),
            ("&ca=sha256:", "&ca=md5:"),
            ("enroll=cdcd", "enroll=zzcd"),
            ("name=sandbox%20one", "name=a%2Fb"),
            ("hub=[::1]:8788", "hub=[::1]:0"),
        ] {
            let broken = link.replacen(from, to, 1);
            assert!(JoinCode::parse(&broken).is_err(), "{broken}");
        }
        assert!(JoinCode::parse("https://example/connect?x=1").is_err());
        assert!(JoinCode::parse(&format!("{link}&name=again")).is_err());
    }

    #[test]
    fn host_port_splits_bracketed_and_refuses_junk() {
        assert_eq!(split_host_port("hub:1"), Ok(("hub".to_owned(), 1)));
        assert_eq!(split_host_port("[::1]:9"), Ok(("::1".to_owned(), 9)));
        for bad in ["hub", ":1", "hub:0", "hub:x", "[::1:9", "a/b:1"] {
            assert!(split_host_port(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn plans_refuse_relative_identities_and_skip_disabled_entries() {
        let entry = |enabled: bool, cert: &str| UplinkConfigEntry {
            name: "sandbox".to_owned(),
            hub: "hub:8788".to_owned(),
            server_name: None,
            ca_fingerprint: format!("sha256:{}", "ab".repeat(32)),
            client_cert: PathBuf::from(cert),
            client_key: PathBuf::from("/s/u.key"),
            enabled,
        };
        let plans = plan_uplinks(&[entry(true, "/s/u.pem"), entry(false, "relative")]).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].server_name, "hub");
        assert!(plan_uplinks(&[entry(true, "relative.pem")]).is_err());
    }
}
