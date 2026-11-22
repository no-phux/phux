//! The tunnel wire, as both the relay and a service's connector speak it.
//!
//! - A service dials the relay offering [`crate::Protocol::tunnel_alpn`],
//!   with an initial destination connection ID tagged by
//!   [`crate::Protocol::cid_tag`].
//! - It opens stream 0 and writes one auth preamble, `len: u32 BE` + the raw
//!   route token ([`encode_preamble`]), at most [`MAX_TOKEN_PREAMBLE`]
//!   bytes, and never writes to stream 0 again.
//! - The relay opens one bidirectional stream over the tunnel per consumer
//!   and forwards that consumer's first stream onto it, both ways, opaquely.
//! - Refusals are application closes with the codes below.
//!
//! The connection-ID tag selects the tunnel's flow-control config, which
//! quinn fixes at accept time before the ALPN is known. It never selects a
//! role: admission is still by ALPN.

/// Close code: bad, missing, or unknown tunnel token.
pub const AUTH_FAILED_CODE: u32 = 0x01;

/// Close code: the route is enrolled but no tunnel is live for it. Unknown
/// routes are refused at TLS instead.
pub const ROUTE_OFFLINE_CODE: u32 = 0x02;

/// Close code: a newer claim on the same route superseded this tunnel
/// (last-writer-wins).
pub const RECLAIMED_CODE: u32 = 0x03;

/// Close code: a peer broke the wire, such as a tunnel writing to stream 0
/// after its preamble, or a consumer that never opened a stream.
pub const PROTOCOL_VIOLATION_CODE: u32 = 0x04;

/// Close code: the relay is at its connection cap; existing connections are
/// unaffected.
pub const OVER_CAP_CODE: u32 = 0x05;

/// Upper bound on a preamble's token.
pub const MAX_TOKEN_PREAMBLE: usize = 256;

/// Length of a tagged connection ID: QUIC's 20-byte maximum.
pub const CID_LEN: usize = 20;

/// The four bytes a tunnel's initial destination connection ID starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CidTag(pub [u8; 4]);

impl CidTag {
    /// Whether an initial destination connection ID carries this tag.
    #[must_use]
    pub fn matches(self, cid: &[u8]) -> bool {
        cid.len() == CID_LEN && cid.starts_with(&self.0)
    }

    /// A fresh tagged connection ID: the tag plus 16 CSPRNG bytes.
    ///
    /// # Errors
    ///
    /// The OS random source failed.
    pub fn fresh_cid(self) -> Result<quinn::ConnectionId, rustls::crypto::GetRandomFailed> {
        let mut bytes = [0_u8; CID_LEN];
        bytes[..self.0.len()].copy_from_slice(&self.0);
        rustls::crypto::ring::default_provider()
            .secure_random
            .fill(&mut bytes[self.0.len()..])?;
        Ok(quinn::ConnectionId::new(&bytes))
    }
}

/// The auth preamble for `token`: `len: u32 BE` + the raw bytes. `None` when
/// the token is longer than [`MAX_TOKEN_PREAMBLE`].
#[must_use]
pub fn encode_preamble(token: &[u8]) -> Option<Vec<u8>> {
    if token.len() > MAX_TOKEN_PREAMBLE {
        return None;
    }
    let len = u32::try_from(token.len()).ok()?;
    let mut preamble = Vec::with_capacity(4 + token.len());
    preamble.extend_from_slice(&len.to_be_bytes());
    preamble.extend_from_slice(token);
    Some(preamble)
}

/// Read one auth preamble, bounded by [`MAX_TOKEN_PREAMBLE`]; `None` on a
/// short read or an oversized length.
pub async fn read_preamble<R>(recv: &mut R) -> Option<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt as _;
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf).await.ok()?;
    let len = usize::try_from(u32::from_be_bytes(len_buf)).ok()?;
    if len > MAX_TOKEN_PREAMBLE {
        return None;
    }
    let mut token = vec![0u8; len];
    recv.read_exact(&mut token).await.ok()?;
    Some(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tagged_cids_are_full_length_random_and_recognized() {
        let tag = CidTag(*b"tunT");
        let first = tag.fresh_cid().unwrap();
        let second = tag.fresh_cid().unwrap();
        assert!(tag.matches(&first) && tag.matches(&second));
        assert_ne!(first[4..], second[4..]);
        let mut tagged = [0x42_u8; CID_LEN];
        tagged[..4].copy_from_slice(b"tunT");
        assert!(tag.matches(&tagged));
        assert!(!CidTag(*b"else").matches(&tagged));
        assert!(!tag.matches(&[0x42_u8; CID_LEN]));
        assert!(!tag.matches(b"tunT"));
        assert!(!tag.matches(&tagged[..8]));
    }

    #[tokio::test]
    async fn preambles_round_trip_and_are_bounded() {
        let encoded = encode_preamble(b"token").unwrap();
        assert_eq!(&encoded[..4], &5u32.to_be_bytes());
        assert_eq!(read_preamble(&mut &encoded[..]).await.unwrap(), b"token");
        assert!(encode_preamble(&[0; MAX_TOKEN_PREAMBLE + 1]).is_none());
        let oversized = u32::try_from(MAX_TOKEN_PREAMBLE + 1).unwrap().to_be_bytes();
        assert!(read_preamble(&mut &oversized[..]).await.is_none());
        assert!(
            read_preamble(&mut &encoded[..6]).await.is_none(),
            "short read"
        );
    }
}
