//! Remote dial planning, credentials, and trust.
use std::process::ExitCode;

use phux_client::attach::{CertTrust, Dial, QuicDial, WsDial};
use super::OVERLAY_REACHABILITY_HINT;
/// Refuse a WebSocket dial that leaves the machine without a TLS pin or a
/// bearer token (or at all in plaintext), naming the flag that clears each.
fn require_ws_dial_credentials(
    target: &phux_dial::ws::WsTarget,
    url: &str,
    token: Option<&str>,
    cert_fingerprint: Option<&str>,
) -> Result<(), DialRefusal> {
    if target.is_loopback() {
        return Ok(());
    }
    let url = url.to_owned();
    if !target.secure {
        return Err(DialRefusal::Plaintext { url });
    }
    if cert_fingerprint.is_none() {
        return Err(DialRefusal::UnpinnedWs { url });
    }
    if token.is_none() {
        return Err(DialRefusal::NoToken { url });
    }
    Ok(())
}

/// Check that a supplied bearer token is well-formed hex before it is dialed
/// with, and normalize its surrounding whitespace away.
fn validated_ws_token(token: Option<String>) -> Result<Option<String>, DialRefusal> {
    let Some(token) = token else {
        return Ok(None);
    };
    phux_dial::quic::parse_token_hex(&token)
        .map(|_| Some(token.trim().to_owned()))
        .map_err(|err| DialRefusal::BadToken(err.to_string()))
}
/// A remote dial ready to connect, plus whether it stays on this machine
/// (the failure hints key on it). Shared by `attach --quic/--ws` and the
/// headless verbs' `--remote`.
pub(crate) struct DialPlan {
    pub(crate) dial: Dial,
    pub(crate) loopback: bool,
}

impl DialPlan {
    /// Present `identity` on this dial's TLS handshake: a registry entry's
    /// enrolled workload certificate. `None` leaves the dial reading
    /// `PHUX_WORKLOAD_CERT` / `PHUX_WORKLOAD_KEY` as it always has.
    pub(crate) fn with_identity(mut self, identity: Option<phux_dial::TlsClientIdentity>) -> Self {
        match &mut self.dial {
            Dial::Quic(quic) => quic.identity = identity,
            Dial::Ws(ws) => ws.identity = identity,
            _ => {}
        }
        self
    }

    /// Pin the server's certificate authority in place of the leaf pin this
    /// plan was built with (ADR-0153), when pairing recorded one. A plan with
    /// no leaf pin (loopback) is left as it is. Through a relay route the
    /// leaf pin stays the relay's and the dial runs an end-to-end session to
    /// the server inside the stream, presenting the workload identity there
    /// (ADR-0180).
    ///
    /// # Errors
    ///
    /// A relay route with no CA pinned for the server behind it (the relay
    /// would read the session), or a half-configured environment identity.
    /// `name` labels that refusal: the registry entry, else the target as
    /// typed.
    pub(crate) fn with_authority(
        mut self,
        name: &str,
        pin: &phux_client_runtime::target::AuthorityPin,
    ) -> Result<Self, DialRefusal> {
        let trust = match &mut self.dial {
            Dial::Quic(quic) => &mut quic.trust,
            Dial::Ws(ws) => &mut ws.trust,
            _ => return Ok(self),
        };
        if let CertTrust::Pinned(leaf) = trust
            && let Some(pinned) = pin.trust(Some(leaf.as_str()))
        {
            *trust = pinned;
        }
        let Dial::Quic(quic) = &mut self.dial else {
            return Ok(self);
        };
        let Some(route) = pin.route.as_deref() else {
            return Ok(self);
        };
        let identity = match quic.identity.clone() {
            Some(identity) => identity,
            None => phux_dial::tls::client_identity_from_env()
                .map_err(|err| DialRefusal::Malformed(err.to_string()))?,
        };
        let inner = pin.inner(&identity).ok_or_else(|| {
            DialRefusal::Malformed(format!(
                "{} (`phux attach --remote NAME --code LINK` with a link from `phux pair \
                 --relay-route {route}` on the server, or `phux host add ... --ca-fingerprint`)",
                phux_client_runtime::dial::unpinned_relay_route(name, route),
            ))
        })?;
        quic.inner = Some(inner);
        quic.identity = Some(phux_dial::TlsClientIdentity::None);
        Ok(self)
    }
}

