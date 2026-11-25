//! TCP port forwarding (`FORWARD_*`, behind `features_ext.FORWARD_TCP`;
//! `docs/ssh.md#forwarding-a-port`).
//!
//! Each call is one correlated command whose answer resolves a
//! [`ForwardCall`], and [`relay`] pumps one local connection through a
//! forward. The ssh-agent calls (`AGENT_OFFER`, `AGENT_REPLY`, behind
//! `features_ext.FORWARD_AGENT`) share the plumbing. Forward ids are the server's and live as long as the
//! connection; after a reconnect every call on an earlier forward is
//! answered `NOT_FOUND`.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use phux_protocol::forward::{ForwardChunk, ForwardOp, MAX_FORWARD_CHUNK_BYTES, op_fits};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode, FrameKind};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;

use super::{ControlPlane, Pending};
use crate::runtime::Client;

/// Why a forward call has no value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ForwardError {
    /// Nothing was sent: the connection is not established.
    #[error("not connected")]
    NotConnected,
    /// Nothing was sent: the server does not advertise the feature
    /// (`FORWARD_TCP`, or `FORWARD_AGENT` for the ssh-agent calls).
    #[error("this server does not serve that kind of forwarding")]
    Unsupported,
    /// Nothing was sent: the call exceeds the wire bounds.
    #[error("the call exceeds the FORWARD bounds")]
    Invalid,
    /// The connection ended before the answer.
    #[error("the connection ended before the server answered")]
    Disconnected,
    /// The server answered with a typed refusal.
    #[error("{message}")]
    Refused {
        /// `PERMISSION_DENIED`, `UNAVAILABLE`, `NOT_FOUND`, ...
        code: ErrorCode,
        /// Display-only detail.
        message: String,
    },
    /// The local connection failed.
    #[error("local connection: {0}")]
    Local(String),
}

/// The pending answer to one forward command.
#[derive(Debug)]
#[must_use = "a forward command's answer arrives through its call"]
pub struct ForwardCall<T> {
    answer: oneshot::Receiver<CommandResult>,
    decode: fn(CommandResult) -> Result<T, ForwardError>,
}

impl<T> Future for ForwardCall<T> {
    type Output = Result<T, ForwardError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let call = self.get_mut();
        Pin::new(&mut call.answer).poll(cx).map(|answer| {
            answer
                .map_err(|_| ForwardError::Disconnected)
                .and_then(call.decode)
        })
    }
}

impl ControlPlane {
    /// Whether the connected server advertises `FORWARD_TCP`.
    #[must_use]
    pub fn forward_supported(&self) -> bool {
        self.method_advertised("FORWARD_OPEN")
    }

    /// Ask the server to dial `addr:port`; the call resolves with the
    /// forward's id.
    pub fn forward_open(
        &mut self,
        addr: String,
        port: u16,
    ) -> Result<ForwardCall<u64>, ForwardError> {
        self.send_forward(ForwardOp::Open { addr, port }, opened)
    }

    /// Read the next bytes the target sent: at least one byte, the end, or
    /// after the server's idle deadline nothing (read again).
    pub fn forward_read(
        &mut self,
        id: u64,
        max_bytes: u32,
    ) -> Result<ForwardCall<ForwardChunk>, ForwardError> {
        self.send_forward(ForwardOp::Read { id, max_bytes }, chunk)
    }

    /// Send `bytes` to the target, then with `fin` end this side. Resolves
    /// once the server has written them.
    pub fn forward_write(
        &mut self,
        id: u64,
        bytes: Vec<u8>,
        fin: bool,
    ) -> Result<ForwardCall<()>, ForwardError> {
        self.send_forward(ForwardOp::Write { id, bytes, fin }, done)
    }

    /// Close forward `id`, ending anything in flight on it. Idempotent.
    pub fn forward_close(&mut self, id: u64) -> Result<ForwardCall<()>, ForwardError> {
        self.send_forward(ForwardOp::Close { id }, done)
    }

    /// Whether the connected server advertises `FORWARD_AGENT`.
    #[must_use]
    pub fn forward_agent_supported(&self) -> bool {
        self.method_advertised("AGENT_OFFER")
    }

    /// Offer this machine's ssh-agent to the server's panes for as long as
    /// this connection lasts. Each request then arrives as an event
    /// [`agent_request`] reads, to be answered with [`Self::agent_reply`]. A
    /// reconnect is a new connection: offer again.
    pub fn agent_offer(&mut self) -> Result<ForwardCall<()>, ForwardError> {
        self.send_forward(ForwardOp::AgentOffer, done)
    }

    /// Answer ssh-agent request `id` with `message`, an ssh-agent response
    /// without its length prefix. `NOT_FOUND` when the server stopped
    /// waiting.
    pub fn agent_reply(
        &mut self,
        id: u64,
        message: Vec<u8>,
    ) -> Result<ForwardCall<()>, ForwardError> {
        self.send_forward(ForwardOp::AgentReply { id, message }, done)
    }

