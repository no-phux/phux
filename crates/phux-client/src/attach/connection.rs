//! UDS transport with length-prefixed frame I/O.
//!
//! Wraps a [`UnixStream`] split into owned read and write halves, so the
//! attach loop can `tokio::select!` over the server's frames concurrently
//! with stdin and signal sources. Both directions share the SPEC §5
//! framing: a four-byte big-endian length header followed by the type byte
//! and payload, capped at [`MAX_FRAME_LEN`].
//!
//! Decoding lives in [`phux_protocol::wire`] — this module owns only the
//! byte-level reassembly. Errors funnel into [`super::driver::AttachError`].

use std::io;
use std::path::{Path, PathBuf};

use bytes::{Buf, BytesMut};
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, MAX_FRAME_LEN, MoveResult, Scope, SpawnResult,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use super::driver::AttachError;
use super::quic;
pub use super::quic::{CertTrust, QuicDial};
use super::ws;
pub use super::ws::WsDial;

/// Number of bytes in the SPEC §5 length prefix.
const LENGTH_PREFIX: usize = 4;

/// How an attach should reach its server.
///
/// Either the always-local Unix domain socket, or a remote QUIC listener
/// (`phux-y8v6`, ADR-0007). Threaded through the attach loop so the reconnect
/// machinery dials the same way on each attempt.
#[derive(Debug, Clone)]
pub enum Dial {
    /// Connect over the Unix domain socket at this path.
    Uds(PathBuf),
    /// Dial a remote QUIC listener.
    Quic(QuicDial),
    /// Dial a remote WebSocket listener.
    Ws(WsDial),
}

impl Dial {
    /// A `Dial::Uds` borrowing-then-owning the given path. Lets the many
    /// `&Path` call sites build a dial target without restating the variant.
    #[must_use]
    pub fn uds(path: &Path) -> Self {
        Self::Uds(path.to_path_buf())
    }
}

/// A connected, owned transport split into framed read and write halves.
///
/// Construction performs the connect (UDS or QUIC); the two halves are
/// independent after that. The struct keeps them together so the simple "send +
/// recv on the same task" case is one type. Both transports carry the identical
/// SPEC §5 framing — the variant only changes the byte plumbing underneath.
///
/// # The COMMAND interleave contract
///
/// `docs/spec/L1.md` §5: "A `COMMAND` is asynchronous: the server MAY emit
/// other messages (including events relevant to the command's effect) before
/// `COMMAND_RESULT`. Clients MUST tolerate that ordering."
///
/// This is not a theoretical allowance — the reference server exercises it on
/// paths a client cannot avoid, and the frames it emits ahead of the ack are
/// **never re-sent**:
///
/// - `ATTACH_TERMINAL`: `handle_attach_terminal`
///   (`crates/phux-server/src/runtime/commands.rs`) pushes the authoritative
///   `TERMINAL_SNAPSHOT` "before the pump's first delta and before the Ok
///   reply". A caller that drops it has no opening screen and no geometry —
///   the exact defect that made every `phux rec` capture come back as a 0x0
///   grid.
/// - `GET_STATE` on a federation hub: `handle_get_state_federated` pushes an
///   uncorrelated `ERROR` frame per unreachable satellite, deliberately
///   ("observable degradation, not silence"), *before* returning the merged
///   snapshot the ack carries. A caller that drops it reports a silently
///   partial view of the fleet as if it were complete.
/// - Any command on a connection that also holds a subscription
///   (`ATTACH_TERMINAL`, `SUBSCRIBE_EVENTS`, an event registration): the
///   handler's internal `.await` points let the pane actor's `EVENT` and
///   `TERMINAL_OUTPUT` fanout reach this client's mailbox first.
///
/// Because a consumed frame is gone, `recv` alone cannot express a correct
/// request/response. [`Connection::request`] is the safe form: it hands back
/// the interleaved frames with the ack so a caller cannot lose them by
/// omission. Reach for raw [`Connection::send`] + [`Connection::recv`] only in
/// a full-duplex loop that already routes every frame kind (the attach
/// driver).
#[derive(Debug)]
pub struct Connection {
    reader: FrameReader,
    writer: FrameWriter,
    /// Pid of the peer process, read from the UDS peer credentials at
    /// connect time (`SO_PEERCRED` on Linux, `LOCAL_PEEREPID` on macOS).
    /// `None` on the remote transports (QUIC/WS have no such channel) and
    /// on platforms whose credentials carry no pid.
    peer_pid: Option<i32>,
}

/// Read half — pulls one [`FrameKind`] per call, over either transport.
#[derive(Debug)]
pub enum FrameReader {
    /// Unix-domain-socket read half with a streaming reassembly buffer.
    Uds(UdsReader),
    /// QUIC bidi-stream read half.
    Quic(QuicReader),
    /// WebSocket message read half.
    Ws(WsReader),
}

/// Write half — encodes one [`FrameKind`] per call, over either transport.
#[derive(Debug)]
pub enum FrameWriter {
    /// Unix-domain-socket write half.
    Uds(UdsWriter),
    /// QUIC bidi-stream write half.
    Quic(QuicWriter),
    /// WebSocket message write half.
    Ws(WsWriter),
}

/// UDS read half — reads chunks into a buffer and decodes whole frames.
#[derive(Debug)]
pub struct UdsReader {
    inner: OwnedReadHalf,
    /// Streaming receive buffer. The socket is read in chunks (not one
    /// `read_exact` per frame) so a single syscall can surface several
    /// queued frames at once; [`Self::recv`] and [`Self::try_recv`] decode
    /// complete frames out of the front and retain any partial tail for the
    /// next read. This buffering is what lets the attach loop coalesce a
    /// back-to-back output burst into one paint (phux-jhv8).
    buf: BytesMut,
}

/// UDS write half.
#[derive(Debug)]
pub struct UdsWriter {
    inner: OwnedWriteHalf,
    /// Reusable encode buffer.
    out: BytesMut,
}

/// QUIC read half.
///
/// Reassembles length-prefixed frames off the bidi stream, byte-for-byte the
/// same framing as the UDS path. quinn's `RecvStream` is a `tokio` `AsyncRead`,
/// so this reads in chunks into a buffer exactly like [`UdsReader`] — a single
/// read can surface several queued frames, which `try_recv` then drains so a
/// back-to-back burst still coalesces into one paint (phux-jhv8). The
/// cloned endpoint + connection are held so the I/O driver outlives the stream
/// and the connection can be closed cleanly on teardown.
#[derive(Debug)]
pub struct QuicReader {
    recv: quinn::RecvStream,
    buf: BytesMut,
    _endpoint: quinn::Endpoint,
    _connection: quinn::Connection,
}