/// Why a remote dial could not be planned; each caller words its own
/// remedy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DialRefusal {
    /// The endpoint text did not parse: a bad `HOST:PORT` or URL.
    Malformed(String),
    /// The host did not resolve. `dns_name` is false for an IP literal,
    /// which never touches DNS and so never earns the overlay hint.
    Unresolved {
        target: String,
        detail: String,
        dns_name: bool,
    },
    /// A routable QUIC endpoint with no certificate pin.
    UnpinnedQuic { target: String },
    /// A routable `wss://` endpoint with no certificate pin.
    UnpinnedWs { url: String },
    /// Plaintext `ws://` to a routable address.
    Plaintext { url: String },
    /// A routable `wss://` endpoint with no bearer token.
    NoToken { url: String },
    /// The bearer token is not well-formed hex.
    BadToken(String),
}

impl DialRefusal {
    /// The lines `phux attach --quic/--ws` prints for this refusal, the same
    /// wording the planners printed before they returned a typed error.
    pub(super) fn attach_lines(&self) -> Vec<String> {
        match self {
            Self::Malformed(err) | Self::BadToken(err) => vec![err.to_owned()],
            Self::Unresolved {
                target,
                detail,
                dns_name,
            } => {
                let mut lines = vec![format!("QUIC attach to {target} failed: {detail}")];
                if *dns_name {
                    lines.push(OVERLAY_REACHABILITY_HINT.to_owned());
                }
                lines
            }
            Self::UnpinnedQuic { target } => vec![
                format!(
                    "refusing to dial non-loopback QUIC server {target} without --cert-fingerprint."
                ),
                "Run `phux pair` on the server host to print its certificate fingerprint,"
                    .to_owned(),
                format!("then pass it: phux attach --quic {target} --cert-fingerprint <FP>"),
            ],
            Self::UnpinnedWs { url } => vec![
                format!(
                    "refusing to dial non-loopback WebSocket server {url} without --cert-fingerprint."
                ),
                "Run `phux pair` on the server host, then pass the printed fingerprint.".to_owned(),
            ],
            Self::Plaintext { url } => vec![
                format!("refusing plaintext WebSocket attach to non-loopback URL {url}."),
                "Use wss:// plus `phux pair` credentials for remote devices.".to_owned(),
            ],
            Self::NoToken { url } => vec![
                format!("refusing remote WebSocket attach to {url} without --token."),
                "Run `phux pair` on the server host and pass the printed token once.".to_owned(),
            ],
        }
    }

    /// Print the attach wording and return the failure exit code.
    pub(crate) fn report_for_attach(&self) -> ExitCode {
        crate::output::error_block(self.attach_lines().iter().map(String::as_str));
        ExitCode::FAILURE
    }
}

/// Resolve `target` and build its QUIC dial; trust keys on the resolved
/// address.
pub(crate) fn plan_quic_dial(
    rt: &tokio::runtime::Runtime,
    target: &str,
    token: Option<String>,
    cert_fingerprint: Option<String>,
    server_name: Option<String>,
) -> Result<DialPlan, DialRefusal> {
    let (addr, default_server_name) = resolve_quic_target(rt, target)?;
    let loopback = addr.ip().is_loopback();
    let trust = quic_trust(target, cert_fingerprint, loopback)?;
    let token = parsed_quic_token(token)?;
    Ok(DialPlan {
        dial: Dial::Quic(QuicDial {
            addr,
            server_name: server_name.unwrap_or(default_server_name),
            token,
            trust,
            identity: None,
            inner: None,
        }),
        loopback,
    })
}

