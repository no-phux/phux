//! From a resolved registry entry to a dial: the CLI's trust rules, the
//! operator-facing wording of every failure, and the SPEC §5 frame cutting
//! the WebSocket lane needs.
//!
//! Trust and credential rules are the CLI's (`phux attach`'s
//! `plan_quic_dial` / `plan_ws_dial`): a routable host needs a certificate
//! pin, and a routable WebSocket also needs `wss://` and a bearer token. The
//! token file is read by [`load_token`] just before the dial, on the dialing
//! thread, and the owned copy is dropped as soon as the dial returns. Client
//! TLS identity is always [`phux_dial::TlsClientIdentity::None`]: an
//! embedder must not inherit `PHUX_WORKLOAD_CERT` / `PHUX_WORKLOAD_KEY` from
//! a launcher shell.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use bytes::BytesMut;
use phux_dial::quic::{QuicDial, parse_token_hex};
use phux_dial::ws::{WS_LIVENESS_TIMEOUT, WsDial, WsTarget};
use phux_dial::{CertTrust, DialError};
use phux_protocol::wire::framing;

use crate::target::{Resolved, read_token};

/// The bound on name resolution plus transport establishment.
///
/// The CLI has no overall bound here; an embedder does, because a status
/// line that says "connecting" for macOS's 75-second TCP connect timeout
/// reads as a hang.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(15);

/// Largest frame body a consumer may hand the WebSocket lane: the SPEC §5
/// hard bound, the same ceiling Cockpit's socket worker enforces inbound
/// (`transport.max_frame_bytes` less its 4-byte prefix).
pub const MAX_FRAME_BODY: usize = 16 * 1024 * 1024;

/// Plan the QUIC dial for `authority` (`HOST:PORT`): resolve the name, apply
/// the pin rule, and parse the bearer token. The returned plan owns the
/// token; drop it as soon as the dial has sent its preamble.
pub async fn plan_quic(resolved: &Resolved, authority: &str) -> Result<QuicDial, String> {
    let name = resolved.name.as_str();
    let (bare, addr) = resolve_quic_addr(name, authority).await?;
    let trust = quic_trust(
        name,
        resolved.cert_fingerprint.as_deref(),
        addr.ip().is_loopback(),
    )?;
    let token = match load_token(resolved)? {
        Some(token) => Some(parse_token_hex(&token).map_err(|_| bad_token(resolved))?),
        None => None,
    };
    Ok(QuicDial {
        addr,
        server_name: quic_server_name(bare),
        token,
        trust,
    })
}

/// Split `HOST:PORT` and resolve it to its first address, as the CLI's
/// `resolve_quic_target` does. Returns the unbracketed host beside it.
async fn resolve_quic_addr<'a>(
    name: &str,
    authority: &'a str,
) -> Result<(&'a str, SocketAddr), String> {
    let (host, port) = authority
        .rsplit_once(':')
        .and_then(|(host, port)| Some((host, port.parse::<u16>().ok()?)))
        .ok_or_else(|| format!("{name}: endpoint quic://{authority} needs HOST:PORT"))?;
    let bare = host.trim_matches(['[', ']']);
    let addr = tokio::net::lookup_host((bare, port))
        .await
        .map_err(|err| {
            format!("{name}: could not resolve {bare}: {err}; is its network connected?")
        })?
        .next()
        .ok_or_else(|| format!("{name}: {bare} resolved to no addresses"))?;
    Ok((bare, addr))
}

