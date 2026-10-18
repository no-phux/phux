//! One-shot sessions: a verb's connection, negotiated once and never
//! redialed (ADR-0171 decision 2).
//!
//! A [`Session`] dials a [`Dial`] (or plans a [`crate::Target`] under the
//! registry's trust rules), negotiates `HELLO` with the caller's client name
//! and capabilities, and then offers framed [`Session::send`] /
//! [`Session::recv`] / [`Session::try_recv`] for streaming consumers and an
//! awaitable correlated request for every request frame. It attaches to
//! nothing on its own: what it subscribes to is the caller's business.
//!
//! It shares the transport with the reconnecting driver
//! ([`crate::connection`]): the same establishment, framing, send budgets,
//! WebSocket liveness, and [`crate::handshake`]. What is its own is what a
//! request/reply caller needs: correlation, corking, and QUIC per-Terminal
//! streams.
//!
//! # The interleave contract
//!
//! L1 §5 lets the server emit other frames before a reply, and it does so on
//! paths a client cannot avoid: `ATTACH_RESOURCE` pushes the bootstrap
//! transcript before its ack, a federation hub pushes one uncorrelated
//! `ERROR` per unreachable satellite before a `GET_STATE` reply, and any
//! subscription on the connection can fan events in first. Those frames are
//! never re-sent, so the request methods return them inside [`Reply`]. Raw
//! [`Session::send`] + [`Session::recv`] is only for a full-duplex loop that
//! routes every frame kind.
//!
//! # Facade mapping
//!
//! `phux_client::attach::connection::Connection` is a thin facade over this
//! type. Each of its methods maps to one call here:
//!
//! | `Connection`                         | `Session`                                       |
//! |--------------------------------------|-------------------------------------------------|
//! | `connect(socket)`                    | `connect(&Dial::uds(socket), name, control_caps())` |
//! | `connect_with_hello(socket, n, c)`   | `connect(&Dial::uds(socket), n, c)`             |
//! | `connect_quic(d)` / `_with_hello`    | `connect(&Dial::Quic(d.clone()), n, c)`         |
//! | `connect_ws_with_hello(d, n, c)`     | `connect(&Dial::Ws(d.clone()), n, c)`           |
//! | `connect_dial(d)` / `_with_hello`    | `connect(d, n, c)` (`Dial` is this crate's)     |
//! | `connect_quic_with_terminal_frame_deadline_for_test` | `connect` + `set_terminal_frame_deadline_for_test` (feature `testing`) |
//! | `from_stream(stream)`                | `from_stream(stream)`                           |
//! | `negotiate(n, c)`                    | `negotiate(n, c)`                               |
//! | `negotiated_bootstrap()`             | `negotiated_bootstrap()`                        |
//! | `server_id()`                        | `server_id()`                                   |
//! | `observer_role_policy()`             | `observer_role_policy()`                        |
//! | `peer_pid()`                         | `peer_pid()`                                    |
//! | `next_attach_id()`                   | `next_attach_id()`                              |
//! | `send(f)` / `recv()` / `try_recv()`  | `send(f)` / `recv()` / `try_recv()`             |
//! | `write_hit_closed_peer()`            | `write_hit_closed_peer()`                       |
//! | `cork()` / `uncork()`                | `cork()` / `uncork()`                           |
//! | `request(id, cmd)`                   | `request(id, cmd)`                              |
//! | `request_metadata(id, scope, key)`   | `request_metadata(id, scope, key)`              |
//! | `request_metadata_many(first, reads)`| `request_metadata_many(first, reads)`           |
//! | `request_metadata_keys(id, scope)`   | `request_metadata_keys(id, scope)`              |
//! | `request_spawn(f)` / `request_move(f)` | `request_spawn(f)` / `request_move(f)`        |
//! | `multistream_enabled()`              | `multistream_enabled()`                         |
//! | `can_route_terminal(t)`              | `can_route_terminal(t)`                         |
//! | `bind_terminal(t)` / `unbind_terminal(t)` / `unbind_all_terminals()` | same names |
//! | `shutdown()`                         | `shutdown()`                                    |
//! | `Reply`, `Answer`, `Refusal`, `METADATA_WINDOW` | [`Reply`], [`Answer`], [`Refusal`], [`METADATA_WINDOW`] |
//! | `NegotiatedBootstrap`                | [`NegotiatedBootstrap`]                         |
//! | `AttachError` (transport variants)   | [`SessionError`], one variant each, by name     |
//! | `explain::unexpected_reply`          | [`unexpected_reply`], re-exported                |

