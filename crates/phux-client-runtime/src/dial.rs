//! From a resolved registry entry to a dial.
//!
//! The lower native planners share the CLI's route/pin/token decisions.
//! This adapter resolves addresses, reads token files and words failures.
//! An in-memory token overrides the file without reading it. The token is
//! read just before the dial and dropped after it. The client TLS identity
//! is the one the registry entry enrolled
//! ([`crate::target::Resolved::client_identity`]), else
//! [`phux_dial::TlsClientIdentity::None`]; it is never read from the
//! environment, so an embedder never inherits a launcher shell's workload
//! certificate.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use phux_dial::DialError;
pub use phux_dial::quic::{InnerTls, QuicDial};
use phux_dial::ws::WsDial;
pub use phux_dial::{CertTrust, PlanRefusal, WsTarget};

use crate::reconnect::HOST_REFUSED_TOKEN;
use crate::target::{Resolved, read_token};

/// A planned dial: how to reach one server, trust already decided.
///
/// [`crate::Target::plan`] produces one under the registry's trust rules;
/// a caller that planned its own (a CLI flag naming a pin) builds it
/// directly. The reconnecting driver and the one-shot
/// [`crate::session::Session`] both establish through it.
#[derive(Debug, Clone)]
pub enum Dial {
    /// Connect over the Unix domain socket at this path.
    Uds(PathBuf),
    /// A Linux Unix socket bridged over a native Windows WSL child's stdio.
    #[cfg(windows)]
    Wsl {
        /// Absolute Linux server socket path.
        socket: PathBuf,
        /// The host-selected Linux phux executable.
        phux: PathBuf,
    },
    /// The protocol over `ssh DESTINATION REMOTE_PHUX stdio-bridge`, for a
    /// host UDP cannot reach. ssh authenticates it.
    Ssh {
        /// The ssh destination, passed to ssh verbatim.
        destination: String,
        /// The `phux` to run on the host.
        remote_phux: String,
        /// [`Target::ssh_askpass`](crate::connection::Target::ssh_askpass):
        /// how a login this lane makes itself asks.
        askpass: Option<PathBuf>,
    },
    /// Dial a QUIC listener; the plan owns its bearer token.
    Quic(QuicDial),
    /// Dial a WebSocket listener; the plan owns its bearer token.
    Ws(WsDial),
}

impl Dial {
    /// A [`Dial::Uds`] for `path`.
    #[must_use]
    pub fn uds(path: &Path) -> Self {
        Self::Uds(path.to_path_buf())
    }
}

/// Why a dial could not be planned.
#[derive(Debug)]
pub(crate) enum PlanError {
    /// Configuration no retry changes: a missing pin or token, a bad
    /// endpoint.
    Refused(String),
    /// The host's name did not resolve; it may once its network is up
    /// (Tailscale `MagicDNS` still starting).
    Unresolved(String),
}

impl From<String> for PlanError {
    fn from(message: String) -> Self {
        Self::Refused(message)
    }
}

impl From<PlanError> for String {
    fn from(error: PlanError) -> Self {
        match error {
            PlanError::Refused(message) | PlanError::Unresolved(message) => message,
        }
    }
}

/// Bound on name resolution plus transport establishment, so "connecting"
/// never sits through a 75-second OS connect timeout.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(15);

/// Plan a QUIC dial with an optional in-memory token overriding the registry
/// file. Native embedders use this for Keychain-backed credentials.
pub(crate) async fn plan_quic_with_token(
    resolved: &Resolved,
    authority: &str,
    token: Option<String>,
) -> Result<QuicDial, PlanError> {
    let name = resolved.name.as_str();
    let (bare, addr) = resolve_quic_addr(name, authority).await?;
    // An in-memory override must not even read a stale/inaccessible token
    // file (Keychain-backed embedders deliberately have no such file).
    let token = match token {
        Some(token) => Some(token),
        None => load_token(resolved)?,
    };
    let route = resolved
        .authority
        .route
        .as_deref()
        .or(resolved.tls_server_name.as_deref());
    QuicDial::plan(
        addr,
        quic_server_name(bare),
        token.as_deref(),
        resolved.cert_fingerprint.as_deref(),
        resolved.authority.ca.as_deref(),
        route,
        Some(resolved.client_identity.clone()),
    )
    .map_err(|err| native_refusal(resolved, authority, &err).into())
}