/// Pin when the registry carries a fingerprint, trust loopback's dev
/// certificate, and refuse an unpinned routable dial (the CLI's `quic_trust`).
fn quic_trust(name: &str, pin: Option<&str>, loopback: bool) -> Result<CertTrust, String> {
    match pin {
        Some(fingerprint) => Ok(CertTrust::Pinned(fingerprint.to_owned())),
        None if loopback => Ok(CertTrust::SkipVerify),
        None => Err(unpinned(name)),
    }
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
pub fn plan_ws(resolved: &Resolved, url: &str, token: Option<String>) -> Result<WsDial, String> {
    let name = resolved.name.as_str();
    let target = WsTarget::parse(url).map_err(|err| format!("{name}: {err}"))?;
    if !target.is_loopback() {
        if !target.secure {
            return Err(format!(
                "{name}: {url} is plaintext; a routable WebSocket needs wss://"
            ));
        }
        if resolved.cert_fingerprint.is_none() {
            return Err(unpinned(name));
        }
        if token.is_none() {
            return Err(format!(
                "{name} has no token file in the registry; re-pair it with `phux host enroll {name}`"
            ));
        }
    }
    let token = match token {
        Some(token) => {
            parse_token_hex(&token).map_err(|_| bad_token(resolved))?;
            Some(token.trim().to_owned())
        }
        None => None,
    };
    Ok(WsDial {
        url: url.to_owned(),
        token,
        trust: resolved
            .cert_fingerprint
            .clone()
            .map_or(CertTrust::SkipVerify, CertTrust::Pinned),
        tls_server_name: None,
    })
}

/// Read the bearer token for this dial. Failures name the file, never bytes.
pub fn load_token(resolved: &Resolved) -> Result<Option<String>, String> {
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

/// Split one complete `len:u32 BE` + body frame off the front of `pending`,
/// leaving a partial tail for the next read.
pub fn take_frame(pending: &mut BytesMut) -> Result<Option<BytesMut>, &'static str> {
    let Some(header) = pending.get(..4) else {
        return Ok(None);
    };
    let declared = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
    if declared == 0 || declared > MAX_FRAME_BODY {
        return Err("the embedder sent a frame with an invalid length");
    }
    if pending.len() < 4 + declared {
        return Ok(None);
    }
    Ok(Some(pending.split_to(4 + declared)))
}

/// Check that one binary message is exactly one SPEC §5 frame.
///
/// This is the check the server applies to its own inbound messages. A
/// message that disagrees with its declared length would desynchronize the
/// consumer's framed stream, so it fails the connection with a reason
/// instead.
pub fn check_inbound(name: &str, frame: &[u8]) -> Result<(), String> {
    framing::check_frame(frame)
        .map(|_| ())
        .map_err(|err| format!("{name} sent a malformed frame: {err}"))
}

/// The remedy for a routable host with no pin in the registry.
#[must_use]
pub fn unpinned(name: &str) -> String {
    format!(
        "{name} has no certificate pin in the registry; re-pair it with `phux host enroll {name}`"
    )
}

/// A dial that outlasted [`DIAL_TIMEOUT`].
#[must_use]
pub fn timed_out(name: &str) -> String {
    format!(
        "{name} did not answer within {}s; check that it is up and its network is connected",
        DIAL_TIMEOUT.as_secs()
    )
}

/// A WebSocket lane that went silent past the liveness timeout.
#[must_use]
pub fn stalled(name: &str) -> String {
    format!(
        "{name} stopped answering (no WebSocket traffic for {}s and no pong for our keepalive ping)",
        WS_LIVENESS_TIMEOUT.as_secs()
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
            "{name} refused the pairing token ({detail}); re-pair it with `phux host enroll {name}`"
        ),
        DialError::Io(detail) => format!("{name}: {detail}"),
        DialError::Stalled(detail) => format!("{name} stopped answering ({detail})"),
    }
}

/// Preserve the QUIC application-close code: quinn's stream I/O displays
/// `connection lost` even when the peer sent `AUTH_FAILED`.
#[must_use]
pub fn quic_closed_message(name: &str, err: &quinn::ConnectionError) -> String {
    dial_message(name, &phux_dial::quic::close_error(err))
}

