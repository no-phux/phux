//! The ssh origin `phux stdio-bridge` stamps on the HELLO it relays
//! (`docs/spec/proto.md` §6.1 field 9, `docs/spec/L3.md` §3.9).
//!
//! `ssh HOST phux stdio-bridge` makes the bridge a Unix-socket client of
//! HOST's server, so on its own the server would report the ssh-bridged
//! connection as a local one. The bridge knows more: sshd puts the
//! connection's endpoints in the remote command's environment
//! (`SSH_CONNECTION`). The bridge rewrites the client's HELLO to carry them
//! as the additive `ssh_origin` field. The server accepts the field only from
//! a Unix-socket peer running as the serving uid, and then reports the route
//! as `ssh-stdio`.
//!
//! The field only labels a connection. It grants nothing: the connection is
//! authenticated and authorized exactly like any other Unix-socket peer. The
//! value is whatever the connecting side reported, not an authenticated fact.
//! The ssh client chooses the remote command and can set the bridge's
//! `SSH_CONNECTION`, an older bridge forwards a client's own field, and any
//! process running as the serving user can send one. The bridge always
//! removes any `ssh_origin` the remote client put in its own HELLO before
//! adding its own. That guarantees the sshd-reported value only when ssh
//! forces the bridge command.
//! Gated on [`ServerFeature::SshOrigin`](crate::caps::ServerFeature::SshOrigin).

use std::net::{IpAddr, SocketAddr};

use bytes::BytesMut;

use super::decode::Decoder;
use super::encode::Encoder;
use super::field;
use super::frame::{MAX_FRAME_LEN, TYPE_HELLO};
use super::framing::LENGTH_PREFIX_LEN;

/// The two ends of the ssh connection a bridged client arrived over, as sshd
/// reports them to the remote command. Nothing in it is secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SshOrigin {
    /// The ssh client's address and source port.
    pub client: SocketAddr,
    /// The sshd address and port the client reached, when known.
    /// `SSH_CLIENT`, the older variable, does not name the server address.
    pub server: Option<SocketAddr>,
}

/// Write `origin` as the positional HELLO field-9 value:
/// `client_addr: str, client_port: u16, has_server: u8, [server_addr: str, server_port: u16]`.
pub(in crate::wire) fn encode_ssh_origin(origin: &SshOrigin, enc: &mut Encoder<'_>) {
    write_endpoint(origin.client, enc);
    match origin.server {
        Some(server) => {
            enc.write_u8(1);
            write_endpoint(server, enc);
        }
        None => enc.write_u8(0),
    }
}

fn write_endpoint(endpoint: SocketAddr, enc: &mut Encoder<'_>) {
    enc.write_str(&endpoint.ip().to_string());
    enc.write_u16_be(endpoint.port());
}

/// Read a HELLO field-9 value. An unreadable value decodes as `None`: an
/// origin the server cannot parse is ignored, never a reason to refuse the
/// HELLO.
pub(in crate::wire) fn decode_ssh_origin(value: &[u8]) -> Option<SshOrigin> {
    let mut dec = Decoder::new(value);
    let client = read_endpoint(&mut dec)?;
    let server = match dec.read_u8().ok()? {
        0 => None,
        1 => Some(read_endpoint(&mut dec)?),
        _ => return None,
    };
    Some(SshOrigin { client, server })
}

fn read_endpoint(dec: &mut Decoder<'_>) -> Option<SocketAddr> {
    let ip: IpAddr = dec.read_str().ok()?.parse().ok()?;
    let port = dec.read_u16_be().ok()?;
    Some(SocketAddr::new(ip, port))
}

/// Rewrite one complete `HELLO` frame so that its `ssh_origin` field is
/// exactly `origin`.
///
/// `frame` is the whole frame: length prefix, type byte, and body. Every other
/// field is copied byte for byte and in order. Every `ssh_origin` already
/// present is always dropped. `origin`, when `Some`, is appended only if the
/// HELLO still fits the frame cap; otherwise the HELLO leaves with no
/// `ssh_origin` at all, and the server reports `uds`. A client-supplied field
/// 9 never survives a rewrite, whatever the frame's size.
///
/// Returns `None` only when forwarding `frame` unchanged cannot carry a
/// client-supplied field 9 into an accepted HELLO: it is not one well-framed
/// HELLO, its body is not a TLV field sequence (the server walks the same
/// fields and refuses such a HELLO), or it carries no `ssh_origin` and none is
/// to be added.
#[must_use]
pub fn restamp_hello(frame: &[u8], origin: Option<SshOrigin>) -> Option<Vec<u8>> {
    let body = hello_body(frame)?;
    let mut fields = fields_without_origin(body)?;
    if fields.len() == body.len() && origin.is_none() {
        return None;
    }
    if let Some(origin) = origin {
        append_origin_if_it_fits(&mut fields, origin);
    }
    Some(hello_frame(&fields))
}