/// The certificate authority the phux server at `authority` (`HOST:PORT`)
/// presents, to pin before dialing a machine on your tailnet that was never
/// paired (`docs/config.md#tailnet-owner-devices`).
///
/// Only a name that resolves to a tailnet address is probed: the tailnet
/// already vouches for the address, as `phux host add
/// --accept-new-host-key` relies on, and the pin then guards every later
/// dial. Nothing is sent: the handshake is refused on a pin no authority
/// matches, and the refusal names the presented one.
///
/// # Errors
///
/// A name off the tailnet, an unreachable server, or one presenting no
/// authority.
pub async fn tailnet_authority(authority: &str) -> Result<String, String> {
    let (bare, addr) = resolve_quic_addr(authority, authority).await?;
    if !phux_config_core::remote::is_tailnet(addr.ip()) {
        return Err(format!(
            "{bare} is not on your tailnet; pair it with phux pair instead"
        ));
    }
    let dial = QuicDial {
        addr,
        server_name: quic_server_name(bare),
        token: None,
        trust: CertTrust::Authority {
            ca: format!("sha256:{}", "0".repeat(64)),
        },
        inner: None,
        identity: Some(phux_dial::TlsClientIdentity::None),
    };
    let probe = phux_dial::quic::connect_with_alpn(&dial, phux_protocol::policy::QUIC_ALPN);
    match tokio::time::timeout(DIAL_TIMEOUT, probe).await {
        Ok(Err(DialError::AuthorityChanged(change))) => change.presented.ok_or_else(|| {
            format!("{bare} presents no certificate authority; pair it with phux pair instead")
        }),
        Ok(Err(err)) => Err(format!("{bare}: {err}")),
        Ok(Ok(_)) => Err(format!("{bare}: the probe was not refused")),
        Err(_) => Err(format!("{bare} did not answer; is phux running there?")),
    }
}

/// [`tailnet_authority`] on a runtime of its own, for an embedder's thread.
///
/// # Errors
///
/// As [`tailnet_authority`], or no runtime.
pub fn tailnet_authority_blocking(authority: &str) -> Result<String, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("probe runtime: {err}"))?
        .block_on(tailnet_authority(authority))
}

/// Why a relay route without the server's CA is not dialed, and the fix.
///
/// Transport-neutral: native embedders (phux-mobile through the FFI) get
/// this text too, so it names no CLI command; the CLI appends its own.
#[must_use]
pub fn unpinned_relay_route(name: &str, route: &str) -> String {
    format!(
        "{name} is reached through relay route {route:?}, but no certificate authority is \
         pinned for the server behind it, so the relay could read the session; pin the \
         server's CA from its relay link and dial again"
    )
}

/// Split `HOST:PORT` and resolve it to its first address, as the CLI's
/// `resolve_quic_target` does. Returns the unbracketed host beside it.
async fn resolve_quic_addr<'a>(
    name: &str,
    authority: &'a str,
) -> Result<(&'a str, SocketAddr), PlanError> {
    let (host, port) = authority
        .rsplit_once(':')
        .and_then(|(host, port)| Some((host, port.parse::<u16>().ok()?)))
        .ok_or_else(|| format!("{name}: endpoint quic://{authority} needs HOST:PORT"))?;
    let bare = host.trim_matches(['[', ']']);
    let addr = tokio::net::lookup_host((bare, port))
        .await
        .map_err(|err| {
            PlanError::Unresolved(format!(
                "{name}: could not resolve {bare}: {err}; is its network connected?"
            ))
        })?
        .next()
        .ok_or_else(|| PlanError::Unresolved(format!("{name}: {bare} resolved to no addresses")))?;
    Ok((bare, addr))
}

/// Conventional SNI for a DNS name; the historical `localhost` for an IP
/// literal, matching the server's self-signed SANs (as the CLI does).
fn quic_server_name(bare: &str) -> String {
    if bare.parse::<IpAddr>().is_ok() {
        "localhost".to_owned()
    } else {
        bare.to_owned()
    }
}

