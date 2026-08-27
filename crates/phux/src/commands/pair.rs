//! `phux pair` — mint, rotate, or revoke remote credentials (ADR-0031).
//!
//! The token authenticates a device that attaches over `wss://`; the server
//! reads the same token store at `PHUX_WS_TOKENS`. These operations only write
//! that file — they never contact a running server — so they work before the
//! server starts and need no socket.

use std::net::IpAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Subcommand;

const DEFAULT_ROTATION_OVERLAP_SECONDS: i64 = 300;

#[derive(Debug, Subcommand)]
pub(crate) enum PairAction {
    /// Replace a credential's bearer secret with a bounded overlap.
    Rotate {
        /// Stable credential ID printed when the credential was minted.
        #[arg(value_name = "CREDENTIAL_ID")]
        credential_id: String,

        /// Seconds the previous generation remains valid. Its existing
        /// absolute expiry still wins when it is sooner; an already-expired
        /// credential cannot be rotated.
        #[arg(
            long,
            default_value_t = DEFAULT_ROTATION_OVERLAP_SECONDS,
            value_parser = clap::value_parser!(i64).range(0..=86_400),
            value_name = "SECONDS"
        )]
        overlap_seconds: i64,
    },
    /// Revoke every generation of a credential for new connections.
    Revoke {
        /// Stable credential ID printed when the credential was minted.
        #[arg(value_name = "CREDENTIAL_ID")]
        credential_id: String,
    },
}

/// Scheme + host for the one-tap connect deep-link (and the QR that encodes
/// it). A device that opens or scans it gets the server URL, the cert
/// fingerprint (MITM defense), and the token (credential) in one shot —
/// no typing a 32-byte hex token by hand:
/// `phux://connect?url=<ws(s)-url>[&name=<n>][&fp=<sha256>]&token=<hex>`,
/// where `url` is mandatory — without it the device has nothing to dial and
/// rejects the link — so a link is only emitted when an address is known.
///
/// THIS SHAPE IS OWNED HERE, not by any consumer. [ADR-0031] is the decision
/// record: "A remote consumer parsing the link must accept this exact shape."
/// An earlier version of this comment said the opposite — that the shape
/// belonged to phux-mobile's parser — and the resulting circular ownership
/// cost a real outage: phux-mobile rejected every token-bearing link (which is
/// every link this function emits, since `&token=` is unconditional), so
/// one-tap pairing and QR pairing were both dead while each repo's own tests
/// stayed green. Changing the shape is a change to ADR-0031 and a coordinated
/// consumer update, never a silent edit here.
///
/// [ADR-0031]: ../../../../ADR/0031-remote-consumer-auth-and-encryption.md
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

/// The credentials a `phux://connect?...` link carries.
///
/// The link is the same artifact `phux pair` prints and `phux pair --qr`
/// renders: a phone scans it, and a laptop pastes it into `phux attach
/// --remote HOST --code '<link>'`. Both ends of the pairing therefore share
/// one format, and `--code` needs no second credential shape to exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConnectLink {
    /// The `ws://`/`wss://` endpoint to dial.
    pub(crate) url: String,
    /// The operator's label for the server, when the link carries one.
    pub(crate) name: Option<String>,
    /// The TLS certificate SHA-256 pin.
    pub(crate) cert_fingerprint: Option<String>,
    /// The bearer pairing token. A secret — never echoed back.
    pub(crate) token: String,
}