/// QUIC write half. Holds the endpoint + connection for the same reasons as
/// [`QuicReader`]; its [`Drop`] issues a best-effort `CONNECTION_CLOSE`.
#[derive(Debug)]
pub struct QuicWriter {
    send: quinn::SendStream,
    /// Reusable encode buffer.
    out: BytesMut,
    endpoint: quinn::Endpoint,
    connection: quinn::Connection,
}

/// WebSocket read half: one binary message is one encoded phux frame.
#[derive(Debug)]
pub struct WsReader {
    inner: ws::WsReader,
}

/// WebSocket write half.
#[derive(Debug)]
pub struct WsWriter {
    inner: ws::WsWriter,
    out: BytesMut,
}

impl Drop for QuicWriter {
    fn drop(&mut self) {
        // Best-effort clean teardown: a `CONNECTION_CLOSE` lets the server reap
        // this consumer immediately instead of waiting out its 30s idle timeout.
        // The endpoint clone is still alive in this struct, so its driver can
        // transmit the frame. For a guaranteed flush (the reconnect probe) the
        // caller uses [`Connection::shutdown`], which also awaits `wait_idle`.
        self.connection.close(0u32.into(), b"phux: detach");
    }
}

impl Connection {
    /// Open the UDS at `socket` and return a framed connection.
    ///
    /// # Errors
    ///
    /// Surfaces `AttachError::Io` on any connect failure. The OS-level
    /// reason (ENOENT, ECONNREFUSED, EACCES, ...) is preserved in the
    /// inner `io::Error`.
    pub async fn connect(socket: &Path) -> Result<Self, AttachError> {
        let stream = UnixStream::connect(socket).await.map_err(AttachError::Io)?;
        // Read the peer credentials while the stream is still whole: the
        // split halves do not expose them, and the pid is free to capture
        // here. Best-effort — a platform without a pid in its credentials
        // yields `None`, never an error.
        let peer_pid = stream.peer_cred().ok().and_then(|cred| cred.pid());
        let (read, write) = stream.into_split();
        Ok(Self {
            reader: FrameReader::Uds(UdsReader {
                inner: read,
                buf: BytesMut::with_capacity(8192),
            }),
            writer: FrameWriter::Uds(UdsWriter {
                inner: write,
                out: BytesMut::with_capacity(4096),
            }),
            peer_pid,
        })
    }

    /// Dial a remote QUIC listener and return a framed connection.
    ///
    /// Establishes the TLS 1.3 handshake (phux ALPN), opens one bidirectional
    /// stream, and writes the bearer-token preamble when [`QuicDial::token`] is
    /// set, all before returning — so the first [`Self::send`]/[`Self::recv`]
    /// sees a stream the server is already reading phux frames off.
    ///
    /// # Errors
    ///
    /// Surfaces [`AttachError::Connect`] on any handshake, certificate, or
    /// preamble failure (the address, the pin, or the token).
    pub async fn connect_quic(dial: &QuicDial) -> Result<Self, AttachError> {
        let (endpoint, connection, send, recv) = quic::dial(dial).await?;
        Ok(Self {
            reader: FrameReader::Quic(QuicReader {
                recv,
                buf: BytesMut::with_capacity(8192),
                _endpoint: endpoint.clone(),
                _connection: connection.clone(),
            }),
            writer: FrameWriter::Quic(QuicWriter {
                send,
                out: BytesMut::with_capacity(4096),
                endpoint,
                connection,
            }),
            peer_pid: None,
        })
    }

    /// Dial a remote WebSocket listener and return a framed connection.
    ///
    /// The server uses one binary WebSocket message per encoded phux frame.
    /// This is the native TCP fallback for networks where UDP/QUIC is blocked.
    pub async fn connect_ws(dial: &WsDial) -> Result<Self, AttachError> {
        let ws = ws::dial(dial).await?;
        let (tx, rx) = futures_util::StreamExt::split(ws);
        Ok(Self {
            reader: FrameReader::Ws(WsReader {
                inner: ws::WsReader { rx },
            }),
            writer: FrameWriter::Ws(WsWriter {
                inner: ws::WsWriter { tx },
                out: BytesMut::with_capacity(4096),
            }),
            peer_pid: None,
        })
    }

    /// Pid of the peer process on a UDS connection, captured from the
    /// socket's peer credentials at connect time — for a client dialing the
    /// server socket, that is the server's pid. `None` on the remote
    /// transports and on platforms whose peer credentials carry no pid.
    ///
    /// This is an OS fact about the socket, not a wire exchange: the server
    /// neither knows nor participates, so it works against any server
    /// version.
    #[must_use]
    pub const fn peer_pid(&self) -> Option<i32> {
        self.peer_pid
    }

    /// Close the connection cleanly, awaiting transmission of the close frame.
    ///
    /// For QUIC this issues a `CONNECTION_CLOSE` and awaits `wait_idle`, so the
    /// server reaps the consumer at once rather than at its idle timeout — used
    /// by the reconnect probe, which would otherwise leave a phantom connection
    /// per attempt. For UDS this is a no-op (dropping the socket halves is a
    /// clean close already). [`QuicWriter`]'s [`Drop`] is the best-effort
    /// backstop on paths that cannot await.
    pub async fn shutdown(self) {
        if let FrameWriter::Quic(writer) = &self.writer {
            writer.connection.close(0u32.into(), b"phux: detach");
            writer.endpoint.wait_idle().await;
        }
    }

    /// Connect over whichever transport `dial` names.
    ///
    /// # Errors
    ///
    /// Propagates [`Self::connect`] / [`Self::connect_quic`] errors.
    pub async fn connect_dial(dial: &Dial) -> Result<Self, AttachError> {
        match dial {
            Dial::Uds(path) => Self::connect(path).await,
            Dial::Quic(quic) => Self::connect_quic(quic).await,
            Dial::Ws(ws) => Self::connect_ws(ws).await,
        }
    }

    /// Build a `Connection` from an already-connected [`UnixStream`].
    ///
    /// Test-only seam: lets the dispatcher unit tests drive a real framed
    /// transport over an in-process `UnixStream::pair` without a server
    /// socket on disk. Mirrors the wiring [`Self::connect`] does after the
    /// connect resolves.
    #[cfg(test)]
    pub(crate) fn from_stream(stream: UnixStream) -> Self {
        let peer_pid = stream.peer_cred().ok().and_then(|cred| cred.pid());
        let (read, write) = stream.into_split();
        Self {
            reader: FrameReader::Uds(UdsReader {
                inner: read,
                buf: BytesMut::with_capacity(8192),
            }),
            writer: FrameWriter::Uds(UdsWriter {
                inner: write,
                out: BytesMut::with_capacity(4096),
            }),
            peer_pid,
        }
    }

