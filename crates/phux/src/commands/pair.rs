//! `phux pair` — mint a bearer pairing token for a remote consumer (ADR-0031).
//!
//! The token authenticates a device that attaches over `wss://`; the server
//! reads the same token store at `PHUX_WS_TOKENS`. This verb only writes the
//! token file — it never contacts a running server — so it works before the
//! server starts and needs no socket.

use std::net::IpAddr;
use std::path::PathBuf;
use std::process::ExitCode;

/// Scheme + host for the one-tap connect deep-link (and the QR that encodes
/// it). A device that opens or scans it gets the server URL, the cert
/// fingerprint (MITM defense), and the token (credential) in one shot —
/// no typing a 32-byte hex token by hand. The shape is owned by the mobile
/// consumer's parser (`ServerConfig.fromConnectLink` in phux-mobile):
/// `phux://connect?url=<ws(s)-url>[&name=<n>][&fp=<sha256>]&token=<hex>`,
/// where `url` is mandatory — without it the device has nothing to dial and
/// rejects the link — so a link is only emitted when an address is known.
const CONNECT_URI_PREFIX: &str = "phux://connect";

/// Build the `phux://connect?...` one-tap link. `url` is a ws(s):// URL,
/// `token` lowercase hex, and `fingerprint` colon-separated hex — all
/// query-safe as-is (RFC 3986 `pchar` allows `:` and `/` in query strings,
/// and the mobile parser reads them unencoded). `name` is free-form operator
/// input, so it alone is percent-encoded.
fn build_connect_link(
    url: &str,
    name: Option<&str>,
    fingerprint: Option<&str>,
    token: &str,
) -> String {
    let mut link = format!("{CONNECT_URI_PREFIX}?url={url}");
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
    link
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

/// Resolve the ws(s):// URL the connect link embeds. `--host` wins: a full
/// `ws://`/`wss://` URL passes through, a bare `host:port` gets the `wss://`
/// the remote path always uses (ADR-0031: a routable bind is always TLS).
/// Without `--host`, fall back to the first detected overlay address
/// (ADR-0037) plus the port of `ws_addr` (the caller passes `PHUX_WS_ADDR`,
/// the env the server's listener reads) when both are known. `None` when no
/// address source exists; the caller then prints no link (the device enters
/// the address itself).
fn resolve_server_url(
    host: Option<&str>,
    overlay: &[IpAddr],
    ws_addr: Option<&str>,
) -> Option<String> {
    if let Some(host) = host {
        if host.starts_with("ws://") || host.starts_with("wss://") {
            return Some(host.to_owned());
        }
        return Some(format!("wss://{host}"));
    }
    let ip = overlay.first()?;
    // The port of a HOST:PORT value; never guess one.
    let port: u16 = ws_addr?.rsplit_once(':')?.1.parse().ok()?;
    Some(match ip {
        IpAddr::V4(v4) => format!("wss://{v4}:{port}"),
        IpAddr::V6(v6) => format!("wss://[{v6}]:{port}"),
    })
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

/// Mint a token into the store and print it with the certificate fingerprint.
///
/// Defaults match the server's seamless path (ADR-0031): the token store and
/// the auto-generated certificate live at shared paths under the state dir, so
/// `phux pair` with no flags pairs against the same material the server will
/// read. The certificate is provisioned here if absent, so pairing works before
/// the first server start.
///
/// When the server address is known (`--host`, or a detected overlay address
/// plus the `PHUX_WS_ADDR` port), the credentials are also printed as a
/// `phux://connect` one-tap link, and `--qr` renders that same link as a
/// scannable terminal QR (ADR-0031's "shown as a QR" pairing idiom).
#[allow(
    clippy::needless_pass_by_value,
    reason = "CLI entry point owns the args clap dispatch hands it; taking them by value keeps the call site clean"
)]
pub(crate) fn run_pair(
    tokens: Option<PathBuf>,
    cert: Option<PathBuf>,
    qr: bool,
    host: Option<String>,
    name: Option<String>,
    json: bool,
) -> ExitCode {
    let tokens = tokens
        .or_else(|| std::env::var_os("PHUX_WS_TOKENS").map(PathBuf::from))
        .unwrap_or_else(phux_server::auth::default_token_store_path);
    let operator_cert = cert.is_some() || std::env::var_os("PHUX_WS_TLS_CERT").is_some();
    let cert = cert
        .or_else(|| std::env::var_os("PHUX_WS_TLS_CERT").map(PathBuf::from))
        .unwrap_or_else(phux_server::transport::tls::default_cert_path);
    let key = std::env::var_os("PHUX_WS_TLS_KEY")
        .map_or_else(phux_server::transport::tls::default_key_path, PathBuf::from);

    // Provision the self-signed cert at the default paths if it isn't there yet,
    // so the fingerprint below is the one the server will actually present. An
    // operator-supplied cert is used as-is, never generated over.
    if !operator_cert && let Err(err) = phux_server::transport::tls::ensure_self_signed(&cert, &key)
    {
        eprintln!("phux pair: warning: could not provision certificate: {err}");
    }

    let token = match phux_server::auth::mint_token(&tokens) {
        Ok(token) => token,
        Err(err) => {
            eprintln!("phux pair: failed to mint token: {err}");
            return ExitCode::FAILURE;
        }
    };

    // `--json` keeps stdout a single document (the repo-wide contract in
    // docs/consumers/agents.md): the human blocks below are suppressed and
    // every diagnostic still goes to stderr. `phux enroll` consumes this
    // over ssh, which is what keeps a 64-hex token out of human hands.
    if !json {
        outln!("Pairing token (a secret — give it to the device once):");
        outln!("  {token}");
        outln!();
    }

    let fingerprint = match phux_server::transport::tls::cert_fingerprint(&cert) {
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
    };

    // Best-effort (ADR-0037): `detect` is infallible by construction — it
    // returns an empty vec when nothing is detected — so this block can
    // never affect the exit code.
    let overlay = super::overlay::detect();
    if !json && !overlay.is_empty() {
        outln!("Overlay network addresses (dial one of these from the device):");
        for addr in &overlay {
            outln!("  {addr}");
        }
        outln!();
    }

    // The one-tap link (and its QR form) carries the token — it is as much
    // a secret as the token line above, shown once on the same terminal.
    let ws_addr = std::env::var("PHUX_WS_ADDR").ok();
    let link = resolve_server_url(host.as_deref(), &overlay, ws_addr.as_deref())
        .map(|url| build_connect_link(&url, name.as_deref(), fingerprint.as_deref(), &token));

    if json {
        return print_pair_json(
            &token,
            fingerprint.as_deref(),
            &overlay,
            ws_addr.as_deref(),
            link.as_deref(),
            &tokens,
        );
    }

    if let Some(link) = &link {
        outln!("One-tap connect link (open on the device — carries the token):");
        outln!("  {link}");
        outln!();
        if qr {
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
    } else if qr {
        eprintln!(
            "phux pair: warning: --qr needs a server address; pass --host HOST:PORT \
             (no overlay address + PHUX_WS_ADDR port to derive one from)"
        );
    }

    outln!("Token written to {}", tokens.display());
    ExitCode::SUCCESS
}

/// Emit the machine-readable pairing document.
///
/// `quic_addr` and `ws_addr` are reported as the server's *configured bind*
/// (from the environment the listener reads), not a dialable address — the
/// consumer pairs them with an overlay address to build an endpoint. They are
/// null when this host has no listener configured, which is exactly the
/// signal `phux enroll` uses to fall back to `ssh://`.
fn print_pair_json(
    token: &str,
    fingerprint: Option<&str>,
    overlay: &[IpAddr],
    ws_addr: Option<&str>,
    connect_link: Option<&str>,
    tokens_path: &std::path::Path,
) -> ExitCode {
    let document = serde_json::json!({
        "token": token,
        "cert_fingerprint": fingerprint,
        "overlay_addresses": overlay
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>(),
        "ws_addr": ws_addr,
        "quic_addr": std::env::var("PHUX_QUIC_ADDR").ok(),
        "connect_link": connect_link,
        "tokens_path": tokens_path.display().to_string(),
    });
    match serde_json::to_string_pretty(&document) {
        Ok(text) => {
            outln!("{text}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("phux pair: could not encode JSON: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{build_connect_link, percent_encode, render_qr, resolve_server_url};
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn link_includes_only_present_fields_in_stable_order() {
        // url + token are the floor.
        assert_eq!(
            build_connect_link("wss://h:1", None, None, "deadbeef"),
            "phux://connect?url=wss://h:1&token=deadbeef"
        );
        // Full house, in the order the mobile parser documents.
        assert_eq!(
            build_connect_link(
                "wss://10.0.0.2:8787",
                Some("mini"),
                Some("AB:CD"),
                "deadbeef"
            ),
            "phux://connect?url=wss://10.0.0.2:8787&name=mini&fp=AB:CD&token=deadbeef"
        );
        // No fingerprint — the fp param is absent, not empty.
        assert_eq!(
            build_connect_link("wss://h:1", Some("mini"), None, "deadbeef"),
            "phux://connect?url=wss://h:1&name=mini&token=deadbeef"
        );
    }

    #[test]
    fn name_is_percent_encoded() {
        assert_eq!(percent_encode("studio mini"), "studio%20mini");
        assert_eq!(percent_encode("plain-name_1.ok~"), "plain-name_1.ok~");
        assert_eq!(percent_encode("a&b=c"), "a%26b%3Dc");
        assert_eq!(
            build_connect_link("wss://h:1", Some("studio mini"), None, "aa"),
            "phux://connect?url=wss://h:1&name=studio%20mini&token=aa"
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
        // The flag also beats a detected overlay address.
        let overlay = [IpAddr::V4(Ipv4Addr::new(100, 64, 0, 9))];
        assert_eq!(
            resolve_server_url(Some("mini:1"), &overlay, Some("0.0.0.0:2")),
            Some("wss://mini:1".to_owned())
        );
    }

    #[test]
    fn overlay_fallback_derives_url_only_with_a_port() {
        let overlay = [IpAddr::V4(Ipv4Addr::new(100, 64, 0, 2))];
        // Overlay IP + PHUX_WS_ADDR port -> self-contained wss URL.
        assert_eq!(
            resolve_server_url(None, &overlay, Some("0.0.0.0:8787")),
            Some("wss://100.64.0.2:8787".to_owned())
        );
        // No port to borrow -> no derived URL (never guess a port).
        assert_eq!(resolve_server_url(None, &overlay, None), None);
        assert_eq!(resolve_server_url(None, &overlay, Some("no-port")), None);
        // No host flag and no overlay address -> nothing to build.
        assert_eq!(resolve_server_url(None, &[], Some("0.0.0.0:8787")), None);
        // A v6 overlay address is bracketed so the URL stays parseable.
        let v6 = [IpAddr::V6(Ipv6Addr::LOCALHOST)];
        assert_eq!(
            resolve_server_url(None, &v6, Some("0.0.0.0:8787")),
            Some("wss://[::1]:8787".to_owned())
        );
    }

    #[test]
    fn renders_a_nonempty_qr_for_a_realistic_payload() {
        // A real 32-byte hex token + SHA-256 fingerprint is well within QR
        // capacity; the renderer must produce non-empty half-block art.
        let link = build_connect_link(
            "wss://100.64.0.2:8787",
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
}
