//! `phux pair` — mint, rotate, or revoke remote credentials (ADR-0031).
//!
//! The token authenticates a device that attaches over `wss://` or QUIC; the
//! server reads the same token store at `PHUX_WS_TOKENS`. Minting first asks
//! the running server which remote listeners it has bound (`GET_STATE`'s
//! listener report) and refuses when none would accept the credential, so a
//! token, link, or QR is only ever printed for a door that is open, and the
//! link names the address that door is actually bound to (ADR-0141). `ls`,
//! `prune`, `rotate`, and `revoke` only edit the store and need no server.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use phux_config::overlay::MagicDns;

use usage::Subcommands;

const DEFAULT_ROTATION_OVERLAP_SECONDS: i64 = 300;

#[derive(Debug, Subcommands)]
pub(crate) enum PairAction {
    /// List credentials in the store (id, minted, last seen, revoked).
    Ls,
    /// Revoke credentials unused for at least DURATION.
    Prune {
        /// How long a credential may sit idle before prune revokes it
        /// (`30d`, `24h`, `90m`, or a bare number of seconds). Idle time is
        /// `last_seen` when recorded, otherwise the mint time.
        #[usage(long = "unused-for", value_name = "DURATION")]
        unused_for: String,
    },
    /// Replace a credential's bearer secret with a bounded overlap.
    Rotate {
        /// Stable credential ID printed when the credential was minted.
        #[usage(value_name = "CREDENTIAL_ID")]
        credential_id: String,

        /// Seconds the previous generation remains valid. Its existing
        /// absolute expiry still wins when it is sooner; an already-expired
        /// credential cannot be rotated. Live sessions still on the previous
        /// generation are disconnected when the overlap ends.
        #[usage(
            long,
            default = "300", default_value_t = DEFAULT_ROTATION_OVERLAP_SECONDS,
            validate = "int(value) >= 0 && int(value) <= 86400", validate_error = "must be between 0 and 86400 seconds",
            value_name = "SECONDS"
        )]
        overlap_seconds: i64,
    },
    /// Revoke every generation of a credential for new connections.
    ///
    /// A `sha256:` id names an enrolled workload client certificate (the id
    /// `phux host add` and `phux workload add-key` print); it is revoked in
    /// the workload registry, exactly as `phux workload revoke` would.
    Revoke {
        /// Stable credential ID printed when the credential was minted, or
        /// an enrolled certificate's `sha256:` credential id.
        #[usage(value_name = "CREDENTIAL_ID")]
        credential_id: String,
    },
}

/// Prefix of the one-tap connect link (and its QR):
/// `https://phux.sh/connect?url=<ws(s)-url>[&quic=<quic-url>][&name=<n>][&fp=<sha256>]&token=<hex>`.
/// `url` stays mandatory for older app builds; newer apps prefer `quic` when
/// the running server reports a device-dialable QUIC listener.
///
/// A relay link (ADR-0149) is
/// `...?quic=quic://<relay>&sni=<route>[&name=<n>][&fp=<relay-sha256>]&token=<hex>`:
/// `sni` is the TLS server name the `quic` dial offers, which the relay
/// routes on (ADR-0052), so it requires `quic` and carries no `url` (the
/// relay has no WebSocket leg). A parser that predates `sni` refuses the
/// link for its missing `url` rather than dialing the relay unrouted.
///
/// An https Universal Link rather than a custom scheme because it carries a
/// bearer token: any iOS app may claim a custom scheme, but only the app that
/// owns the domain receives a Universal Link.
///
/// This shape is owned HERE, per ADR-0031: consumers must accept it exactly, and
/// changing it means changing the ADR and the consumers together.
const CONNECT_URI_PREFIX: &str = "https://phux.sh/connect";

/// The pre-`phux.sh` host. Still parsed (saved links must keep pairing), no
/// longer emitted: it only redirects, and a redirect breaks a Universal Link.
const LEGACY_HOST_CONNECT_URI_PREFIX: &str = "https://phux.phall.io/connect";

/// The custom-scheme spelling of the same link, parsed for `--code` and
/// printed beneath the https form for app builds without the Universal Link
/// entitlement. The QR encodes the https form only.
const LEGACY_CONNECT_URI_PREFIX: &str = "phux://connect";

/// Build the one-tap link. `url`, `token`, and `fingerprint` are query-safe
/// as-is; only the free-form `name` is percent-encoded.
#[cfg(test)]
fn build_connect_link(
    url: &str,
    quic: Option<&str>,
    name: Option<&str>,
    fingerprint: Option<&str>,
    token: &str,
) -> String {
    build_connect_link_pinned(
        url,
        quic,
        name,
        ServerPins {
            leaf: fingerprint,
            authority: None,
            enrollment: None,
        },
        token,
    )
}

/// What a link pins: the leaf fingerprint (`fp`) and, when the server's
/// certificate chains to its workload CA, that CA (`ca`, ADR-0153).
///
/// A link may also carry a single-use enrollment ticket (`enroll`, ADR-0154).
#[derive(Debug, Clone, Copy, Default)]
struct ServerPins<'a> {
    leaf: Option<&'a str>,
    authority: Option<&'a str>,
    enrollment: Option<&'a str>,
}

/// [`build_connect_link`] carrying `ca=` after `fp=` when the server presents
/// its CA. A consumer that predates `ca` ignores it and pins the leaf.
fn build_connect_link_pinned(
    url: &str,
    quic: Option<&str>,
    name: Option<&str>,
    pins: ServerPins<'_>,
    token: &str,
) -> String {
    let mut link = format!("{CONNECT_URI_PREFIX}?url={url}");
    if let Some(quic) = quic {
        link.push_str("&quic=");
        link.push_str(quic);
    }
    push_link_credentials(&mut link, name, pins, token);
    link
}

/// Build a relay link (ADR-0149): the relay's `quic://` endpoint, the route
/// it is dialed with as `sni`, the relay's pin, and the server's token.
fn build_relay_connect_link(
    relay: &str,
    route: &str,
    name: Option<&str>,
    pins: ServerPins<'_>,
    token: &str,
) -> String {
    let mut link = format!("{CONNECT_URI_PREFIX}?quic=quic://{relay}&sni={route}");
    // `fp` pins the relay, which terminates the outer TLS (ADR-0149); `ca`
    // pins the server, which the end-to-end session inside verifies
    // (ADR-0154).
    push_link_credentials(&mut link, name, pins, token);
    link
}

/// Append the fields every link ends with, in the documented order.
fn push_link_credentials(link: &mut String, name: Option<&str>, pins: ServerPins<'_>, token: &str) {
    if let Some(name) = name {
        link.push_str("&name=");
        link.push_str(&percent_encode(name));
    }
    if let Some(fp) = pins.leaf {
        link.push_str("&fp=");
        link.push_str(fp);
    }
    if let Some(ca) = pins.authority {
        link.push_str("&ca=");
        link.push_str(ca);
    }
    if let Some(ticket) = pins.enrollment {
        link.push_str("&enroll=");
        link.push_str(ticket);
    }
    link.push_str("&token=");
    link.push_str(token);
}

/// Respell an https connect link with [`LEGACY_CONNECT_URI_PREFIX`], carrying
/// the query byte-for-byte. `None` for anything else.
fn legacy_connect_link(link: &str) -> Option<String> {
    link.strip_prefix(CONNECT_URI_PREFIX)
        .map(|query| format!("{LEGACY_CONNECT_URI_PREFIX}{query}"))
}

/// Percent-encode everything outside RFC 3986 `unreserved` — conservative on
/// purpose, since the value lands inside a URI query a phone must parse.
fn percent_encode(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[usize::from(byte >> 4)] as char);
                out.push(HEX[usize::from(byte & 0x0F)] as char);
            }
        }
    }
    out
}

/// The credentials a connect link carries: what a phone scans and what a
/// laptop pastes into `phux attach --remote HOST --code '<link>'`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConnectLink {
    /// The `ws://`/`wss://` fallback endpoint old clients continue to dial.
    /// Present on every link but a relay link, which has no WebSocket leg.
    pub(crate) url: Option<String>,
    /// The preferred `quic://` endpoint, when the live listener is dialable;
    /// on a relay link, the relay.
    pub(crate) quic: Option<String>,
    /// The TLS server name the `quic` dial offers: the relay route
    /// (ADR-0149). `Some` only on a relay link, which always has `quic`.
    pub(crate) tls_server_name: Option<String>,
    /// The operator's label for the server, when the link carries one.
    pub(crate) name: Option<String>,
    /// The TLS certificate SHA-256 pin.
    pub(crate) cert_fingerprint: Option<String>,
    /// The `sha256:` fingerprint of the CA the server's leaf chains to
    /// (`ca`, ADR-0153), canonical; `None` when the link carries none.
    pub(crate) ca_fingerprint: Option<String>,
    /// The single-use enrollment ticket (`enroll`, ADR-0154), hex. A secret
    /// like the token — never echoed back.
    pub(crate) enrollment_ticket: Option<String>,
    /// The bearer pairing token. A secret — never echoed back.
    pub(crate) token: String,
}

/// Parse a connect link: the exact inverse of [`build_connect_link`] and
/// [`build_relay_connect_link`], also accepting the legacy prefixes. Strict
/// about the endpoint (`url`, or `quic` with `sni`) and `token`, tolerant of
/// unknown query keys.
pub(crate) fn parse_connect_link(link: &str) -> Result<ConnectLink, String> {
    let trimmed = link.trim().trim_matches(|c| c == '\'' || c == '"');
    let query = trimmed
        .strip_prefix(CONNECT_URI_PREFIX)
        .or_else(|| trimmed.strip_prefix(LEGACY_HOST_CONNECT_URI_PREFIX))
        .or_else(|| trimmed.strip_prefix(LEGACY_CONNECT_URI_PREFIX))
        .and_then(|rest| rest.strip_prefix('?'))
        .ok_or_else(|| {
            format!(
                "a connect code must start with `{CONNECT_URI_PREFIX}?` \
                 (or `{LEGACY_CONNECT_URI_PREFIX}?`; paste the whole link `phux pair` printed)"
            )
        })?;

    let ConnectFields {
        url,
        quic,
        sni,
        name,
        fingerprint,
        authority,
        enrollment,
        token,
    } = parse_connect_fields(query)?;
    let ca_fingerprint = authority
        .filter(|ca| !ca.is_empty())
        .map(|ca| {
            phux_config::known_authorities::canonical_authority(&ca).ok_or_else(|| {
                "connect code ca must be a sha256: certificate fingerprint".to_owned()
            })
        })
        .transpose()?;
    let tls_server_name = sni
        .filter(|sni| !sni.is_empty())
        .map(|sni| {
            phux_client_runtime::target::validate_tls_server_name(&sni)
                .map_err(|err| format!("connect code sni: {err}"))
        })
        .transpose()?;
    let (url, quic) = validate_connect_endpoints(url, quic, tls_server_name.is_some())?;
    let token = token
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "connect code carries no `token=` — it grants no access".to_owned())?;

    Ok(ConnectLink {
        url,
        quic,
        tls_server_name,
        name: name.filter(|name| !name.is_empty()),
        cert_fingerprint: fingerprint.filter(|fp| !fp.is_empty()),
        ca_fingerprint,
        enrollment_ticket: enrollment.filter(|ticket| !ticket.is_empty()),
        token,
    })
}

#[derive(Default)]
struct ConnectFields {
    url: Option<String>,
    quic: Option<String>,
    sni: Option<String>,
    name: Option<String>,
    fingerprint: Option<String>,
    authority: Option<String>,
    enrollment: Option<String>,
    token: Option<String>,
}

fn parse_connect_fields(query: &str) -> Result<ConnectFields, String> {
    let mut fields = ConnectFields::default();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, raw) = pair
            .split_once('=')
            .ok_or_else(|| format!("connect code field {pair:?} has no value"))?;
        let decoded = percent_decode(raw)?;
        let slot = match key {
            "url" => &mut fields.url,
            "quic" => &mut fields.quic,
            "sni" => &mut fields.sni,
            "name" => &mut fields.name,
            "fp" => &mut fields.fingerprint,
            "ca" => &mut fields.authority,
            "enroll" => &mut fields.enrollment,
            "token" => &mut fields.token,
            // Unknown keys are forward-compat room, not an error.
            _ => continue,
        };
        assign_connect_field(slot, key, decoded)?;
    }
    Ok(fields)
}

/// Preserve an unambiguous credential contract while unknown additive fields
/// remain forward-compatible.
fn assign_connect_field(slot: &mut Option<String>, key: &str, value: String) -> Result<(), String> {
    if slot.replace(value).is_some() {
        return Err(format!("connect code carries duplicate `{key}=` fields"));
    }
    Ok(())
}

/// Validate the fallback and additive endpoint fields without involving a
/// dialer. Reachability remains the runtime's job. A relay link (`routed`,
/// it carries `sni`) needs `quic` and may omit `url`; any other needs `url`.
fn validate_connect_endpoints(
    url: Option<String>,
    quic: Option<String>,
    routed: bool,
) -> Result<(Option<String>, Option<String>), String> {
    let url = url.filter(|url| !url.is_empty());
    let quic = quic.filter(|quic| !quic.is_empty());
    if routed && quic.is_none() {
        return Err(
            "connect code carries `sni=` but no `quic=` — a relay route is dialed over QUIC"
                .to_owned(),
        );
    }
    if !routed && url.is_none() {
        return Err("connect code carries no `url=` — it cannot name a server to dial".to_owned());
    }
    if let Some(url) = url.as_deref()
        && !url.starts_with("wss://")
        && !url.starts_with("ws://")
    {
        return Err(format!("connect code url {url:?} must be ws:// or wss://"));
    }
    if let Some(endpoint) = quic.as_deref() {
        validate_quic_endpoint(endpoint)?;
    }
    Ok((url, quic))
}