    /// Encode `frame` and write it to the server.
    pub async fn send(&mut self, frame: &FrameKind) -> Result<(), AttachError> {
        self.writer.send(frame).await
    }

    /// Read the next frame from the server.
    pub async fn recv(&mut self) -> Result<FrameKind, AttachError> {
        self.reader.recv().await
    }

    /// Pull a frame that is *already available* without awaiting the socket.
    ///
    /// Returns `Ok(Some(frame))` when a complete frame can be decoded from
    /// data already buffered (or readable without blocking), `Ok(None)` when
    /// the next frame is not yet fully here. Lets the attach loop drain a
    /// back-to-back burst after the first `recv` so the whole run coalesces
    /// into a single paint (phux-jhv8).
    pub fn try_recv(&mut self) -> Result<Option<FrameKind>, AttachError> {
        self.reader.try_recv()
    }

    /// Send one `COMMAND` and wait for its reply, keeping every frame the
    /// server interleaved ahead of it.
    ///
    /// This is the only correct request/response primitive on a `Connection`
    /// — see the interleave contract on the type. The hand-rolled
    /// `loop { match recv() { mine => return, _ => {} } }` it replaces is
    /// *silently lossy*: the frames it drops are already consumed off the
    /// socket and nothing re-sends them.
    ///
    /// # Why the frames come back in the return value
    ///
    /// Three shapes were possible; only one makes the loss impossible rather
    /// than merely discouraged.
    ///
    /// - A `FnMut(FrameKind)` callback would let the discarding caller write
    ///   `|_| {}` — a shorter, more innocent-looking spelling of the very bug
    ///   this exists to prevent. It is also synchronous, so a caller that
    ///   wants to *await* on each frame (feed a pump, write a cast event)
    ///   cannot use it.
    /// - A caller-supplied `&mut Vec<FrameKind>` sink has the same hole:
    ///   `&mut Vec::new()` reads as ordinary setup, and a `&mut` out-param
    ///   cannot be `#[must_use]`.
    /// - Returning them inside [`Reply`] — which also owns the
    ///   [`CommandResult`] — means the ack is *unreachable* without passing
    ///   through a named accessor, and the only way to drop the frames is
    ///   [`Reply::into_result_ignoring_interleaved`], whose name is the audit
    ///   trail. `grep` for it and you have the complete list of places that
    ///   claim the server cannot interleave; each one owes a citation of the
    ///   server handler that makes the claim true.
    ///
    /// The `Vec` is allocated per call and is empty in the common case, which
    /// is the right trade for a control-plane round trip: these are one per
    /// CLI verb, not per keystroke.
    ///
    /// # Terminating frames
    ///
    /// Either a `COMMAND_RESULT` carrying `request_id`, or an `ERROR` frame
    /// *correlated* to it (`proto.md` §9: `request_id` is "present if the
    /// error is associated with a COMMAND"), which is normalized to
    /// [`CommandResult::Error`]. Honouring the correlated `ERROR` is not
    /// cosmetic — every hand-rolled loop in the workspace waited on
    /// `COMMAND_RESULT` alone and would hang forever against a peer that
    /// answers the way L1 §5 permits (`ERROR { code: INVALID_COMMAND }` for a
    /// command the server does not implement). A hub already normalizes that
    /// shape on the return leg from a satellite
    /// (`crates/phux-server/src/hub/relay.rs`, `handle_inbound`); this does
    /// the same for a direct peer.
    ///
    /// An *uncorrelated* `ERROR` (`request_id: None`) is not terminal — it is
    /// the federation degradation notice, and it lands in
    /// [`Reply::interleaved`] like any other pushed frame.
    ///
    /// # Errors
    ///
    /// Propagates transport and decode failures from [`Self::send`] /
    /// [`Self::recv`]; a server that closes without replying surfaces as
    /// [`AttachError::Disconnected`].
    pub async fn request(
        &mut self,
        request_id: u32,
        command: Command,
    ) -> Result<Reply, AttachError> {
        self.send(&FrameKind::Command {
            request_id,
            command,
        })
        .await?;
        let mut interleaved = Vec::new();
        let answer = self
            .await_answer(request_id, &mut interleaved, |frame| match frame {
                FrameKind::CommandResult { request_id, result } => {
                    Some((*request_id, result.clone()))
                }
                _ => None,
            })
            .await?;
        // `CommandResult` already has an `Error` variant with exactly the
        // `ERROR` frame's payload, so this pair folds the refusal into its
        // reply type rather than surfacing an `Answer` — which is why the
        // public signature is unchanged from before the engine existed.
        let result = answer.unwrap_or_else(|refusal| CommandResult::Error {
            code: refusal.code,
            message: refusal.message,
        });
        Ok(Reply {
            result,
            interleaved,
        })
    }

    /// Send one `GET_METADATA` and wait for its `METADATA_VALUE`, keeping
    /// every frame the peer interleaved ahead of it.
    ///
    /// The L3 twin of [`Self::request`]. A refusal is an [`Answer::Err`]
    /// rather than a `None` value: "the key is unset" and "the peer will not
    /// serve this scope" are different facts, and collapsing them is how
    /// `phux new` came to report "server did not register session" for a
    /// server that had refused the read outright.
    ///
    /// # Errors
    ///
    /// Propagates transport and decode failures from [`Self::send`] /
    /// [`Self::recv`].
    pub async fn request_metadata(
        &mut self,
        request_id: u32,
        scope: Scope,
        key: String,
    ) -> Result<Reply<Answer<Option<Vec<u8>>>>, AttachError> {
        self.send(&FrameKind::GetMetadata {
            request_id,
            scope,
            key,
        })
        .await?;
        let mut interleaved = Vec::new();
        let result = self
            .await_answer(request_id, &mut interleaved, |frame| match frame {
                FrameKind::MetadataValue { request_id, value } => {
                    Some((*request_id, value.clone()))
                }
                _ => None,
            })
            .await?;
        Ok(Reply {
            result,
            interleaved,
        })
    }

