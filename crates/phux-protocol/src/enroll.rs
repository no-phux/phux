//! The enrollment exchange on [`crate::policy::ENROLL_ALPN`]
//! (`docs/spec/workload-auth.md` §8.2, ADR-0154).
//!
//! One bidirectional stream carries one request and one reply, each sent
//! whole and then finished:
//!
//! ```text
//! request = version:u8 (= 1) | ticket_len:u8 | ticket | csr_len:u32 BE | csr (DER)
//! reply   = status:u8 (0 issued, 1 refused) | len:u32 BE | body
//! ```
//!
//! An issued reply's body is the PEM chain (leaf, then CA); a refused one's
//! is [`REFUSED`]. Decoders take the whole buffer and refuse a wrong version,
//! a length past its bound, truncation, or trailing bytes.

/// The only request version.
pub const VERSION: u8 = 1;

/// Longest ticket a request may carry.
pub const MAX_TICKET: usize = 64;

/// Longest CSR a request may carry.
pub const MAX_CSR: usize = 16 * 1024;

/// Longest reply body: a two-certificate PEM chain fits many times over.
pub const MAX_REPLY_BODY: usize = 64 * 1024;

/// Longest encoded request.
pub const MAX_REQUEST: usize = 1 + 1 + MAX_TICKET + 4 + MAX_CSR;

/// Longest encoded reply.
pub const MAX_REPLY: usize = 1 + 4 + MAX_REPLY_BODY;

/// The body of every refusal: which check failed is not disclosed
/// (`workload-auth.md` §7).
pub const REFUSED: &str = "refused";

/// One enrollment request: the single-use ticket and a CSR for the key the
/// device holds. Neither is a private key.
#[derive(Clone, PartialEq, Eq)]
pub struct Request {
    /// The ticket `phux pair --enroll` minted (raw bytes).
    pub ticket: Vec<u8>,
    /// The PKCS#10 certificate signing request, DER.
    pub csr: Vec<u8>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("ticket", &"[withheld]")
            .field("csr_len", &self.csr.len())
            .finish()
    }
}

/// The server's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// The issued PEM chain: the client certificate, then the CA.
    Issued(String),
    /// Refused; nothing was enrolled.
    Refused,
}

/// Why bytes are not a request or a reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// The version byte is not [`VERSION`].
    #[error("unknown enrollment version")]
    Version,
    /// A length exceeds its bound, or is zero where something is required.
    #[error("enrollment field length out of bounds")]
    Length,
    /// The buffer ends before a declared length.
    #[error("truncated enrollment message")]
    Truncated,
    /// Bytes follow the last field.
    #[error("trailing bytes after the enrollment message")]
    Trailing,
    /// A status byte other than 0 or 1.
    #[error("unknown enrollment status")]
    Status,
    /// An issued body that is not UTF-8.
    #[error("enrollment reply is not text")]
    Text,
}

/// Why a request cannot be encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("enrollment ticket or CSR is empty or too long")]
pub struct EncodeError;

impl Request {
    /// The encoded request.
    ///
    /// # Errors
    ///
    /// An empty or oversized ticket or CSR.
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        let ticket_len = u8::try_from(self.ticket.len()).map_err(|_| EncodeError)?;
        let csr_len = u32::try_from(self.csr.len()).map_err(|_| EncodeError)?;
        if self.ticket.is_empty()
            || self.ticket.len() > MAX_TICKET
            || self.csr.is_empty()
            || self.csr.len() > MAX_CSR
        {
            return Err(EncodeError);
        }
        let mut out = Vec::with_capacity(6 + self.ticket.len() + self.csr.len());
        out.push(VERSION);
        out.push(ticket_len);
        out.extend_from_slice(&self.ticket);
        out.extend_from_slice(&csr_len.to_be_bytes());
        out.extend_from_slice(&self.csr);
        Ok(out)
    }

    /// Decode one whole request.
    ///
    /// # Errors
    ///
    /// See [`DecodeError`].
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let (&version, rest) = bytes.split_first().ok_or(DecodeError::Truncated)?;
        if version != VERSION {
            return Err(DecodeError::Version);
        }
        let (&ticket_len, rest) = rest.split_first().ok_or(DecodeError::Truncated)?;
        let ticket_len = usize::from(ticket_len);
        if ticket_len == 0 || ticket_len > MAX_TICKET {
            return Err(DecodeError::Length);
        }
        let (ticket, rest) = take(rest, ticket_len)?;
        let (len, rest) = take(rest, 4)?;
        let csr_len = usize::try_from(u32::from_be_bytes([len[0], len[1], len[2], len[3]]))
            .map_err(|_| DecodeError::Length)?;
        if csr_len == 0 || csr_len > MAX_CSR {
            return Err(DecodeError::Length);
        }
        let (csr, rest) = take(rest, csr_len)?;
        if !rest.is_empty() {
            return Err(DecodeError::Trailing);
        }
        Ok(Self {
            ticket: ticket.to_vec(),
            csr: csr.to_vec(),
        })
    }
}

