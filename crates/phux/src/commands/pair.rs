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
fn build_connect_link(
    url: &str,
    quic: Option<&str>,
    name: Option<&str>,
    fingerprint: Option<&str>,
    token: &str,
) -> String {
    let mut link = format!("{CONNECT_URI_PREFIX}?url={url}");
    if let Some(quic) = quic {
        link.push_str("&quic=");
        link.push_str(quic);
    }
    push_link_credentials(&mut link, name, fingerprint, token);
    link
}

/// Build a relay link (ADR-0149): the relay's `quic://` endpoint, the route
/// it is dialed with as `sni`, the relay's pin, and the server's token.
fn build_relay_connect_link(
    relay: &str,
    route: &str,
    name: Option<&str>,
    fingerprint: Option<&str>,
    token: &str,
) -> String {
    let mut link = format!("{CONNECT_URI_PREFIX}?quic=quic://{relay}&sni={route}");
    push_link_credentials(&mut link, name, fingerprint, token);
    link
}

/// Append the fields every link ends with, in the documented order.
fn push_link_credentials(
    link: &mut String,
    name: Option<&str>,
    fingerprint: Option<&str>,
    token: &str,
) {
    if let Some(name) = name {
        link.push_str("&name=");
        link.push_str(&percent_encode(name));
    }
    if let Some(fp) = fingerprint {
        link.push_str("&fp=");
        link.push_str(fp);
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
        token,
    } = parse_connect_fields(query)?;
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
/// advertised to another device.
fn resolve_bound_endpoint(
    scheme: &str,
    overlay: &[IpAddr],
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
    Some(format!("{scheme}://{}", SocketAddr::new(ip, bound.port())))
}

/// Resolve the ws(s):// URL the link embeds: `--host` wins (a bare
/// `host:port` gets `wss://`); otherwise use the live wss listener.
fn resolve_server_url(
    host: Option<&str>,
    overlay: &[IpAddr],
    bound_wss: Option<SocketAddr>,
) -> Option<String> {
    if let Some(host) = host {
        if host.starts_with("ws://") || host.starts_with("wss://") {
            return Some(host.to_owned());
        }
        return Some(format!("wss://{host}"));
    }
    resolve_bound_endpoint("wss", overlay, bound_wss)
}

/// Resolve the preferred raw-QUIC endpoint from the running listener only.
fn resolve_quic_endpoint(overlay: &[IpAddr], bound_quic: Option<SocketAddr>) -> Option<String> {
    resolve_bound_endpoint("quic", overlay, bound_quic)
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
         give it one: restart it with `--listen HOST:PORT` or `--quic HOST:PORT` \
         (`PHUX_WS_ADDR` / `PHUX_QUIC_ADDR`), or join an overlay network so the default-profile \
         server auto-binds one (`phux upgrade` restarts it with panes intact); \
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
/// form: the link host (via [`WsTarget::parse`], the dialer's own parser) and
/// each overlay address. An unparseable URL contributes nothing.
fn advertised_names(server_url: Option<&str>, overlay: &[IpAddr]) -> Vec<String> {
    use phux_client::attach::ws::WsTarget;

    let mut names: Vec<String> = server_url
        .and_then(|url| WsTarget::parse(url).ok())
        .map(|target| target.server_name)
        .into_iter()
        .collect();
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
    eprintln!(
        "phux pair: warning: this certificate does not name {} — a device that pins \
         the fingerprint above is unaffected, but a client that validates the server \
         name (a browser, or curl --cacert) will refuse the handshake.",
        uncovered.join(", ")
    );
    eprintln!(
        "phux pair: warning: widening it means a NEW certificate and a NEW fingerprint, \
         which un-pairs every already-paired device. To do it deliberately: rm {} {} \
         && phux pair, then re-pair every device.",
        cert.display(),
        key.display()
    );
}

/// Render `payload` as a Unicode half-block QR string (`Dense1x2`, two module
/// rows per glyph row) with a quiet zone, or an error message on the rare
/// encode failure (payload beyond QR's ~2.9 KB byte capacity).
fn render_qr(payload: &str) -> Result<String, String> {
    use qrcode::QrCode;
    use qrcode::render::unicode;

    QrCode::new(payload.as_bytes())
        .map(|code| code.render::<unicode::Dense1x2>().quiet_zone(true).build())
        .map_err(|err| format!("could not encode pairing QR: {err}"))
}

/// Mint a token into the store and print it with the certificate
/// fingerprint. Defaults are the shared paths the server reads (ADR-0031), and
/// the certificate is provisioned if absent. Nothing is minted unless the
/// server on `socket` has a remote listener bound (ADR-0141). With a
/// reachable wss listener the credentials are also printed as a connect
/// link, and `--qr` renders it. With `relay` the credential is minted for a
/// relay route instead ([`mint_relay_link`]).
#[allow(
    clippy::needless_pass_by_value,
    reason = "CLI entry point owns the args clap dispatch hands it; taking them by value keeps the call site clean"
)]
#[allow(
    clippy::too_many_arguments,
    reason = "the existing mint options remain flat for CLI compatibility while the optional action selects lifecycle operations"
)]
pub(crate) fn run_pair(
    action: Option<PairAction>,
    socket: Option<PathBuf>,
    tokens: Option<PathBuf>,
    cert: Option<PathBuf>,
    qr: bool,
    host: Option<String>,
    name: Option<String>,
    json: bool,
    migrate_legacy: bool,
    replace_token: Option<String>,
    relay: Option<RelayRoute>,
) -> ExitCode {
    let tokens = tokens
        .or_else(|| std::env::var_os("PHUX_WS_TOKENS").map(PathBuf::from))
        .unwrap_or_else(phux_server::auth::default_token_store_path);
    if let Some(action) = action {
        return run_credential_action(&tokens, action, json);
    }
    let certificate = resolve_certificate_paths(cert);

    if migrate_legacy && !migrate_legacy_credentials(&tokens) {
        return ExitCode::FAILURE;
    }

    // Everything that could make the credential useless is settled before
    // the store is touched. The store's own refusal comes first, in the
    // words a failed mint uses, because `phux host add` answers the
    // legacy-store one by migrating.
    if let Err(err) = phux_server::auth::TokenStore::load(&tokens) {
        eprintln!("phux pair: failed to mint token: {err}");
        return ExitCode::FAILURE;
    }
    let socket = socket.unwrap_or_else(phux_server::runtime::default_socket_path);
    if let Some(relay) = relay {
        let output = LinkOutput {
            name: name.as_deref(),
            qr,
            json,
        };
        return mint_relay_link(&relay, &socket, &tokens, output, replace_token.as_deref());
    }
    let live = match query_live_listeners(&socket) {
        Ok(live) => live,
        Err(refusal) => {
            eprintln!("phux pair: {refusal}");
            return ExitCode::FAILURE;
        }
    };
    let addresses = resolve_pair_addresses(host.as_deref(), live);
    if let Some(refusal) = link_refusal(
        host.as_deref(),
        qr,
        &addresses.live,
        addresses.server_url.as_deref(),
    ) {
        eprintln!("phux pair: {refusal}");
        return ExitCode::FAILURE;
    }
    provision_pairing_certificate(&certificate, &addresses.advertised);

    let Some(minted) = mint_pairing_credential(&tokens, replace_token.as_deref()) else {
        return ExitCode::FAILURE;
    };
    let token = minted.secret().to_owned();

    // `--json` keeps stdout a single document; `phux host add` consumes it over
    // ssh.
    if !json {
        print_credential_block(&minted.id, &token);
    }

    let fingerprint = read_pairing_fingerprint(&certificate.cert, json);
    warn_on_uncovered_names(&certificate.cert, &certificate.key, &addresses.advertised);

    if !json {
        print_overlay_addresses(&addresses.overlay);
    }

    // The one-tap link (and its QR form) carries the token — it is as much
    // a secret as the token line above, shown once on the same terminal.
    let link = addresses.server_url.as_deref().map(|url| {
        build_connect_link(
            url,
            addresses.quic_endpoint.as_deref(),
            name.as_deref(),
            fingerprint.as_deref(),
            &token,
        )
    });

    if json {
        return crate::output::json(&pair_document(
            &token,
            fingerprint.as_deref(),
            &addresses.overlay,
            &addresses.live,
            link.as_deref(),
            &tokens,
            &minted.id,
            minted.generation,
        ));
    }

    if let Some(link) = link.as_deref() {
        print_connect_link(link, qr);
    }

    outln!("Token written to {}", tokens.display());
    ExitCode::SUCCESS
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
    /// Render the link as a QR too.
    qr: bool,
    /// One JSON document on stdout instead of prose.
    json: bool,
}