impl ConnectLink {
    /// The endpoint a registry entry built from this link dials: the relay
    /// for a relay link, else the WebSocket `url`. Always `Some` for a link
    /// [`parse_connect_link`] accepted.
    pub(crate) fn registry_endpoint(&self) -> Option<&str> {
        if self.tls_server_name.is_some() {
            self.quic.as_deref()
        } else {
            self.url.as_deref()
        }
    }
}

fn validate_quic_endpoint(endpoint: &str) -> Result<(), String> {
    let authority = endpoint.strip_prefix("quic://").ok_or_else(|| {
        format!("connect code quic endpoint {endpoint:?} must start with quic://")
    })?;
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or_else(|| format!("connect code quic endpoint {endpoint:?} needs HOST:PORT"))?;
    let bracketed = host.starts_with('[') && host.ends_with(']');
    let bare_host = host.trim_matches(['[', ']']);
    let valid_host = !bare_host.is_empty()
        && !authority.contains(['/', '?', '#', '@'])
        && (bracketed || !host.contains(':'));
    let valid_port = port.parse::<u16>().is_ok_and(|port| port != 0);
    if valid_host && valid_port {
        return Ok(());
    }
    Err(format!(
        "connect code quic endpoint {endpoint:?} needs HOST:PORT with a non-zero numeric port"
    ))
}

/// Decode the percent-escapes [`percent_encode`] produces. Only `name` is
/// ever encoded on the minting side, but decoding every field keeps the
/// parser honest against a link written by some other tool.
fn percent_decode(value: &str) -> Result<String, String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes
                .get(index + 1..index + 3)
                .and_then(|hex| std::str::from_utf8(hex).ok())
                .ok_or_else(|| format!("truncated percent-escape in {value:?}"))?;
            let byte = u8::from_str_radix(hex, 16)
                .map_err(|_| format!("invalid percent-escape `%{hex}` in {value:?}"))?;
            out.push(byte);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|_| format!("connect code field {value:?} is not UTF-8"))
}

/// Turn a live listener bind into a device-dialable endpoint. An unspecified
/// bind (`0.0.0.0`/`::`) uses the first overlay address; loopback is not
/// advertised to another device. An address the host's `MagicDNS` name stands
/// for is named by that name instead (ADR-0164), so the link survives the
/// tailnet reassigning the address.
fn resolve_bound_endpoint(
    scheme: &str,
    overlay: &[IpAddr],
    magic_dns: Option<&MagicDns>,
    bound: Option<SocketAddr>,
) -> Option<String> {
    let bound = bound?;
    let ip = if bound.ip().is_unspecified() {
        overlay
            .iter()
            .copied()
            .find(|candidate| candidate.is_ipv4() == bound.ip().is_ipv4())?
    } else if bound.ip().is_loopback() {
        return None;
    } else {
        bound.ip()
    };
    if let Some(magic_dns) = magic_dns.filter(|magic_dns| magic_dns.names(ip)) {
        return Some(format!("{scheme}://{}:{}", magic_dns.name, bound.port()));
    }
    Some(format!("{scheme}://{}", SocketAddr::new(ip, bound.port())))
}

/// Resolve the ws(s):// URL the link embeds: `--host` wins (a bare
/// `host:port` gets `wss://`); otherwise use the live wss listener.
fn resolve_server_url(
    host: Option<&str>,
    overlay: &[IpAddr],
    magic_dns: Option<&MagicDns>,
    bound_wss: Option<SocketAddr>,
) -> Option<String> {
    if let Some(host) = host {
        if host.starts_with("ws://") || host.starts_with("wss://") {
            return Some(host.to_owned());
        }
        return Some(format!("wss://{host}"));
    }
    resolve_bound_endpoint("wss", overlay, magic_dns, bound_wss)
}

/// Resolve the preferred raw-QUIC endpoint from the running listener only.
fn resolve_quic_endpoint(
    overlay: &[IpAddr],
    magic_dns: Option<&MagicDns>,
    bound_quic: Option<SocketAddr>,
) -> Option<String> {
    resolve_bound_endpoint("quic", overlay, magic_dns, bound_quic)
}

/// The `HOST:PORT` another machine dials this server's live QUIC listener at,
/// resolved as a pairing link's `quic` is (overlay address, `MagicDNS`
/// name). `Ok(None)` when no QUIC listener is bound or it binds loopback.
///
/// # Errors
///
/// No server answers, or it has no remote listener bound.
pub(crate) fn advertised_quic_endpoint(socket: &Path) -> Result<Option<String>, String> {
    let live = query_live_listeners(socket)?;
    let overlay = phux_config::overlay::detect();
    let magic_dns = phux_config::overlay::detect_magic_dns();
    Ok(
        resolve_quic_endpoint(&overlay, magic_dns.as_ref(), live.quic_addr())
            .map(|endpoint| endpoint.trim_start_matches("quic://").to_owned()),
    )
}

/// The workload CA this server's certificate chains to (ADR-0153), what a
/// machine joining it pins; `None` for a certificate no CA issued.
pub(crate) fn presented_server_authority() -> Option<String> {
    phux_server::transport::tls::presented_authority(&resolve_certificate_paths(None).cert)
        .ok()
        .flatten()
}

/// The words of the refusal when the server has no remote listener bound.
/// `phux host add` recognizes them to restart a server that disabled its
/// listeners at boot, so they are a contract with that caller.
pub(crate) const NO_BOUND_LISTENER: &str = "has no remote listener bound";

/// The remote listeners the running server reports bound: what a credential
/// minted now can actually authenticate against.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct LiveListeners {
    /// The bound wss listener's address.
    wss: Option<String>,
    /// The bound QUIC listener's address.
    quic: Option<String>,
}

impl LiveListeners {
    /// The wss bind as a socket address, when it parses as one.
    fn wss_addr(&self) -> Option<SocketAddr> {
        self.wss.as_deref().and_then(|addr| addr.parse().ok())
    }

    /// The QUIC bind as a socket address, when it parses as one.
    fn quic_addr(&self) -> Option<SocketAddr> {
        self.quic.as_deref().and_then(|addr| addr.parse().ok())
    }
}

/// Ask the server on `socket` which remote listeners it has bound. `Err` is
/// the refusal to print: no server, or a server with nothing bound, would
/// leave a minted credential authenticating nothing.
fn query_live_listeners(socket: &Path) -> Result<LiveListeners, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("could not build a runtime to ask the server: {err}"))?;
    let view = runtime
        .block_on(phux_client::state::get_state(socket))
        .map_err(|err| no_live_server(socket, &err))?;
    live_listeners(socket, view.snapshot().listeners())
}

/// The refusal when `GET_STATE` could not be asked at all.
fn no_live_server(socket: &Path, err: &phux_client::attach::AttachError) -> String {
    let reason = match err {
        phux_client::attach::AttachError::Io(io)
            if matches!(
                io.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            format!("no server is running at {}", socket.display())
        }
        other => format!(
            "could not ask the server at {} what it is listening on: {other}",
            socket.display()
        ),
    };
    format!(
        "{reason}; a credential minted now would pair with nothing, so none was minted.\n  \
         start one that stays up (`phux service install`, or `phux server --ensure`), \
         then rerun `phux pair`; pass --socket PATH to pair with a server on another socket"
    )
}