    /// Send one `SPAWN_TERMINAL` and wait for its `TERMINAL_SPAWNED`, keeping
    /// every frame the peer interleaved ahead of it.
    ///
    /// The correlation id is read out of `frame` rather than passed alongside
    /// it, so the id waited on and the id sent cannot disagree.
    ///
    /// A satellite MAY answer a relayed spawn with a correlated `ERROR`
    /// instead of `TERMINAL_SPAWNED` — a hub already normalizes exactly that
    /// shape on the return leg (`crates/phux-server/src/hub/relay.rs`,
    /// `handle_inbound`: "a satellite MAY answer a relayed spawn with a
    /// generic correlated ERROR"). This is the same normalization for a
    /// direct peer, and without it `phux spawn --satellite` waits forever for
    /// a frame that is never coming.
    ///
    /// # Errors
    ///
    /// [`AttachError::Protocol`] when `frame` is not a `SPAWN_TERMINAL`;
    /// otherwise transport and decode failures from [`Self::send`] /
    /// [`Self::recv`].
    pub async fn request_spawn(
        &mut self,
        frame: &FrameKind,
    ) -> Result<Reply<Answer<SpawnResult>>, AttachError> {
        let FrameKind::SpawnTerminal { request_id, .. } = frame else {
            return Err(AttachError::Protocol(format!(
                "request_spawn needs a SPAWN_TERMINAL frame, got {frame:?}",
            )));
        };
        let request_id = *request_id;
        self.send(frame).await?;
        let mut interleaved = Vec::new();
        let result = self
            .await_answer(request_id, &mut interleaved, |frame| match frame {
                FrameKind::TerminalSpawned { request_id, result } => {
                    Some((*request_id, result.clone()))
                }
                _ => None,
            })
            .await?;
        Ok(Reply {
            result,
            interleaved,
        })
    }

    /// Send one `MOVE_TERMINAL` and wait for its `TERMINAL_MOVED`, keeping
    /// every frame the peer interleaved ahead of it (ADR-0056).
    ///
    /// The correlation id is read out of `frame` rather than passed
    /// alongside it, so the id waited on and the id sent cannot disagree —
    /// the same contract as [`Self::request_spawn`].
    ///
    /// # Errors
    ///
    /// [`AttachError::Protocol`] when `frame` is not a `MOVE_TERMINAL`;
    /// otherwise transport and decode failures from [`Self::send`] /
    /// [`Self::recv`].
    pub async fn request_move(
        &mut self,
        frame: &FrameKind,
    ) -> Result<Reply<Answer<MoveResult>>, AttachError> {
        let FrameKind::MoveTerminal { request_id, .. } = frame else {
            return Err(AttachError::Protocol(format!(
                "request_move needs a MOVE_TERMINAL frame, got {frame:?}",
            )));
        };
        let request_id = *request_id;
        self.send(frame).await?;
        let mut interleaved = Vec::new();
        let result = self
            .await_answer(request_id, &mut interleaved, |frame| match frame {
                FrameKind::TerminalMoved { request_id, result } => {
                    Some((*request_id, result.clone()))
                }
                _ => None,
            })
            .await?;
        Ok(Reply {
            result,
            interleaved,
        })
    }

    /// The workspace's only correlation loop.
    ///
    /// Reads frames until the peer answers `request_id`, pushing every frame
    /// that is not that answer onto `interleaved`. `recognize` reports the
    /// request id and payload of a frame that is *this pair's* reply frame,
    /// and `None` for anything else.
    ///
    /// # Why one engine and typed wrappers, rather than a public generic
    ///
    /// The catalogued bug is a caller that waits on one frame variant and
    /// drops the rest, so a peer answering with a correlated `ERROR`
    /// (`proto.md` §9) wedges it until the transport dies. Two shapes could
    /// have fixed it; only one makes the broken version unwriteable.
    ///
    /// - **A public generic `request_frame(id, frame, predicate)`.** The
    ///   caller supplies the correlation, which means the caller can get the
    ///   correlation wrong — and the specific way every site here got it
    ///   wrong was *omitting the `ERROR` arm*. Handing that arm back to the
    ///   author who already forgot it once is not an abstraction, it is a
    ///   rename.
    /// - **This: one private engine, one public method per pair.** The
    ///   `ERROR` arm is not the pair's business at all; adding a pair means
    ///   naming its reply frame, and the author cannot forget a rule they are
    ///   never asked to state. Adding a pair *without* the engine means
    ///   writing a visible `loop { recv() }` next to three methods that
    ///   don't — reviewable in the diff, which the discarding version never
    ///   was.
    ///
    /// `recognize` takes `&FrameKind` and clones the payload out rather than
    /// consuming the frame. That is the load-bearing detail: a closure taking
    /// the frame by value could not hand back the ones it does not recognize,
    /// and "the frame it did not recognize is now gone" is the entire bug
    /// class. The cost is one clone of a control-plane payload per round
    /// trip — these are one per CLI verb, not per keystroke.
    ///
    /// # Errors
    ///
    /// Propagates transport and decode failures from [`Self::recv`]; a peer
    /// that closes without answering surfaces as
    /// [`AttachError::Disconnected`].
    async fn await_answer<T>(
        &mut self,
        request_id: u32,
        interleaved: &mut Vec<FrameKind>,
        recognize: impl Fn(&FrameKind) -> Option<(u32, T)>,
    ) -> Result<Answer<T>, AttachError> {
        loop {
            let frame = self.recv().await?;
            if let Some((got, value)) = recognize(&frame)
                && got == request_id
            {
                return Ok(Ok(value));
            }
            // `proto.md` §9: an `ERROR` carrying a `request_id` is *that*
            // request's answer. An uncorrelated one (`request_id: None`) is
            // not — on this wire it is the hub's per-satellite degradation
            // notice — so it falls through to `interleaved` like any other
            // pushed frame.
            if let FrameKind::Error {
                request_id: Some(got),
                code,
                message,
            } = &frame
                && *got == request_id
            {
                return Ok(Err(Refusal {
                    code: *code,
                    message: message.clone(),
                }));
            }
            interleaved.push(frame);
        }
    }
}

/// The peer's correlated `ERROR` answer to one request (`proto.md` §9).
///
/// Distinct from an uncorrelated `ERROR`, which answers nothing and reaches
/// the caller through [`Reply::interleaved`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct Refusal {
    /// The typed code the peer refused with.
    pub code: ErrorCode,
    /// The peer's human-readable explanation.
    pub message: String,
}