/// Parse a `phux://connect?...` link back into its parts.
///
/// The exact inverse of [`build_connect_link`], and pinned to it by
/// `connect_link_round_trips`: the link shape is a cross-repo contract
/// (see [`CONNECT_URI_PREFIX`]'s note on the phux-mobile outage), so the
/// parser must never drift from the builder that feeds the QR.
///
/// Strict about the two fields a dial cannot proceed without — a `url` and a
/// `token` — and tolerant of unknown query keys, so a newer minting phux can
/// add one without breaking an older `--code`.
pub(crate) fn parse_connect_link(link: &str) -> Result<ConnectLink, String> {
    let trimmed = link.trim().trim_matches(|c| c == '\'' || c == '"');
    let query = trimmed
        .strip_prefix(CONNECT_URI_PREFIX)
        .and_then(|rest| rest.strip_prefix('?'))
        .ok_or_else(|| {
            format!("a connect code must start with `{CONNECT_URI_PREFIX}?` (paste the whole link `phux pair` printed)")
        })?;

    let (mut url, mut name, mut fingerprint, mut token) = (None, None, None, None);
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, raw) = pair
            .split_once('=')
            .ok_or_else(|| format!("connect code field {pair:?} has no value"))?;
        let decoded = percent_decode(raw)?;
        match key {
            "url" => url = Some(decoded),
            "name" => name = Some(decoded),
            "fp" => fingerprint = Some(decoded),
            "token" => token = Some(decoded),
            // Unknown keys are forward-compat room, not an error.
            _ => {}
        }
    }

    let url = url.filter(|url| !url.is_empty()).ok_or_else(|| {
        "connect code carries no `url=` — it cannot name a server to dial".to_owned()
    })?;
    if !url.starts_with("wss://") && !url.starts_with("ws://") {
        return Err(format!("connect code url {url:?} must be ws:// or wss://"));
    }
    let token = token
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "connect code carries no `token=` — it grants no access".to_owned())?;

    Ok(ConnectLink {
        url,
        name: name.filter(|name| !name.is_empty()),
        cert_fingerprint: fingerprint.filter(|fp| !fp.is_empty()),
        token,
    })
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

/// Every server name this pairing run advertises, in the shape a TLS
/// certificate SAN and a rustls `ServerName` both take: the host of the
/// connect-link URL first, then each detected overlay address.
///
/// The link host is derived with [`WsTarget::parse`] — the same parser the
/// dialer uses to pick its TLS server name — so a certificate minted from this
/// list names exactly what a client will ask for, brackets stripped from a v6
/// literal and no port. Overlay addresses are included beyond the link host
/// because `phux pair` prints them all under "dial one of these from the
/// device", and an address phux tells you to dial is an address its
/// certificate should claim.
///
/// A URL that will not parse contributes nothing rather than erroring: this
/// feeds a best-effort SAN list, and pairing must still mint a token on a host
/// whose `--host` value is odd.
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