/// The pure half of [`query_live_listeners`]: the bound rows of `report`, or
/// the refusal naming why nothing is bound.
fn live_listeners(
    socket: &Path,
    report: Option<&phux_protocol::wire::RemoteListenersReport>,
) -> Result<LiveListeners, String> {
    use phux_protocol::wire::RemoteListenerTransport;

    let bound_addr = |transport| {
        report?
            .listeners
            .iter()
            .find(|slot| slot.transport == transport && slot.bound)
            .map(|slot| slot.addr.clone().unwrap_or_default())
    };
    let live = LiveListeners {
        wss: bound_addr(RemoteListenerTransport::Wss),
        quic: bound_addr(RemoteListenerTransport::Quic),
    };
    let any_bound = report.is_some_and(|report| report.listeners.iter().any(|slot| slot.bound));
    if any_bound {
        return Ok(live);
    }
    let disabled = report
        .map(|report| {
            report
                .unhealthy()
                .map(|slot| {
                    let reason = slot.disabled_reason.map_or(
                        "unknown",
                        phux_protocol::wire::ListenerDisabledReason::as_str,
                    );
                    format!(
                        "{} at {} disabled ({reason})",
                        slot.transport,
                        slot.addr.as_deref().unwrap_or("?")
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let why = if disabled.is_empty() {
        String::new()
    } else {
        format!(" ({})", disabled.join("; "))
    };
    Err(format!(
        "the server at {} {NO_BOUND_LISTENER}{why}, so nothing would accept a \
         credential and none was minted.\n  \
         give it one: first set `[policy] mode = \"paired\"` in config.toml (a remote \
         listener serves only enrolled clients, and with no mode the server binds none), \
         then restart it with `--listen HOST:PORT` or `--quic HOST:PORT` \
         (`PHUX_WS_ADDR` / `PHUX_QUIC_ADDR`), or let the default-profile server auto-bind \
         one on an overlay network (`phux upgrade` re-binds with panes intact); \
         `phux doctor` names the cause",
        socket.display()
    ))
}

/// Refuse, before anything is minted, a link or QR that cannot work: `--host`
/// names a wss endpoint, so it needs a bound wss listener behind it, and
/// `--qr` needs a link at all.
fn link_refusal(
    host: Option<&str>,
    qr: bool,
    live: &LiveListeners,
    server_url: Option<&str>,
) -> Option<String> {
    if host.is_some() && live.wss.is_none() {
        return Some(
            "--host names the WebSocket endpoint the connect link carries, but the server has \
             no wss listener bound, so the link could not connect; none was minted.\n  \
             restart the server with `--listen HOST:PORT` (`PHUX_WS_ADDR`), or drop --host \
             and --qr to mint a credential for its other listeners"
                .to_owned(),
        );
    }
    if qr && server_url.is_none() {
        let state = live.wss.as_deref().map_or_else(
            || "not bound".to_owned(),
            |addr| format!("bound to {addr}, which a device cannot dial"),
        );
        return Some(format!(
            "--qr needs a connect link, and the server's wss listener is {state}; \
             none was minted.\n  \
             pass --host HOST:PORT naming how the device reaches the server, or bind a \
             reachable wss listener (`--listen`, or an overlay address)"
        ));
    }
    None
}

/// Every server name this pairing run advertises, in SAN / `ServerName`
/// form: the link host (via [`WsTarget::parse`], the dialer's own parser),
/// the `quic` endpoint's host, and each overlay address. An unparseable
/// endpoint contributes nothing.
fn advertised_names(
    server_url: Option<&str>,
    quic_endpoint: Option<&str>,
    overlay: &[IpAddr],
) -> Vec<String> {
    use phux_dial::ws::WsTarget;

    let mut names: Vec<String> = server_url
        .and_then(|url| WsTarget::parse(url).ok())
        .map(|target| target.server_name)
        .into_iter()
        .collect();
    let quic_host = quic_endpoint
        .and_then(|endpoint| endpoint.strip_prefix("quic://"))
        .and_then(|authority| authority.rsplit_once(':'))
        .map(|(host, _)| host.trim_matches(['[', ']']).to_owned())
        .filter(|host| !host.is_empty());
    if let Some(host) = quic_host
        && !names.contains(&host)
    {
        names.push(host);
    }
    for addr in overlay {
        let name = phux_server::transport::tls::san_name(*addr);
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Warn when the certificate being fingerprinted does not name the address
/// the link advertises (ADR-0091). Widening the SANs rotates the fingerprint
/// every paired device pins, so it stays an explicit operator action.
fn warn_on_uncovered_names(cert: &std::path::Path, key: &std::path::Path, advertised: &[String]) {
    let Ok(uncovered) = phux_server::transport::tls::uncovered_names(cert, advertised) else {
        // Unreadable certificate: the fingerprint read alongside this already
        // reported it. Nothing to add.
        return;
    };
    if uncovered.is_empty() {
        return;
    }
    warn!(
        "pair: this certificate does not name {} — a device that pins \
         the fingerprint above is unaffected, but a client that validates the server \
         name (a browser, or curl --cacert) will refuse the handshake.",
        uncovered.join(", ")
    );
    warn!(
        "pair: widening it means a NEW certificate and a NEW fingerprint, \
         which un-pairs every already-paired device. To do it deliberately: rm {} {} \
         && phux pair, then re-pair every device.",
        cert.display(),
        key.display()
    );
}

/// Render `payload` as a Unicode half-block QR (`Dense1x2`) with a quiet
/// zone. `scale` is the module size in terminal cells on each axis (1 is the
/// historical size). Encode failure is the rare payload past QR's ~2.9 KB.
fn render_qr_scaled(payload: &str, scale: u32) -> Result<String, String> {
    use qrcode::QrCode;
    use qrcode::render::unicode;

    let scale = scale.clamp(1, 3);
    QrCode::new(payload.as_bytes())
        .map(|code| {
            code.render::<unicode::Dense1x2>()
                .quiet_zone(true)
                .module_dimensions(scale, scale)
                .build()
        })
        .map_err(|err| format!("could not encode pairing QR: {err}"))
}

/// Mint a token into the store and print it with the certificate
/// fingerprint. Defaults are the shared paths the server reads (ADR-0031), and
/// the certificate is provisioned if absent. Nothing is minted unless the
/// server on `socket` has a remote listener bound (ADR-0141). With a
/// reachable wss listener the credentials are also printed as a connect
/// link, followed on a terminal by its QR unless `--no-qr`; `--qr` is the
/// camera form, the QR alone. With `relay` the credential is minted for a
/// relay route instead ([`mint_relay_link`]).
#[allow(
    clippy::needless_pass_by_value,
    reason = "CLI entry point owns the args clap dispatch hands it; taking them by value keeps the call site clean"
)]
#[allow(
    clippy::too_many_arguments,
    clippy::fn_params_excessive_bools,
    reason = "the existing mint options remain flat for CLI compatibility while the optional action selects lifecycle operations"
)]
pub(crate) fn run_pair(
    action: Option<PairAction>,
    socket: Option<PathBuf>,
    tokens: Option<PathBuf>,
    cert: Option<PathBuf>,
    qr: bool,
    secret_file: Option<PathBuf>,
    expires: Option<String>,
    no_qr: bool,
    host: Option<String>,
    name: Option<String>,
    json: bool,
    replace_token: Option<String>,
    door: Door,
) -> ExitCode {
    let tokens = tokens
        .or_else(|| std::env::var_os("PHUX_WS_TOKENS").map(PathBuf::from))
        .unwrap_or_else(phux_server::auth::default_token_store_path);
    if let Some(action) = action {
        return run_credential_action(&tokens, action, json);
    }
    if json && (secret_file.is_some() || qr) {
        error!("pair: --json prints the bearer and cannot combine with --qr or --secret-file");
        return ExitCode::from(2);
    }
    if let Some(path) = secret_file.as_deref()
        && let Err(err) = prepare_secret_file(path)
    {
        error!("pair: {err}");
        return ExitCode::FAILURE;
    }
    let expires_for = match expires.as_deref().map(parse_expires).transpose() {
        Ok(duration) => duration,
        Err(err) => {
            error!("pair: {err}");
            return ExitCode::from(2);
        }
    };
    let certificate = resolve_certificate_paths(cert);

    // Everything that could make the credential useless is settled before
    // the store is touched. The store's own refusal comes first, in the
    // words a failed mint uses (a retired legacy store names its remedy).
    if let Err(err) = phux_server::auth::TokenStore::load(&tokens) {
        error!("pair: failed to mint token: {err}");
        return ExitCode::FAILURE;
    }
    let socket = socket.unwrap_or_else(phux_server::runtime::default_socket_path);
    let expires_at = expires_for.map(|duration| chrono::Utc::now() + duration);
    let output = LinkOutput {
        name: name.as_deref(),
        qr,
        default_qr: shows_default_qr(
            qr || no_qr || json || secret_file.is_some(),
            std::io::IsTerminal::is_terminal(&std::io::stdout()),
        ),
        json,
        secret_file: secret_file.as_deref(),
        expires_at,
        expires_label: expires.as_deref(),
    };
    let enroll = match door {
        // A camera QR is a phone pairing invitation, not a bearer-only link.
        // Keep plain and JSON mints unchanged for machine clients.
        Door::Listener { enroll } => phone_enrollment_requested(enroll, qr),
        Door::Relay { route, enroll } => {
            let replace = replace_token.as_deref();
            return mint_relay_link(&route, &socket, &tokens, output, replace, enroll);
        }
    };
    let live = match query_live_listeners(&socket) {
        Ok(live) => live,
        Err(refusal) => {
            error!("pair: {refusal}");
            return ExitCode::FAILURE;
        }
    };
    let addresses = resolve_pair_addresses(host.as_deref(), live);
    let refusal = link_refusal(
        host.as_deref(),
        qr,
        &addresses.live,
        addresses.server_url.as_deref(),
    )
    .or_else(|| {
        let camera = qr || (output.default_qr && addresses.server_url.is_some());
        (enroll && !addresses.enrollment_endpoint_ready(camera))
            .then(|| ENROLL_NEEDS_QUIC.to_owned())
    });
    if let Some(refusal) = refusal {
        error!("pair: {refusal}");
        return ExitCode::FAILURE;
    }
    provision_pairing_certificate(&certificate, &addresses.advertised);
    // A device enrolls trusting the CA its link pins and never learns one
    // (ADR-0179), so a certificate no workload CA issued has nothing to give
    // it. Refused before anything is minted.
    if enroll
        && phux_server::transport::tls::presented_authority(&certificate.cert)
            .ok()
            .flatten()
            .is_none()
    {
        error!("pair: {ENROLL_NEEDS_AUTHORITY}");
        return ExitCode::FAILURE;
    }

    let Some(minted) = mint_pairing_credential(&tokens, replace_token.as_deref(), expires_at)
    else {
        return ExitCode::FAILURE;
    };
    present_listener_credential(&minted, &tokens, &certificate, &addresses, output, enroll)
}

/// Print or file a listener mint. `--json` is the only path that writes the
/// bearer to stdout; camera and secret-file mints keep it out of the text.
fn present_listener_credential(
    minted: &phux_server::auth::MintedCredential,
    tokens: &Path,
    certificate: &CertificatePaths,
    addresses: &PairAddresses,
    output: LinkOutput<'_>,
    enroll: bool,
) -> ExitCode {
    let token = minted.secret();
    let hide_bearer = hides_bearer(output.qr, output.secret_file.is_some(), output.json);
    if !output.json && !hide_bearer {
        print_credential_block(&minted.id, token);
    }

    let fingerprint = read_pairing_fingerprint(&certificate.cert, output.json || hide_bearer);
    let authority = read_pairing_authority(&certificate.cert, output.json);
    let ticket = match enroll.then(|| mint_enrollment_ticket(output.json, secret_surface(output))) {
        Some(None) => return ExitCode::FAILURE,
        minted_ticket => minted_ticket.flatten(),
    };
    let checked = addresses.checked_names();
    warn_on_uncovered_names(&certificate.cert, &certificate.key, &checked);

    if !output.json {
        print_overlay_addresses(&addresses.overlay);
    }

    // The one-tap link (and its QR form) carries the token — it is as much
    // a secret as the token line above, shown once on the same terminal.
    let pins = ServerPins {
        leaf: fingerprint.as_deref(),
        authority: authority.as_deref(),
        enrollment: ticket.as_ref().map(|ticket| ticket.secret_hex.as_str()),
    };
    let link = addresses.server_url.as_deref().map(|url| {
        build_connect_link_pinned(
            url,
            addresses.quic_endpoint.as_deref(),
            output.name,
            pins,
            token,
        )
    });

    let expires_text = output.expires_at.map(format_rfc3339);
    if output.json {
        let mut document = pair_document(
            token,
            fingerprint.as_deref(),
            &addresses.overlay,
            &addresses.live,
            link.as_deref(),
            tokens,
            &minted.id,
            minted.generation,
            expires_text.as_deref(),
        );
        add_pins_to_document(&mut document, authority.as_deref(), ticket.as_ref());
        return crate::output::json(&document);
    }

    if present_human_secret(&HumanSecret {
        credential_id: &minted.id,
        token,
        fingerprint: fingerprint.as_deref(),
        link: link.as_deref(),
        expires_at: output.expires_at,
        expires_label: output.expires_label,
        qr: output.qr,
        default_qr: output.default_qr,
        secret_file: output.secret_file,
        hide_bearer,
    }) != ExitCode::SUCCESS
    {
        return ExitCode::FAILURE;
    }
    outln!("Token written to {}", tokens.display());
    ExitCode::SUCCESS
}

/// Whether a mint prints the QR beneath its text link by default: on a
/// terminal, unless the operator chose another form (`--qr` is the camera
/// form, `--no-qr` opts out, `--json` and `--secret-file` keep the bearer
/// off the screen). Piped output stays plain text a script can read.
const fn shows_default_qr(other_form: bool, stdout_is_terminal: bool) -> bool {
    !other_form && stdout_is_terminal
}

/// What a mint's link reaches the server through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Door {
    /// A bound listener; with `enroll`, the link also carries a single-use
    /// enrollment ticket (ADR-0154).
    Listener {
        /// `--enroll`.
        enroll: bool,
    },
    /// A relay route (`--relay-route`, ADR-0149); with `enroll`, as for a
    /// listener, enrolled end to end through the relay (ADR-0154).
    Relay {
        /// The route.
        route: RelayRoute,
        /// `--enroll`.
        enroll: bool,
    },
}

/// Explicit camera pairing must work for a new phone; ordinary bearer and
/// machine-readable mints keep their existing opt-in enrollment behavior.
const fn phone_enrollment_requested(enroll: bool, qr: bool) -> bool {
    enroll || qr
}

/// Why phone enrollment refuses a server certificate no workload CA issued.
const ENROLL_NEEDS_AUTHORITY: &str = "phone enrollment needs a server certificate issued by its \
     workload CA, but this certificate predates the CA or was supplied separately; nothing was \
     minted. `phux workload authority --rotate` then `phux upgrade` repairs an auto-provisioned \
     legacy certificate, but rotates the CA: every paired device must re-pair and every workload \
     credential must re-enroll. Operator-supplied certificates must be replaced by their owner";

const ENROLL_NEEDS_QUIC: &str = "phone pairing needs a device-dialable QUIC listener to enroll \
     its certificate; none was minted. Bind one with `phux server --quic ADDR` (or use the \
     overlay auto-listener), then retry. A loopback or wildcard bind without an overlay \
     address cannot be advertised to a phone";

/// The fields a mint document gained after its first schema: the CA pin
/// (ADR-0153) and an enrollment ticket (ADR-0154). Additive: `phux host add`
/// reads them, and older readers ignore them.
fn add_pins_to_document(
    document: &mut serde_json::Value,
    authority: Option<&str>,
    ticket: Option<&phux_server::workload::tickets::MintedTicket>,
) {
    document["ca_fingerprint"] = serde_json::json!(authority);
    if let Some(ticket) = ticket {
        document["enrollment_ticket"] = serde_json::json!(ticket.secret_hex);
        document["enrollment_ticket_id"] = serde_json::json!(ticket.id);
        document["enrollment_expires_at"] = serde_json::json!(ticket.expires_at);
    }
}

/// `phux pair --relay-route ROUTE [--relay HOST:PORT]`: which relay route a
/// credential is minted for (ADR-0149).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelayRoute {
    /// The route the relay enrolled this server's connector under.
    pub(crate) route: String,
    /// The `[[connector]]` entry's `relay`, when several are configured.
    pub(crate) relay: Option<String>,
}

/// How a mint presents its link.
#[derive(Debug, Clone, Copy)]
struct LinkOutput<'a> {
    /// The label the link carries.
    name: Option<&'a str>,
    /// Render the link as a camera QR and hide the bearer text.
    qr: bool,
    /// Render the QR beneath the printed link ([`shows_default_qr`]).
    default_qr: bool,
    /// One JSON document on stdout instead of prose.
    json: bool,
    /// Off-camera file that receives the bearer. Never printed.
    secret_file: Option<&'a Path>,
    /// Absolute credential expiry, when `--expires` was set.
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The duration the operator typed (`15m`), for the expiry line.
    expires_label: Option<&'a str>,
}

/// Mint a credential that reaches this server through a relay route, and
/// print the relay link (ADR-0149). The door is the server's outbound
/// connector, not a listener, so the ADR-0141 gate becomes: the server
/// answers on `socket`, and the config names the `[[connector]]` relay the
/// link dials. The link pins the relay's certificate (the relay terminates
/// only the outer TLS) and carries the route as `sni`, and pins this server's
/// CA as `ca`: the device runs its session end to end with this server, and
/// the token crosses the relay inside it (ADR-0180).
fn mint_relay_link(
    relay: &RelayRoute,
    socket: &Path,
    tokens: &Path,
    output: LinkOutput<'_>,
    replace_token: Option<&str>,
    enroll: bool,
) -> ExitCode {
    let connector = match relay_connector(relay).and_then(|connector| {
        server_answers(socket)?;
        Ok(connector)
    }) {
        Ok(connector) => connector,
        Err(refusal) => {
            error!("pair: {refusal}");
            return ExitCode::FAILURE;
        }
    };
    // The server's own CA, which a consumer verifies end to end through the
    // relay (ADR-0154). A relayed consumer is admitted only through that
    // session, so a server whose certificate predates its CA has nothing to
    // pair through a relay: the plain relayed stream is retired (ADR-0179).
    // Refused before anything is minted.
    let Some(authority) =
        phux_server::transport::tls::presented_authority(&resolve_certificate_paths(None).cert)
            .ok()
            .flatten()
    else {
        error!(
            "pair: pairing through a relay needs the server's certificate to chain to its \
             workload CA (ADR-0153); this one predates it. `phux workload authority --rotate` \
             issues one (every device then re-pairs)"
        );
        return ExitCode::FAILURE;
    };
    let Some(minted) = mint_pairing_credential(tokens, replace_token, output.expires_at) else {
        return ExitCode::FAILURE;
    };
    let token = minted.secret().to_owned();
    let hide_bearer = hides_bearer(output.qr, output.secret_file.is_some(), output.json);
    let fingerprint = connector.cert_fingerprint.as_deref();
    let ticket = match enroll.then(|| mint_enrollment_ticket(output.json, secret_surface(output))) {
        Some(None) => return ExitCode::FAILURE,
        minted => minted.flatten(),
    };
    let pins = ServerPins {
        leaf: fingerprint,
        authority: Some(authority.as_str()),
        enrollment: ticket.as_ref().map(|ticket| ticket.secret_hex.as_str()),
    };
    let link = build_relay_connect_link(&connector.relay, &relay.route, output.name, pins, &token);
    let expires_text = output.expires_at.map(format_rfc3339);
    if output.json {
        let mut doc = pair_document(
            &token,
            fingerprint,
            &[],
            &LiveListeners::default(),
            Some(&link),
            tokens,
            &minted.id,
            minted.generation,
            expires_text.as_deref(),
        );
        doc["relay"] = serde_json::json!({
            "endpoint": connector.relay,
            "route": relay.route,
        });
        add_pins_to_document(&mut doc, Some(authority.as_str()), ticket.as_ref());
        return crate::output::json(&doc);
    }
    if !hide_bearer {
        print_credential_block(&minted.id, &token);
    }
    outln!(
        "Relay route (the device dials {} naming route \"{}\"):",
        connector.relay,
        relay.route
    );
    match fingerprint {
        Some(fingerprint) => {
            outln!(
                "  relay certificate SHA-256 {fingerprint} (the device pins the relay, which sees only ciphertext)"
            );
        }
        None => outln!("  no relay pin configured: a loopback relay only"),
    }
    outln!("  server certificate authority {authority} (verified end to end through the relay)");
    outln!();
    if present_human_secret(&HumanSecret {
        credential_id: &minted.id,
        token: &token,
        fingerprint,
        link: Some(&link),
        expires_at: output.expires_at,
        expires_label: output.expires_label,
        qr: output.qr,
        default_qr: output.default_qr,
        secret_file: output.secret_file,
        hide_bearer,
    }) != ExitCode::SUCCESS
    {
        return ExitCode::FAILURE;
    }
    outln!("Token written to {}", tokens.display());
    ExitCode::SUCCESS
}

