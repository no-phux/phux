//! TCP port forwarding over the phux connection
//! (`features_ext.FORWARD_TCP`, `docs/forwarding.md`).
//!
//! A client asks the server to dial a TCP port with `FORWARD_OPEN`
//! (answered with `CommandValue::ForwardOpened`, the forward's id), pulls
//! what the target sends with repeated `FORWARD_READ`s (each answered with
//! `CommandValue::ForwardChunk`), pushes bytes to it with `FORWARD_WRITE`
//! (answered once the server has written them), and ends it with
//! `FORWARD_CLOSE`. ssh-agent forwarding (`features_ext.FORWARD_AGENT`)
//! is brokered messages instead: a client offers its agent with
//! `AGENT_OFFER`, receives each request as an `agent_request` event, and
//! answers with `AGENT_REPLY`. The decoder enforces every bound here with
//! [`DecodeError::ForwardLimitExceeded`](crate::wire::DecodeError::ForwardLimitExceeded).

/// The most bytes one `FORWARD_READ` may ask for and one `FORWARD_WRITE`
/// may carry, and so the largest [`ForwardChunk`].
pub const MAX_FORWARD_CHUNK_BYTES: usize = 256 * 1024;
/// Most bytes of a `FORWARD_OPEN` address: one DNS name.
pub const MAX_FORWARD_ADDR_BYTES: usize = 253;

/// One step of a forward (`Command::Forward`); each has its own command tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForwardOp {
    /// `FORWARD_OPEN`: dial `addr:port` from the server.
    Open {
        /// `localhost`, `127.0.0.1`, `::1`, or the host of a target the
        /// server's `[forward] allow` lists. Never resolved for the client.
        addr: String,
        /// The TCP port.
        port: u16,
    },
    /// `FORWARD_READ`: the next bytes the target sent. One read per forward
    /// is in flight at a time; `max_bytes` is the client's window.
    Read {
        /// The forward `FORWARD_OPEN` named.
        id: u64,
        /// Most bytes to return, 1 to [`MAX_FORWARD_CHUNK_BYTES`].
        max_bytes: u32,
    },
    /// `FORWARD_WRITE`: send `bytes` to the target. Answered only after the
    /// server wrote them all, so the answer is the backpressure; one write
    /// per forward is in flight at a time.
    Write {
        /// The forward `FORWARD_OPEN` named.
        id: u64,
        /// At most [`MAX_FORWARD_CHUNK_BYTES`]; may be empty with `fin`.
        bytes: Vec<u8>,
        /// The client's side is done: shut down the write half after
        /// `bytes`.
        fin: bool,
    },
    /// `FORWARD_CLOSE`: end the forward and anything in flight on it.
    /// Idempotent.
    Close {
        /// The forward `FORWARD_OPEN` named.
        id: u64,
    },
    /// `AGENT_OFFER`: this connection serves its local ssh-agent to the
    /// server's panes until it ends (`features_ext.FORWARD_AGENT`). The
    /// newest offer on a server replaces the one before it.
    AgentOffer,
    /// `AGENT_REPLY`: the answer to one `agent_request` event.
    AgentReply {
        /// The request the event named.
        id: u64,
        /// The ssh-agent response, without its length prefix: at most
        /// [`MAX_AGENT_MESSAGE_BYTES`].
        message: Vec<u8>,
    },
}

/// The largest ssh-agent message either way, without its length prefix:
/// the bound ssh-agent itself enforces.
pub const MAX_AGENT_MESSAGE_BYTES: usize = 256 * 1024;
/// How long the server holds a pane's ssh-agent request for the offering
/// client's answer, the user's approval included.
pub const AGENT_REQUEST_SECS: u64 = 60;
/// `SSH_AGENT_FAILURE`, the whole body of a refusal.
pub const SSH_AGENT_FAILURE: u8 = 5;
/// `SSH_AGENTC_REQUEST_IDENTITIES`: list the agent's keys.
pub const SSH_AGENTC_REQUEST_IDENTITIES: u8 = 11;
/// `SSH_AGENT_IDENTITIES_ANSWER`.
pub const SSH_AGENT_IDENTITIES_ANSWER: u8 = 12;
/// `SSH_AGENTC_SIGN_REQUEST`: sign with one of the agent's keys.
pub const SSH_AGENTC_SIGN_REQUEST: u8 = 13;
/// `SSH_AGENT_SIGN_RESPONSE`.
pub const SSH_AGENT_SIGN_RESPONSE: u8 = 14;

/// The response type that answers ssh-agent `request`, or `None` when the
/// request is not one of the two that are ever forwarded.
#[must_use]
pub fn agent_answer_type(request: &[u8]) -> Option<u8> {
    match *request.first()? {
        SSH_AGENTC_REQUEST_IDENTITIES => Some(SSH_AGENT_IDENTITIES_ANSWER),
        SSH_AGENTC_SIGN_REQUEST => Some(SSH_AGENT_SIGN_RESPONSE),
        _ => None,
    }
}

/// The public key blob a `SSH_AGENTC_SIGN_REQUEST` asks to sign with.
#[must_use]
pub fn sign_request_key(request: &[u8]) -> Option<&[u8]> {
    let rest = request.strip_prefix(&[SSH_AGENTC_SIGN_REQUEST])?;
    let (len, rest) = rest.split_first_chunk::<4>()?;
    rest.get(..usize::try_from(u32::from_be_bytes(*len)).ok()?)
}

/// Bytes read from a forward's target (`CommandValue::ForwardChunk`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardChunk {
    /// The bytes, in order. Empty with `eof: false` when the read's idle
    /// deadline passed with nothing new: read again.
    pub bytes: Vec<u8>,
    /// The target closed its side; nothing follows these bytes. Writes
    /// still work until the forward is closed.
    pub eof: bool,
}

/// Whether `op` fits the wire bounds a decoder enforces.
#[must_use]
pub fn op_fits(op: &ForwardOp) -> bool {
    match op {
        ForwardOp::Open { addr, .. } => addr.len() <= MAX_FORWARD_ADDR_BYTES,
        ForwardOp::Read { max_bytes, .. } => {
            usize::try_from(*max_bytes).is_ok_and(|max| max <= MAX_FORWARD_CHUNK_BYTES)
        }
        ForwardOp::Write { bytes, .. } => bytes.len() <= MAX_FORWARD_CHUNK_BYTES,
        ForwardOp::Close { .. } | ForwardOp::AgentOffer => true,
        ForwardOp::AgentReply { message, .. } => message.len() <= MAX_AGENT_MESSAGE_BYTES,
    }
}
