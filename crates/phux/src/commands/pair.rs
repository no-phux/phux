//! `phux pair` — mint, rotate, or revoke remote credentials (ADR-0031).
//!
//! The token authenticates a device that attaches over `wss://` or QUIC; the
//! server reads the same token store at `PHUX_WS_TOKENS`. Minting first asks
//! the running server which remote listeners it has bound (`GET_STATE`'s
//! listener report) and refuses when none would accept the credential, so a
//! token, link, or QR is only ever printed for a door that is open, and the
//! link names the address that door is actually bound to (ADR-0141). When
//! nothing is bound because `[policy] mode` is unset, a terminal offers to
//! write `paired` and restart the server (`--enable` does that without
//! asking; `--json` does not ask). `ls`, `prune`, `rotate`, and `revoke` only edit the store and
//! need no server.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use phux_client_runtime::pairing::{CONNECT_URI_PREFIX, LEGACY_CONNECT_URI_PREFIX};
pub(crate) use phux_client_runtime::pairing::{ConnectLink, parse_connect_link};

mod card;
mod connector;
#[cfg(test)]
mod endpoint_tests;
mod link_name;
mod mint;
pub(crate) use mint::run;
#[cfg(test)]
mod link_tests;
use card::{HumanSecret, finish_human_mint, listener_pins, relay_pins};
use connector::relay_connector;
use link_name::{link_name, percent_encode};
mod manage;
mod paired_mode;
mod rebind;
use manage::{parse_expires, run_credential_action};
use paired_mode::{bound_or_refusal, bound_remote_listener};
use phux_config::overlay::MagicDns;
use rebind::wait_for_rebind;

use usage::Subcommands;

const DEFAULT_ROTATION_OVERLAP_SECONDS: i64 = 300;

#[derive(Debug, Subcommands)]
pub(crate) enum PairAction {
    /// List credentials in the store (id, minted, last seen, status:
    /// unused, active, expired, or revoked, with when it stops working).
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

/// Append the fields every link ends with, in the documented order. The
/// secrets (`enroll`, `token`) go after `#`: a browser never sends the
/// fragment, so a link opened on the web page leaves them on the device.
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
    link.push('#');
    if let Some(ticket) = pins.enrollment {
        link.push_str("enroll=");
        link.push_str(ticket);
        link.push('&');
    }
    link.push_str("token=");
    link.push_str(token);
}