/// A QUIC stream that ended: the connection's close reason when it has
/// one, the stream error otherwise.
#[must_use]
pub fn quic_stream_lost(
    name: &str,
    connection: &quinn::Connection,
    err: impl std::fmt::Display,
) -> String {
    connection.close_reason().map_or_else(
        || format!("{name}: the connection was lost: {err}"),
        |close| quic_closed_message(name, &close),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::target::Transport;

    #[test]
    fn frames_are_cut_at_their_declared_length() {
        let mut pending = BytesMut::from(&b"\x00\x00\x00\x02hi\x00\x00\x00\x03ab"[..]);
        let first = take_frame(&mut pending).expect("valid").expect("complete");
        assert_eq!(&first[..], b"\x00\x00\x00\x02hi");
        // The second frame is short one byte: nothing is taken, nothing lost.
        assert!(take_frame(&mut pending).expect("valid").is_none());
        assert_eq!(&pending[..], b"\x00\x00\x00\x03ab");
        pending.extend_from_slice(b"c");
        assert_eq!(
            &take_frame(&mut pending).expect("valid").expect("complete")[..],
            b"\x00\x00\x00\x03abc"
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn zero_and_oversized_lengths_are_refused() {
        assert!(take_frame(&mut BytesMut::from(&b"\x00\x00\x00\x00"[..])).is_err());
        let oversized = u32::try_from(MAX_FRAME_BODY + 1)
            .expect("fits")
            .to_be_bytes();
        assert!(take_frame(&mut BytesMut::from(&oversized[..])).is_err());
    }

    #[test]
    fn an_inbound_message_must_be_exactly_one_frame() {
        assert!(check_inbound("mini", b"\x00\x00\x00\x02hi").is_ok());
        for malformed in [
            &b"\x00\x00\x00"[..],
            &b"\x00\x00\x00\x03hi"[..],
            &b"\x00\x00\x00\x01hi"[..],
            &b"\x00\x00\x00\x01h\x00\x00\x00\x01i"[..],
        ] {
            let err = check_inbound("mini", malformed).expect_err("malformed");
            assert!(err.starts_with("mini sent a malformed frame"), "{err}");
        }
    }

    fn resolved(url: &str, pin: Option<&str>) -> Resolved {
        Resolved {
            name: "mini".to_owned(),
            endpoint: url.to_owned(),
            session: None,
            transport: Transport::Ws(url.to_owned()),
            token_file: Some(PathBuf::from("/secret/mini.token")),
            cert_fingerprint: pin.map(str::to_owned),
        }
    }

    #[test]
    fn routable_websocket_needs_tls_pin_and_token_like_the_cli() {
        let pin = "ab".repeat(32);
        let routable = "wss://mini.ts.net:8787";
        assert!(
            plan_ws(
                &resolved("ws://mini.ts.net:8787", Some(&pin)),
                "ws://mini.ts.net:8787",
                Some("ab".to_owned())
            )
            .is_err()
        );
        assert!(plan_ws(&resolved(routable, None), routable, Some("ab".to_owned())).is_err());
        assert!(plan_ws(&resolved(routable, Some(&pin)), routable, None).is_err());
        let ok = plan_ws(
            &resolved(routable, Some(&pin)),
            routable,
            Some(" ab ".to_owned()),
        )
        .expect("pinned");
        assert_eq!(ok.token.as_deref(), Some("ab"));
        assert_eq!(ok.trust, CertTrust::Pinned(pin));
        // Loopback is the development path: no pin, no token, plaintext.
        let local = plan_ws(
            &resolved("ws://127.0.0.1:8787", None),
            "ws://127.0.0.1:8787",
            None,
        )
        .expect("loopback");
        assert_eq!(local.trust, CertTrust::SkipVerify);
    }

    #[test]
    fn a_malformed_token_is_reported_by_file_never_by_its_bytes() {
        let pin = "ab".repeat(32);
        let routable = "wss://mini.ts.net:8787";
        let err = plan_ws(
            &resolved(routable, Some(&pin)),
            routable,
            Some("zq-secret".to_owned()),
        )
        .expect_err("not hex");
        assert_eq!(
            err,
            "mini: the token file /secret/mini.token is not valid hex"
        );
        for leaked in ['z', 'q', 's'] {
            assert!(!err[..err.find(" the token file").expect("prefix")].contains(leaked));
        }
    }

    #[test]
    fn routable_quic_without_a_pin_is_refused_after_resolution() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let mut unpinned = resolved("quic://192.0.2.1:8788", None);
        unpinned.transport = Transport::Quic("192.0.2.1:8788".to_owned());
        unpinned.token_file = None;
        let err = runtime
            .block_on(plan_quic(&unpinned, "192.0.2.1:8788"))
            .expect_err("unpinned");
        assert!(err.contains("certificate pin"), "{err}");
        let loopback = runtime
            .block_on(plan_quic(&unpinned, "127.0.0.1:8788"))
            .expect("loopback");
        assert_eq!(loopback.trust, CertTrust::SkipVerify);
        assert_eq!(loopback.server_name, "localhost");
    }

    #[test]
    fn revoked_quic_token_is_re_pair_not_connection_lost() {
        let msg = dial_message(
            "stale-token",
            &DialError::AuthRefused("unauthorized".to_owned()),
        );
        assert!(
            msg.contains("refused the pairing token") && msg.contains("unauthorized"),
            "{msg}"
        );
        assert!(
            msg.contains("phux host enroll stale-token"),
            "B9 remedy must name re-pair: {msg}"
        );
        assert!(
            !msg.contains("connection was lost") && !msg.contains("connection lost"),
            "{msg}"
        );
    }

    #[test]
    fn quic_auth_close_is_not_generic_loss() {
        let err = quinn::ConnectionError::ApplicationClosed(quinn::ApplicationClose {
            error_code: quinn::VarInt::from_u32(phux_dial::quic::AUTH_FAILED_CODE),
            reason: b"unauthorized".as_slice().into(),
        });
        let msg = quic_closed_message("stale-token", &err);
        assert!(msg.contains("refused the pairing token"), "{msg}");
        assert!(!msg.contains("connection was lost"), "{msg}");
        assert!(!msg.contains("connection lost"), "{msg}");
    }

    #[test]
    fn quic_idle_timeout_is_reachability_not_auth() {
        let msg = quic_closed_message("mini", &quinn::ConnectionError::TimedOut);
        assert!(msg.contains("did not answer"), "{msg}");
        assert!(!msg.contains("pairing token"), "{msg}");
    }

    #[test]
    fn graceful_quic_close_is_not_auth() {
        let err = quinn::ConnectionError::ApplicationClosed(quinn::ApplicationClose {
            error_code: quinn::VarInt::from_u32(0),
            reason: b"bye".as_slice().into(),
        });
        let msg = quic_closed_message("mini", &err);
        assert!(!msg.contains("pairing token"), "{msg}");
        assert!(!msg.contains("enroll"), "{msg}");
    }
}