/// The validated route and the `[[connector]]` entry its link dials.
fn relay_connector(relay: &RelayRoute) -> Result<phux_config::ConnectorConfigEntry, String> {
    phux_relay::validate_route_name(&relay.route).map_err(|err| format!("--relay-route: {err}"))?;
    let config = phux_config::loader::load().map_err(|err| {
        format!(
            "could not read the phux config {}: {err}",
            phux_config::loader::config_path().display()
        )
    })?;
    let connector = select_relay_connector(config.connector, relay.relay.as_deref())?;
    // The link must parse on the device: refuse a relay address it could not
    // dial before anything is minted.
    validate_quic_endpoint(&format!("quic://{}", connector.relay)).map_err(|err| {
        format!(
            "the [[connector]] relay {:?} cannot go in a link ({err}); none was minted",
            connector.relay
        )
    })?;
    Ok(connector)
}

/// Pick the `[[connector]]` entry a relay link dials: the one `--relay`
/// names, else the only one. Nothing configured, or an ambiguous choice, is
/// a refusal: a link must name the relay this server actually tunnels to.
fn select_relay_connector(
    configured: Vec<phux_config::ConnectorConfigEntry>,
    relay: Option<&str>,
) -> Result<phux_config::ConnectorConfigEntry, String> {
    let names = || {
        configured
            .iter()
            .map(|entry| entry.relay.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    if let Some(relay) = relay {
        let listed = names();
        return configured
            .into_iter()
            .find(|entry| entry.relay == relay)
            .ok_or_else(|| {
                format!(
                    "no [[connector]] entry dials relay {relay:?} (configured: {}), so none was minted",
                    if listed.is_empty() { "none" } else { &listed }
                )
            });
    }
    if configured.len() > 1 {
        return Err(format!(
            "several [[connector]] relays are configured ({}); pass --relay HOST:PORT naming \
             the one this route is enrolled on",
            names()
        ));
    }
    configured.into_iter().next().ok_or_else(|| {
        "this server has no [[connector]] relay configured, so a relay link would route to \
         nothing and none was minted.\n  \
         enroll the route with `phux relay pair --route ROUTE` on the relay host, add a \
         [[connector]] entry naming that relay, and restart the server"
            .to_owned()
    })
}

/// Ask the server on `socket` for its state, only to learn that it answers:
/// a relay link minted for a server that is not running reaches nothing.
fn server_answers(socket: &Path) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("could not build a runtime to ask the server: {err}"))?;
    runtime
        .block_on(phux_client::state::get_state(socket))
        .map(|_| ())
        .map_err(|err| no_live_server(socket, &err))
}

/// The certificate material one pairing run reads and may provision.
struct CertificatePaths {
    /// Whether the operator named the certificate (`--cert` or
    /// `PHUX_WS_TLS_CERT`). An operator-supplied cert is used as-is, never
    /// generated over.
    operator_supplied: bool,
    /// The certificate whose fingerprint pairing prints.
    cert: PathBuf,
    /// The private key beside it.
    key: PathBuf,
}

/// Resolve the certificate and key paths: by default the shared paths the
/// server itself reads.
fn resolve_certificate_paths(cert: Option<PathBuf>) -> CertificatePaths {
    let operator_supplied = cert.is_some() || std::env::var_os("PHUX_WS_TLS_CERT").is_some();
    let cert = cert
        .or_else(|| std::env::var_os("PHUX_WS_TLS_CERT").map(PathBuf::from))
        .unwrap_or_else(phux_server::transport::tls::default_cert_path);
    let key = std::env::var_os("PHUX_WS_TLS_KEY")
        .map_or_else(phux_server::transport::tls::default_key_path, PathBuf::from);
    CertificatePaths {
        operator_supplied,
        cert,
        key,
    }
}

/// Every address a single pairing run knows about.
struct PairAddresses {
    /// Detected overlay addresses, printed as "dial one of these".
    overlay: Vec<IpAddr>,
    /// What the running server has bound.
    live: LiveListeners,
    /// The ws(s):// fallback URL the link embeds, when one can be resolved.
    server_url: Option<String>,
    /// The preferred quic:// endpoint, only when the live bind is dialable.
    quic_endpoint: Option<String>,
    /// The names a certificate minted for this run must claim.
    advertised: Vec<String>,
    /// The `MagicDNS` name an endpoint was named by, when one was (ADR-0164).
    magic_dns_name: Option<String>,
}

impl PairAddresses {
    /// Phone QRs need an advertised route. Explicit machine-readable enrollment
    /// can use a bound loopback listener without advertising it to a phone.
    fn enrollment_endpoint_ready(&self, camera: bool) -> bool {
        if camera {
            self.quic_endpoint.is_some()
        } else {
            self.live.quic.is_some()
        }
    }

    /// The advertised names an existing certificate is warned about. The
    /// `MagicDNS` name is claimed by a certificate this run generates but is
    /// not warned about: it stands for an overlay address that is checked,
    /// the server's own listener provisions most certificates without it,
    /// and phux's pinning clients never check it (ADR-0164).
    fn checked_names(&self) -> Vec<String> {
        self.advertised
            .iter()
            .filter(|name| self.magic_dns_name.as_ref() != Some(*name))
            .cloned()
            .collect()
    }
}

/// Resolve every address this run advertises. Runs before the certificate
/// is provisioned, because SANs are chosen at generation time (ADR-0091).
/// `--host` is the operator's own claim about how devices reach the server,
/// so it keeps the link on addresses rather than consulting `MagicDNS`.
fn resolve_pair_addresses(host: Option<&str>, live: LiveListeners) -> PairAddresses {
    let overlay = phux_config::overlay::detect();
    let magic_dns = host
        .is_none()
        .then(phux_config::overlay::detect_magic_dns)
        .flatten();
    let server_url = resolve_server_url(host, &overlay, magic_dns.as_ref(), live.wss_addr());
    let quic_endpoint = resolve_quic_endpoint(&overlay, magic_dns.as_ref(), live.quic_addr());
    let advertised = advertised_names(server_url.as_deref(), quic_endpoint.as_deref(), &overlay);
    let magic_dns_name = magic_dns.map(|magic_dns| magic_dns.name).filter(|name| {
        let named = |endpoint: &Option<String>| {
            endpoint
                .as_deref()
                .is_some_and(|endpoint| endpoint.contains(&format!("://{name}:")))
        };
        named(&server_url) || named(&quic_endpoint)
    });
    PairAddresses {
        overlay,
        live,
        server_url,
        quic_endpoint,
        advertised,
        magic_dns_name,
    }
}

/// Provision the server cert at the default paths if it isn't there yet (issued
/// by the workload CA, ADR-0153, as the server itself would),
/// so the fingerprint printed later is the one the server will actually
/// present. An operator-supplied cert is used as-is, never generated over.
fn provision_pairing_certificate(certificate: &CertificatePaths, advertised: &[String]) {
    if certificate.operator_supplied {
        return;
    }
    if let Err(err) = phux_server::transport::tls::ensure_server_identity(
        &certificate.cert,
        &certificate.key,
        advertised,
        &phux_server::workload::WorkloadPaths::from_env(),
    ) {
        warn!("pair: could not provision certificate: {err}");
    }
}

/// Mint a credential, reporting failures on stderr (`None` means exit with
/// failure). `replace_token` revokes the matching old bearer in the same
/// rewrite.
fn mint_pairing_credential(
    tokens: &std::path::Path,
    replace_token: Option<&str>,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<phux_server::auth::MintedCredential> {
    let minted = if replace_token.is_none() && expires_at.is_none() {
        phux_server::auth::mint_token(tokens)
    } else {
        phux_server::auth::mint_terminal_credential(tokens, replace_token, expires_at)
    };
    let minted = match minted {
        Ok(minted) => minted,
        Err(err) => {
            error!("pair: failed to mint token: {err}");
            return None;
        }
    };
    if !minted.is_durable() {
        warn!(
            "pair: credential is active, but the store directory could not be synced; do not retry pairing"
        );
    }
    Some(minted)
}

/// Print the credential ID and its secret for a human operator.
fn print_credential_block(credential_id: &str, token: &str) {
    outln!("Credential ID (use with `phux pair ls|prune|rotate|revoke`):");
    outln!("  {credential_id}");
    outln!();
    outln!("Pairing token (a secret — give it to the device once):");
    outln!("  {token}");
    outln!();
}

/// Read the certificate fingerprint the device pins, printing it for a human
/// operator and reporting an unreadable certificate on stderr.
fn read_pairing_fingerprint(cert: &std::path::Path, json: bool) -> Option<String> {
    match phux_server::transport::tls::cert_fingerprint(cert) {
        Ok(fingerprint) => {
            if !json {
                outln!("Server certificate SHA-256 (verify on the device to defeat MITM):");
                outln!("  {fingerprint}");
                outln!();
            }
            Some(fingerprint)
        }
        Err(err) => {
            warn!("pair: could not read certificate fingerprint: {err}");
            None
        }
    }
}

/// Mint the single-use enrollment ticket a `--enroll` link carries
/// (ADR-0154), with the ceiling `phux host add` enrolls with. `None` after
/// reporting a failure.
fn mint_enrollment_ticket(
    json: bool,
    surface: SecretSurface,
) -> Option<phux_server::workload::tickets::MintedTicket> {
    use phux_server::workload::tickets;
    let paths = phux_server::workload::WorkloadPaths::from_env();
    match tickets::mint_ticket(
        &tickets::tickets_path(&paths.registry),
        vec![super::enroll::HOST_ADD_WORKLOAD_SCOPE.to_owned()],
        tickets::DEFAULT_CREDENTIAL_SECONDS,
        tickets::DEFAULT_TICKET_SECONDS,
    ) {
        Ok(ticket) => {
            if !json {
                let where_held = match surface {
                    SecretSurface::Text => "in the link below",
                    SecretSurface::Qr => "in the QR below, not printed as text",
                    SecretSurface::File => "in the secret file, not printed",
                    SecretSurface::QrAndFile => {
                        "in the QR and the secret file, not printed as text"
                    }
                };
                let when = chrono::DateTime::from_timestamp(ticket.expires_at, 0).map_or_else(
                    || format!("unix {}", ticket.expires_at),
                    |stamp| stamp.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                );
                outln!(
                    "Enrollment ticket {} ({where_held}; single use):",
                    ticket.id
                );
                outln!(
                    "  EXPIRES {when} ({} minutes)",
                    tickets::DEFAULT_TICKET_SECONDS / 60
                );
                outln!(
                    "  the device enrolls its own key with it; nothing secret leaves the device"
                );
                outln!();
            }
            Some(ticket)
        }
        Err(err) => {
            error!("pair: could not mint an enrollment ticket: {err}");
            None
        }
    }
}

/// The workload CA the server's certificate chains to, printed for a human
/// operator (ADR-0153); `None` for a self-signed or operator-supplied leaf.
fn read_pairing_authority(cert: &std::path::Path, json: bool) -> Option<String> {
    let authority = phux_server::transport::tls::presented_authority(cert)
        .ok()
        .flatten()?;
    if !json {
        outln!("Server certificate authority (pinned by the device; it survives leaf renewal):");
        outln!("  {authority}");
        outln!();
    }
    Some(authority)
}

/// List the overlay addresses a device can dial.
fn print_overlay_addresses(overlay: &[IpAddr]) {
    if overlay.is_empty() {
        return;
    }
    outln!("Overlay network addresses (dial one of these from the device):");
    for addr in overlay {
        outln!("  {addr}");
    }
    outln!();
}

/// Where a human mint puts the bearer. JSON is a separate path and always
/// prints it, for `phux host add`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecretSurface {
    /// Print the bearer and the connect link.
    Text,
    /// The bearer is only inside the QR.
    Qr,
    /// The bearer is only in the secret file.
    File,
    /// The bearer is in the QR and the secret file, never as text.
    QrAndFile,
}

