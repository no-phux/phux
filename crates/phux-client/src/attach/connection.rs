//! A negotiated connection to a phux server: a facade over the client
//! runtime's one-shot [`Session`] (ADR-0171).
//!
//! The runtime owns the transport: dial, `HELLO`, SPEC §5 framing, send
//! budgets, corking, QUIC per-Terminal streams, and the classification of
//! every failure. This type keeps the API the verbs, MCP, and the TUI call,
//! and turns a `SessionError` into an [`AttachError`] with nothing more
//! than a variant-for-variant conversion.

use std::path::Path;

pub use phux_client_runtime::session::{
    Answer, Dial, METADATA_WINDOW, NegotiatedBootstrap, Refusal, Reply,
};
use phux_client_runtime::session::{Session, control_caps};
pub use phux_dial::CertTrust;
pub use phux_dial::quic::QuicDial;
pub use phux_dial::ws::WsDial;
use phux_protocol::caps::ClientCapabilities;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{Command, FrameKind, MoveResult, RolePolicy, Scope, SpawnResult};
#[cfg(any(test, feature = "testkit"))]
use tokio::net::UnixStream;

use super::outcome::AttachError;

/// A connection that has completed `HELLO` negotiation.
///
/// # The COMMAND interleave contract
///
/// L1 §5 lets the server emit other frames before a `COMMAND_RESULT`, and it
/// does so on paths a client cannot avoid: `ATTACH_RESOURCE` pushes the
/// bootstrap transcript before its ack, a federation hub pushes one
/// uncorrelated `ERROR` per unreachable satellite before a `GET_STATE`
/// reply, and any subscription on the connection can fan events in first.
/// Those frames are never re-sent, so use [`Connection::request`] (and its
/// typed siblings), which return them with the answer. Raw
/// [`Connection::send`] + [`Connection::recv`] is only for a full-duplex loop
/// that routes every frame kind.
#[derive(Debug)]
pub struct Connection {
    session: Session,
}

fn default_client_name() -> String {
    format!("phux-client/{}", env!("CARGO_PKG_VERSION"))
}

impl Connection {
    /// Open the UDS at `socket` and negotiate the control-plane profile
    /// (L3 included, for metadata requests).
    ///
    /// # Errors
    ///
    /// Connect failures, or handshake errors when the server refuses or does
    /// not acknowledge `HELLO`.
    pub async fn connect(socket: &Path) -> Result<Self, AttachError> {
        Self::connect_dial(&Dial::uds(socket)).await
    }

    /// Open the UDS and negotiate with the supplied client profile.
    ///
    /// # Errors
    ///
    /// Returns transport, protocol, or server-refusal errors from negotiation.
    pub async fn connect_with_hello(
        socket: &Path,
        client_name: String,
        client_caps: ClientCapabilities,
    ) -> Result<Self, AttachError> {
        Self::connect_dial_with_hello(&Dial::uds(socket), client_name, client_caps).await
    }

    /// Dial a remote QUIC listener and negotiate the control-plane profile.
    ///
    /// # Errors
    ///
    /// Returns transport, protocol, or server-refusal errors from negotiation.
    pub async fn connect_quic(dial: &QuicDial) -> Result<Self, AttachError> {
        Self::connect_dial(&Dial::Quic(dial.clone())).await
    }

    /// Dial QUIC and negotiate with the supplied client profile; a plain
    /// QUIC link also offers per-Terminal streams (proto.md §4.2).
    ///
    /// # Errors
    ///
    /// Returns transport, protocol, or server-refusal errors from negotiation.
    pub async fn connect_quic_with_hello(
        dial: &QuicDial,
        client_name: String,
        client_caps: ClientCapabilities,
    ) -> Result<Self, AttachError> {
        Self::connect_dial_with_hello(&Dial::Quic(dial.clone()), client_name, client_caps).await
    }

    /// [`Self::connect_quic`] with a short incomplete-Terminal-frame deadline.
    ///
    /// # Errors
    ///
    /// Returns transport, protocol, or server-refusal errors from negotiation.
    #[cfg(feature = "testkit")]
    pub async fn connect_quic_with_terminal_frame_deadline_for_test(
        dial: &QuicDial,
        terminal_frame_deadline: std::time::Duration,
    ) -> Result<Self, AttachError> {
        let mut conn = Self::connect_quic(dial).await?;
        conn.session
            .set_terminal_frame_deadline_for_test(terminal_frame_deadline);
        Ok(conn)
    }

    /// Dial WebSocket and negotiate with the supplied client profile.
    ///
    /// # Errors
    ///
    /// Returns transport, protocol, or server-refusal errors from negotiation.
    pub async fn connect_ws_with_hello(
        dial: &WsDial,
        client_name: String,
        client_caps: ClientCapabilities,
    ) -> Result<Self, AttachError> {
        Self::connect_dial_with_hello(&Dial::Ws(dial.clone()), client_name, client_caps).await
    }