/// Plan the WebSocket dial for `url`: a routable target must be `wss://`,
/// pinned, and carry a token; loopback may be plaintext with neither.
pub(crate) fn plan_ws(
    resolved: &Resolved,
    url: &str,
    token: Option<String>,
) -> Result<WsDial, String> {
    let name = resolved.name.as_str();
    let target = WsTarget::parse(url).map_err(|err| format!("{name}: {err}"))?;
    let (trust, token) = target
        .credentials(
            resolved
                .authority
                .trust(resolved.cert_fingerprint.as_deref()),
            token,
            resolved.authority.route.as_deref(),
        )
        .map_err(|err| native_refusal(resolved, url, &err))?;
    Ok(WsDial {
        url: url.to_owned(),
        token,
        trust,
        tls_server_name: resolved.tls_server_name.clone(),
        // Explicit, so the plan never falls back to the environment.
        identity: Some(resolved.client_identity.clone()),
    })
}

fn native_refusal(resolved: &Resolved, endpoint: &str, error: &PlanRefusal) -> String {
    let name = &resolved.name;
    match error {
        PlanRefusal::Unpinned => unpinned(name),
        PlanRefusal::BadToken => bad_token(resolved),
        PlanRefusal::NoToken => format!(
            "{name} has no token file in the registry; re-pair it with `phux host add {name}`"
        ),
        PlanRefusal::Plaintext => {
            format!("{name}: {endpoint} is plaintext; a routable WebSocket needs wss://")
        }
        PlanRefusal::UnpinnedRelay => unpinned_relay_route(
            name,
            resolved
                .authority
                .route
                .as_deref()
                .or(resolved.tls_server_name.as_deref())
                .unwrap_or(""),
        ),
        PlanRefusal::RelayRequiresQuic => format!("{name}: a relay route needs a quic:// endpoint"),
    }
}

/// Read the bearer token for this dial. Failures name the file, never bytes.
pub(crate) fn load_token(resolved: &Resolved) -> Result<Option<String>, String> {
    read_token(resolved.token_file.as_deref()).map_err(|err| format!("{}: {err}", resolved.name))
}

/// Fixed wording, as the CLI's: a decoder's message can quote a character
/// of the token, and the token file's path is all an operator needs.
fn bad_token(resolved: &Resolved) -> String {
    resolved.token_file.as_ref().map_or_else(
        || format!("{}: the token is not valid hex", resolved.name),
        |path| {
            format!(
                "{}: the token file {} is not valid hex",
                resolved.name,
                path.display()
            )
        },
    )
}

/// The remedy for a routable host with no pin in the registry.
#[must_use]
pub fn unpinned(name: &str) -> String {
    format!("{name} has no certificate pin in the registry; re-pair it with `phux host add {name}`")
}

/// A dial that outlasted `timeout`.
#[must_use]
pub fn timed_out(name: &str, timeout: Duration) -> String {
    format!(
        "{name} did not answer within {}s; check that it is up and its network is connected",
        timeout.as_secs_f64()
    )
}

/// Word a dial failure the way an operator can act on it: reachability
/// versus credentials, the split `DialError` already draws.
#[must_use]
pub fn dial_message(name: &str, err: &DialError) -> String {
    match err {
        DialError::Unreachable(detail) => format!(
            "{name} did not answer ({detail}); check that it is up and its network is connected"
        ),
        DialError::Connect(detail) => format!("{name}: {detail}"),
        DialError::AuthRefused(detail) => format!(
            "{name} {HOST_REFUSED_TOKEN} ({detail}); re-pair it with `phux host add {name}`"
        ),
        DialError::Io(detail) => format!("{name}: {detail}"),
        DialError::Stalled(detail) => format!("{name} stopped answering ({detail})"),
        DialError::AuthorityChanged(change) => authority_changed_message(name, change),
    }
}

/// The `authority_changed` refusal (ADR-0153): both fingerprints, why it is
/// refused, and the re-pair that accepts a rotation the operator made.
#[must_use]
pub(crate) fn authority_changed_message(name: &str, change: &phux_dial::AuthorityChange) -> String {
    format!(
        "{name}: {change}. Refusing to connect: a rotated authority and an impostor look \
         the same from here. If the host's operator rotated it (`phux workload authority \
         --rotate`), re-pair: `phux host add {name}`"
    )
}

#[cfg(test)]
mod tests;