/// Mint a credential that reaches this server through a relay route, and
/// print the relay link (ADR-0149). The door is the server's outbound
/// connector, not a listener, so the ADR-0141 gate becomes: the server
/// answers on `socket`, and the config names the `[[connector]]` relay the
/// link dials. The link pins the relay's certificate (the relay terminates
/// TLS, ADR-0051) and carries the route as `sni`; the token crosses the
/// relay opaquely and is verified by this server.
fn mint_relay_link(
    relay: &RelayRoute,
    socket: &Path,
    tokens: &Path,
    output: LinkOutput<'_>,
    replace_token: Option<&str>,
) -> ExitCode {
    let connector = match relay_connector(relay).and_then(|connector| {
        server_answers(socket)?;
        Ok(connector)
    }) {
        Ok(connector) => connector,
        Err(refusal) => {
            eprintln!("phux pair: {refusal}");
            return ExitCode::FAILURE;
        }
    };
    let Some(minted) = mint_pairing_credential(tokens, replace_token) else {
        return ExitCode::FAILURE;
    };
    let token = minted.secret().to_owned();
    let fingerprint = connector.cert_fingerprint.as_deref();
    let link = build_relay_connect_link(
        &connector.relay,
        &relay.route,
        output.name,
        fingerprint,
        &token,
    );
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
        );
        doc["relay"] = serde_json::json!({
            "endpoint": connector.relay,
            "route": relay.route,
        });
        return crate::output::json(&doc);
    }
    print_credential_block(&minted.id, &token);
    outln!(
        "Relay route (the device dials {} naming route \"{}\"):",
        connector.relay,
        relay.route
    );
    match fingerprint {
        Some(fingerprint) => {
            outln!(
                "  relay certificate SHA-256 {fingerprint} (the device pins the relay, which terminates TLS)"
            );
        }
        None => outln!("  no relay pin configured: a loopback relay only"),
    }
    outln!();
    print_connect_link(&link, output.qr);
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
}