    fn send_forward<T>(
        &mut self,
        op: ForwardOp,
        decode: fn(CommandResult) -> Result<T, ForwardError>,
    ) -> Result<ForwardCall<T>, ForwardError> {
        if !self.handshake_ready {
            return Err(ForwardError::NotConnected);
        }
        let supported = match op {
            ForwardOp::AgentOffer | ForwardOp::AgentReply { .. } => self.forward_agent_supported(),
            _ => self.forward_supported(),
        };
        if !supported {
            return Err(ForwardError::Unsupported);
        }
        if !op_fits(&op) {
            return Err(ForwardError::Invalid);
        }
        let request_id = self.next_request_id();
        let (answer_tx, answer) = oneshot::channel();
        // The answer slots are shared with `SERVICE_HTTP_*`: a correlated
        // command whose answer resolves a call, dropped on disconnect.
        self.pending.insert(request_id, Pending::ServiceHttp);
        self.extensions.service_http.insert(request_id, answer_tx);
        self.queue_frame(&FrameKind::Command {
            request_id,
            command: Command::Forward { op },
        });
        Ok(ForwardCall { answer, decode })
    }
}

/// One ssh-agent request from a pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRequest {
    /// Names the request in [`ControlPlane::agent_reply`].
    pub id: u64,
    /// The pane it seems to come from; see
    /// [`Question::pane`](crate::agent_forward::Question::pane).
    pub pane: Option<phux_protocol::ResourceId>,
    /// The ssh-agent request, without its length prefix.
    pub message: Vec<u8>,
}

/// The ssh-agent request `event` carries, when it is one: what a connection
/// that called [`ControlPlane::agent_offer`] is sent for each pane request.
#[must_use]
pub fn agent_request(event: super::Event) -> Option<AgentRequest> {
    let super::Event::Frame(frame) = event else {
        return None;
    };
    request_in(&frame)
}

/// [`agent_request`] for a client that reads frames itself.
#[must_use]
pub fn request_in(frame: &FrameKind) -> Option<AgentRequest> {
    match frame {
        FrameKind::Event {
            terminal,
            event: phux_protocol::wire::frame::AgentEvent::SshAgentRequest { id, message },
            ..
        } => Some(AgentRequest {
            id: *id,
            pane: terminal.clone(),
            message: message.clone(),
        }),
        _ => None,
    }
}

fn opened(result: CommandResult) -> Result<u64, ForwardError> {
    match result {
        CommandResult::OkWith(CommandValue::ForwardOpened(id)) => Ok(id),
        other => Err(refused(other)),
    }
}

fn chunk(result: CommandResult) -> Result<ForwardChunk, ForwardError> {
    match result {
        CommandResult::OkWith(CommandValue::ForwardChunk(chunk)) => Ok(chunk),
        other => Err(refused(other)),
    }
}

fn done(result: CommandResult) -> Result<(), ForwardError> {
    match result {
        CommandResult::Ok => Ok(()),
        other => Err(refused(other)),
    }
}

fn refused(result: CommandResult) -> ForwardError {
    match result {
        CommandResult::Error { code, message } => ForwardError::Refused { code, message },
        other => ForwardError::Refused {
            code: ErrorCode::InternalError,
            message: format!("unexpected FORWARD reply: {other:?}"),
        },
    }
}

/// Bytes asked for per read and carried per write: the wire's ceiling.
#[allow(
    clippy::cast_possible_truncation,
    reason = "MAX_FORWARD_CHUNK_BYTES is 256 KiB, far below u32::MAX"
)]
const WINDOW: u32 = MAX_FORWARD_CHUNK_BYTES as u32;

async fn call<T>(
    client: &Client,
    send: impl FnOnce(&mut ControlPlane) -> Result<ForwardCall<T>, ForwardError>,
) -> Result<T, ForwardError> {
    client.with_control(send)?.await
}

/// Ask the server behind `client` to dial `addr:port`.
pub async fn open(client: &Client, addr: &str, port: u16) -> Result<u64, ForwardError> {
    call(client, |control| {
        control.forward_open(addr.to_owned(), port)
    })
    .await
}

/// Carry `local` through forward `id` until both directions have ended.
///
/// One pump runs per direction, and each side's end reaches the other as
/// a half-close; then the forward is closed. On any failure, a dropped
/// phux connection included, `local` is closed at once, so its peer reads
/// the end after whatever was already on its way.
// ponytail: one command round trip per chunk, so throughput is about
// 256 KiB per round trip each way; add a QUIC stream path if that hurts.
pub async fn relay(client: &Client, id: u64, mut local: TcpStream) -> Result<(), ForwardError> {
    let io = |err: std::io::Error| ForwardError::Local(err.to_string());
    let (mut rd, mut wr) = local.split();
    let down = async {
        loop {
            let chunk = call(client, |control| control.forward_read(id, WINDOW)).await?;
            wr.write_all(&chunk.bytes).await.map_err(io)?;
            if chunk.eof {
                return wr.shutdown().await.map_err(io);
            }
        }
    };
    let up = async {
        let mut bytes = vec![0; MAX_FORWARD_CHUNK_BYTES];
        loop {
            let read = rd.read(&mut bytes).await.map_err(io)?;
            let (chunk, fin) = (bytes[..read].to_vec(), read == 0);
            call(client, |control| control.forward_write(id, chunk, fin)).await?;
            if fin {
                return Ok(());
            }
        }
    };
    let ended = tokio::try_join!(down, up).map(|_| ());
    // Nobody waits for the answer; after a disconnect there is none to send.
    let _ = client.with_control(|control| control.forward_close(id));
    if ended.is_err() {
        // Closed, not reset. macOS drops a reset sent while bytes are still
        // queued for a slow reader (13 of 60 tries on loopback), and that
        // peer then waits forever; a close always arrives. Unread input
        // would turn the close into such a reset, so it is discarded first.
        // ponytail: a cut-off stream reads as an early end; reset once the
        // send queue is known empty if a peer needs to tell them apart.
        while matches!(local.try_read(&mut [0; 4096]), Ok(read) if read > 0) {}
    }
    ended
}