/// Pin the certificate when a fingerprint was given, trust loopback's
/// self-signed dev cert, and refuse an unpinned routable dial.
fn quic_trust(
    target: &str,
    cert_fingerprint: Option<String>,
    loopback: bool,
) -> Result<CertTrust, DialRefusal> {
    if let Some(fingerprint) = cert_fingerprint {
        return Ok(CertTrust::Pinned(fingerprint));
    }
    if loopback {
        return Ok(CertTrust::SkipVerify);
    }
    Err(DialRefusal::UnpinnedQuic {
        target: target.to_owned(),
    })
}

/// Decode a hex bearer token for the QUIC preamble.
fn parsed_quic_token(token: Option<String>) -> Result<Option<Vec<u8>>, DialRefusal> {
    token
        .map(|token| phux_dial::quic::parse_token_hex(&token))
        .transpose()
        .map_err(|err| DialRefusal::BadToken(err.to_string()))
}

/// Validate `url` and its credentials and build the WebSocket dial, or say
/// why it cannot be.
pub(crate) fn plan_ws_dial(
    url: String,
    token: Option<String>,
    cert_fingerprint: Option<String>,
    tls_server_name: Option<String>,
) -> Result<DialPlan, DialRefusal> {
    let target = phux_dial::ws::WsTarget::parse(&url)
        .map_err(|err| DialRefusal::Malformed(err.to_string()))?;
    require_ws_dial_credentials(&target, &url, token.as_deref(), cert_fingerprint.as_deref())?;
    let token = validated_ws_token(token)?;
    let trust = cert_fingerprint.map_or(CertTrust::SkipVerify, CertTrust::Pinned);
    Ok(DialPlan {
        dial: Dial::Ws(WsDial {
            url,
            token,
            trust,
            tls_server_name,
            identity: None,
        }),
        loopback: target.is_loopback(),
    })
}
/// Split a `--quic` `HOST:PORT` dial target into host and port. HOST may be
/// a DNS name, an IPv4 literal, or a bracketed IPv6 literal (`[::1]:8788`);
/// brackets stay on the host half for the caller to trim.
pub(super) fn split_host_port(target: &str) -> Result<(&str, u16), String> {
    let (host, port) = target.rsplit_once(':').ok_or_else(|| {
        format!("--quic target '{target}' is missing a port (expected HOST:PORT)")
    })?;
    if host.is_empty() {
        return Err(format!(
            "--quic target '{target}' is missing a host (expected HOST:PORT)"
        ));
    }
    let port = port
        .parse::<u16>()
        .map_err(|err| format!("--quic target '{target}' has an invalid port: {err}"))?;
    Ok((host, port))
}

/// Resolve a `--quic` `HOST:PORT` to its first address plus the default TLS
/// server name. Resolution precedes the trust decision, which keys on the
/// resolved address.
fn resolve_quic_target(
    rt: &tokio::runtime::Runtime,
    target: &str,
) -> Result<(std::net::SocketAddr, String), DialRefusal> {
    let (host, port) = split_host_port(target).map_err(DialRefusal::Malformed)?;
    let bare_host = host.trim_matches(['[', ']']);
    let host_is_ip_literal = bare_host.parse::<std::net::IpAddr>().is_ok();

    let resolved = rt
        .block_on(tokio::net::lookup_host((bare_host, port)))
        .map(|mut addrs| addrs.next());
    let detail = match resolved {
        Ok(Some(addr)) => {
            // The TLS server name defaults to the dialed hostname when one
            // was given (conventional SNI); an IP-literal target keeps the
            // historical `localhost` default, matching the server's
            // self-signed SANs.
            let server_name = if host_is_ip_literal {
                "localhost".to_owned()
            } else {
                bare_host.to_owned()
            };
            return Ok((addr, server_name));
        }
        Ok(None) => "name resolution returned no addresses".to_owned(),
        Err(err) => format!("name resolution failed: {err}"),
    };
    // An unresolvable name is the overlay-down shape (MagicDNS unreachable),
    // so it earns the same hint as an unanswered dial.
    Err(DialRefusal::Unresolved {
        target: target.to_owned(),
        detail,
        dns_name: !host_is_ip_literal,
    })
}