use std::collections::VecDeque;

use bytes::{Bytes, BytesMut};
use futures_util::FutureExt;
use phux_protocol::caps::{BootstrapLimits, ClientCapabilities, Layer, LayerSet, ServerFeature};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{FrameKind, RolePolicy, TYPE_PONG};
use phux_protocol::wire::framing::{self, FramingError, LENGTH_PREFIX_LEN};
use tokio::net::UnixStream;

use crate::connection::establish;
use crate::connection::io::Io;
use crate::control::encode;
use crate::dial::DIAL_TIMEOUT;
pub use crate::dial::Dial;
pub use crate::handshake::NegotiatedBootstrap;
use crate::handshake::{self, HelloOk, ServerInfo};
use multistream::{MUX_FRAME_BYTES, MUX_FRAME_CHANNEL, Multistream, TERMINAL_FRAME_DEADLINE};

mod error;
mod multistream;
mod reply;
mod requests;

pub use error::SessionError;
pub use reply::{Answer, Refusal, Reply, error_code_label, unexpected_reply};
pub use requests::METADATA_WINDOW;

/// The control-plane offer a verb negotiates by default: L3 metadata and
/// the default bootstrap profile.
#[must_use]
pub const fn control_caps() -> ClientCapabilities {
    ClientCapabilities::new().with_layers(LayerSet::with(&[Layer::L3]))
}

/// A connection that attaches to nothing, never redials, and lives as long
/// as one verb. See the [module documentation](self).
#[derive(Debug)]
pub struct Session {
    link: Io,
    /// Complete control-stream frames read but not yet delivered.
    inbound: VecDeque<Bytes>,
    /// The batch accumulating between [`Self::cork`] and [`Self::uncork`].
    corked: Option<BytesMut>,
    /// What `HELLO_OK` negotiated; `None` until [`Self::negotiate`].
    server: Option<ServerInfo>,
    next_attach_id: u32,
    /// Per-Terminal QUIC stream state; on a plain QUIC link only.
    multistream: Option<Multistream>,
    /// A write found the server already gone.
    write_hit_closed_peer: bool,
}

// A verb borrows its session across awaits, and `phux-client`'s facade
// is held in `Send` futures, so the session stays `Send + Sync`.
const _: () = {
    const fn send_and_sync<T: Send + Sync>() {}
    send_and_sync::<Session>();
};

impl Session {
    /// Dial `dial` within [`DIAL_TIMEOUT`] and negotiate `HELLO` as
    /// `client_name` offering `client_caps`. On a plain QUIC link the offer
    /// adds per-Terminal streams (proto.md §4.2).
    ///
    /// A development build refuses the production Unix socket
    /// (`phux_config_core::socket::refuse_dev_on_production`).
    ///
    /// # Errors
    ///
    /// Transport failures, a dial that outlasts its bound
    /// ([`SessionError::Unreachable`]), or the handshake errors of
    /// [`Self::negotiate`].
    pub async fn connect(
        dial: &Dial,
        client_name: String,
        client_caps: ClientCapabilities,
    ) -> Result<Self, SessionError> {
        refuse_production_socket(dial)?;
        let link = tokio::time::timeout(DIAL_TIMEOUT, establish(dial))
            .await
            .map_err(|_| dial_timed_out())??;
        let mut session = Self::over(link);
        session.negotiate(client_name, client_caps).await?;
        Ok(session)
    }

    /// Plan `target` under the registry's trust rules
    /// ([`crate::Target::plan`]), then [`Self::connect`] to it.
    ///
    /// # Errors
    ///
    /// A refused plan is [`SessionError::Connect`]; otherwise as
    /// [`Self::connect`].
    pub async fn connect_target(
        target: &crate::Target,
        client_name: String,
        client_caps: ClientCapabilities,
    ) -> Result<Self, SessionError> {
        let dial = tokio::time::timeout(DIAL_TIMEOUT, target.plan())
            .await
            .map_err(|_| dial_timed_out())?
            .map_err(SessionError::Connect)?;
        Self::connect(&dial, client_name, client_caps).await
    }

    /// An unnegotiated session over an already-connected [`UnixStream`]:
    /// the in-process seam tests use for either end of a socket pair.
    #[must_use]
    pub fn from_stream(stream: UnixStream) -> Self {
        Self::over(Io::from_unix_stream(stream))
    }