/// A peer's answer to one correlated request: the reply payload, or the
/// [`Refusal`] it answered with instead.
///
/// A plain `Result` on purpose. Every caller of a correlated round trip has
/// to decide what a refusal means for it, and `Result` is the one shape the
/// language will not let them skip: there is no accessor that yields the
/// value while leaving the refusal unexamined, `?` and `map_err` compose the
/// refusal into the caller's own error type, and [`Refusal`] is an
/// `std::error::Error` so `thiserror` wrappers take it directly.
pub type Answer<T> = Result<T, Refusal>;

/// The complete outcome of one correlated round trip: the answer, plus every
/// frame the peer pushed ahead of it.
///
/// Deliberately opaque. The fields are reachable only through methods, so a
/// caller cannot destructure away the half it did not think about — the whole
/// point of the type (see [`Connection::request`] for the design rationale and
/// the interleave contract on [`Connection`] for why the frames matter).
///
/// `T` is whatever the pair's answer is: [`CommandResult`] for
/// [`Connection::request`] (which folds a refusal into `CommandResult::Error`
/// because that variant already carries exactly the `ERROR` payload), and
/// [`Answer`] for the pairs whose reply type has nowhere to put one.
#[derive(Debug)]
#[must_use = "the reply carries frames the server will never re-send; dropping \
              it loses them"]
pub struct Reply<T = CommandResult> {
    /// The pair's answer: its reply-frame payload, or the peer's refusal.
    result: T,
    /// Frames observed while waiting, in arrival order.
    interleaved: Vec<FrameKind>,
}

impl<T> Reply<T> {
    /// The answer and the interleaved frames, in arrival order.
    ///
    /// The default way to consume a reply: binding both halves is what makes
    /// forgetting one a visible act rather than an omission.
    #[must_use]
    pub fn into_parts(self) -> (T, Vec<FrameKind>) {
        (self.result, self.interleaved)
    }

    /// Borrow the answer without consuming the reply.
    #[must_use]
    pub const fn result(&self) -> &T {
        &self.result
    }

    /// Borrow the frames the server interleaved ahead of the ack.
    #[must_use]
    pub fn interleaved(&self) -> &[FrameKind] {
        &self.interleaved
    }

    /// Take the answer and drop the interleaved frames on the floor.
    ///
    /// **Only correct when the server provably pushes nothing to this
    /// connection's mailbox before the ack** — i.e. the handler emits no frame
    /// of its own *and* the connection holds no subscription that could fan
    /// out onto it. Cite the server handler in a comment at every call site;
    /// this method name is how the next audit finds you.
    ///
    /// A non-empty drop is logged at `warn`, so the failure mode is at worst
    /// noisy rather than silent. The dropped frames are still gone: the log is
    /// a diagnostic, not a recovery.
    #[must_use]
    pub fn into_result_ignoring_interleaved(self) -> T {
        if !self.interleaved.is_empty() {
            tracing::warn!(
                dropped = self.interleaved.len(),
                frames = ?self.interleaved,
                "correlated reply discarded frames the server interleaved ahead \
                 of the answer; the server will not re-send them",
            );
        }
        self.result
    }
}

impl FrameWriter {
    /// Encode `frame` and write it to the server over whichever transport.
    pub async fn send(&mut self, frame: &FrameKind) -> Result<(), AttachError> {
        match self {
            Self::Uds(w) => w.send(frame).await,
            Self::Quic(w) => w.send(frame).await,
            Self::Ws(w) => w.send(frame).await,
        }
    }
}

impl FrameReader {
    /// Read one complete frame off the wire over whichever transport.
    pub async fn recv(&mut self) -> Result<FrameKind, AttachError> {
        match self {
            Self::Uds(r) => r.recv().await,
            Self::Quic(r) => r.recv().await,
            Self::Ws(r) => r.recv().await,
        }
    }

    /// Non-blocking sibling of [`Self::recv`]: decode a frame only if one is
    /// already buffered (or, for UDS, becomes readable without blocking).
    ///
    /// Returns `Ok(None)` when the next frame is not yet fully available. Both
    /// transports drain a coalesced burst out of their receive buffer; the UDS
    /// path additionally tops up from the socket without blocking (quinn exposes
    /// no sync ready-check, so QUIC drains buffered bytes only).
    pub fn try_recv(&mut self) -> Result<Option<FrameKind>, AttachError> {
        match self {
            Self::Uds(r) => r.try_recv(),
            Self::Quic(r) => r.try_recv(),
            Self::Ws(_) => Ok(None),
        }
    }
}

impl UdsWriter {
    /// Encode `frame` into the internal buffer and flush it to the socket.
    async fn send(&mut self, frame: &FrameKind) -> Result<(), AttachError> {
        self.out.clear();
        frame.encode(&mut self.out);
        self.inner
            .write_all(&self.out)
            .await
            .map_err(AttachError::Io)?;
        // `flush` on a `UnixStream` half is a no-op, but harmless and explicit.
        self.inner.flush().await.map_err(AttachError::Io)?;
        Ok(())
    }
}

impl UdsReader {
    /// Read one complete frame off the wire.
    ///
    /// Returns [`AttachError::Disconnected`] on a clean EOF — the SPEC §5
    /// length prefix is the only legal cut point. Drains a complete frame
    /// from the receive buffer when one is already buffered; otherwise reads more
    /// bytes (awaiting the socket) until a full frame lands.
    async fn recv(&mut self) -> Result<FrameKind, AttachError> {
        loop {
            if let Some(frame) = decode_buffered(&mut self.buf)? {
                return Ok(frame);
            }
            // No complete frame buffered — pull more bytes. A read of zero is
            // a clean EOF; mid-frame that is a truncated stream, but the only
            // SPEC §5 cut point is a frame boundary, which `decode_buffered`
            // already returned above.
            let n = self
                .inner
                .read_buf(&mut self.buf)
                .await
                .map_err(AttachError::Io)?;
            if n == 0 {
                return Err(AttachError::Disconnected);
            }
        }
    }

    /// Non-blocking sibling of [`Self::recv`]: decode a frame only if one is
    /// already buffered or becomes readable without blocking.
    fn try_recv(&mut self) -> Result<Option<FrameKind>, AttachError> {
        // A frame may already be sitting in the buffer behind the one `recv`
        // just returned; hand it over before touching the socket.
        if let Some(frame) = decode_buffered(&mut self.buf)? {
            return Ok(Some(frame));
        }
        // Top up from the socket without blocking. `WouldBlock` just means
        // nothing more is queued right now.
        match self.inner.try_read_buf(&mut self.buf) {
            Ok(0) => return Err(AttachError::Disconnected),
            Ok(_) => {}
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(err) => return Err(AttachError::Io(err)),
        }
        decode_buffered(&mut self.buf)
    }
}