impl Reply {
    /// The encoded reply. An issued body longer than [`MAX_REPLY_BODY`] is
    /// encoded as a refusal: no client would accept it.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let (status, body) = match self {
            Self::Issued(chain) if chain.len() <= MAX_REPLY_BODY && !chain.is_empty() => {
                (0_u8, chain.as_bytes())
            }
            _ => (1_u8, REFUSED.as_bytes()),
        };
        let mut out = Vec::with_capacity(5 + body.len());
        out.push(status);
        // Bounded by MAX_REPLY_BODY above.
        out.extend_from_slice(&u32::try_from(body.len()).unwrap_or(0).to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    /// Decode one whole reply.
    ///
    /// # Errors
    ///
    /// See [`DecodeError`].
    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let (&status, rest) = bytes.split_first().ok_or(DecodeError::Truncated)?;
        let (len, rest) = take(rest, 4)?;
        let body_len = usize::try_from(u32::from_be_bytes([len[0], len[1], len[2], len[3]]))
            .map_err(|_| DecodeError::Length)?;
        if body_len > MAX_REPLY_BODY {
            return Err(DecodeError::Length);
        }
        let (body, rest) = take(rest, body_len)?;
        if !rest.is_empty() {
            return Err(DecodeError::Trailing);
        }
        match status {
            0 if !body.is_empty() => std::str::from_utf8(body)
                .map(|chain| Self::Issued(chain.to_owned()))
                .map_err(|_| DecodeError::Text),
            0 => Err(DecodeError::Length),
            1 => Ok(Self::Refused),
            _ => Err(DecodeError::Status),
        }
    }
}

const fn take(bytes: &[u8], len: usize) -> Result<(&[u8], &[u8]), DecodeError> {
    if bytes.len() < len {
        return Err(DecodeError::Truncated);
    }
    Ok(bytes.split_at(len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Every encodable request and reply decodes to itself, and no
        /// prefix of one decodes at all.
        #[test]
        fn encodings_round_trip_and_never_decode_truncated(
            ticket in proptest::collection::vec(any::<u8>(), 1..=MAX_TICKET),
            csr in proptest::collection::vec(any::<u8>(), 1..2048),
            chain in "[ -~]{1,512}",
        ) {
            let request = Request { ticket, csr };
            let bytes = request.encode().unwrap();
            prop_assert_eq!(Request::decode(&bytes).unwrap(), request);
            prop_assert!(Request::decode(&bytes[..bytes.len() - 1]).is_err());
            let reply = Reply::Issued(chain);
            let bytes = reply.encode();
            prop_assert_eq!(Reply::decode(&bytes).unwrap(), reply);
            prop_assert!(Reply::decode(&bytes[..bytes.len() - 1]).is_err());
        }
    }

    fn request() -> Request {
        Request {
            ticket: vec![7; 32],
            csr: vec![0x30, 0x82, 1, 2],
        }
    }

    #[test]
    fn a_request_and_reply_round_trip() {
        let bytes = request().encode().unwrap();
        assert_eq!(bytes[0], VERSION);
        assert_eq!(Request::decode(&bytes).unwrap(), request());
        for reply in [Reply::Issued("PEM".to_owned()), Reply::Refused] {
            assert_eq!(Reply::decode(&reply.encode()).unwrap(), reply);
        }
        assert!(
            !format!("{:?}", request()).contains("7, 7"),
            "ticket withheld"
        );
    }

    #[test]
    fn malformed_messages_are_refused_not_normalized() {
        let good = request().encode().unwrap();
        let mut wrong_version = good.clone();
        wrong_version[0] = 2;
        assert_eq!(Request::decode(&wrong_version), Err(DecodeError::Version));
        assert_eq!(
            Request::decode(&good[..good.len() - 1]),
            Err(DecodeError::Truncated)
        );
        let mut trailing = good;
        trailing.push(0);
        assert_eq!(Request::decode(&trailing), Err(DecodeError::Trailing));
        let mut huge = vec![VERSION, 1, 9];
        huge.extend_from_slice(&u32::try_from(MAX_CSR + 1).unwrap().to_be_bytes());
        assert_eq!(Request::decode(&huge), Err(DecodeError::Length));
        assert_eq!(Request::decode(&[VERSION, 0]), Err(DecodeError::Length));
        assert!(
            Request {
                ticket: vec![1; MAX_TICKET + 1],
                csr: vec![1],
            }
            .encode()
            .is_err()
        );
        assert_eq!(Reply::decode(&[2, 0, 0, 0, 0]), Err(DecodeError::Status));
        assert_eq!(Reply::decode(&[0, 0, 0, 0, 0]), Err(DecodeError::Length));
        assert_eq!(
            Reply::decode(&Reply::Issued("x".repeat(MAX_REPLY_BODY + 1)).encode()),
            Ok(Reply::Refused),
            "an oversized chain is never sent"
        );
    }
}