    /// Connect over `dial` and negotiate the control-plane profile.
    ///
    /// # Errors
    ///
    /// Returns transport, protocol, or server-refusal errors from negotiation.
    pub async fn connect_dial(dial: &Dial) -> Result<Self, AttachError> {
        Self::connect_dial_with_hello(dial, default_client_name(), control_caps()).await
    }

    /// Connect over `dial` and negotiate with the supplied client profile.
    /// A development build refuses the production Unix socket.
    ///
    /// # Errors
    ///
    /// Returns transport, protocol, or server-refusal errors from negotiation.
    pub async fn connect_dial_with_hello(
        dial: &Dial,
        client_name: String,
        client_caps: ClientCapabilities,
    ) -> Result<Self, AttachError> {
        let session = Session::connect(dial, client_name, client_caps).await?;
        Ok(Self { session })
    }

    /// An unnegotiated connection over an already-connected [`UnixStream`]:
    /// the in-process test seam.
    #[cfg(any(test, feature = "testkit"))]
    #[must_use]
    pub fn from_stream(stream: UnixStream) -> Self {
        Self {
            session: Session::from_stream(stream),
        }
    }

    /// Negotiate the protocol once. Production constructors already did; this
    /// is public for tests driving a `from_stream` connection.
    ///
    /// # Errors
    ///
    /// Any handshake failure, as [`AttachError`].
    pub async fn negotiate(
        &mut self,
        client_name: String,
        client_caps: ClientCapabilities,
    ) -> Result<(), AttachError> {
        Ok(self.session.negotiate(client_name, client_caps).await?)
    }

    /// The server's pid, from the UDS peer credentials (an OS fact, not a wire
    /// exchange). `None` on remote transports or when the platform omits it.
    #[must_use]
    pub const fn peer_pid(&self) -> Option<i32> {
        self.session.peer_pid()
    }

    /// Close cleanly: finish every Terminal stream, and on QUIC send
    /// `CONNECTION_CLOSE` and wait for its delivery, so the server reaps the
    /// consumer at once.
    pub async fn shutdown(self) {
        self.session.shutdown().await;
    }

    /// What `HELLO_OK` selected; `None` only on the `from_stream` test seam.
    #[must_use]
    pub fn negotiated_bootstrap(&self) -> Option<NegotiatedBootstrap> {
        self.session.negotiated_bootstrap()
    }

    /// The role an observer attaches with (ADR-0127): `VIEWER` when the server
    /// advertises `ATTACH_ROLES`, so it can never type into what it watches.
    #[must_use]
    pub fn observer_role_policy(&self) -> Option<RolePolicy> {
        self.session.observer_role_policy()
    }

    /// `HELLO_OK.server_id`, the server-incarnation id (ADR-0053).
    #[must_use]
    pub fn server_id(&self) -> Option<&[u8]> {
        self.session.server_id()
    }

    /// Allocate a connection-local, non-zero correlation id for an `ATTACH`.
    pub const fn next_attach_id(&mut self) -> u32 {
        self.session.next_attach_id()
    }

    /// Encode `frame` and write it to the server.
    ///
    /// # Errors
    ///
    /// The write failure; under QUIC multistream also a Terminal frame for a
    /// Terminal that was never bound.
    pub async fn send(&mut self, frame: &FrameKind) -> Result<(), AttachError> {
        Ok(self.session.send(frame).await?)
    }

    /// Whether a write failed because the server had already closed the
    /// connection; `recv` then names the ending (see
    /// [`Session::write_hit_closed_peer`]).
    #[must_use]
    pub const fn write_hit_closed_peer(&self) -> bool {
        self.session.write_hit_closed_peer()
    }

    /// Whether a Terminal-targeted frame for `terminal_id` can be sent now
    /// (see [`Session::can_route_terminal`]).
    #[must_use]
    pub fn can_route_terminal(&self, terminal_id: &ResourceId) -> bool {
        self.session.can_route_terminal(terminal_id)
    }

    /// Whether this negotiated QUIC connection carries per-Terminal streams.
    #[must_use]
    pub fn multistream_enabled(&self) -> bool {
        self.session.multistream_enabled()
    }

    /// Open and bind a QUIC stream for `terminal_id` (a no-op when multistream
    /// is off or it is already bound).
    ///
    /// # Errors
    ///
    /// Past the stream cap, or when the stream cannot be opened or bound.
    pub async fn bind_terminal(&mut self, terminal_id: &ResourceId) -> Result<(), AttachError> {
        Ok(self.session.bind_terminal(terminal_id).await?)
    }