impl QuicWriter {
    /// Encode `frame` and write it to the QUIC stream. quinn's `write_all`
    /// queues the bytes for ordered, reliable delivery — no separate flush.
    async fn send(&mut self, frame: &FrameKind) -> Result<(), AttachError> {
        self.out.clear();
        frame.encode(&mut self.out);
        self.send
            .write_all(&self.out)
            .await
            .map_err(|err| AttachError::Io(io::Error::other(err)))?;
        Ok(())
    }
}

impl QuicReader {
    /// Read one complete frame off the QUIC stream. quinn's `RecvStream` is a
    /// `tokio` `AsyncRead`, so this is the same chunk-and-reassemble loop as the
    /// UDS path: a clean stream finish at a frame boundary surfaces as a read of
    /// zero ([`AttachError::Disconnected`]).
    async fn recv(&mut self) -> Result<FrameKind, AttachError> {
        loop {
            if let Some(frame) = decode_buffered(&mut self.buf)? {
                return Ok(frame);
            }
            let n = self
                .recv
                .read_buf(&mut self.buf)
                .await
                .map_err(AttachError::Io)?;
            if n == 0 {
                return Err(AttachError::Disconnected);
            }
        }
    }

    /// Drain a frame already sitting in the buffer behind the one [`Self::recv`]
    /// just returned. quinn has no sync ready-check, so this never reads from
    /// the stream — it only peels off bytes a prior `recv` over-read.
    fn try_recv(&mut self) -> Result<Option<FrameKind>, AttachError> {
        decode_buffered(&mut self.buf)
    }
}

impl WsWriter {
    async fn send(&mut self, frame: &FrameKind) -> Result<(), AttachError> {
        self.out.clear();
        frame.encode(&mut self.out);
        self.inner.send(&self.out).await.map_err(AttachError::from)
    }
}

impl WsReader {
    async fn recv(&mut self) -> Result<FrameKind, AttachError> {
        let Some(frame) = self.inner.recv_message().await? else {
            return Err(AttachError::Disconnected);
        };
        if frame.len() < LENGTH_PREFIX || frame.len() > LENGTH_PREFIX + MAX_FRAME_LEN as usize {
            return Err(AttachError::Protocol(format!(
                "server sent WebSocket frame with out-of-range length {}",
                frame.len()
            )));
        }
        let (decoded, rest) = FrameKind::decode(&frame).map_err(|err| {
            AttachError::Protocol(format!("server sent undecodable frame: {err:?}"))
        })?;
        if !rest.is_empty() {
            return Err(AttachError::Protocol(
                "server sent trailing bytes after WebSocket frame".to_owned(),
            ));
        }
        Ok(decoded)
    }
}