/// Warn when the certificate whose fingerprint is about to be printed does not
/// name the address the link is about to advertise (phux-q9a0, ADR-0091).
///
/// Printed here because this is where the mismatch becomes user-visible: the
/// link, the fingerprint, and the address all leave the machine together. The
/// remedy is deliberately an explicit operator action — deleting the pair is
/// the only way to widen the SANs, and it rotates the fingerprint every already
/// paired device pins, so phux will not do it on anyone's behalf.
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
#[allow(
    clippy::too_many_arguments,
    reason = "the existing mint options remain flat for CLI compatibility while the optional action selects lifecycle operations"
)]
#[allow(
    clippy::too_many_lines,
    reason = "pairing keeps address, certificate, secret, and output sequencing together so diagnostics cannot leak into JSON stdout"
)]
pub(crate) fn run_pair(
    action: Option<PairAction>,
    tokens: Option<PathBuf>,
    cert: Option<PathBuf>,
    qr: bool,
    host: Option<String>,
    name: Option<String>,
    json: bool,
    migrate_legacy: bool,
) -> ExitCode {
    let tokens = tokens
        .or_else(|| std::env::var_os("PHUX_WS_TOKENS").map(PathBuf::from))
        .unwrap_or_else(phux_server::auth::default_token_store_path);
    if let Some(action) = action {
        return run_credential_action(&tokens, action, json);
    }
    let operator_cert = cert.is_some() || std::env::var_os("PHUX_WS_TLS_CERT").is_some();
    let cert = cert
        .or_else(|| std::env::var_os("PHUX_WS_TLS_CERT").map(PathBuf::from))
        .unwrap_or_else(phux_server::transport::tls::default_cert_path);
    let key = std::env::var_os("PHUX_WS_TLS_KEY")
        .map_or_else(phux_server::transport::tls::default_key_path, PathBuf::from);

    if migrate_legacy && !migrate_legacy_credentials(&tokens) {
        return ExitCode::FAILURE;
    }

    // Address resolution comes FIRST, before the certificate is provisioned,
    // because SANs can only be chosen at generation time (phux-q9a0,
    // ADR-0091). This is the one place that knows the address the link will
    // advertise, so it is the one place that can name it in the certificate.
    //
    // Best-effort (ADR-0037): `detect` is infallible by construction — it
    // returns an empty vec when nothing is detected — so this block can
    // never affect the exit code.
    let overlay = phux_config::overlay::detect();

    // phux-onbd: fall back to the port the server auto-binds on the overlay
    // address when `PHUX_WS_ADDR` is unset. Without this, pairing on an
    // otherwise perfectly working host printed "--qr needs a server address"
    // and left the user to discover a port number and pass `--host` by hand —
    // while the server was already listening on exactly that address.
    let ws_addr = std::env::var("PHUX_WS_ADDR").ok().or_else(|| {
        (!overlay.is_empty()).then(|| format!(":{}", phux_server::runtime::DEFAULT_WS_PORT))
    });
    let server_url = resolve_server_url(host.as_deref(), &overlay, ws_addr.as_deref());
    let advertised = advertised_names(server_url.as_deref(), &overlay);

    // Provision the self-signed cert at the default paths if it isn't there yet,
    // so the fingerprint below is the one the server will actually present. An
    // operator-supplied cert is used as-is, never generated over.
    if !operator_cert
        && let Err(err) =
            phux_server::transport::tls::ensure_self_signed_for(&cert, &key, &advertised)
    {
        eprintln!("phux pair: warning: could not provision certificate: {err}");
    }

    let minted = match phux_server::auth::mint_token(&tokens) {
        Ok(minted) => minted,
        Err(err) => {
            eprintln!("phux pair: failed to mint token: {err}");
            return ExitCode::FAILURE;
        }
    };
    if !minted.is_durable() {
        eprintln!(
            "phux pair: warning: credential is active, but the store directory could not be synced; do not retry pairing"
        );
    }
    let token = minted.secret().to_owned();

    // `--json` keeps stdout a single document (the repo-wide contract in
    // docs/consumers/agents.md): the human blocks below are suppressed and
    // every diagnostic still goes to stderr. `phux host enroll` consumes
    // this over ssh, which is what keeps a 64-hex token out of human hands.
    if !json {
        outln!("Credential ID (use with `phux pair rotate|revoke`):");
        outln!("  {}", minted.id);
        outln!();
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
    warn_on_uncovered_names(&cert, &key, &advertised);

    if !json && !overlay.is_empty() {
        outln!("Overlay network addresses (dial one of these from the device):");
        for addr in &overlay {
            outln!("  {addr}");
        }
        outln!();
    }

    // The one-tap link (and its QR form) carries the token — it is as much
    // a secret as the token line above, shown once on the same terminal.
    let link = server_url
        .as_deref()
        .map(|url| build_connect_link(url, name.as_deref(), fingerprint.as_deref(), &token));

    if json {
        return print_pair_json(
            &token,
            fingerprint.as_deref(),
            &overlay,
            ws_addr.as_deref(),
            link.as_deref(),
            &tokens,
            &minted.id,
            minted.generation,
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

fn run_credential_action(tokens: &std::path::Path, action: PairAction, json: bool) -> ExitCode {
    match action {
        PairAction::Rotate {
            credential_id,
            overlap_seconds,
        } => {
            let overlap = chrono::Duration::seconds(overlap_seconds);
            let rotated =
                match phux_server::auth::rotate_credential(tokens, &credential_id, overlap) {
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
                return print_action_json(&serde_json::json!({
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
            outln!();
            outln!("Pairing token (a secret — give it to the device once):");
            outln!("  {}", rotated.secret());
            outln!();
            outln!("Token written to {}", tokens.display());
            ExitCode::SUCCESS
        }
        PairAction::Revoke { credential_id } => {
            let outcome = match phux_server::auth::revoke_credential(tokens, &credential_id) {
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
                return print_action_json(&serde_json::json!({
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
    }
}

fn print_action_json(document: &serde_json::Value) -> ExitCode {
    match serde_json::to_string_pretty(&document) {
        Ok(text) => {
            outln!("{text}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("phux pair: could not encode JSON: {error}");
            ExitCode::FAILURE
        }
    }
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

/// Emit the machine-readable pairing document.
///
/// `quic_addr` and `ws_addr` are reported as the server's *configured bind*
/// (from the environment the listener reads), not a dialable address — the
/// consumer pairs them with an overlay address to build an endpoint. They are
/// null when this host has no listener configured, which is exactly the
/// signal `phux host enroll` uses to fall back to `ssh://`.
#[allow(
    clippy::too_many_arguments,
    reason = "one argument per pairing document source keeps secret-bearing output construction explicit"
)]
fn print_pair_json(
    token: &str,
    fingerprint: Option<&str>,
    overlay: &[IpAddr],
    ws_addr: Option<&str>,
    connect_link: Option<&str>,
    tokens_path: &std::path::Path,
    credential_id: &str,
    generation: u64,
) -> ExitCode {
    let document = pair_document(
        token,
        fingerprint,
        overlay,
        ws_addr,
        std::env::var("PHUX_QUIC_ADDR").ok().as_deref(),
        connect_link,
        tokens_path,
        credential_id,
        generation,
    );
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

/// The `phux pair --json` document. Pure, so the shape (including
/// `schema_version`) is unit-testable without touching the environment.
#[allow(
    clippy::too_many_arguments,
    reason = "one field per documented key; a struct would only move the same names one level up"
)]
fn pair_document(
    token: &str,
    fingerprint: Option<&str>,
    overlay: &[IpAddr],
    ws_addr: Option<&str>,
    quic_addr: Option<&str>,
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
        "ws_addr": ws_addr,
        "quic_addr": quic_addr,
        "connect_link": connect_link,
        "tokens_path": tokens_path.display().to_string(),
        "credential_id": credential_id,
        "generation": generation,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        advertised_names, build_connect_link, pair_document, parse_connect_link, percent_encode,
        render_qr, resolve_server_url,
    };
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::path::Path;

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
            Some("0.0.0.0:8787"),
            Some("0.0.0.0:8788"),
            Some("phux://connect?url=wss://100.64.0.2:8787&token=deadbeef"),
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
            "phux://connect?url=wss://100.64.0.2:8787&token=deadbeef"
        );
        assert_eq!(doc["tokens_path"], "/state/remote-tokens");
        assert_eq!(doc["credential_id"], "credential-a");
        assert_eq!(doc["generation"], 1);
        assert_eq!(doc.as_object().map(serde_json::Map::len), Some(10));

        // No address material known: nulls, not absent keys.
        let doc = pair_document(
            "deadbeef",
            None,
            &[],
            None,
            None,
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

    /// The parser is the builder's exact inverse. The link shape is a
    /// cross-repo contract (phux-mobile reads it too), so a change to either
    /// side that the other does not follow must fail here rather than in the
    /// field.
    #[test]
    fn connect_link_round_trips() {
        let link = build_connect_link(
            "wss://100.64.0.2:8787",
            Some("mini box"),
            Some("AB:CD:EF"),
            "deadbeef",
        );
        let parsed = parse_connect_link(&link).expect("parse");
        assert_eq!(parsed.url, "wss://100.64.0.2:8787");
        assert_eq!(parsed.name.as_deref(), Some("mini box"));
        assert_eq!(parsed.cert_fingerprint.as_deref(), Some("AB:CD:EF"));
        assert_eq!(parsed.token, "deadbeef");
    }

    /// A link with no `name`/`fp` (the minimal shape the builder emits) is
    /// still parseable — those two are genuinely optional.
    #[test]
    fn connect_link_without_optional_fields_parses() {
        let link = build_connect_link("wss://mini.ts.net:8787", None, None, "abc123");
        let parsed = parse_connect_link(&link).expect("parse");
        assert_eq!(parsed.name, None);
        assert_eq!(parsed.cert_fingerprint, None);
        assert_eq!(parsed.token, "abc123");
    }

    /// Shells and chat clients wrap a pasted link in quotes; stripping them
    /// is cheaper than teaching every operator to remove them.
    #[test]
    fn connect_link_tolerates_pasted_quotes_and_whitespace() {
        let link = build_connect_link("wss://mini:8787", None, Some("AB"), "tok");
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
        let parsed =
            parse_connect_link("phux://connect?url=wss://mini:8787&brand_new=42&token=tok")
                .expect("parse");
        assert_eq!(parsed.token, "tok");
    }

    /// The two fields a dial cannot proceed without are rejected loudly,
    /// and a non-ws scheme is refused rather than dialed.
    #[test]
    fn connect_link_refuses_links_that_cannot_dial() {
        // Not a connect link at all.
        assert!(parse_connect_link("https://example.com").is_err());
        // No token: grants no access.
        assert!(parse_connect_link("phux://connect?url=wss://mini:8787").is_err());
        // No url: names no server.
        assert!(parse_connect_link("phux://connect?token=tok").is_err());
        // A scheme the WebSocket dialer cannot use.
        assert!(parse_connect_link("phux://connect?url=quic://mini:8788&token=tok").is_err());
        // Empty values are the same as absent.
        assert!(parse_connect_link("phux://connect?url=wss://mini:8787&token=").is_err());
    }
}
