//! A blind QUIC tunnel relay.
//!
//! A service behind NAT dials **out** to the relay and registers a tunnel
//! for a named route; consumers dial **in** and name the route in the TLS
//! server name. The relay opens one stream over the tunnel per consumer and
//! splices the two without reading them. Whatever the consumer and the
//! service run inside the stream (their own TLS session, ideally) is opaque
//! to the relay: it routes on one label and forwards bytes.
//!
//! Why QUIC rather than TCP over a WebSocket bridge: every consumer is its
//! own stream, so one stalled consumer never holds back another (no
//! head-of-line blocking across the tunnel); the tunnel survives the
//! service's address changing (connection migration); and both legs share
//! one UDP endpoint and one handshake budget.
//!
//! The pieces:
//!
//! - [`Protocol`]: the ALPNs and connection-ID tag that make a deployment
//!   its own; nothing in this crate names an application.
//! - [`tunnel`]: the wire both ends speak (the stream-0 token preamble, the
//!   close codes, the connection-ID tag).
//! - [`connect`]: the two client legs, a service's [`connect::Tunnel`] and a
//!   consumer's [`connect::dial_route`], with the relay's leaf pinned
//!   ([`pin`]).
//! - [`relay`] (feature `relay`): the relay itself.
//! - [`window`]: the congestion-tracked QUIC send window the relay holds its
//!   consumer writers to; useful to any quinn writer on a slow path.
//! - [`cert`] and [`secret_file`] (feature `provision`): self-signed
//!   certificates and owner-only secret files.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "private modules keep pub(crate) items; conflicts with unreachable_pub"
)]

#[cfg(feature = "provision")]
pub mod cert;
pub mod connect;
pub mod pin;
#[cfg(feature = "relay")]
pub mod relay;
#[cfg(feature = "provision")]
pub mod secret_file;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod tunnel;
pub mod window;

pub use tunnel::CidTag;
pub use window::{SendWindow, TrackedSend};

/// What makes one relay deployment its own: the ALPN a service's tunnel
/// offers, the ALPNs a consumer may offer, and the tag a tunnel's initial
/// connection ID carries.
///
/// A connection's leg is decided by the negotiated ALPN alone, never by what
/// it sends, so the tunnel ALPN must differ from every consumer ALPN
/// ([`Self::validate`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Protocol {
    /// The ALPN a service's tunnel offers.
    pub tunnel_alpn: Vec<u8>,
    /// The ALPNs a consumer may offer; the relay admits any of them and
    /// forwards blindly.
    pub consumer_alpns: Vec<Vec<u8>>,
    /// The tag a tunnel dial puts on its initial destination connection ID,
    /// so the relay can pick the tunnel's flow control before the handshake.
    pub cid_tag: CidTag,
}

impl Protocol {
    /// A protocol with one consumer ALPN.
    #[must_use]
    pub fn new(tunnel_alpn: &[u8], consumer_alpn: &[u8], cid_tag: CidTag) -> Self {
        Self {
            tunnel_alpn: tunnel_alpn.to_vec(),
            consumer_alpns: vec![consumer_alpn.to_vec()],
            cid_tag,
        }
    }

    /// Check the ALPNs can decide a leg: none empty, at least one consumer
    /// ALPN, and the tunnel ALPN never one of them.
    ///
    /// # Errors
    ///
    /// The rule broken.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.tunnel_alpn.is_empty() {
            return Err("the tunnel ALPN is empty");
        }
        if self.consumer_alpns.is_empty() {
            return Err("no consumer ALPN");
        }
        if self.consumer_alpns.iter().any(Vec::is_empty) {
            return Err("a consumer ALPN is empty");
        }
        if self.consumer_alpns.contains(&self.tunnel_alpn) {
            return Err("the tunnel ALPN is also a consumer ALPN");
        }
        Ok(())
    }

    /// Whether `alpn` is one of [`Self::consumer_alpns`].
    #[must_use]
    pub fn is_consumer_alpn(&self, alpn: &[u8]) -> bool {
        self.consumer_alpns.iter().any(|offered| offered == alpn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_protocol_must_tell_its_legs_apart() {
        let tag = CidTag(*b"test");
        assert!(Protocol::new(b"t/1", b"c/1", tag).validate().is_ok());
        assert!(Protocol::new(b"", b"c/1", tag).validate().is_err());
        assert!(Protocol::new(b"t/1", b"", tag).validate().is_err());
        assert!(Protocol::new(b"same", b"same", tag).validate().is_err());
        let mut none = Protocol::new(b"t/1", b"c/1", tag);
        none.consumer_alpns.clear();
        assert!(none.validate().is_err());
        let mut two = Protocol::new(b"t/1", b"c/1", tag);
        two.consumer_alpns.push(b"h3".to_vec());
        assert!(two.validate().is_ok());
        assert!(two.is_consumer_alpn(b"h3") && !two.is_consumer_alpn(b"t/1"));
    }
}