/// Respell an https connect link with [`LEGACY_CONNECT_URI_PREFIX`], carrying
/// the query and fragment byte-for-byte. `None` for anything else.
fn legacy_connect_link(link: &str) -> Option<String> {
    link.strip_prefix(CONNECT_URI_PREFIX)
        .map(|query| format!("{LEGACY_CONNECT_URI_PREFIX}{query}"))
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

mod listener_endpoint;
pub(crate) use listener_endpoint::{advertised_quic_endpoint, bound_quic_listener};

/// The words of the refusal when the server has no remote listener bound.
/// `phux host add` recognizes them to restart a server that disabled its
/// listeners at boot, so they are a contract with that caller.
pub(crate) const NO_BOUND_LISTENER: &str = "has no remote listener bound";

mod live;
#[cfg(test)]
use live::live_listeners;
use live::{
    ListenerQueryError, LiveListeners, no_live_server, query_listeners_with, query_live_listeners,
};

/// How long to wait for the restarted server to answer, and then for an
/// overlay listener to show up, after `phux upgrade` re-binds it.
const REBIND_WAIT: Duration = Duration::from_secs(15);
/// Once the restarted server is answering and still unbound, how long
/// overlay detection gets before the refusal names `--listen` / `--quic`.
const LISTENER_SETTLE: Duration = Duration::from_secs(3);
/// Cadence of the post-restart listener poll.
const REBIND_POLL: Duration = Duration::from_millis(100);
/// One `GET_STATE` during that poll. A wedged accept must not hold the wait.
const REBIND_QUERY_TIMEOUT: Duration = Duration::from_millis(500);

/// Re-exec the server so the config just written takes effect, then wait
/// until it answers again. Panes stay attached (`phux upgrade`).
pub(crate) fn rebind_server(socket: &Path) -> Result<(), String> {
    let outcome = super::upgrade::LiveUpgrade::new(socket, false).and_then(|mut env| {
        // Same consent as `phux upgrade` with no `--close-agent-sessions`:
        // a current server re-binds without a second question. Only a
        // server old enough to refuse while agent sessions exist asks again.
        super::upgrade::drive(
            &mut env,
            super::upgrade::Consent::detect(false),
            Some(socket),
        )
    });
    match outcome {
        Ok(super::upgrade::UpgradeAck::Upgrading) => wait_for_rebind(socket),
        Ok(
            super::upgrade::UpgradeAck::Refused(message)
            | super::upgrade::UpgradeAck::Unexpected(message),
        ) => Err(format!(
            "could not restart the server to bind a listener: {message}"
        )),
        Err(err) => Err(format!(
            "could not restart the server at {} to bind a listener: {err}",
            socket.display()
        )),
    }
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
         its fingerprint is unaffected, but a client that validates the server \
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

/// Print or file a listener mint. `--json` is the only path that writes the
/// bearer to stdout; camera and secret-file mints keep it out of the text.
fn present_listener_credential(
    minted: &phux_server::auth::MintedCredential,
    ticket: Option<&phux_server::workload::tickets::MintedTicket>,
    tokens: &Path,
    certificate: &CertificatePaths,
    addresses: &PairAddresses,
    output: LinkOutput<'_>,
) -> ExitCode {
    let token = minted.secret();
    let fingerprint = read_pairing_fingerprint(&certificate.cert);
    let authority = read_pairing_authority(&certificate.cert);
    let checked = addresses.checked_names();
    warn_on_uncovered_names(&certificate.cert, &certificate.key, &checked);

    // The one-tap link (and its QR form) carries the token: it is the
    // secret, shown once on this terminal.
    let host_name = link_name(output.name, addresses.magic_dns_name.as_deref());
    let pins = ServerPins {
        leaf: fingerprint.as_deref(),
        authority: authority.as_deref(),
        enrollment: ticket.as_ref().map(|ticket| ticket.secret_hex.as_str()),
    };
    let link = addresses.server_url.as_deref().map(|url| {
        build_connect_link_pinned(
            url,
            addresses.quic_endpoint.as_deref(),
            Some(&host_name),
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
        add_pins_to_document(&mut document, authority.as_deref(), ticket, output);
        return crate::output::json(&document);
    }

    let mut secret = HumanSecret::new(&minted.id, token, output);
    secret.fingerprint = fingerprint.as_deref();
    secret.link = link.as_deref();
    secret.host_name = &host_name;
    secret.ticket = ticket;
    secret.pins = listener_pins(
        fingerprint.as_deref(),
        authority.as_deref(),
        &addresses.overlay,
    );
    finish_human_mint(&secret, tokens)
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

/// A server with a remote listener is in `paired` mode and admits only
/// enrolled devices, so every link a human mint prints, QR or not, lets a
/// phone enroll (#2397). `--json` enrolls only on `--enroll`; `--no-enroll`
/// opts out of all of it.
const fn phone_enrollment_requested(enroll: bool, output: LinkOutput<'_>, has_link: bool) -> bool {
    !output.no_enroll && (enroll || output.qr || (!output.json && has_link))
}

/// A single-use link's window, the enrollment ticket's (#2398).
const LINK_SECONDS: i64 = phux_server::workload::tickets::DEFAULT_TICKET_SECONDS;

/// `--reusable` and `--no-enroll`: what a mint leaves out of its link.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Lifetime {
    /// Mint the bearer valid until revoked (or `--expires`).
    pub(crate) reusable: bool,
    /// Leave the phone enrollment ticket out.
    pub(crate) no_enroll: bool,
}

impl Lifetime {
    /// A printed link, or any mint meant for a phone, is single use (#2398);
    /// `--expires`, `--reusable`, and a plain `--json` mint (`phux host add`)
    /// keep a bearer any holder may use.
    fn first_use_by(self, expires: bool, json: bool, door: &Door) -> Option<jiff::Timestamp> {
        let enroll = matches!(
            door,
            Door::Listener { enroll: true } | Door::Relay { enroll: true, .. }
        );
        (!self.reusable && !expires && (!json || enroll))
            .then(|| jiff::Timestamp::now() + jiff::SignedDuration::from_secs(LINK_SECONDS))
    }
}

/// Why phone enrollment refuses a server certificate no workload CA issued.
const ENROLL_NEEDS_AUTHORITY: &str = "phone enrollment needs a server certificate issued by its \
     workload CA, but this certificate predates the CA or was supplied separately; nothing was \
     minted. `phux workload authority --rotate` then `phux upgrade` repairs an auto-provisioned \
     legacy certificate, but rotates the CA: every paired device must re-pair and every workload \
     credential must re-enroll. Operator-supplied certificates must be replaced by their owner. \
     `--no-enroll` mints a link for a device that is already enrolled";

const ENROLL_NEEDS_QUIC: &str = "phone pairing needs a device-dialable QUIC listener to enroll \
     its certificate; none was minted. Bind one with `phux server --quic ADDR` (or use the \
     overlay auto-listener), then retry. A loopback or wildcard bind without an overlay \
     address cannot be advertised to a phone. `--no-enroll` mints a link for a device that is \
     already enrolled";

/// The fields a mint document gained after its first schema: the CA pin
/// (ADR-0153), an enrollment ticket (ADR-0154), and the single-use link
/// deadline (#2398). Additive: `phux host add` reads them, and older readers
/// ignore them.
fn add_pins_to_document(
    document: &mut serde_json::Value,
    authority: Option<&str>,
    ticket: Option<&phux_server::workload::tickets::MintedTicket>,
    output: LinkOutput<'_>,
) {
    document["ca_fingerprint"] = serde_json::json!(authority);
    document["single_use"] = serde_json::json!(output.first_use_by.is_some());
    document["link_expires_at"] = serde_json::json!(output.first_use_by.map(format_rfc3339));
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
#[allow(
    clippy::struct_excessive_bools,
    reason = "one independent CLI flag per bool"
)]
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
    expires_at: Option<jiff::Timestamp>,
    /// The duration the operator typed (`15m`), for the expiry line.
    expires_label: Option<&'a str>,
    /// Single-use link: unused by then, the bearer stops working.
    first_use_by: Option<jiff::Timestamp>,
    /// `--no-enroll`.
    no_enroll: bool,
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
    let Some((minted, ticket)) = mint_pairing_bundle(tokens, replace_token, output, enroll) else {
        return ExitCode::FAILURE;
    };
    let token = minted.secret().to_owned();
    let fingerprint = connector.cert_fingerprint.as_deref();
    let host_name = link_name(output.name, None);
    let pins = ServerPins {
        leaf: fingerprint,
        authority: Some(authority.as_str()),
        enrollment: ticket.as_ref().map(|ticket| ticket.secret_hex.as_str()),
    };
    let link = build_relay_connect_link(
        &connector.relay,
        &relay.route,
        Some(&host_name),
        pins,
        &token,
    );
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
        add_pins_to_document(&mut doc, Some(authority.as_str()), ticket.as_ref(), output);
        return crate::output::json(&doc);
    }
    let mut secret = HumanSecret::new(&minted.id, &token, output);
    secret.fingerprint = fingerprint;
    secret.link = Some(&link);
    secret.host_name = &host_name;
    secret.ticket = ticket.as_ref();
    secret.pins = relay_pins(&connector.relay, &relay.route, fingerprint, &authority);
    finish_human_mint(&secret, tokens)
}

/// Ask the server on `socket` for its state, only to learn that it answers:
/// a relay link minted for a server that is not running reaches nothing.
fn server_answers(socket: &Path) -> Result<(), String> {
    let runtime = crate::commands::build_cli_runtime()
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
    let (env_cert, env_key) =
        phux_server::transport::tls::operator_tls_paths(|var| std::env::var_os(var));
    let operator_supplied = cert.is_some() || env_cert.is_some();
    let cert = cert
        .or(env_cert)
        .unwrap_or_else(phux_server::transport::tls::default_cert_path);
    let key = env_key.unwrap_or_else(phux_server::transport::tls::default_key_path);
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
    const fn enrollment_endpoint_ready(&self, camera: bool) -> bool {
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

/// Mint the ticket before touching the credential store. A failed ticket write
/// must not leave a live bearer or revoke the predecessor of `replace_token`.
/// If credential mint fails, withdraw the unexposed ticket best-effort.
fn mint_pairing_bundle(
    tokens: &Path,
    replace_token: Option<&str>,
    output: LinkOutput<'_>,
    enroll: bool,
) -> Option<(
    phux_server::auth::MintedCredential,
    Option<phux_server::workload::tickets::MintedTicket>,
)> {
    use phux_server::workload::{WorkloadPaths, tickets};
    let ticket = if enroll {
        Some(mint_enrollment_ticket()?)
    } else {
        None
    };
    let Some(credential) = mint_pairing_credential(tokens, replace_token, output) else {
        if let Some(ticket) = ticket.as_ref() {
            let path = tickets::tickets_path(&WorkloadPaths::from_env().registry);
            if let Err(err) = tickets::revoke_ticket(&path, &ticket.id) {
                warn!("pair: could not withdraw unused enrollment ticket: {err}");
            }
        }
        return None;
    };
    Some((credential, ticket))
}

/// Mint a credential, reporting failures on stderr (`None` means exit with
/// failure). `replace_token` revokes the matching old bearer in the same
/// rewrite.
fn mint_pairing_credential(
    tokens: &std::path::Path,
    replace_token: Option<&str>,
    output: LinkOutput<'_>,
) -> Option<phux_server::auth::MintedCredential> {
    let (expires_at, first_use_by) = (output.expires_at, output.first_use_by);
    let minted = if replace_token.is_none() && expires_at.is_none() && first_use_by.is_none() {
        phux_server::auth::mint_token(tokens)
    } else {
        phux_server::auth::mint_terminal_credential(tokens, replace_token, expires_at, first_use_by)
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

/// The certificate fingerprint the device pins; an unreadable certificate
/// is reported on stderr and pins nothing.
fn read_pairing_fingerprint(cert: &std::path::Path) -> Option<String> {
    phux_server::transport::tls::cert_fingerprint(cert)
        .map_err(|err| warn!("pair: could not read certificate fingerprint: {err}"))
        .ok()
}

/// The workload CA the server's certificate chains to (ADR-0153); `None` for
/// a self-signed or operator-supplied leaf.
fn read_pairing_authority(cert: &std::path::Path) -> Option<String> {
    phux_server::transport::tls::presented_authority(cert)
        .ok()
        .flatten()
}

/// Mint the single-use enrollment ticket a `--enroll` link carries
/// (ADR-0154), with the ceiling `phux host add` enrolls with. `None` after
/// reporting a failure.
fn mint_enrollment_ticket() -> Option<phux_server::workload::tickets::MintedTicket> {
    use phux_server::workload::tickets;
    let paths = phux_server::workload::WorkloadPaths::from_env();
    match tickets::mint_ticket(
        &tickets::tickets_path(&paths.registry),
        vec![super::enroll::HOST_ADD_WORKLOAD_SCOPE.to_owned()],
        tickets::DEFAULT_CREDENTIAL_SECONDS,
        tickets::DEFAULT_TICKET_SECONDS,
    ) {
        Ok(ticket) => Some(ticket),
        Err(err) => {
            error!("pair: could not mint an enrollment ticket: {err}");
            None
        }
    }
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

fn format_rfc3339(stamp: jiff::Timestamp) -> String {
    format!("{stamp:.0}")
}

/// Print the QR of `link` scaled to the terminal; `false` after warning that
/// it could not be rendered.
fn print_camera_qr(link: &str) -> bool {
    let columns = stdout_columns();
    let scale = columns.map_or(1, |columns| qr_scale_that_fits(link, columns));
    match render_qr_scaled(link, scale) {
        Ok(art) => {
            let width = art_width(&art);
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
        if art_width(&art) <= usize::from(columns) {
            return scale;
        }
    }
    1
}

fn art_width(art: &str) -> usize {
    art.lines()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0)
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
    use super::connector::select_relay_connector;
    use super::paired_mode::{mode_unset_refusal, other_unbound_refusal};
    use super::{
        LinkOutput, ListenerQueryError, LiveListeners, MagicDns, NO_BOUND_LISTENER, PairAddresses,
        ServerPins, advertised_names, build_connect_link, build_connect_link_pinned,
        build_relay_connect_link, legacy_connect_link, link_refusal, live_listeners, pair_document,
        parse_connect_link, percent_encode, phone_enrollment_requested, resolve_quic_endpoint,
        resolve_server_url, shows_default_qr,
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
        let overlay = [V4];
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
            "https://phux.sh/connect?url=wss://h:1#token=deadbeef"
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
            "https://phux.sh/connect?url=wss://10.0.0.2:8787&quic=quic://10.0.0.2:8788&name=mini&fp=AB:CD#token=deadbeef"
        );
        // No fingerprint — the fp param is absent, not empty.
        assert_eq!(
            build_connect_link("wss://h:1", None, Some("mini"), None, "deadbeef"),
            "https://phux.sh/connect?url=wss://h:1&name=mini#token=deadbeef"
        );
    }

    #[test]
    fn name_is_percent_encoded() {
        assert_eq!(percent_encode("studio mini"), "studio%20mini");
        assert_eq!(percent_encode("plain-name_1.ok~"), "plain-name_1.ok~");
        assert_eq!(percent_encode("a&b=c"), "a%26b%3Dc");
        assert_eq!(
            build_connect_link("wss://h:1", None, Some("studio mini"), None, "aa"),
            "https://phux.sh/connect?url=wss://h:1&name=studio%20mini#token=aa"
        );
    }

    const V4: IpAddr = IpAddr::V4(Ipv4Addr::new(100, 64, 0, 2));
    const V6: IpAddr = IpAddr::V6(Ipv6Addr::new(0xfd7a, 0x115c, 0xa1e0, 0, 0, 0, 0, 1));

    #[test]
    fn host_flag_wins_and_gets_wss_scheme() {
        for (host, expected) in [
            // Bare host:port gets the wss:// the remote path always uses.
            ("100.64.0.2:8787", "wss://100.64.0.2:8787"),
            // A full URL passes through untouched (loopback dev path stays ws://).
            ("ws://127.0.0.1:8787", "ws://127.0.0.1:8787"),
            (
                "wss://mini.tail-net.ts.net:8787",
                "wss://mini.tail-net.ts.net:8787",
            ),
        ] {
            assert_eq!(
                resolve_server_url(Some(host), &[], None, None).as_deref(),
                Some(expected)
            );
        }
        // The flag also beats the bound listener and a detected overlay address.
        assert_eq!(
            resolve_server_url(Some("mini:1"), &[V4], None, addr("100.64.0.2:2")).as_deref(),
            Some("wss://mini:1")
        );
    }

    /// Without `--host`, the wss URL and the QUIC endpoint name the live bind
    /// as a device dials it: concrete addresses keep their port, a wildcard
    /// bind uses an overlay address of the same family, loopback is never
    /// advertised, and IPv6 is bracketed.
    #[test]
    fn derived_endpoints_are_the_live_device_dialable_binds() {
        let v4: &[IpAddr] = &[V4];
        let v6: &[IpAddr] = &[V6];
        let mixed: &[IpAddr] = &[V4, V6];
        let v6_loopback: &[IpAddr] = &[IpAddr::V6(Ipv6Addr::LOCALHOST)];
        let none: &[IpAddr] = &[];
        for (overlay, bound, expected) in [
            (v4, Some("100.64.0.2:8787"), Some("100.64.0.2:8787")),
            (v4, Some("192.168.1.5:9000"), Some("192.168.1.5:9000")),
            (v4, Some("0.0.0.0:9001"), Some("100.64.0.2:9001")),
            (none, Some("0.0.0.0:9001"), None),
            (v6_loopback, Some("0.0.0.0:9001"), None),
            (v4, Some("127.0.0.1:8787"), None),
            (v4, None, None),
            (v6, Some("[::]:8787"), Some("[fd7a:115c:a1e0::1]:8787")),
            (mixed, Some("[::]:8788"), Some("[fd7a:115c:a1e0::1]:8788")),
        ] {
            let bound = bound.and_then(addr);
            let wss = resolve_server_url(None, overlay, None, bound);
            assert_eq!(wss, expected.map(|e| format!("wss://{e}")), "{bound:?}");
            let quic = resolve_quic_endpoint(overlay, None, bound);
            assert_eq!(quic, expected.map(|e| format!("quic://{e}")), "{bound:?}");
        }
    }

    fn magic_dns() -> MagicDns {
        MagicDns {
            name: "studio.tail9a8b.ts.net".to_owned(),
            addresses: vec![V4, V6],
        }
    }

    /// ADR-0164: an endpoint on an address the host's `MagicDNS` name stands
    /// for is named by that name, on both the wss and quic legs, so the link
    /// survives the tailnet reassigning the address. Any other address, and
    /// `--host`, keep the address behaviour.
    #[test]
    fn magic_dns_names_the_tailnet_endpoints() {
        let magic = magic_dns();
        let wss = |host, overlay: &[IpAddr], bound| {
            resolve_server_url(host, overlay, Some(&magic), addr(bound))
        };
        let quic =
            |overlay: &[IpAddr], bound| resolve_quic_endpoint(overlay, Some(&magic), addr(bound));
        let name = "studio.tail9a8b.ts.net";
        assert_eq!(
            wss(None, &[V4], "100.64.0.2:8787"),
            Some(format!("wss://{name}:8787"))
        );
        assert_eq!(
            wss(None, &[V4], "0.0.0.0:9001"),
            Some(format!("wss://{name}:9001"))
        );
        assert_eq!(
            quic(&[V4], "100.64.0.2:8788"),
            Some(format!("quic://{name}:8788"))
        );
        // The name stands for the v6 tailnet address too, unbracketed.
        assert_eq!(
            quic(&[V6], "[::]:8788"),
            Some(format!("quic://{name}:8788"))
        );
        // A bind the name does not resolve to keeps its address.
        assert_eq!(
            wss(None, &[V4], "192.168.1.5:9000").as_deref(),
            Some("wss://192.168.1.5:9000")
        );
        assert_eq!(
            quic(&[], "100.64.0.9:8788").as_deref(),
            Some("quic://100.64.0.9:8788")
        );
        // Loopback and unresolvable wildcards are still not advertised.
        assert_eq!(quic(&[V4], "127.0.0.1:8788"), None);
        assert_eq!(quic(&[], "0.0.0.0:8788"), None);
        // `--host` is the operator's claim and wins over the name.
        let host = wss(Some("100.64.0.2:8787"), &[V4], "100.64.0.2:8787");
        assert_eq!(host.as_deref(), Some("wss://100.64.0.2:8787"));
    }

    /// A `MagicDNS` link is the same shape, and both legs parse: the CLI's own
    /// parser accepts `quic://NAME:PORT`, and the certificate SANs carry the
    /// name whichever leg names it.
    #[test]
    fn a_magic_dns_link_round_trips_and_names_its_sans() {
        let magic = magic_dns();
        let v4 = [V4];
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
             &quic=quic://studio.tail9a8b.ts.net:8788&name=studio&fp=AB:CD#token=deadbeef"
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
            overlay: vec![V4],
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

    /// Every printed link lets a phone enroll (#2397); `--json` only on
    /// `--enroll`, and `--no-enroll` opts out of all of it.
    #[test]
    fn every_printed_link_enrolls_but_machine_mints_do_not() {
        for (enroll, qr, json, no_enroll, has_link, expected) in [
            (false, false, false, false, true, true),
            (false, true, false, false, false, true),
            (true, false, false, false, false, true),
            (false, false, false, false, false, false),
            (false, false, true, false, true, false),
            (true, false, true, false, true, true),
            (false, true, false, true, true, false),
            (false, false, false, true, true, false),
        ] {
            let output = LinkOutput {
                name: None,
                qr,
                default_qr: false,
                json,
                secret_file: None,
                expires_at: None,
                expires_label: None,
                first_use_by: None,
                no_enroll,
            };
            assert_eq!(
                phone_enrollment_requested(enroll, output, has_link),
                expected,
                "enroll={enroll} qr={qr} json={json} no_enroll={no_enroll} link={has_link}"
            );
        }
    }

    /// No listener report, or one with nothing bound, is an unbound result;
    /// any bound row admits a mint. The unset-mode refusal is one line that
    /// names `--enable` and the setting; another cause names `--listen`.
    #[test]
    fn a_mint_needs_a_bound_remote_listener() {
        let socket = Path::new("/run/phux.sock");

        let absent = live_listeners(None).expect_err("no report, nothing bound");
        assert!(
            matches!(absent, ListenerQueryError::Unbound { ref disabled } if disabled.is_empty()),
            "{absent:?}"
        );

        let unset = mode_unset_refusal(socket);
        assert!(!unset.contains('\n'), "{unset}");
        assert!(
            unset.contains("/run/phux.sock") && unset.contains(NO_BOUND_LISTENER),
            "{unset}"
        );
        assert!(unset.contains("none was minted"), "{unset}");
        assert!(
            unset.contains("phux pair --enable") && unset.contains("[policy] mode = \"paired\""),
            "{unset}"
        );
        assert!(unset.contains("phux doctor"), "{unset}");
        assert!(
            !unset.contains("PHUX_WS_ADDR") && !unset.contains("overlay network"),
            "{unset}"
        );

        let empty = RemoteListenersReport::new();
        assert!(live_listeners(Some(&empty)).is_err());

        let wss_down = RemoteListenerSlot::disabled(
            RemoteListenerTransport::Wss,
            Some("100.64.0.2:8787".to_owned()),
            ListenerDisabledReason::BindFailed,
        );
        let disabled = RemoteListenersReport::new().with_listeners(vec![wss_down.clone()]);
        let ListenerQueryError::Unbound { disabled: rows } =
            live_listeners(Some(&disabled)).expect_err("disabled is not bound")
        else {
            panic!("expected an unbound report");
        };
        let refusal = other_unbound_refusal(socket, &rows);
        assert!(!refusal.contains('\n'), "{refusal}");
        assert!(
            refusal.contains("wss at 100.64.0.2:8787 disabled (bind_failed)"),
            "{refusal}"
        );
        assert!(
            refusal.contains("service install --adopt --quic"),
            "{refusal}"
        );
        assert!(!refusal.contains("phux pair --enable"), "{refusal}");

        let quic_only = RemoteListenersReport::new().with_listeners(vec![
            wss_down,
            RemoteListenerSlot::bound(RemoteListenerTransport::Quic, "0.0.0.0:8788"),
        ]);
        assert_eq!(
            live_listeners(Some(&quic_only)).expect("QUIC accepts credentials"),
            live(None, Some("0.0.0.0:8788"))
        );

        let both = RemoteListenersReport::new().with_listeners(vec![
            RemoteListenerSlot::bound(RemoteListenerTransport::Wss, "100.64.0.2:8787"),
            RemoteListenerSlot::bound(RemoteListenerTransport::Quic, "100.64.0.2:8788"),
        ]);
        assert_eq!(
            live_listeners(Some(&both)).expect("bound"),
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
        let overlay = [V4];

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
    fn camera_and_secret_file_output_omit_the_bearer() {
        use std::os::unix::fs::PermissionsExt;
        let token = "ab".repeat(32);
        let link = build_connect_link("wss://100.64.0.2:8787", None, None, None, &token);
        let secret = super::HumanSecret {
            credential_id: "credential-a",
            token: &token,
            fingerprint: Some("AB:CD"),
            link: Some(&link),
            host_name: "mini",
            ticket: None,
            pins: Vec::new(),
            expires_at: None,
            expires_label: None,
            first_use_by: None,
            qr: true,
            default_qr: false,
            secret_file: Some(Path::new("/tmp/pair-secret")),
            surface: super::SecretSurface::QrAndFile,
            hide_bearer: true,
        };
        let body = super::card::secret_file_body(&secret);
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
        super::card::write_secret_file(&path, &secret).expect("write");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(
            super::prepare_secret_file(&path).is_err(),
            "refuse to overwrite"
        );
        assert!(super::manage::parse_expires("15m").is_ok());
        assert!(super::manage::parse_expires("31d").is_err());
        assert!(super::manage::parse_expires("0s").is_err());
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
                "https://phux.sh/connect?url=wss://100.64.0.2:8787&fp=AB:CD&ca={authority}#token=deadbeef"
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
            "phux://connect?url=wss://mini:8787&name=studio%20mini&fp=AB:CD#token=tok"
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
             &name=mini%20box&fp=AB:CD#token=deadbeef"
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

    /// `write_paired_mode` creates a missing file and, on an existing one,
    /// keeps every other key and comment.
    #[test]
    fn write_paired_mode_preserves_existing_keys_and_comments() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("config.toml");
        let original = "\
# header stays
[sidebar]
width = 30 # keep

[policy]
# chosen later
";
        std::fs::write(&config, original).expect("seed config");
        crate::commands::service::write_paired_mode(&config).expect("set paired");
        let written = std::fs::read_to_string(&config).expect("read config");
        assert!(written.contains("# header stays"), "{written}");
        assert!(written.contains("width = 30 # keep"), "{written}");
        assert!(written.contains("# chosen later"), "{written}");
        assert!(written.contains("mode = \"paired\""), "{written}");
        let loaded = phux_config::loader::load_from(&config).expect("loads");
        assert_eq!(loaded.policy.mode, Some(phux_config::PolicyMode::Paired));
        assert_eq!(loaded.sidebar.width, 30);

        let missing = dir.path().join("nested").join("config.toml");
        crate::commands::service::write_paired_mode(&missing).expect("create");
        let created = std::fs::read_to_string(&missing).expect("created");
        assert!(created.contains("[policy]"), "{created}");
        assert!(created.contains("mode = \"paired\""), "{created}");
        let loaded = phux_config::loader::load_from(&missing).expect("created loads");
        assert_eq!(loaded.policy.mode, Some(phux_config::PolicyMode::Paired));
    }
}