/// The TLV body of `frame` when it is exactly one HELLO frame.
fn hello_body(frame: &[u8]) -> Option<&[u8]> {
    let (header, rest) = frame.split_first_chunk::<LENGTH_PREFIX_LEN>()?;
    let declared = usize::try_from(u32::from_be_bytes(*header)).ok()?;
    let (&type_byte, body) = rest.split_first()?;
    (declared == rest.len() && type_byte == TYPE_HELLO).then_some(body)
}

/// The raw bytes of every field in `body` except `ssh_origin`, or `None` when
/// `body` is not a TLV field sequence.
fn fields_without_origin(body: &[u8]) -> Option<Vec<u8>> {
    let mut dec = Decoder::new(body);
    let mut kept = Vec::with_capacity(body.len());
    loop {
        let start = dec.position();
        let Some((id, _)) = dec.read_field().ok()? else {
            return Some(kept);
        };
        if id != field::hello::SSH_ORIGIN {
            kept.extend_from_slice(body.get(start..dec.position())?);
        }
    }
}

/// Append `origin` as field 9 unless the HELLO would then exceed the frame
/// cap. Leaving it out only makes the server report `uds`; the client's own
/// field 9 is already gone either way.
fn append_origin_if_it_fits(fields: &mut Vec<u8>, origin: SshOrigin) {
    let mut encoded = BytesMut::new();
    Encoder::new(&mut encoded).write_field_with(field::hello::SSH_ORIGIN, |enc| {
        encode_ssh_origin(&origin, enc);
    });
    // The body is the type byte plus every field.
    let body_len = 1 + fields.len() + encoded.len();
    if u32::try_from(body_len).is_ok_and(|len| len <= MAX_FRAME_LEN) {
        fields.extend_from_slice(&encoded);
    }
}