const fn secret_surface(output: LinkOutput<'_>) -> SecretSurface {
    match (output.qr, output.secret_file.is_some()) {
        (true, true) => SecretSurface::QrAndFile,
        (true, false) => SecretSurface::Qr,
        (false, true) => SecretSurface::File,
        (false, false) => SecretSurface::Text,
    }
}

/// Camera QR and `--secret-file` keep the bearer off stdout. `--json` does not.
const fn hides_bearer(qr: bool, secret_file: bool, json: bool) -> bool {
    !json && (qr || secret_file)
}

/// Refuse to clobber or invent a parent for a pairing secret, before minting.
fn prepare_secret_file(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty() || path.file_name().is_none_or(std::ffi::OsStr::is_empty) {
        return Err("--secret-file needs a file path".to_owned());
    }
    if path.exists() {
        return Err(format!(
            "{} already exists; refuse to overwrite a pairing secret",
            path.display()
        ));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent
        && !parent.is_dir()
    {
        return Err(format!(
            "--secret-file parent {} is not a directory",
            parent.display()
        ));
    }
    Ok(())
}

fn format_rfc3339(stamp: chrono::DateTime<chrono::Utc>) -> String {
    stamp.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The fields a human mint may show. `token` is written only to the secret
/// file, and only when `hide_bearer` is false is it absent from this path's
/// stdout by the caller having already printed it. This function never prints
/// `token` or `link` when `hide_bearer` is set.
struct HumanSecret<'a> {
    credential_id: &'a str,
    token: &'a str,
    fingerprint: Option<&'a str>,
    link: Option<&'a str>,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    expires_label: Option<&'a str>,
    qr: bool,
    /// Render the QR beneath the text link (never with `hide_bearer`).
    default_qr: bool,
    secret_file: Option<&'a Path>,
    hide_bearer: bool,
}

fn present_human_secret(secret: &HumanSecret<'_>) -> ExitCode {
    print_expiry_banner(
        secret.credential_id,
        secret.expires_at,
        secret.expires_label,
        secret.hide_bearer,
    );
    if secret.hide_bearer {
        outln!(
            "Credential ID (not the token; revoke with `phux pair revoke {}`):",
            secret.credential_id
        );
        outln!("  {}", secret.credential_id);
        outln!();
        if let Some(fingerprint) = secret.fingerprint {
            outln!("Server certificate SHA-256 (compare on the device; not the token):");
            outln!("  {fingerprint}");
            outln!();
        }
        let mut failed = false;
        if let Some(path) = secret.secret_file {
            match write_secret_file(path, secret) {
                Ok(()) => {
                    outln!(
                        "Secret written to {} (mode 0600). The bearer is not shown here.",
                        path.display()
                    );
                    outln!();
                }
                Err(err) => {
                    error!(
                        "pair: credential {} was minted but the secret file could not be written: {err}",
                        secret.credential_id
                    );
                    hint!(
                        "pair: the bearer was not printed. Delete {} if it exists, revoke with `phux pair revoke {}`, and retry.",
                        path.display(),
                        secret.credential_id
                    );
                    failed = true;
                }
            }
        }
        if secret.qr {
            match secret.link {
                Some(link) if print_camera_qr(link) => {}
                Some(_) => failed = true,
                None => {
                    warn!("pair: no connect link to render; nothing scannable was printed");
                    failed = true;
                }
            }
        }
        return if failed {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        };
    }
    if let Some(link) = secret.link {
        print_connect_link(link);
        // A QR that cannot render has already warned; the text link stands.
        if secret.default_qr {
            print_camera_qr(link);
        }
    }
    ExitCode::SUCCESS
}

fn print_expiry_banner(
    credential_id: &str,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    expires_label: Option<&str>,
    loud: bool,
) {
    let line = match (expires_at.map(format_rfc3339), expires_label) {
        (Some(stamp), Some(label)) => format!("EXPIRES {stamp} ({label} from mint)"),
        (Some(stamp), None) => format!("EXPIRES {stamp}"),
        (None, _) => format!("EXPIRES NEVER — valid until `phux pair revoke {credential_id}`"),
    };
    if loud {
        outln!("================================================================");
        outln!("PAIRING — the bearer is not printed on this screen.");
        outln!("{line}");
        if expires_at.is_none() {
            outln!("Do not film a credential that does not expire.");
        }
        outln!("================================================================");
        outln!();
        return;
    }
    outln!("{line}");
    outln!();
}

fn secret_file_body(secret: &HumanSecret<'_>) -> String {
    use std::fmt::Write;
    let mut body = String::new();
    let expires = secret
        .expires_at
        .map_or_else(|| "never".to_owned(), format_rfc3339);
    let _ = write!(
        body,
        "# phux pairing secret. Mode 0600. Delete after the device has paired.\n\
         credential_id={}\n\
         expires_at={expires}\n\
         token={}\n",
        secret.credential_id, secret.token
    );
    if let Some(link) = secret.link {
        let _ = writeln!(body, "connect_link={link}");
        if let Some(legacy) = legacy_connect_link(link) {
            let _ = writeln!(body, "legacy_connect_link={legacy}");
        }
    }
    if let Some(fingerprint) = secret.fingerprint {
        let _ = writeln!(body, "cert_fingerprint={fingerprint}");
    }
    body
}

fn write_secret_file(path: &Path, secret: &HumanSecret<'_>) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let body = secret_file_body(secret);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|err| format!("could not create {}: {err}", path.display()))?;
    file.write_all(body.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|err| format!("could not write {}: {err}", path.display()))?;
    Ok(())
}

/// Print the connect link as text. The QR form never uses this: the link
/// contains the bearer.
fn print_connect_link(link: &str) {
    outln!("One-tap connect link (open on the device — carries the token):");
    outln!("  {link}");
    outln!();
    // App builds without the Universal Link entitlement cannot claim the
    // https form and would open it in a browser; the custom-scheme spelling
    // of the same credentials reaches them. Same secret, same query.
    if let Some(legacy) = legacy_connect_link(link) {
        outln!("Same link for app builds that predate Universal Link support:");
        outln!("  {legacy}");
        outln!();
    }
}

fn print_camera_qr(link: &str) -> bool {
    let columns = stdout_columns();
    let scale = columns.map_or(1, |columns| qr_scale_that_fits(link, columns));
    match render_qr_scaled(link, scale) {
        Ok(art) => {
            let width = art
                .lines()
                .map(|line| line.chars().count())
                .max()
                .unwrap_or(0);
            outln!("Scan to pair (module scale {scale}, {width} columns). The code must not wrap.");
            if let Some(columns) = columns
                && width > usize::from(columns)
            {
                warn!(
                    "pair: the QR is {width} columns and the terminal is {columns}; widen the window or it will not scan"
                );
            }
            outln!();
            out!("{art}");
            outln!();
            true
        }
        Err(err) => {
            warn!("pair: {err}");
            false
        }
    }
}

fn stdout_columns() -> Option<u16> {
    use std::io::IsTerminal;
    let stdout = std::io::stdout();
    if !stdout.is_terminal() {
        return None;
    }
    rustix::termios::tcgetwinsize(&stdout)
        .ok()
        .and_then(|size| (size.ws_col > 0).then_some(size.ws_col))
}

/// Largest module scale in 1..=3 whose rendered width fits `columns`.
fn qr_scale_that_fits(payload: &str, columns: u16) -> u32 {
    for scale in [3, 2, 1] {
        let Ok(art) = render_qr_scaled(payload, scale) else {
            return 1;
        };
        let width = art
            .lines()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0);
        if width <= usize::from(columns) {
            return scale;
        }
    }
    1
}

fn run_credential_action(tokens: &std::path::Path, action: PairAction, json: bool) -> ExitCode {
    match action {
        PairAction::Ls => run_pair_ls(tokens, json),
        PairAction::Prune { unused_for } => run_pair_prune(tokens, &unused_for, json),
        PairAction::Rotate {
            credential_id,
            overlap_seconds,
        } => run_pair_rotate(tokens, &credential_id, overlap_seconds, json),
        PairAction::Revoke { credential_id } => run_pair_revoke(tokens, &credential_id, json),
    }
}

fn run_pair_ls(tokens: &std::path::Path, json: bool) -> ExitCode {
    let rows = match phux_server::auth::list_credentials(tokens) {
        Ok(rows) => rows,
        Err(error) => {
            error!("pair ls: {error}");
            return ExitCode::FAILURE;
        }
    };
    if json {
        let credentials: Vec<serde_json::Value> = rows
            .iter()
            .map(|row| {
                serde_json::json!({
                    "id": row.id,
                    "issued_at": row.issued_at.to_rfc3339(),
                    "last_seen": row.last_seen.map(|t| t.to_rfc3339()),
                    "revoked": row.revoked,
                    "generation": row.generation,
                })
            })
            .collect();
        return crate::output::json(&serde_json::json!({
            "schema_version": 1,
            "operation": "ls",
            "tokens_path": tokens.display().to_string(),
            "credentials": credentials,
        }));
    }
    if rows.is_empty() {
        outln!("No credentials in {}.", tokens.display());
        return ExitCode::SUCCESS;
    }
    outln!(
        "{:<36}  {:<20}  {:<20}  {}",
        "ID",
        "MINTED",
        "LAST SEEN",
        "STATUS"
    );
    for row in rows {
        let minted = row.issued_at.format("%Y-%m-%d %H:%M:%S").to_string();
        let seen = row.last_seen.map_or_else(
            || "never".to_owned(),
            |t| t.format("%Y-%m-%d %H:%M:%S").to_string(),
        );
        let status = if row.revoked { "revoked" } else { "active" };
        outln!("{:<36}  {:<20}  {:<20}  {}", row.id, minted, seen, status);
    }
    ExitCode::SUCCESS
}

fn run_pair_prune(tokens: &std::path::Path, unused_for: &str, json: bool) -> ExitCode {
    let duration = match parse_unused_for(unused_for) {
        Ok(duration) => duration,
        Err(error) => {
            error!("pair prune: {error}");
            return ExitCode::FAILURE;
        }
    };
    let outcome = match phux_server::auth::prune_unused(tokens, duration) {
        Ok(outcome) => outcome,
        Err(error) => {
            error!("pair prune: {error}");
            return ExitCode::FAILURE;
        }
    };
    if !outcome.is_durable() {
        warn!(
            "pair prune: revocation is active, but the store directory could not be synced; do not retry"
        );
    }
    if json {
        return crate::output::json(&serde_json::json!({
            "schema_version": 1,
            "operation": "prune",
            "unused_for": unused_for,
            "revoked": outcome.revoked_ids,
            "tokens_path": tokens.display().to_string(),
        }));
    }
    if outcome.revoked_ids.is_empty() {
        outln!("No unused credentials to prune in {}.", tokens.display());
    } else {
        outln!(
            "Revoked {} unused credential(s):",
            outcome.revoked_ids.len()
        );
        for id in &outcome.revoked_ids {
            outln!("  {id}");
        }
    }
    ExitCode::SUCCESS
}

fn run_pair_rotate(
    tokens: &std::path::Path,
    credential_id: &str,
    overlap_seconds: i64,
    json: bool,
) -> ExitCode {
    let overlap = chrono::Duration::seconds(overlap_seconds);
    let rotated = match phux_server::auth::rotate_credential(tokens, credential_id, overlap) {
        Ok(rotated) => rotated,
        Err(error) => {
            error!("pair rotate: {error}");
            return ExitCode::FAILURE;
        }
    };
    if !rotated.is_durable() {
        warn!(
            "pair rotate: rotation is active, but the store directory could not be synced; do not retry"
        );
    }
    if json {
        return crate::output::json(&serde_json::json!({
            "schema_version": 1,
            "operation": "rotate",
            "credential_id": rotated.id,
            "generation": rotated.generation,
            "token": rotated.secret(),
            "overlap_seconds": overlap_seconds,
            "tokens_path": tokens.display().to_string(),
        }));
    }
    outln!(
        "Rotated credential {} to generation {}.",
        rotated.id,
        rotated.generation
    );
    outln!(
        "Previous generations remain valid for at most {overlap_seconds} seconds and never beyond their absolute expiry."
    );
    outln!(
        "Live sessions still on a previous generation are disconnected when that overlap ends; rotate with --overlap-seconds (up to 86400) to give devices longer to pick up the new token."
    );
    outln!();
    outln!("Pairing token (a secret — give it to the device once):");
    outln!("  {}", rotated.secret());
    outln!();
    outln!("Token written to {}", tokens.display());
    ExitCode::SUCCESS
}

fn run_pair_revoke(tokens: &std::path::Path, credential_id: &str, json: bool) -> ExitCode {
    // Bearer ids never carry the digest prefix; an enrolled certificate's
    // always does. Its revocation lives in the workload registry.
    if credential_id.starts_with("sha256:") {
        return super::workload::run(
            super::workload::WorkloadAction::Revoke {
                credential_id: credential_id.to_owned(),
            },
            json,
        );
    }
    let outcome = match phux_server::auth::revoke_credential(tokens, credential_id) {
        Ok(outcome) => outcome,
        Err(error) => {
            error!("pair revoke: {error}");
            return ExitCode::FAILURE;
        }
    };
    if !outcome.is_durable() {
        warn!(
            "pair revoke: revocation is active, but the store directory could not be synced; do not retry"
        );
    }
    if json {
        return crate::output::json(&serde_json::json!({
            "schema_version": 1,
            "operation": "revoke",
            "credential_id": credential_id,
            "tokens_path": tokens.display().to_string(),
        }));
    }
    outln!("Revoked credential {credential_id} for new connections.");
    outln!("Established sessions remain active until disconnected.");
    ExitCode::SUCCESS
}