/// Resolve every address this run advertises. Runs before the certificate
/// is provisioned, because SANs are chosen at generation time (ADR-0091).
fn resolve_pair_addresses(host: Option<&str>, live: LiveListeners) -> PairAddresses {
    let overlay = phux_config::overlay::detect();
    let server_url = resolve_server_url(host, &overlay, live.wss_addr());
    let quic_endpoint = resolve_quic_endpoint(&overlay, live.quic_addr());
    let advertised = advertised_names(server_url.as_deref(), &overlay);
    PairAddresses {
        overlay,
        live,
        server_url,
        quic_endpoint,
        advertised,
    }
}

/// Provision the self-signed cert at the default paths if it isn't there yet,
/// so the fingerprint printed later is the one the server will actually
/// present. An operator-supplied cert is used as-is, never generated over.
fn provision_pairing_certificate(certificate: &CertificatePaths, advertised: &[String]) {
    if certificate.operator_supplied {
        return;
    }
    if let Err(err) = phux_server::transport::tls::ensure_self_signed_for(
        &certificate.cert,
        &certificate.key,
        advertised,
    ) {
        eprintln!("phux pair: warning: could not provision certificate: {err}");
    }
}

/// Mint a credential, reporting failures on stderr (`None` means exit with
/// failure). `replace_token` revokes the matching old bearer in the same
/// rewrite.
fn mint_pairing_credential(
    tokens: &std::path::Path,
    replace_token: Option<&str>,
) -> Option<phux_server::auth::MintedCredential> {
    let minted = replace_token.map_or_else(
        || phux_server::auth::mint_token(tokens),
        |previous| phux_server::auth::mint_token_replacing(tokens, previous),
    );
    let minted = match minted {
        Ok(minted) => minted,
        Err(err) => {
            eprintln!("phux pair: failed to mint token: {err}");
            return None;
        }
    };
    if !minted.is_durable() {
        eprintln!(
            "phux pair: warning: credential is active, but the store directory could not be synced; do not retry pairing"
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
            eprintln!("phux pair: warning: could not read certificate fingerprint: {err}");
            None
        }
    }
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

/// Print the connect link, and its QR under `--qr`.
fn print_connect_link(link: &str, qr: bool) {
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
    if !qr {
        return;
    }
    match render_qr(link) {
        Ok(art) => {
            outln!("Scan to pair:");
            outln!();
            out!("{art}");
            outln!();
        }
        Err(err) => eprintln!("phux pair: warning: {err}"),
    }
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
            eprintln!("phux pair ls: {error}");
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
            eprintln!("phux pair prune: {error}");
            return ExitCode::FAILURE;
        }
    };
    let outcome = match phux_server::auth::prune_unused(tokens, duration) {
        Ok(outcome) => outcome,
        Err(error) => {
            eprintln!("phux pair prune: {error}");
            return ExitCode::FAILURE;
        }
    };
    if !outcome.is_durable() {
        eprintln!(
            "phux pair prune: warning: revocation is active, but the store directory could not be synced; do not retry"
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
            eprintln!("phux pair rotate: {error}");
            return ExitCode::FAILURE;
        }
    };
    if !rotated.is_durable() {
        eprintln!(
            "phux pair rotate: warning: rotation is active, but the store directory could not be synced; do not retry"
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
            eprintln!("phux pair revoke: {error}");
            return ExitCode::FAILURE;
        }
    };
    if !outcome.is_durable() {
        eprintln!(
            "phux pair revoke: warning: revocation is active, but the store directory could not be synced; do not retry"
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

/// Parse `--unused-for`: `30d`, `24h`, `90m`, `45s`, or a bare number of seconds.
fn parse_unused_for(raw: &str) -> Result<chrono::Duration, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("--unused-for needs a duration (e.g. 30d, 24h, 90m)".to_owned());
    }
    let split = raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len());
    let (digits, unit) = raw.split_at(split);
    if digits.is_empty() {
        return Err(format!("invalid --unused-for duration: {raw:?}"));
    }
    let value: i64 = digits
        .parse()
        .map_err(|_| format!("invalid --unused-for duration: {raw:?}"))?;
    if value <= 0 {
        return Err("--unused-for must be greater than zero".to_owned());
    }
    let seconds = match unit {
        "" | "s" => value,
        "m" => value.saturating_mul(60),
        "h" => value.saturating_mul(3600),
        "d" => value.saturating_mul(86400),
        _ => {
            return Err(format!(
                "invalid --unused-for unit in {raw:?}; use s, m, h, or d"
            ));
        }
    };
    Ok(chrono::Duration::seconds(seconds))
}