    /// Finish and forget one Terminal stream, if it is bound.
    pub fn unbind_terminal(&mut self, terminal_id: &ResourceId) {
        self.session.unbind_terminal(terminal_id);
    }

    /// Finish all client-opened Terminal streams.
    pub fn unbind_all_terminals(&mut self) {
        self.session.unbind_all_terminals();
    }

    /// Accumulate sends until [`Self::uncork`], so one input batch costs one
    /// write. Callers MUST uncork on every exit path, errors included.
    pub fn cork(&mut self) {
        self.session.cork();
    }

    /// Ship everything corked since [`Self::cork`], in send order, in one
    /// write. A no-op if nothing was sent or the transport does not cork.
    ///
    /// # Errors
    ///
    /// The batch's write failure.
    pub async fn uncork(&mut self) -> Result<(), AttachError> {
        Ok(self.session.uncork().await?)
    }

    /// Read the next frame from the server (see [`Session::recv`]).
    ///
    /// # Errors
    ///
    /// [`AttachError::Disconnected`] on a clean close; transport, framing,
    /// and decode failures otherwise.
    pub async fn recv(&mut self) -> Result<FrameKind, AttachError> {
        Ok(self.session.recv().await?)
    }

    /// A frame that is already available without waiting, or `Ok(None)`, so
    /// the attach loop can drain a burst into one paint. A clean close is
    /// reported by the next [`Self::recv`].
    ///
    /// # Errors
    ///
    /// As [`Self::recv`].
    pub fn try_recv(&mut self) -> Result<Option<FrameKind>, AttachError> {
        Ok(self.session.try_recv()?)
    }

    /// Send one `COMMAND` and wait for its reply, keeping every frame the
    /// server interleaved ahead of it (see [`Session::request`]).
    ///
    /// # Errors
    ///
    /// Transport and decode failures; a server that closes without replying
    /// is [`AttachError::Disconnected`].
    pub async fn request(
        &mut self,
        request_id: u32,
        command: Command,
    ) -> Result<Reply, AttachError> {
        Ok(self.session.request(request_id, command).await?)
    }

    /// Send one `GET_METADATA` and wait for its `METADATA_VALUE`. A refusal
    /// is an `Err` answer, distinct from an unset key (`Ok(None)`).
    ///
    /// # Errors
    ///
    /// Transport and decode failures.
    pub async fn request_metadata(
        &mut self,
        request_id: u32,
        scope: Scope,
        key: String,
    ) -> Result<Reply<Answer<Option<Vec<u8>>>>, AttachError> {
        Ok(self
            .session
            .request_metadata(request_id, scope, key)
            .await?)
    }

    /// One `GET_METADATA` per `(scope, key)`, pipelined [`METADATA_WINDOW`]
    /// at a time; answers come back in `reads` order.
    ///
    /// # Errors
    ///
    /// Transport and decode failures; a server that closes before answering
    /// every read is [`AttachError::Disconnected`].
    pub async fn request_metadata_many(
        &mut self,
        first_request_id: u32,
        reads: Vec<(Scope, String)>,
    ) -> Result<Reply<Vec<Answer<Option<Vec<u8>>>>>, AttachError> {
        Ok(self
            .session
            .request_metadata_many(first_request_id, reads)
            .await?)
    }

    /// Send one `LIST_METADATA` and wait for its `METADATA_KEYS`.
    ///
    /// # Errors
    ///
    /// Transport and decode failures.
    pub async fn request_metadata_keys(
        &mut self,
        request_id: u32,
        scope: Scope,
    ) -> Result<Reply<Answer<Vec<String>>>, AttachError> {
        Ok(self
            .session
            .request_metadata_keys(request_id, scope)
            .await?)
    }

    /// Send one `SPAWN_RESOURCE` and wait for its `RESOURCE_SPAWNED`.
    ///
    /// # Errors
    ///
    /// [`AttachError::Protocol`] when `frame` is not a `SPAWN_RESOURCE`;
    /// otherwise transport and decode failures.
    pub async fn request_spawn(
        &mut self,
        frame: &FrameKind,
    ) -> Result<Reply<Answer<SpawnResult>>, AttachError> {
        Ok(self.session.request_spawn(frame).await?)
    }

    /// Send one `MOVE_RESOURCE` and wait for its `RESOURCE_MOVED`
    /// (ADR-0056).
    ///
    /// # Errors
    ///
    /// [`AttachError::Protocol`] when `frame` is not a `MOVE_RESOURCE`;
    /// otherwise transport and decode failures.
    pub async fn request_move(
        &mut self,
        frame: &FrameKind,
    ) -> Result<Reply<Answer<MoveResult>>, AttachError> {
        Ok(self.session.request_move(frame).await?)
    }
}