/// Parse `--expires`: same spellings as `--unused-for`, capped at 30 days.
fn parse_expires(raw: &str) -> Result<chrono::Duration, String> {
    let duration = parse_duration(raw, "--expires")?;
    let max = chrono::Duration::days(30);
    if duration > max {
        return Err("--expires must be at most 30 days".to_owned());
    }
    Ok(duration)
}

/// Parse `--unused-for`: `30d`, `24h`, `90m`, `45s`, or a bare number of seconds.
fn parse_unused_for(raw: &str) -> Result<chrono::Duration, String> {
    parse_duration(raw, "--unused-for")
}

fn parse_duration(raw: &str, flag: &str) -> Result<chrono::Duration, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(format!("{flag} needs a duration (e.g. 30d, 24h, 90m, 45s)"));
    }
    let split = raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len());
    let (digits, unit) = raw.split_at(split);
    if digits.is_empty() {
        return Err(format!("invalid {flag} duration: {raw:?}"));
    }
    let value: i64 = digits
        .parse()
        .map_err(|_| format!("invalid {flag} duration: {raw:?}"))?;
    if value <= 0 {
        return Err(format!("{flag} must be greater than zero"));
    }
    let seconds = match unit {
        "" | "s" => value,
        "m" => value.saturating_mul(60),
        "h" => value.saturating_mul(3600),
        "d" => value.saturating_mul(86400),
        _ => {
            return Err(format!("invalid {flag} unit in {raw:?}; use s, m, h, or d"));
        }
    };
    Ok(chrono::Duration::seconds(seconds))
}