fn migrate_legacy_credentials(tokens: &std::path::Path) -> bool {
    match phux_server::auth::migrate_legacy_store(tokens) {
        Ok(outcome) => {
            eprintln!(
                "phux pair: migrated {} legacy credential(s) to the versioned store",
                outcome.migrated()
            );
            if !outcome.is_durable() {
                let durable = phux_server::auth::migrate_legacy_store(tokens)
                    .is_ok_and(phux_server::auth::MigrationOutcome::is_durable);
                if !durable {
                    eprintln!(
                        "phux pair: warning: migration is active, but the store directory could not be synced"
                    );
                }
            }
            true
        }
        Err(err) => {
            eprintln!("phux pair: failed to migrate legacy credentials: {err}");
            false
        }
    }
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
    })
}

#[cfg(test)]
mod tests {
    use super::{
        LiveListeners, advertised_names, build_connect_link, build_relay_connect_link,
        legacy_connect_link, link_refusal, live_listeners, pair_document, parse_connect_link,
        percent_encode, render_qr, resolve_quic_endpoint, resolve_server_url,
        select_relay_connector,
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
        assert_eq!(doc.as_object().map(serde_json::Map::len), Some(10));

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
        );
        assert!(doc["cert_fingerprint"].is_null());
        assert!(doc["ws_addr"].is_null());
        assert!(doc["quic_addr"].is_null());
        assert!(doc["connect_link"].is_null());
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
            resolve_server_url(Some("100.64.0.2:8787"), &[], None),
            Some("wss://100.64.0.2:8787".to_owned())
        );
        // A full URL passes through untouched (loopback dev path stays ws://).
        assert_eq!(
            resolve_server_url(Some("ws://127.0.0.1:8787"), &[], None),
            Some("ws://127.0.0.1:8787".to_owned())
        );
        assert_eq!(
            resolve_server_url(Some("wss://mini.tail-net.ts.net:8787"), &[], None),
            Some("wss://mini.tail-net.ts.net:8787".to_owned())
        );
        // The flag also beats the bound listener and a detected overlay address.
        let overlay = [IpAddr::V4(Ipv4Addr::new(100, 64, 0, 9))];
        assert_eq!(
            resolve_server_url(Some("mini:1"), &overlay, addr("100.64.0.9:2")),
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
            resolve_server_url(None, &overlay, addr("100.64.0.2:8787")),
            Some("wss://100.64.0.2:8787".to_owned())
        );
        // A concrete routable bind is dialed as bound, on its real port.
        assert_eq!(
            resolve_server_url(None, &overlay, addr("192.168.1.5:9000")),
            Some("wss://192.168.1.5:9000".to_owned())
        );
        // An unspecified bind is dialed on the overlay address, bound port...
        assert_eq!(
            resolve_server_url(None, &overlay, addr("0.0.0.0:9001")),
            Some("wss://100.64.0.2:9001".to_owned())
        );
        // ...and yields nothing with no overlay address to dial it on.
        assert_eq!(resolve_server_url(None, &[], addr("0.0.0.0:9001")), None);
        // A loopback bind is unreachable from a device.
        assert_eq!(
            resolve_server_url(None, &overlay, addr("127.0.0.1:8787")),
            None
        );
        // No wss listener bound: no link, whatever the overlay says.
        assert_eq!(resolve_server_url(None, &overlay, None), None);
        // A v6 address is bracketed so the URL stays parseable.
        let v6 = [IpAddr::V6(Ipv6Addr::new(
            0xfd7a, 0x115c, 0xa1e0, 0, 0, 0, 0, 1,
        ))];
        assert_eq!(
            resolve_server_url(None, &v6, addr("[::]:8787")),
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
            resolve_quic_endpoint(&v4, addr("100.64.0.2:8788")),
            Some("quic://100.64.0.2:8788".to_owned())
        );
        assert_eq!(
            resolve_quic_endpoint(&v4, addr("192.168.1.5:9000")),
            Some("quic://192.168.1.5:9000".to_owned())
        );
        assert_eq!(
            resolve_quic_endpoint(&v4, addr("0.0.0.0:9001")),
            Some("quic://100.64.0.2:9001".to_owned())
        );
        assert_eq!(resolve_quic_endpoint(&[], addr("0.0.0.0:9001")), None);
        let v6_only = [IpAddr::V6(Ipv6Addr::LOCALHOST)];
        assert_eq!(
            resolve_quic_endpoint(&v6_only, addr("0.0.0.0:9001")),
            None,
            "an IPv4 wildcard does not make an IPv6 address dialable"
        );
        assert_eq!(resolve_quic_endpoint(&v4, addr("127.0.0.1:8788")), None);
        assert_eq!(resolve_quic_endpoint(&v4, None), None);

        let v6 = [IpAddr::V6(Ipv6Addr::new(
            0xfd7a, 0x115c, 0xa1e0, 0, 0, 0, 0, 1,
        ))];
        assert_eq!(
            resolve_quic_endpoint(&v6, addr("[::]:8788")),
            Some("quic://[fd7a:115c:a1e0::1]:8788".to_owned())
        );
        let mixed = [v4[0], v6[0]];
        assert_eq!(
            resolve_quic_endpoint(&mixed, addr("[::]:8788")),
            Some("quic://[fd7a:115c:a1e0::1]:8788".to_owned()),
            "a wildcard bind uses an overlay address of the same family"
        );
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
            advertised_names(Some("wss://100.64.0.2:8787"), &overlay),
            vec!["100.64.0.2"]
        );

        // A `--host` name is carried alongside every detected overlay address:
        // `phux pair` prints them all as dialable, so all of them belong in
        // the certificate.
        assert_eq!(
            advertised_names(Some("wss://mini.tail-net.ts.net:8787"), &overlay),
            vec!["mini.tail-net.ts.net", "100.64.0.2"]
        );

        // v6 arrives bracketed in a URL and must be unbracketed in a SAN.
        let v6 = [IpAddr::V6(Ipv6Addr::LOCALHOST)];
        assert_eq!(
            advertised_names(Some("wss://[fd7a:115c:a1e0::1]:8787"), &v6),
            vec!["fd7a:115c:a1e0::1", "::1"]
        );

        // No link at all: still name whatever was detected, since the operator
        // may dial it by hand.
        assert_eq!(advertised_names(None, &overlay), vec!["100.64.0.2"]);
        assert!(advertised_names(None, &[]).is_empty());

        // A URL that will not parse contributes nothing rather than poisoning
        // the list — pairing still has a token to mint.
        assert_eq!(
            advertised_names(Some("not a url"), &overlay),
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
        let art = render_qr(&link).expect("QR should encode");
        assert!(!art.is_empty(), "QR render must be non-empty");
        // Dense1x2 uses half-block glyphs; at least one must appear.
        assert!(
            art.chars().any(|c| matches!(c, '█' | '▀' | '▄' | ' ')),
            "QR render must contain half-block glyphs",
        );
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
            Some("AB:CD"),
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