    fn over(link: Io) -> Self {
        let multistream = link.terminal_stream_connection().map(Multistream::new);
        Self {
            link,
            inbound: VecDeque::new(),
            corked: None,
            server: None,
            next_attach_id: 1,
            multistream,
            write_hit_closed_peer: false,
        }
    }

    /// Negotiate the protocol once. [`Self::connect`] already did; this is
    /// for a [`Self::from_stream`] session.
    ///
    /// # Errors
    ///
    /// [`SessionError::Refused`] when the server answers with an `ERROR`;
    /// [`SessionError::Protocol`] for a second negotiation, a `HELLO_OK`
    /// outside the offer, or any other reply.
    pub async fn negotiate(
        &mut self,
        client_name: String,
        client_caps: ClientCapabilities,
    ) -> Result<(), SessionError> {
        if self.server.is_some() {
            return Err(SessionError::Protocol(
                "HELLO negotiation already completed on this session".to_owned(),
            ));
        }
        let offered = client_caps.with_quic_streams(self.multistream.is_some());
        self.send(&handshake::hello(client_name, offered)).await?;
        let answer = self.recv().await?;
        self.server = Some(accept_hello_reply(&offered, answer)?);
        Ok(())
    }

    /// Everything `HELLO_OK` negotiated; `None` before [`Self::negotiate`].
    #[must_use]
    pub const fn server(&self) -> Option<&ServerInfo> {
        self.server.as_ref()
    }

    /// What `HELLO_OK` selected; `None` before [`Self::negotiate`].
    #[must_use]
    pub fn negotiated_bootstrap(&self) -> Option<NegotiatedBootstrap> {
        self.server.as_ref().map(ServerInfo::bootstrap)
    }

    /// `HELLO_OK.server_id`, the server-incarnation id (ADR-0053).
    #[must_use]
    pub fn server_id(&self) -> Option<&[u8]> {
        self.server.as_ref().map(|server| server.id.as_slice())
    }

    /// The role an observer attaches with (ADR-0127): `VIEWER` when the
    /// server advertises `ATTACH_ROLES`, so it can never type into what it
    /// watches.
    #[must_use]
    pub fn observer_role_policy(&self) -> Option<RolePolicy> {
        self.advertises(ServerFeature::AttachRoles)
            .then_some(RolePolicy::VIEWER)
    }

    fn advertises(&self, feature: ServerFeature) -> bool {
        self.server
            .as_ref()
            .is_some_and(|server| server.has(feature))
    }

    /// The server's pid, from the Unix socket's peer credentials (an OS
    /// fact, not a wire exchange). `None` on remote transports or when the
    /// platform omits it.
    #[must_use]
    pub const fn peer_pid(&self) -> Option<i32> {
        self.link.peer_pid()
    }

    /// Allocate a session-local, non-zero correlation id for an `ATTACH`.
    pub const fn next_attach_id(&mut self) -> u32 {
        let id = self.next_attach_id;
        self.next_attach_id = self.next_attach_id.wrapping_add(1);
        if self.next_attach_id == 0 {
            self.next_attach_id = 1;
        }
        id
    }

    /// Whether a write on this session failed because the server had
    /// already closed it (`BrokenPipe`, `ConnectionReset`,
    /// `ConnectionAborted`).
    ///
    /// Such a write is not why the session ended: the read side still holds
    /// whatever the server sent before it went (an explained exit, or the
    /// EOF a reconnect keys on), so a caller that sees this lets
    /// [`Self::recv`] name the ending instead of failing on the write.
    #[must_use]
    pub const fn write_hit_closed_peer(&self) -> bool {
        self.write_hit_closed_peer
    }

    /// Accumulate control-stream sends until [`Self::uncork`], so one input
    /// batch costs one write. Callers MUST uncork on every exit path, errors
    /// included. Only a Unix socket batches: QUIC coalesces in its own send
    /// buffer, and WebSocket needs one message per frame.
    pub fn cork(&mut self) {
        if self.link.batches_writes() {
            self.corked = Some(BytesMut::with_capacity(4096));
        }
    }

    /// Ship everything corked since [`Self::cork`], in send order, in one
    /// write. A no-op if nothing was sent or the transport does not cork.
    ///
    /// # Errors
    ///
    /// The batch's write failure.
    pub async fn uncork(&mut self) -> Result<(), SessionError> {
        let Some(batch) = self.corked.take() else {
            return Ok(());
        };
        if batch.is_empty() {
            return Ok(());
        }
        let shipped = self
            .link
            .write_frame(&batch)
            .await
            .map_err(SessionError::from);
        self.note_write(shipped)
    }