/// The `phux pair --json` document. Pure, so the shape (including
/// `schema_version`) is unit-testable without touching the environment.
/// `ws_addr`/`quic_addr` are the listeners the running server reports bound,
/// null when that transport is not listening, which is what makes
/// `phux host add` fall back to `ssh://`.
#[allow(
    clippy::too_many_arguments,
    reason = "one field per documented key; a struct would only move the same names one level up"
)]
fn pair_document(
    token: &str,
    fingerprint: Option<&str>,
    overlay: &[IpAddr],
    live: &LiveListeners,
    connect_link: Option<&str>,
    tokens_path: &std::path::Path,
    credential_id: &str,
    generation: u64,
    expires_at: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "token": token,
        "cert_fingerprint": fingerprint,
        "overlay_addresses": overlay
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>(),
        "ws_addr": live.wss,
        "quic_addr": live.quic,
        "connect_link": connect_link,
        "tokens_path": tokens_path.display().to_string(),
        "credential_id": credential_id,
        "generation": generation,
        "expires_at": expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        LiveListeners, MagicDns, PairAddresses, ServerPins, advertised_names, build_connect_link,
        build_connect_link_pinned, build_relay_connect_link, legacy_connect_link, link_refusal,
        live_listeners, pair_document, parse_connect_link, percent_encode,
        phone_enrollment_requested, resolve_quic_endpoint, resolve_server_url,
        select_relay_connector, shows_default_qr,
    };
    use phux_protocol::wire::{
        ListenerDisabledReason, RemoteListenerSlot, RemoteListenerTransport, RemoteListenersReport,
    };
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    use std::path::Path;

    fn live(wss: Option<&str>, quic: Option<&str>) -> LiveListeners {
        LiveListeners {
            wss: wss.map(str::to_owned),
            quic: quic.map(str::to_owned),
        }
    }

    fn addr(text: &str) -> Option<SocketAddr> {
        text.parse().ok()
    }

    /// `phux pair --json` pins `schema_version` 1 plus the documented
    /// fields, with absent addresses/link reported as `null` rather than
    /// omitted.
    #[test]
    fn pair_document_pins_the_contract_shape() {
        let overlay = [IpAddr::V4(Ipv4Addr::new(100, 64, 0, 2))];
        let doc = pair_document(
            "deadbeef",
            Some("AB:CD"),
            &overlay,
            &live(Some("0.0.0.0:8787"), Some("0.0.0.0:8788")),
            Some("https://phux.sh/connect?url=wss://100.64.0.2:8787&token=deadbeef"),
            Path::new("/state/remote-tokens"),
            "credential-a",
            1,
            None,
        );
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["token"], "deadbeef");
        assert_eq!(doc["cert_fingerprint"], "AB:CD");
        assert_eq!(doc["overlay_addresses"], serde_json::json!(["100.64.0.2"]));
        assert_eq!(doc["ws_addr"], "0.0.0.0:8787");
        assert_eq!(doc["quic_addr"], "0.0.0.0:8788");
        assert_eq!(
            doc["connect_link"],
            "https://phux.sh/connect?url=wss://100.64.0.2:8787&token=deadbeef"
        );
        assert_eq!(doc["tokens_path"], "/state/remote-tokens");
        assert_eq!(doc["credential_id"], "credential-a");
        assert_eq!(doc["generation"], 1);
        assert!(doc["expires_at"].is_null());
        assert_eq!(doc.as_object().map(serde_json::Map::len), Some(11));

        // No listener of a transport bound: nulls, not absent keys.
        let doc = pair_document(
            "deadbeef",
            None,
            &[],
            &live(None, None),
            None,
            Path::new("/state/remote-tokens"),
            "credential-a",
            1,
            Some("2026-10-13T06:30:00Z"),
        );
        assert!(doc["cert_fingerprint"].is_null());
        assert!(doc["ws_addr"].is_null());
        assert!(doc["quic_addr"].is_null());
        assert!(doc["connect_link"].is_null());
        assert_eq!(doc["expires_at"], "2026-10-13T06:30:00Z");
        assert_eq!(doc["overlay_addresses"], serde_json::json!([]));
    }

    #[test]
    fn link_includes_only_present_fields_in_stable_order() {
        // url + token are the floor.
        assert_eq!(
            build_connect_link("wss://h:1", None, None, None, "deadbeef"),
            "https://phux.sh/connect?url=wss://h:1&token=deadbeef"
        );
        // Full house, in the order the mobile parser documents.
        assert_eq!(
            build_connect_link(
                "wss://10.0.0.2:8787",
                Some("quic://10.0.0.2:8788"),
                Some("mini"),
                Some("AB:CD"),
                "deadbeef"
            ),
            "https://phux.sh/connect?url=wss://10.0.0.2:8787&quic=quic://10.0.0.2:8788&name=mini&fp=AB:CD&token=deadbeef"
        );
        // No fingerprint — the fp param is absent, not empty.
        assert_eq!(
            build_connect_link("wss://h:1", None, Some("mini"), None, "deadbeef"),
            "https://phux.sh/connect?url=wss://h:1&name=mini&token=deadbeef"
        );
    }

    #[test]
    fn name_is_percent_encoded() {
        assert_eq!(percent_encode("studio mini"), "studio%20mini");
        assert_eq!(percent_encode("plain-name_1.ok~"), "plain-name_1.ok~");
        assert_eq!(percent_encode("a&b=c"), "a%26b%3Dc");
        assert_eq!(
            build_connect_link("wss://h:1", None, Some("studio mini"), None, "aa"),
            "https://phux.sh/connect?url=wss://h:1&name=studio%20mini&token=aa"
        );
    }

    #[test]
    fn host_flag_wins_and_gets_wss_scheme() {
        // Bare host:port gets the wss:// the remote path always uses.
        assert_eq!(
            resolve_server_url(Some("100.64.0.2:8787"), &[], None, None),
            Some("wss://100.64.0.2:8787".to_owned())
        );
        // A full URL passes through untouched (loopback dev path stays ws://).
        assert_eq!(
            resolve_server_url(Some("ws://127.0.0.1:8787"), &[], None, None),
            Some("ws://127.0.0.1:8787".to_owned())
        );
        assert_eq!(
            resolve_server_url(Some("wss://mini.tail-net.ts.net:8787"), &[], None, None),
            Some("wss://mini.tail-net.ts.net:8787".to_owned())
        );
        // The flag also beats the bound listener and a detected overlay address.
        let overlay = [IpAddr::V4(Ipv4Addr::new(100, 64, 0, 9))];
        assert_eq!(
            resolve_server_url(Some("mini:1"), &overlay, None, addr("100.64.0.9:2")),
            Some("wss://mini:1".to_owned())
        );
    }

    /// Without `--host` the link names the address the server's wss listener
    /// is actually bound to, never a guessed port.
    #[test]
    fn derived_url_is_the_bound_wss_listener() {
        let overlay = [IpAddr::V4(Ipv4Addr::new(100, 64, 0, 2))];
        // The auto-overlay listener binds the overlay address itself.
        assert_eq!(
            resolve_server_url(None, &overlay, None, addr("100.64.0.2:8787")),
            Some("wss://100.64.0.2:8787".to_owned())
        );
        // A concrete routable bind is dialed as bound, on its real port.
        assert_eq!(
            resolve_server_url(None, &overlay, None, addr("192.168.1.5:9000")),
            Some("wss://192.168.1.5:9000".to_owned())
        );
        // An unspecified bind is dialed on the overlay address, bound port...
        assert_eq!(
            resolve_server_url(None, &overlay, None, addr("0.0.0.0:9001")),
            Some("wss://100.64.0.2:9001".to_owned())
        );
        // ...and yields nothing with no overlay address to dial it on.
        assert_eq!(
            resolve_server_url(None, &[], None, addr("0.0.0.0:9001")),
            None
        );
        // A loopback bind is unreachable from a device.
        assert_eq!(
            resolve_server_url(None, &overlay, None, addr("127.0.0.1:8787")),
            None
        );
        // No wss listener bound: no link, whatever the overlay says.
        assert_eq!(resolve_server_url(None, &overlay, None, None), None);
        // A v6 address is bracketed so the URL stays parseable.
        let v6 = [IpAddr::V6(Ipv6Addr::new(
            0xfd7a, 0x115c, 0xa1e0, 0, 0, 0, 0, 1,
        ))];
        assert_eq!(
            resolve_server_url(None, &v6, None, addr("[::]:8787")),
            Some("wss://[fd7a:115c:a1e0::1]:8787".to_owned())
        );
    }

    /// QUIC link endpoints follow the same device-reachability rules as WSS:
    /// concrete addresses keep their live port, wildcard binds use an overlay,
    /// loopback is never advertised, and IPv6 is bracketed by `SocketAddr`.
    #[test]
    fn derived_quic_endpoint_is_live_and_device_dialable() {
        let v4 = [IpAddr::V4(Ipv4Addr::new(100, 64, 0, 2))];
        assert_eq!(
            resolve_quic_endpoint(&v4, None, addr("100.64.0.2:8788")),
            Some("quic://100.64.0.2:8788".to_owned())
        );
        assert_eq!(
            resolve_quic_endpoint(&v4, None, addr("192.168.1.5:9000")),
            Some("quic://192.168.1.5:9000".to_owned())
        );
        assert_eq!(
            resolve_quic_endpoint(&v4, None, addr("0.0.0.0:9001")),
            Some("quic://100.64.0.2:9001".to_owned())
        );
        assert_eq!(resolve_quic_endpoint(&[], None, addr("0.0.0.0:9001")), None);
        let v6_only = [IpAddr::V6(Ipv6Addr::LOCALHOST)];
        assert_eq!(
            resolve_quic_endpoint(&v6_only, None, addr("0.0.0.0:9001")),
            None,
            "an IPv4 wildcard does not make an IPv6 address dialable"
        );
        assert_eq!(
            resolve_quic_endpoint(&v4, None, addr("127.0.0.1:8788")),
            None
        );
        assert_eq!(resolve_quic_endpoint(&v4, None, None), None);

        let v6 = [IpAddr::V6(Ipv6Addr::new(
            0xfd7a, 0x115c, 0xa1e0, 0, 0, 0, 0, 1,
        ))];
        assert_eq!(
            resolve_quic_endpoint(&v6, None, addr("[::]:8788")),
            Some("quic://[fd7a:115c:a1e0::1]:8788".to_owned())
        );
        let mixed = [v4[0], v6[0]];
        assert_eq!(
            resolve_quic_endpoint(&mixed, None, addr("[::]:8788")),
            Some("quic://[fd7a:115c:a1e0::1]:8788".to_owned()),
            "a wildcard bind uses an overlay address of the same family"
        );
    }

    fn magic_dns() -> MagicDns {
        MagicDns {
            name: "studio.tail9a8b.ts.net".to_owned(),
            addresses: vec![
                IpAddr::V4(Ipv4Addr::new(100, 64, 0, 2)),
                IpAddr::V6(Ipv6Addr::new(0xfd7a, 0x115c, 0xa1e0, 0, 0, 0, 0, 1)),
            ],
        }
    }

    /// ADR-0164: an endpoint on an address the host's `MagicDNS` name stands
    /// for is named by that name, on both the wss and quic legs, so the link
    /// survives the tailnet reassigning the address. Any other address, and
    /// `--host`, keep the address behaviour.
    #[test]
    fn magic_dns_names_the_tailnet_endpoints() {
        let magic = magic_dns();
        let magic = Some(&magic);
        let v4 = [IpAddr::V4(Ipv4Addr::new(100, 64, 0, 2))];
        let v6 = [IpAddr::V6(Ipv6Addr::new(
            0xfd7a, 0x115c, 0xa1e0, 0, 0, 0, 0, 1,
        ))];

        // The auto-overlay bind, and a wildcard bind dialed on the overlay.
        assert_eq!(
            resolve_server_url(None, &v4, magic, addr("100.64.0.2:8787")),
            Some("wss://studio.tail9a8b.ts.net:8787".to_owned())
        );
        assert_eq!(
            resolve_server_url(None, &v4, magic, addr("0.0.0.0:9001")),
            Some("wss://studio.tail9a8b.ts.net:9001".to_owned())
        );
        assert_eq!(
            resolve_quic_endpoint(&v4, magic, addr("100.64.0.2:8788")),
            Some("quic://studio.tail9a8b.ts.net:8788".to_owned())
        );
        assert_eq!(
            resolve_quic_endpoint(&v6, magic, addr("[::]:8788")),
            Some("quic://studio.tail9a8b.ts.net:8788".to_owned()),
            "the name stands for the v6 tailnet address too, unbracketed"
        );

        // A bind the name does not resolve to keeps its address.
        assert_eq!(
            resolve_server_url(None, &v4, magic, addr("192.168.1.5:9000")),
            Some("wss://192.168.1.5:9000".to_owned())
        );
        assert_eq!(
            resolve_quic_endpoint(&[], magic, addr("100.64.0.9:8788")),
            Some("quic://100.64.0.9:8788".to_owned())
        );
        // Loopback and unresolvable wildcards are still not advertised.
        assert_eq!(
            resolve_quic_endpoint(&v4, magic, addr("127.0.0.1:8788")),
            None
        );
        assert_eq!(
            resolve_quic_endpoint(&[], magic, addr("0.0.0.0:8788")),
            None
        );
        // `--host` is the operator's claim and wins over the name.
        assert_eq!(
            resolve_server_url(Some("100.64.0.2:8787"), &v4, magic, addr("100.64.0.2:8787")),
            Some("wss://100.64.0.2:8787".to_owned())
        );
    }

    /// A `MagicDNS` link is the same shape, and both legs parse: the CLI's own
    /// parser accepts `quic://NAME:PORT`, and the certificate SANs carry the
    /// name whichever leg names it.
    #[test]
    fn a_magic_dns_link_round_trips_and_names_its_sans() {
        let magic = magic_dns();
        let v4 = [IpAddr::V4(Ipv4Addr::new(100, 64, 0, 2))];
        let url = resolve_server_url(None, &v4, Some(&magic), addr("100.64.0.2:8787"));
        let quic = resolve_quic_endpoint(&v4, Some(&magic), addr("100.64.0.2:8788"));
        let link = build_connect_link(
            url.as_deref().expect("url"),
            quic.as_deref(),
            Some("studio"),
            Some("AB:CD"),
            "deadbeef",
        );
        assert_eq!(
            link,
            "https://phux.sh/connect?url=wss://studio.tail9a8b.ts.net:8787\
             &quic=quic://studio.tail9a8b.ts.net:8788&name=studio&fp=AB:CD&token=deadbeef"
        );
        let parsed = parse_connect_link(&link).expect("parse");
        assert_eq!(
            parsed.url.as_deref(),
            Some("wss://studio.tail9a8b.ts.net:8787")
        );
        assert_eq!(
            parsed.quic.as_deref(),
            Some("quic://studio.tail9a8b.ts.net:8788")
        );

        assert_eq!(
            advertised_names(url.as_deref(), quic.as_deref(), &v4),
            vec!["studio.tail9a8b.ts.net", "100.64.0.2"]
        );
        // A QUIC-only server still names its MagicDNS host in the SANs.
        assert_eq!(
            advertised_names(None, quic.as_deref(), &v4),
            vec!["studio.tail9a8b.ts.net", "100.64.0.2"]
        );
        assert_eq!(
            advertised_names(None, Some("quic://[fd7a:115c:a1e0::1]:8788"), &[]),
            vec!["fd7a:115c:a1e0::1"]
        );
    }

    /// The `MagicDNS` name is claimed by a fresh certificate but never warned
    /// about on an existing one; every other advertised name still is.
    #[test]
    fn an_uncovered_magic_dns_name_is_not_warned_about() {
        let addresses = PairAddresses {
            overlay: vec![IpAddr::V4(Ipv4Addr::new(100, 64, 0, 2))],
            live: live(Some("100.64.0.2:8787"), None),
            server_url: Some("wss://studio.tail9a8b.ts.net:8787".to_owned()),
            quic_endpoint: None,
            advertised: vec!["studio.tail9a8b.ts.net".to_owned(), "100.64.0.2".to_owned()],
            magic_dns_name: Some("studio.tail9a8b.ts.net".to_owned()),
        };
        assert_eq!(addresses.checked_names(), vec!["100.64.0.2"]);
        let without = PairAddresses {
            magic_dns_name: None,
            ..addresses
        };
        assert_eq!(without.checked_names(), without.advertised);
    }

    #[test]
    fn machine_enrollment_accepts_loopback_but_phone_qrs_need_a_dialable_route() {
        let mut addresses = PairAddresses {
            overlay: vec![],
            live: live(None, Some("127.0.0.1:8788")),
            server_url: None,
            quic_endpoint: None,
            advertised: vec![],
            magic_dns_name: None,
        };
        assert!(addresses.enrollment_endpoint_ready(false));
        assert!(!addresses.enrollment_endpoint_ready(true));
        addresses.quic_endpoint = Some("quic://100.64.0.2:8788".to_owned());
        assert!(addresses.enrollment_endpoint_ready(true));
        addresses.live.quic = None;
        addresses.quic_endpoint = None;
        assert!(!addresses.enrollment_endpoint_ready(false));
        assert!(!addresses.enrollment_endpoint_ready(true));
    }

    /// The QR beneath the text link is the default only on a terminal, and
    /// only when no other form was chosen: `--qr` (the camera form),
    /// `--no-qr`, `--json`, or `--secret-file`.
    #[test]
    fn the_default_qr_shows_only_on_a_terminal_with_no_other_form() {
        assert!(shows_default_qr(false, true));
        assert!(!shows_default_qr(false, false), "piped output stays text");
        assert!(!shows_default_qr(true, true), "another form was chosen");
        assert!(!shows_default_qr(true, false));
    }

    #[test]
    fn camera_pairing_enrolls_without_an_extra_flag_but_machine_mints_do_not() {
        assert!(phone_enrollment_requested(false, true));
        assert!(phone_enrollment_requested(true, false));
        assert!(!phone_enrollment_requested(false, false));
    }

    /// No listener report, or one with nothing bound, is a refusal naming
    /// the socket and each disabled row's reason; any bound row admits a mint.
    #[test]
    fn a_mint_needs_a_bound_remote_listener() {
        let socket = Path::new("/run/phux.sock");

        let absent = live_listeners(socket, None).expect_err("no report, nothing bound");
        assert!(
            absent.contains("/run/phux.sock") && absent.contains("no remote listener bound"),
            "{absent}"
        );
        assert!(absent.contains("none was minted"), "{absent}");

        let empty = RemoteListenersReport::new();
        assert!(live_listeners(socket, Some(&empty)).is_err());

        let wss_down = RemoteListenerSlot::disabled(
            RemoteListenerTransport::Wss,
            Some("100.64.0.2:8787".to_owned()),
            ListenerDisabledReason::BindFailed,
        );
        let disabled = RemoteListenersReport::new().with_listeners(vec![wss_down.clone()]);
        let refusal = live_listeners(socket, Some(&disabled)).expect_err("disabled is not bound");
        assert!(
            refusal.contains("wss at 100.64.0.2:8787 disabled (bind_failed)"),
            "{refusal}"
        );

        let quic_only = RemoteListenersReport::new().with_listeners(vec![
            wss_down,
            RemoteListenerSlot::bound(RemoteListenerTransport::Quic, "0.0.0.0:8788"),
        ]);
        assert_eq!(
            live_listeners(socket, Some(&quic_only)).expect("QUIC accepts credentials"),
            live(None, Some("0.0.0.0:8788"))
        );

        let both = RemoteListenersReport::new().with_listeners(vec![
            RemoteListenerSlot::bound(RemoteListenerTransport::Wss, "100.64.0.2:8787"),
            RemoteListenerSlot::bound(RemoteListenerTransport::Quic, "100.64.0.2:8788"),
        ]);
        assert_eq!(
            live_listeners(socket, Some(&both)).expect("bound"),
            live(Some("100.64.0.2:8787"), Some("100.64.0.2:8788"))
        );
    }

    /// `--host` needs a wss listener behind it and `--qr` needs a link; both
    /// are refused before anything is minted.
    #[test]
    fn link_requests_that_cannot_connect_are_refused() {
        let quic_only = live(None, Some("0.0.0.0:8788"));
        let wss = live(Some("100.64.0.2:8787"), None);
        let loopback = live(Some("127.0.0.1:8787"), None);

        // No link asked for: minting for QUIC alone is fine.
        assert_eq!(link_refusal(None, false, &quic_only, None), None);
        // --host with no wss listener cannot connect.
        let refusal = link_refusal(
            Some("mini:8787"),
            false,
            &quic_only,
            Some("wss://mini:8787"),
        )
        .expect("refused");
        assert!(refusal.contains("no wss listener bound"), "{refusal}");
        // --host over a bound wss listener is the operator's claim to keep.
        assert_eq!(
            link_refusal(Some("mini:8787"), true, &loopback, Some("wss://mini:8787")),
            None
        );
        // --qr with nothing to encode names the bind that is unreachable.
        let refusal = link_refusal(None, true, &loopback, None).expect("refused");
        assert!(refusal.contains("bound to 127.0.0.1:8787"), "{refusal}");
        let refusal = link_refusal(None, true, &quic_only, None).expect("refused");
        assert!(refusal.contains("not bound"), "{refusal}");
        // --qr with a derived link goes ahead.
        assert_eq!(
            link_refusal(None, true, &wss, Some("wss://100.64.0.2:8787")),
            None
        );
    }

    /// The SAN list must be exactly what a client will ask for: the link's
    /// host with no scheme, no port, and no v6 brackets (phux-q9a0).
    #[test]
    fn advertised_names_are_the_hosts_a_client_verifies() {
        let overlay = [IpAddr::V4(Ipv4Addr::new(100, 64, 0, 2))];

        // The link host leads, and the overlay address it was derived from
        // does not repeat.
        assert_eq!(
            advertised_names(Some("wss://100.64.0.2:8787"), None, &overlay),
            vec!["100.64.0.2"]
        );

        // A `--host` name is carried alongside every detected overlay address:
        // `phux pair` prints them all as dialable, so all of them belong in
        // the certificate.
        assert_eq!(
            advertised_names(Some("wss://mini.tail-net.ts.net:8787"), None, &overlay),
            vec!["mini.tail-net.ts.net", "100.64.0.2"]
        );

        // v6 arrives bracketed in a URL and must be unbracketed in a SAN.
        let v6 = [IpAddr::V6(Ipv6Addr::LOCALHOST)];
        assert_eq!(
            advertised_names(Some("wss://[fd7a:115c:a1e0::1]:8787"), None, &v6),
            vec!["fd7a:115c:a1e0::1", "::1"]
        );

        // No link at all: still name whatever was detected, since the operator
        // may dial it by hand.
        assert_eq!(advertised_names(None, None, &overlay), vec!["100.64.0.2"]);
        assert!(advertised_names(None, None, &[]).is_empty());

        // A URL that will not parse contributes nothing rather than poisoning
        // the list — pairing still has a token to mint.
        assert_eq!(
            advertised_names(Some("not a url"), None, &overlay),
            ["100.64.0.2"]
        );
    }

    #[test]
    fn renders_a_nonempty_qr_for_a_realistic_payload() {
        // A real 32-byte hex token + SHA-256 fingerprint is well within QR
        // capacity; the renderer must produce non-empty half-block art.
        let link = build_connect_link(
            "wss://100.64.0.2:8787",
            Some("quic://100.64.0.2:8788"),
            Some("mini"),
            Some("CD:".repeat(31).trim_end_matches(':')),
            &"ab".repeat(32),
        );
        let art = super::render_qr_scaled(&link, 1).expect("QR should encode");
        assert!(!art.is_empty(), "QR render must be non-empty");
        // Dense1x2 uses half-block glyphs; at least one must appear.
        assert!(
            art.chars().any(|c| matches!(c, '█' | '▀' | '▄' | ' ')),
            "QR render must contain half-block glyphs",
        );
    }

    #[test]
    fn a_larger_qr_is_wider_and_the_scale_stays_inside_the_terminal() {
        let link = build_connect_link(
            "wss://100.64.0.2:8787",
            None,
            Some("studio"),
            Some("AB:CD"),
            &"ab".repeat(32),
        );
        let small = super::render_qr_scaled(&link, 1).expect("scale 1");
        let large = super::render_qr_scaled(&link, 2).expect("scale 2");
        let width = |art: &str| {
            art.lines()
                .map(|line| line.chars().count())
                .max()
                .unwrap_or(0)
        };
        assert_eq!(width(&large), width(&small) * 2);
        let columns = u16::try_from(width(&small)).unwrap();
        assert_eq!(super::qr_scale_that_fits(&link, columns), 1);
        assert_eq!(
            super::qr_scale_that_fits(&link, columns.saturating_mul(2)),
            2
        );
    }

    #[test]
    fn camera_and_secret_file_output_omit_the_bearer() {
        use std::os::unix::fs::PermissionsExt;
        let token = "ab".repeat(32);
        let link = build_connect_link("wss://100.64.0.2:8787", None, None, None, &token);
        let secret = super::HumanSecret {
            credential_id: "credential-a",
            token: &token,
            fingerprint: Some("AB:CD"),
            link: Some(&link),
            expires_at: None,
            expires_label: None,
            qr: true,
            default_qr: false,
            secret_file: Some(Path::new("/tmp/pair-secret")),
            hide_bearer: true,
        };
        let body = super::secret_file_body(&secret);
        assert!(body.contains(&format!("token={token}")));
        assert!(body.contains(&format!("connect_link={link}")));
        assert!(body.contains("expires_at=never"));
        assert!(!super::hides_bearer(false, false, false));
        assert!(super::hides_bearer(true, false, false));
        assert!(super::hides_bearer(false, true, false));
        assert!(
            !super::hides_bearer(true, true, true),
            "json still carries the bearer"
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("secret");
        super::prepare_secret_file(&path).expect("new path");
        super::write_secret_file(&path, &secret).expect("write");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(
            super::prepare_secret_file(&path).is_err(),
            "refuse to overwrite"
        );
        assert!(super::parse_expires("15m").is_ok());
        assert!(super::parse_expires("31d").is_err());
        assert!(super::parse_expires("0s").is_err());
    }

    /// The parser is the builder's exact inverse; the shape is a cross-repo
    /// contract.
    #[test]
    fn connect_link_round_trips() {
        let link = build_connect_link(
            "wss://100.64.0.2:8787",
            Some("quic://100.64.0.2:8788"),
            Some("mini box"),
            Some("AB:CD:EF"),
            "deadbeef",
        );
        let parsed = parse_connect_link(&link).expect("parse");
        assert_eq!(parsed.url.as_deref(), Some("wss://100.64.0.2:8787"));
        assert_eq!(parsed.tls_server_name, None);
        assert_eq!(parsed.quic.as_deref(), Some("quic://100.64.0.2:8788"));
        assert_eq!(parsed.name.as_deref(), Some("mini box"));
        assert_eq!(parsed.cert_fingerprint.as_deref(), Some("AB:CD:EF"));
        assert_eq!(parsed.token, "deadbeef");
    }

    /// ADR-0153: a direct link carries the server's CA as `ca` after `fp`,
    /// in its canonical spelling; a malformed one refuses the link rather
    /// than pinning nothing silently, and a link without one parses as
    /// before.
    #[test]
    fn a_link_carries_the_certificate_authority_after_the_leaf_pin() {
        let authority = format!("sha256:{}", "ab".repeat(32));
        let link = build_connect_link_pinned(
            "wss://100.64.0.2:8787",
            None,
            None,
            ServerPins {
                leaf: Some("AB:CD"),
                authority: Some(&authority),
                enrollment: None,
            },
            "deadbeef",
        );
        assert_eq!(
            link,
            format!(
                "https://phux.sh/connect?url=wss://100.64.0.2:8787&fp=AB:CD&ca={authority}&token=deadbeef"
            )
        );
        let parsed = parse_connect_link(&link).expect("parse");
        assert_eq!(parsed.ca_fingerprint.as_deref(), Some(authority.as_str()));
        assert_eq!(parsed.cert_fingerprint.as_deref(), Some("AB:CD"));

        let shouted = link.replace(
            &authority,
            &authority.to_uppercase().replace("SHA256", "sha256"),
        );
        assert_eq!(
            parse_connect_link(&shouted)
                .expect("any hex case")
                .ca_fingerprint
                .as_deref(),
            Some(authority.as_str())
        );
        let truncated = link.replace(&authority, "sha256:abcd");
        let err = parse_connect_link(&truncated).expect_err("a truncated pin");
        assert!(err.contains("ca"), "{err}");
        assert_eq!(
            parse_connect_link(&build_connect_link(
                "wss://h:1",
                None,
                None,
                Some("AB"),
                "t"
            ))
            .expect("parse")
            .ca_fingerprint,
            None
        );
    }

    /// A link with no `name`/`fp` (the minimal shape the builder emits) is
    /// still parseable — those two are genuinely optional.
    #[test]
    fn connect_link_without_optional_fields_parses() {
        let link = build_connect_link("wss://mini.ts.net:8787", None, None, None, "abc123");
        let parsed = parse_connect_link(&link).expect("parse");
        assert_eq!(parsed.quic, None);
        assert_eq!(parsed.name, None);
        assert_eq!(parsed.cert_fingerprint, None);
        assert_eq!(parsed.token, "abc123");
    }

    /// Shells and chat clients wrap a pasted link in quotes; stripping them
    /// is cheaper than teaching every operator to remove them.
    #[test]
    fn connect_link_tolerates_pasted_quotes_and_whitespace() {
        let link = build_connect_link("wss://mini:8787", None, None, Some("AB"), "tok");
        let pasted = format!("  '{link}'\n");
        assert_eq!(
            parse_connect_link(&pasted).expect("parse"),
            parse_connect_link(&link).expect("parse"),
        );
    }

    /// A newer minting phux must be able to add a query key without breaking
    /// an older `--code`.
    #[test]
    fn connect_link_tolerates_unknown_query_keys() {
        let parsed = parse_connect_link(
            "https://phux.sh/connect?url=wss://mini:8787&quic=quic://mini:8788&brand_new=42&token=tok",
        )
        .expect("parse");
        assert_eq!(parsed.quic.as_deref(), Some("quic://mini:8788"));
        assert_eq!(parsed.token, "tok");
    }

    /// The custom-scheme spelling parses to identical credentials.
    #[test]
    fn legacy_scheme_is_the_same_link_under_another_prefix() {
        let link = build_connect_link(
            "wss://mini:8787",
            None,
            Some("studio mini"),
            Some("AB:CD"),
            "tok",
        );
        let legacy = legacy_connect_link(&link).expect("built links always respell");
        assert_eq!(
            legacy,
            "phux://connect?url=wss://mini:8787&name=studio%20mini&fp=AB:CD&token=tok"
        );
        assert_eq!(
            parse_connect_link(&legacy).expect("legacy parse"),
            parse_connect_link(&link).expect("https parse"),
        );
        // Only an https connect link has a legacy spelling.
        assert_eq!(
            legacy_connect_link("phux://connect?url=wss://x&token=t"),
            None
        );
        assert_eq!(legacy_connect_link("https://example.com/connect?x=1"), None);
    }

    /// A link minted before the move to `phux.sh` still pairs.
    #[test]
    fn connect_link_still_parses_the_legacy_host() {
        let parsed = parse_connect_link(
            "https://phux.phall.io/connect?url=wss://10.0.0.2:8787&name=mini&fp=AB:CD&token=deadbeef",
        )
        .expect("a link on the pre-move host must still pair");
        assert_eq!(parsed.url.as_deref(), Some("wss://10.0.0.2:8787"));
        assert_eq!(parsed.name.as_deref(), Some("mini"));
        assert_eq!(parsed.cert_fingerprint.as_deref(), Some("AB:CD"));
        assert_eq!(parsed.token, "deadbeef");

        // But it is no longer what we emit.
        assert!(
            build_connect_link("wss://h:1", None, None, None, "tok")
                .starts_with("https://phux.sh/")
        );
    }

    /// The two fields a dial cannot proceed without are rejected loudly,
    /// and a non-ws scheme is refused rather than dialed.
    #[test]
    fn connect_link_refuses_links_that_cannot_dial() {
        // Not a connect link at all: a different host, or a different path
        // under the right host.
        assert!(parse_connect_link("https://example.com").is_err());
        assert!(parse_connect_link("https://example.com/connect?url=wss://m:1&token=t").is_err());
        assert!(parse_connect_link("https://phux.phall.io/other?url=wss://m:1&token=t").is_err());
        assert!(parse_connect_link("https://phux.sh/connected?url=wss://m:1&token=t").is_err());
        // No token: grants no access.
        assert!(parse_connect_link("https://phux.sh/connect?url=wss://mini:8787").is_err());
        // No url: names no server.
        assert!(parse_connect_link("https://phux.sh/connect?token=tok").is_err());
        // `url` stays the WebSocket fallback; QUIC has its own additive key.
        assert!(
            parse_connect_link("https://phux.sh/connect?url=quic://mini:8788&token=tok").is_err()
        );
        for invalid_quic in [
            "https://mini:8788",
            "quic://mini",
            "quic://mini:0",
            "quic://mini:not-a-port",
            "quic://user@mini:8788",
            "quic://mini:8788/path",
            "quic://mini:8788?mode=fast",
            "quic://fd7a:115c:a1e0::1:8788",
        ] {
            assert!(
                parse_connect_link(&format!(
                    "https://phux.sh/connect?url=wss://mini:8787&quic={invalid_quic}&token=tok"
                ))
                .is_err(),
                "accepted {invalid_quic}"
            );
        }
        // Empty values are the same as absent, under either prefix.
        assert!(parse_connect_link("https://phux.sh/connect?url=wss://mini:8787&token=").is_err());
        assert!(parse_connect_link("phux://connect?url=wss://mini:8787&token=").is_err());
        // Ambiguous endpoint and credential fields fail closed.
        for duplicate in [
            "url=wss://one:8787&url=wss://two:8787&token=tok",
            "url=wss://mini:8787&quic=quic://one:8788&quic=quic://two:8788&token=tok",
            "url=wss://mini:8787&token=one&token=two",
        ] {
            assert!(
                parse_connect_link(&format!("https://phux.sh/connect?{duplicate}")).is_err(),
                "accepted {duplicate}"
            );
        }
    }

    /// A relay link (ADR-0149) carries the relay as `quic`, the route as
    /// `sni`, and no `url`; it round-trips under both prefixes.
    #[test]
    fn relay_link_round_trips_without_a_url() {
        let link = build_relay_connect_link(
            "relay.example:4433",
            "mini-route",
            Some("mini box"),
            ServerPins {
                leaf: Some("AB:CD"),
                ..ServerPins::default()
            },
            "deadbeef",
        );
        assert_eq!(
            link,
            "https://phux.sh/connect?quic=quic://relay.example:4433&sni=mini-route\
             &name=mini%20box&fp=AB:CD&token=deadbeef"
        );
        let parsed = parse_connect_link(&link).expect("parse");
        assert_eq!(parsed.url, None);
        assert_eq!(parsed.quic.as_deref(), Some("quic://relay.example:4433"));
        assert_eq!(parsed.tls_server_name.as_deref(), Some("mini-route"));
        assert_eq!(parsed.name.as_deref(), Some("mini box"));
        assert_eq!(parsed.cert_fingerprint.as_deref(), Some("AB:CD"));
        assert_eq!(parsed.token, "deadbeef");
        assert_eq!(
            parsed.registry_endpoint(),
            Some("quic://relay.example:4433")
        );
        let legacy = legacy_connect_link(&link).expect("respells");
        assert_eq!(parse_connect_link(&legacy).expect("legacy parse"), parsed);
    }

    /// Old links (no `sni`) keep registering their WebSocket url; a link with
    /// both registers the relay route, since `sni` only means anything there.
    #[test]
    fn registry_endpoint_is_the_url_unless_the_link_is_routed() {
        let direct = parse_connect_link(
            "https://phux.sh/connect?url=wss://mini:8787&quic=quic://mini:8788&token=tok",
        )
        .expect("parse");
        assert_eq!(direct.registry_endpoint(), Some("wss://mini:8787"));
        let both = parse_connect_link(
            "https://phux.sh/connect?url=wss://mini:8787&quic=quic://relay:4433&sni=r&token=tok",
        )
        .expect("parse");
        assert_eq!(both.registry_endpoint(), Some("quic://relay:4433"));
        assert_eq!(both.tls_server_name.as_deref(), Some("r"));
    }

    /// `sni` without `quic` cannot be dialed, a bad name is refused rather
    /// than offered, and a duplicate is ambiguous.
    #[test]
    fn relay_links_that_cannot_dial_are_refused() {
        let no_quic = parse_connect_link("https://phux.sh/connect?url=wss://m:1&sni=r&token=t")
            .expect_err("sni needs quic");
        assert!(no_quic.contains("quic="), "{no_quic}");
        for bad in ["-r", "127.0.0.1", "a%20b", "a/b"] {
            assert!(
                parse_connect_link(&format!(
                    "https://phux.sh/connect?quic=quic://relay:4433&sni={bad}&token=t"
                ))
                .is_err(),
                "accepted sni={bad}"
            );
        }
        assert!(
            parse_connect_link(
                "https://phux.sh/connect?quic=quic://relay:4433&sni=a&sni=b&token=t"
            )
            .is_err()
        );
        // An empty `sni` is absent, so the link needs its `url` again.
        assert!(
            parse_connect_link("https://phux.sh/connect?quic=quic://relay:4433&sni=&token=t")
                .is_err()
        );
    }

    fn connector(relay: &str) -> phux_config::ConnectorConfigEntry {
        phux_config::ConnectorConfigEntry {
            relay: relay.to_owned(),
            token_file: None,
            cert_fingerprint: Some("AB:CD".to_owned()),
        }
    }

    /// The relay a link dials is the one configured connector, or the one
    /// `--relay` names; nothing or an ambiguous choice is refused.
    #[test]
    fn relay_link_dials_a_configured_connector() {
        let one = vec![connector("relay.example:4433")];
        assert_eq!(
            select_relay_connector(one.clone(), None).expect("only one"),
            connector("relay.example:4433")
        );
        assert!(
            select_relay_connector(one, Some("other:4433"))
                .expect_err("unknown relay")
                .contains("relay.example:4433")
        );
        let none = select_relay_connector(Vec::new(), None).expect_err("none");
        assert!(none.contains("phux relay pair --route"), "{none}");
        let two = vec![connector("a:4433"), connector("b:4433")];
        let ambiguous = select_relay_connector(two.clone(), None).expect_err("ambiguous");
        assert!(ambiguous.contains("--relay HOST:PORT"), "{ambiguous}");
        assert_eq!(
            select_relay_connector(two, Some("b:4433")).expect("named"),
            connector("b:4433")
        );
    }
}