/// Frame `fields` as one HELLO. Infallible: `fields` is never longer than a
/// body that already fit a `u32` length prefix, plus an origin appended only
/// under the cap.
fn hello_frame(fields: &[u8]) -> Vec<u8> {
    let body_len = 1 + fields.len();
    let length = u32::try_from(body_len).unwrap_or(u32::MAX);
    let mut frame = Vec::with_capacity(LENGTH_PREFIX_LEN + body_len);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.push(TYPE_HELLO);
    frame.extend_from_slice(fields);
    frame
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, reason = "tests")]
    #![allow(clippy::panic, reason = "tests")]

    use bytes::BytesMut;

    use super::{SshOrigin, restamp_hello};
    use crate::caps::ClientCapabilities;
    use crate::wire::decode::Decoder;
    use crate::wire::encode::Encoder;
    use crate::wire::field;
    use crate::wire::frame::FrameKind;

    fn origin(client: &str, server: Option<&str>) -> SshOrigin {
        SshOrigin {
            client: client.parse().expect("client endpoint"),
            server: server.map(|server| server.parse().expect("server endpoint")),
        }
    }

    fn hello(caps: ClientCapabilities) -> Vec<u8> {
        let mut buf = BytesMut::new();
        FrameKind::Hello {
            client_name: "hub".to_owned(),
            protocol_major: 0,
            protocol_minor: 9,
            protocol_patch: 0,
            client_caps: caps,
        }
        .encode(&mut buf);
        buf.to_vec()
    }

    fn decoded_origin(frame: &[u8]) -> Option<SshOrigin> {
        let (frame, rest) = Decoder::new(frame).read_frame().expect("decodes");
        assert!(rest.is_empty(), "exactly one frame");
        let FrameKind::Hello { client_caps, .. } = frame else {
            panic!("expected HELLO, got {frame:?}");
        };
        client_caps.ssh_origin
    }

    /// Append a raw TLV field to a whole frame and fix its length prefix.
    fn with_raw_field(frame: &[u8], field_id: u32, value: &[u8]) -> Vec<u8> {
        let mut extra = BytesMut::new();
        Encoder::new(&mut extra).write_field(field_id, value);
        let mut out = frame.to_vec();
        out.extend_from_slice(&extra);
        let length = u32::try_from(out.len() - 4).expect("fits");
        out[..4].copy_from_slice(&length.to_be_bytes());
        out
    }

    #[test]
    fn a_plain_hello_gains_the_bridge_origin() {
        let want = origin("203.0.113.5:52144", Some("198.51.100.7:22"));
        let stamped = restamp_hello(&hello(ClientCapabilities::new()), Some(want))
            .expect("an origin to add is a rewrite");
        assert_eq!(decoded_origin(&stamped), Some(want));
    }

    #[test]
    fn a_client_supplied_origin_is_replaced_by_the_bridge_one() {
        let forged = origin("10.0.0.1:1", None);
        let frame = hello(ClientCapabilities::new().with_ssh_origin(forged));
        let want = origin("[2001:db8::1]:40000", None);
        let stamped = restamp_hello(&frame, Some(want)).expect("rewrite");
        assert_eq!(decoded_origin(&stamped), Some(want));
    }

    #[test]
    fn a_client_supplied_origin_is_stripped_when_the_bridge_has_none() {
        let forged = origin("10.0.0.1:1", None);
        let frame = hello(ClientCapabilities::new().with_ssh_origin(forged));
        let stripped = restamp_hello(&frame, None).expect("a strip is a rewrite");
        assert_eq!(decoded_origin(&stripped), None);
        assert_eq!(stripped, hello(ClientCapabilities::new()));
    }

    #[test]
    fn nothing_to_change_forwards_the_original() {
        assert_eq!(restamp_hello(&hello(ClientCapabilities::new()), None), None);
    }

    #[test]
    fn fields_the_bridge_does_not_know_survive_byte_for_byte() {
        let frame = with_raw_field(&hello(ClientCapabilities::new()), 42, b"future");
        let want = origin("203.0.113.5:1", None);
        let stamped = restamp_hello(&frame, Some(want)).expect("rewrite");
        assert!(
            stamped.windows(frame.len() - 4).any(|w| w == &frame[4..]),
            "the original body, unknown field included, is kept verbatim"
        );
        assert_eq!(decoded_origin(&stamped), Some(want));
    }

    #[test]
    fn a_frame_that_is_not_a_well_formed_hello_is_left_alone() {
        let want = Some(origin("203.0.113.5:1", None));
        let mut ping = BytesMut::new();
        FrameKind::Ping { nonce: 7 }.encode(&mut ping);
        assert_eq!(restamp_hello(&ping, want), None, "not a HELLO");
        let frame = hello(ClientCapabilities::new());
        assert_eq!(
            restamp_hello(&frame[..frame.len() - 1], want),
            None,
            "short"
        );
        // A HELLO whose body is not TLV: the server refuses it itself.
        let garbage = [0, 0, 0, 5, 0x01, b'h', b'e', b'l', b'l'];
        assert_eq!(restamp_hello(&garbage, want), None, "not TLV");
    }

    /// An origin the server cannot parse is ignored, not a HELLO failure.
    #[test]
    fn an_unreadable_origin_decodes_as_absent() {
        let frame = with_raw_field(
            &hello(ClientCapabilities::new()),
            field::hello::SSH_ORIGIN,
            b"\x00\x00\x00\x03abc",
        );
        assert_eq!(decoded_origin(&frame), None);
    }

    /// `base` with one unknown field padding its body to exactly the frame
    /// cap. The pad field's header is its id (1 byte), wire type (1 byte),
    /// and a 4-byte varint length, which covers any length near the cap.
    fn padded_to_cap(base: &[u8]) -> Vec<u8> {
        let cap = usize::try_from(crate::wire::frame::MAX_FRAME_LEN).expect("fits");
        let pad = cap - (base.len() - 4) - 6;
        let frame = with_raw_field(base, 42, &vec![0; pad]);
        assert_eq!(frame.len() - 4, cap, "padded to exactly the cap");
        frame
    }

    /// Security regression: a remote client's HELLO padded to the frame cap
    /// around a forged origin must not keep that origin when the bridge's own
    /// stamp no longer fits. The rewrite sends the HELLO with no origin at
    /// all; it never falls back to the original frame.
    #[test]
    fn an_oversized_hello_never_keeps_a_forged_origin() {
        let forged = origin("1.2.3.4:1", None);
        let frame = padded_to_cap(&hello(ClientCapabilities::new().with_ssh_origin(forged)));
        let bridge = origin("[2001:db8::1]:40000", Some("[2001:db8::2]:2222"));
        let out = restamp_hello(&frame, Some(bridge)).expect("a forged origin is always rewritten");
        assert_eq!(
            decoded_origin(&out),
            None,
            "forged origin gone, stamp too big"
        );
        assert!(out.len() < frame.len(), "never larger than the original");
        let stripped = restamp_hello(&frame, None).expect("a strip is a rewrite");
        assert_eq!(decoded_origin(&stripped), None);
    }
}