    /// Close cleanly: finish every Terminal stream, then on QUIC send
    /// `CONNECTION_CLOSE` and wait until it is delivered, so the server
    /// reaps this consumer at once. Elsewhere dropping the link already is
    /// a clean close.
    pub async fn shutdown(mut self) {
        self.unbind_all_terminals();
        self.link.shutdown().await;
    }

    /// The decode bounds `HELLO_OK` selected, or the defaults before it.
    fn limits(&self) -> BootstrapLimits {
        self.server
            .as_ref()
            .map_or_else(BootstrapLimits::default, |server| server.limits)
    }
}

// ----- framed send and receive ------------------------------------------

impl Session {
    /// Encode `frame` and write it to the server: on its Terminal's stream
    /// under QUIC multistream, else on the control stream (into the batch
    /// while corked).
    ///
    /// # Errors
    ///
    /// The write failure; under multistream also a Terminal frame for a
    /// Terminal that was never bound, or a viewport barrier that failed.
    pub async fn send(&mut self, frame: &FrameKind) -> Result<(), SessionError> {
        let sent = self.send_frame(frame).await;
        self.note_write(sent)
    }

    async fn send_frame(&mut self, frame: &FrameKind) -> Result<(), SessionError> {
        if self.multistream_enabled()
            && let Some(terminal_id) = multistream::terminal_target(frame)
            && let Some(mux) = self.multistream.as_mut()
        {
            return mux.send(terminal_id, frame).await;
        }
        self.write_control(&encode(frame)).await?;
        if self.multistream_enabled() && matches!(frame, FrameKind::ViewportResize { .. }) {
            self.settle_viewport_order().await?;
        }
        Ok(())
    }

    async fn write_control(&mut self, bytes: &[u8]) -> Result<(), SessionError> {
        if let Some(batch) = self.corked.as_mut() {
            batch.extend_from_slice(bytes);
            return Ok(());
        }
        self.link
            .write_frame(bytes)
            .await
            .map_err(SessionError::from)
    }

    /// Record a write that failed on a closed peer, then pass the result on.
    fn note_write(&mut self, result: Result<(), SessionError>) -> Result<(), SessionError> {
        if let Err(SessionError::Io(error)) = &result
            && matches!(
                error.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
            )
        {
            self.write_hit_closed_peer = true;
        }
        result
    }

    /// QUIC orders each stream, not the control stream against Terminal
    /// streams. Wait until the viewport vote has been dispatched before the
    /// caller can send its exact pane layout: a `PING` on control, and every
    /// control frame ahead of its `PONG` kept for [`Self::recv`].
    async fn settle_viewport_order(&mut self) -> Result<(), SessionError> {
        let nonce = uuid::Uuid::new_v4().as_u64_pair().0;
        self.write_control(&encode(&FrameKind::Ping { nonce }))
            .await?;
        let ahead = self.inbound.len();
        tokio::time::timeout(TERMINAL_FRAME_DEADLINE, self.await_pong(nonce, ahead))
            .await
            .map_err(|_| SessionError::Protocol("viewport ordering barrier timed out".to_owned()))?
    }

    /// Read control frames until the `PONG` for `nonce` arrives behind the
    /// `ahead` frames already queued, then drop it from the queue.
    async fn await_pong(&mut self, nonce: u64, ahead: usize) -> Result<(), SessionError> {
        loop {
            let pong = self
                .inbound
                .iter()
                .skip(ahead)
                .position(|frame| is_pong(frame, nonce));
            let before = pong.unwrap_or(self.inbound.len() - ahead);
            check_barrier_bound(self.inbound.iter().skip(ahead).take(before))?;
            if let Some(index) = pong {
                self.inbound.remove(ahead + index);
                return Ok(());
            }
            let batch = self.link.read_frames().await;
            self.queue_inbound(batch)?;
        }
    }

    /// Read the next frame from the server: the control stream, or under
    /// QUIC multistream any bound Terminal stream.
    ///
    /// On WebSocket the read also ping/pong probes an idle peer, so one
    /// that stops answering surfaces as [`SessionError::Disconnected`]. A
    /// SPEC §5 framing violation is answered with `ERROR { FRAME_TOO_LARGE }`
    /// (best-effort) before [`SessionError::Framing`] is returned.
    ///
    /// # Errors
    ///
    /// [`SessionError::Disconnected`] on a clean close; transport, framing,
    /// and decode failures otherwise.
    pub async fn recv(&mut self) -> Result<FrameKind, SessionError> {
        let result = self.recv_frame().await;
        if let Ok(frame) = &result {
            self.retire_closed_terminal(frame);
        }
        if let Err(SessionError::Framing(violation)) = &result {
            self.answer_framing_violation(*violation).await;
        }
        result
    }