/// Decode and consume one complete frame from the front of `buf`.
///
/// Returns `Ok(None)` when fewer than a full frame's bytes are buffered (the
/// length prefix is missing, or the body has not all arrived). The decoded
/// frame's bytes are dropped from the front; any trailing partial frame stays
/// for the next read.
fn decode_buffered(buf: &mut BytesMut) -> Result<Option<FrameKind>, AttachError> {
    if buf.len() < LENGTH_PREFIX {
        return Ok(None);
    }
    let mut header = [0u8; LENGTH_PREFIX];
    header.copy_from_slice(&buf[..LENGTH_PREFIX]);
    let body_len = u32::from_be_bytes(header);
    if !(1..=MAX_FRAME_LEN).contains(&body_len) {
        return Err(AttachError::Protocol(format!(
            "server sent frame with out-of-range length {body_len}",
        )));
    }
    let frame_len = LENGTH_PREFIX + body_len as usize;
    if buf.len() < frame_len {
        // Body still in flight — wait for more bytes.
        return Ok(None);
    }
    let (frame, _rest) = FrameKind::decode(&buf[..frame_len])
        .map_err(|err| AttachError::Protocol(format!("server sent undecodable frame: {err:?}")))?;
    buf.advance(frame_len);
    Ok(Some(frame))
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::*;
    use phux_protocol::PROTOCOL_VERSION;

    #[test]
    fn writer_buffer_starts_empty() {
        // The buffer must be cleared before each encode so frames don't
        // concatenate across calls. We can't easily construct a `FrameWriter`
        // without a real `UnixStream`, so this assertion guards the
        // pre-clear invariant indirectly via the bytes buffer length.
        let buf = BytesMut::with_capacity(64);
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn frame_encode_decode_roundtrip_matches_wire_path() {
        // Sanity: confirm the encoder produces something the decoder can
        // read, using the same SPEC §5 framing the `FrameReader` will see.
        // If the protocol crate's encoder ever drifts, this catches it
        // before the attach loop's I/O path notices in the field.
        let frame = FrameKind::Hello {
            client_name: "phux-client/test".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: phux_protocol::ClientCapabilities::default(),
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        let (decoded, rest) = FrameKind::decode(&buf).expect("roundtrip");
        assert_eq!(decoded, frame);
        assert!(rest.is_empty());
    }

    fn framed(seq: u64) -> BytesMut {
        // A small, cheap-to-build frame with a distinguishing field so the
        // burst-decode test can assert ordering.
        let frame = FrameKind::FrameAck {
            terminal_id: phux_protocol::ids::TerminalId::Local { id: 1 },
            seq,
        };
        let mut buf = BytesMut::new();
        frame.encode(&mut buf);
        buf
    }

    #[test]
    fn decode_buffered_drains_back_to_back_frames_in_order() {
        // The coalescing path (phux-jhv8) relies on a single socket read
        // surfacing several queued frames: decode_buffered must peel them off
        // the front one at a time, in order, leaving nothing behind.
        let mut buf = BytesMut::new();
        for seq in 1..=3 {
            buf.extend_from_slice(&framed(seq));
        }
        let mut seqs = Vec::new();
        while let Some(FrameKind::FrameAck { seq, .. }) = decode_buffered(&mut buf).expect("decode")
        {
            seqs.push(seq);
        }
        assert_eq!(seqs, vec![1, 2, 3]);
        assert!(buf.is_empty(), "fully consumed buffer");
    }

    #[test]
    fn decode_buffered_holds_partial_frame() {
        // A frame split across reads must not decode early: the prefix says
        // more bytes are coming, so decode_buffered returns None and retains
        // the partial bytes until the rest arrives.
        let whole = framed(7);
        let cut = whole.len() - 2;
        let mut buf = BytesMut::from(&whole[..cut]);
        assert!(
            decode_buffered(&mut buf).expect("partial").is_none(),
            "incomplete frame yields None"
        );
        assert_eq!(buf.len(), cut, "partial bytes retained");
        // Deliver the tail; now it decodes and the buffer drains.
        buf.extend_from_slice(&whole[cut..]);
        let frame = decode_buffered(&mut buf).expect("complete");
        assert!(matches!(frame, Some(FrameKind::FrameAck { seq: 7, .. })));
        assert!(buf.is_empty());
    }

    #[test]
    fn decode_buffered_empty_is_none() {
        let mut buf = BytesMut::new();
        assert!(decode_buffered(&mut buf).expect("empty").is_none());
    }

    // --- Connection::request -------------------------------------------
    //
    // `Connection` holds `!Send` transport halves, so the scripted server
    // side runs on a `LocalSet` rather than `tokio::spawn`.

    use phux_protocol::ids::TerminalId;
    use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode};
    use tokio::net::UnixStream;

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        tokio::task::LocalSet::new().block_on(&rt, fut)
    }

    /// Drive one `request` against a server that replies with `script` in
    /// order, and return the resulting reply.
    fn request_against(script: Vec<FrameKind>) -> Reply {
        block_on(async {
            let (client_stream, server_stream) = UnixStream::pair().expect("pair");
            let mut client = Connection::from_stream(client_stream);
            let mut server = Connection::from_stream(server_stream);
            tokio::task::spawn_local(async move {
                // Consume the COMMAND, then play the script back verbatim.
                server.recv().await.expect("COMMAND");
                for frame in &script {
                    server.send(frame).await.expect("scripted frame");
                }
            });
            client
                .request(
                    7,
                    Command::GetState {
                        scope: phux_protocol::wire::frame::StateScope::Server,
                    },
                )
                .await
                .expect("reply")
        })
    }

    fn ack(request_id: u32) -> FrameKind {
        FrameKind::CommandResult {
            request_id,
            result: CommandResult::Ok,
        }
    }

    fn snapshot() -> FrameKind {
        FrameKind::TerminalSnapshot {
            terminal_id: TerminalId::local(1),
            cols: 120,
            rows: 40,
            vt_replay_bytes: b"opening screen".to_vec(),
            scrollback_bytes: None,
        }
    }

    #[test]
    fn pre_ack_snapshot_is_returned_instead_of_discarded() {
        // `handle_attach_terminal` pushes TERMINAL_SNAPSHOT before the Ok
        // reply and never re-sends it. The hand-rolled wait loop this API
        // replaces dropped it, which is what made every `phux rec` capture a
        // 0x0 grid with no opening screen.
        let reply = request_against(vec![snapshot(), ack(7)]);
        assert!(matches!(reply.result(), CommandResult::Ok));
        assert!(
            matches!(reply.interleaved(), [FrameKind::TerminalSnapshot { .. }]),
            "the pre-ack snapshot must reach the caller, got {:?}",
            reply.interleaved()
        );
    }

    #[test]
    fn satellite_degradation_error_is_not_swallowed_by_get_state() {
        // `handle_get_state_federated` emits one uncorrelated ERROR per
        // unreachable satellite *before* the merged snapshot's ack, on
        // purpose ("observable degradation, not silence"). Swallowing it
        // turns a partial fleet view into a confidently complete-looking one.
        let reply = request_against(vec![
            FrameKind::Error {
                request_id: None,
                code: ErrorCode::UnsupportedSatelliteRoute,
                message: "no satellite route to build-box".to_owned(),
            },
            ack(7),
        ]);
        assert!(
            matches!(reply.result(), CommandResult::Ok),
            "an uncorrelated ERROR is degradation, not the command's answer"
        );
        match reply.interleaved() {
            [FrameKind::Error { message, .. }] => {
                assert_eq!(message, "no satellite route to build-box");
            }
            other => panic!("degradation notice must survive, got {other:?}"),
        }
    }

    #[test]
    fn correlated_error_answers_the_request_instead_of_hanging_forever() {
        // proto.md §9: ERROR.request_id is "present if the error is
        // associated with a COMMAND", and L1 §5 requires an unimplemented
        // command to be refused with ERROR { INVALID_COMMAND }. Every
        // hand-rolled loop waited on COMMAND_RESULT alone, so this shape
        // would wedge the caller until the transport died.
        let reply = request_against(vec![FrameKind::Error {
            request_id: Some(7),
            code: ErrorCode::InvalidCommand,
            message: "command not supported by this server".to_owned(),
        }]);
        match reply.result() {
            CommandResult::Error { code, message } => {
                assert_eq!(*code, ErrorCode::InvalidCommand);
                assert_eq!(message, "command not supported by this server");
            }
            other => panic!("a correlated ERROR is this command's answer, got {other:?}"),
        }
        assert!(reply.interleaved().is_empty());
    }

    #[test]
    fn another_requests_ack_is_kept_not_consumed() {
        // Pipelined requests share a connection: a COMMAND_RESULT for a
        // different request_id belongs to someone else's correlation and must
        // survive for them, not vanish into this wait.
        let reply = request_against(vec![ack(99), ack(7)]);
        assert!(
            matches!(
                reply.interleaved(),
                [FrameKind::CommandResult { request_id: 99, .. }]
            ),
            "got {:?}",
            reply.interleaved()
        );
    }

    #[test]
    fn frames_are_returned_in_arrival_order() {
        let bell = FrameKind::Bell {
            terminal_id: TerminalId::local(1),
        };
        let reply = request_against(vec![snapshot(), bell, ack(7)]);
        assert!(
            matches!(
                reply.interleaved(),
                [FrameKind::TerminalSnapshot { .. }, FrameKind::Bell { .. }]
            ),
            "got {:?}",
            reply.interleaved()
        );
    }

    // --- the non-COMMAND pairs (phux-h5hj.12) --------------------------
    //
    // `GET_METADATA` and `SPAWN_TERMINAL` are their own request frames with
    // their own `request_id`, so `Connection::request` never covered them and
    // each grew a hand-rolled wait that matched one reply variant and dropped
    // the rest. Every test below fails by *hanging* against that version,
    // which is exactly how the bug presented in the field — so each one is
    // capped by a timeout rather than left to nextest's slow-test reaper.

    /// Long enough that a loaded machine cannot trip it, short enough that a
    /// genuine wedge fails the run instead of hanging it.
    const WEDGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

    /// Spawn the scripted server half and hand back the client half.
    ///
    /// The script is played verbatim once the client's request frame arrives,
    /// exactly as [`request_against`] does for `COMMAND`.
    fn scripted(script: Vec<FrameKind>) -> Connection {
        let (client_stream, server_stream) = UnixStream::pair().expect("pair");
        let mut server = Connection::from_stream(server_stream);
        tokio::task::spawn_local(async move {
            server.recv().await.expect("request frame");
            for frame in &script {
                server.send(frame).await.expect("scripted frame");
            }
        });
        Connection::from_stream(client_stream)
    }

    /// One `GET_METADATA` round trip against `script`.
    ///
    /// Capped by [`WEDGE_TIMEOUT`] because the defect under test is a wait
    /// that never ends: without the cap a regression hangs the whole run
    /// instead of failing this one test.
    fn metadata_against(script: Vec<FrameKind>) -> Reply<Answer<Option<Vec<u8>>>> {
        block_on(async {
            let mut client = scripted(script);
            tokio::time::timeout(
                WEDGE_TIMEOUT,
                client.request_metadata(7, Scope::Terminal(TerminalId::local(1)), "k".to_owned()),
            )
            .await
            .expect("the read must resolve; a timeout here is the wedge itself")
            .expect("metadata reply")
        })
    }

    /// One `SPAWN_TERMINAL` round trip against `script`, same cap.
    fn spawn_against(script: Vec<FrameKind>) -> Reply<Answer<SpawnResult>> {
        block_on(async {
            let mut client = scripted(script);
            tokio::time::timeout(WEDGE_TIMEOUT, client.request_spawn(&spawn_frame(7)))
                .await
                .expect("the spawn must resolve; a timeout here is the wedge itself")
                .expect("spawn reply")
        })
    }

    fn metadata_value(request_id: u32, value: Option<&[u8]>) -> FrameKind {
        FrameKind::MetadataValue {
            request_id,
            value: value.map(<[u8]>::to_vec),
        }
    }

    fn refusal(request_id: u32) -> FrameKind {
        FrameKind::Error {
            request_id: Some(request_id),
            code: ErrorCode::PermissionDenied,
            message: "policy refused that scope".to_owned(),
        }
    }

    #[test]
    fn metadata_refusal_answers_the_request_instead_of_hanging_forever() {
        // The bug: `phux tag`, `phux new`, `phux config reload` and `phux
        // agent set` each waited on METADATA_VALUE alone, so this shape left
        // the verb running with no output and no exit — after its write had
        // already landed, in the `set` cases.
        let reply = metadata_against(vec![refusal(7)]);
        match reply.result() {
            Err(refused) => {
                assert_eq!(refused.code, ErrorCode::PermissionDenied);
                assert_eq!(refused.message, "policy refused that scope");
            }
            Ok(other) => panic!("a correlated ERROR is this read's answer, got {other:?}"),
        }
    }

    #[test]
    fn metadata_refusal_is_distinct_from_an_unset_key() {
        // Both used to reach the caller as "no value". They are different
        // facts: `phux new` reported "server did not register session" for a
        // server that had refused the read-back outright.
        let refused = metadata_against(vec![refusal(7)]);
        let unset = metadata_against(vec![metadata_value(7, None)]);
        assert!(refused.result().is_err());
        assert_eq!(unset.result().as_ref().ok(), Some(&None));
    }

    #[test]
    fn metadata_wait_keeps_another_requests_reply() {
        // Pipelined reads share a connection: a METADATA_VALUE for a
        // different request_id belongs to someone else's correlation.
        let reply = metadata_against(vec![
            metadata_value(99, Some(b"theirs")),
            metadata_value(7, None),
        ]);
        assert!(
            matches!(
                reply.interleaved(),
                [FrameKind::MetadataValue { request_id: 99, .. }]
            ),
            "got {:?}",
            reply.interleaved()
        );
    }

    #[test]
    fn metadata_wait_keeps_an_uncorrelated_degradation_notice() {
        // A hub's per-satellite ERROR carries no request_id, so it answers
        // nothing and must survive to the caller like any pushed frame.
        let notice = FrameKind::Error {
            request_id: None,
            code: ErrorCode::SatelliteUnreachable,
            message: "satellite build-box is unreachable".to_owned(),
        };
        let reply = metadata_against(vec![notice, metadata_value(7, Some(b"v"))]);
        assert_eq!(
            reply.result().as_ref().ok(),
            Some(&Some(b"v".to_vec())),
            "an uncorrelated ERROR is not this read's answer",
        );
        assert!(matches!(
            reply.interleaved(),
            [FrameKind::Error {
                request_id: None,
                ..
            }]
        ));
    }

    fn spawn_frame(request_id: u32) -> FrameKind {
        FrameKind::SpawnTerminal {
            request_id,
            group: phux_protocol::ids::GroupId::new(1),
            command: None,
            cwd: None,
            env: None,
            term: None,
            satellite: Some(phux_protocol::ids::SatelliteHost::new("build-box")),
            owner_terminal: None,
            agent_session: None,
        }
    }

    #[test]
    fn spawn_refusal_answers_the_request_instead_of_hanging_forever() {
        // `relay.rs`'s `handle_inbound` says a satellite MAY answer a relayed
        // spawn with a generic correlated ERROR. `dispatch_spawn_async`
        // matched TERMINAL_SPAWNED alone, so `phux spawn --satellite` against
        // such a peer never returned.
        let reply = spawn_against(vec![refusal(7)]);
        assert!(
            reply.result().is_err(),
            "a correlated ERROR is this spawn's answer, got {:?}",
            reply.result()
        );
    }

    #[test]
    fn spawn_wait_keeps_frames_pushed_ahead_of_the_reply() {
        let spawned = FrameKind::TerminalSpawned {
            request_id: 7,
            result: SpawnResult::Ok(TerminalId::local(3)),
        };
        let reply = spawn_against(vec![snapshot(), spawned]);
        assert!(matches!(reply.result(), Ok(SpawnResult::Ok(_))));
        assert!(
            matches!(reply.interleaved(), [FrameKind::TerminalSnapshot { .. }]),
            "got {:?}",
            reply.interleaved()
        );
    }

    #[test]
    fn spawn_rejects_a_frame_that_is_not_a_spawn() {
        // The correlation id comes out of the frame, so a caller handing over
        // the wrong frame has no id to wait on. Refuse loudly rather than
        // wait on a fabricated one.
        block_on(async {
            let (client_stream, _server) = UnixStream::pair().expect("pair");
            let mut client = Connection::from_stream(client_stream);
            assert!(matches!(
                client.request_spawn(&ack(1)).await,
                Err(AttachError::Protocol(_))
            ));
        });
    }
}