    /// A lifecycle close on control retires the independent Terminal stream
    /// before queued output can reach the caller's closed session resource.
    fn retire_closed_terminal(&mut self, frame: &FrameKind) {
        if let FrameKind::ResourceClosed { terminal_id, .. } = frame
            && let Some(mux) = self.multistream.as_mut()
        {
            mux.close_terminal(terminal_id);
        }
    }

    async fn recv_frame(&mut self) -> Result<FrameKind, SessionError> {
        let limits = self.limits();
        loop {
            if let Some(frame) = self.inbound.pop_front() {
                return decode_frame(&frame, limits);
            }
            let batch = match self.multistream.as_mut() {
                None => self.link.read_frames().await,
                Some(mux) => tokio::select! {
                    batch = self.link.read_frames() => batch,
                    frame = mux.recv(limits) => return frame,
                },
            };
            self.queue_inbound(batch)?;
        }
    }

    /// A frame that is already available without waiting, or `Ok(None)`,
    /// so a loop can drain a burst into one paint. Framing violations are
    /// answered as in [`Self::recv`], without blocking.
    ///
    /// # Errors
    ///
    /// As [`Self::recv`]; a clean close is reported by the next `recv`.
    pub fn try_recv(&mut self) -> Result<Option<FrameKind>, SessionError> {
        let result = self.try_recv_frame();
        if let Ok(Some(frame)) = &result {
            self.retire_closed_terminal(frame);
        }
        if let Err(SessionError::Framing(violation)) = &result {
            let _ = self.answer_framing_violation(*violation).now_or_never();
        }
        result
    }

    fn try_recv_frame(&mut self) -> Result<Option<FrameKind>, SessionError> {
        let limits = self.limits();
        if self.inbound.is_empty() {
            if let Some(mux) = self.multistream.as_mut()
                && let Some(frame) = mux.try_recv(limits)?
            {
                return Ok(Some(frame));
            }
            let frames = self.link.try_read_frames()?;
            self.inbound
                .extend(frames.into_iter().filter(|frame| !frame.is_empty()));
        }
        self.inbound
            .pop_front()
            .map(|frame| decode_frame(&frame, limits))
            .transpose()
    }

    /// Queue a read batch's frames; a clean close is a disconnect. Empty
    /// frames are WebSocket proof-of-life and carry nothing.
    fn queue_inbound(
        &mut self,
        batch: Result<Option<Vec<Bytes>>, crate::connection::io::LinkError>,
    ) -> Result<(), SessionError> {
        let frames = batch?.ok_or(SessionError::Disconnected)?;
        self.inbound
            .extend(frames.into_iter().filter(|frame| !frame.is_empty()));
        Ok(())
    }

    /// SPEC §5: send `ERROR { FRAME_TOO_LARGE }` before the caller drops
    /// the connection, ahead of (and so dropping) any corked batch.
    /// Best-effort: a vanished peer cannot fail the close.
    async fn answer_framing_violation(&mut self, violation: FramingError) {
        self.corked = None;
        let goodbye = encode(&framing::frame_too_large_error(violation));
        let _ = self.link.write_frame(&goodbye).await;
    }
}

// ----- QUIC per-Terminal streams ------------------------------------------

impl Session {
    /// Whether this negotiated QUIC session carries per-Terminal streams:
    /// a plain QUIC link (never UDS, WebSocket, or a relayed end-to-end
    /// session, proto.md §4.2) whose `HELLO_OK` advertised `QUIC_STREAMS`.
    #[must_use]
    pub fn multistream_enabled(&self) -> bool {
        self.multistream.is_some() && self.advertises(ServerFeature::QuicStreams)
    }

    /// Whether a Terminal-targeted frame for `terminal_id` can be sent now.
    ///
    /// Always `true` off multistream, where every frame rides the one
    /// control stream. On multistream it is `true` once the Terminal's
    /// stream is bound (or has already ended, whose frames [`Self::send`]
    /// drops): a Terminal whose attach has not been confirmed has no stream
    /// yet, and sending to it is the caller bug [`Self::send`] refuses.
    #[must_use]
    pub fn can_route_terminal(&self, terminal_id: &ResourceId) -> bool {
        if !self.multistream_enabled() {
            return true;
        }
        self.multistream
            .as_ref()
            .is_some_and(|mux| mux.can_route(terminal_id))
    }

    /// Open and bind a QUIC stream for `terminal_id` (a no-op when
    /// multistream is off or it is already bound).
    ///
    /// # Errors
    ///
    /// Past the stream cap, or when the stream cannot be opened or bound.
    pub async fn bind_terminal(&mut self, terminal_id: &ResourceId) -> Result<(), SessionError> {
        if !self.multistream_enabled() {
            return Ok(());
        }
        match self.multistream.as_mut() {
            Some(mux) => mux.bind(terminal_id).await,
            None => Ok(()),
        }
    }

    /// Finish and forget one Terminal stream, if it is bound.
    pub fn unbind_terminal(&mut self, terminal_id: &ResourceId) {
        if let Some(mux) = self.multistream.as_mut() {
            mux.unbind(terminal_id);
        }
    }

    /// Finish all client-opened Terminal streams.
    pub fn unbind_all_terminals(&mut self) {
        if let Some(mux) = self.multistream.as_mut() {
            mux.unbind_all();
        }
    }

    /// Bound how long a Terminal frame may take from its first byte to its
    /// complete body, for streams bound from now on. A test seam.
    #[cfg(any(test, feature = "testing"))]
    pub const fn set_terminal_frame_deadline_for_test(&mut self, deadline: std::time::Duration) {
        if let Some(mux) = self.multistream.as_mut() {
            mux.set_terminal_frame_deadline(deadline);
        }
    }
}

/// The negotiated [`ServerInfo`] from the server's answer to `HELLO`.
fn accept_hello_reply(
    offered: &ClientCapabilities,
    answer: FrameKind,
) -> Result<ServerInfo, SessionError> {
    match answer {
        FrameKind::HelloOk {
            protocol_major,
            protocol_minor,
            protocol_patch,
            server_caps,
            server_id,
            selected_profile,
            bootstrap_limits,
        } => HelloOk {
            protocol: (protocol_major, protocol_minor, protocol_patch),
            server_id,
            server_caps,
            profile: selected_profile,
            limits: bootstrap_limits,
        }
        .accept(offered)
        .map_err(|error| SessionError::Protocol(error.to_string())),
        FrameKind::Error { message, .. } => Err(SessionError::Refused(message)),
        _ => Err(SessionError::Protocol(unexpected_reply("HELLO"))),
    }
}

/// Decode one complete, length-checked frame under `limits`.
pub(crate) fn decode_frame(
    frame: &[u8],
    limits: BootstrapLimits,
) -> Result<FrameKind, SessionError> {
    FrameKind::decode_with_limits(frame, limits)
        .map(|(decoded, _rest)| decoded)
        .map_err(|error| {
            SessionError::Protocol(format!("server sent undecodable frame: {error:?}"))
        })
}

/// Whether `frame` is the `PONG` answering `nonce`; only a `PONG`-typed
/// frame is decoded.
fn is_pong(frame: &[u8], nonce: u64) -> bool {
    frame.get(LENGTH_PREFIX_LEN) == Some(&TYPE_PONG)
        && matches!(
            FrameKind::decode(frame),
            Ok((FrameKind::Pong { nonce: got }, [])) if got == nonce
        )
}

/// The viewport barrier holds at most [`MUX_FRAME_CHANNEL`] control frames
/// and [`MUX_FRAME_BYTES`] ahead of its `PONG`.
fn check_barrier_bound<'a>(held: impl Iterator<Item = &'a Bytes>) -> Result<(), SessionError> {
    let (count, bytes) = held.fold((0, 0), |(count, bytes), frame| {
        (count + 1, bytes + frame.len())
    });
    if count > MUX_FRAME_CHANNEL || bytes > MUX_FRAME_BYTES {
        return Err(SessionError::Protocol(
            "viewport ordering barrier exceeded its control-frame bound".to_owned(),
        ));
    }
    Ok(())
}

/// A production Unix socket is off limits to a development build.
fn refuse_production_socket(dial: &Dial) -> Result<(), SessionError> {
    let Dial::Uds(path) = dial else {
        return Ok(());
    };
    phux_config_core::socket::refuse_dev_on_production(path).map_err(|refusal| {
        SessionError::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            refusal,
        ))
    })
}

fn dial_timed_out() -> SessionError {
    SessionError::Unreachable(format!("no answer within {}s", DIAL_TIMEOUT.as_secs()))
}

#[cfg(test)]
mod tests;
