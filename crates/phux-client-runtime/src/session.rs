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
use phux_protocol::caps::{BootstrapLimits, ClientCapabilities, Layer, LayerSet, ServerFeature};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{FrameKind, RolePolicy};
use tokio::net::UnixStream;

use crate::connection::establish;
use crate::connection::io::Io;
use crate::dial::DIAL_TIMEOUT;
pub use crate::dial::Dial;
pub use crate::handshake::NegotiatedBootstrap;
use crate::handshake::{self, HelloOk, ServerInfo};
use multistream::Multistream;

mod error;
mod framed;
mod multistream;
mod reply;
mod requests;

pub use error::SessionError;
pub use reply::{Answer, Refusal, Reply, error_code_label, unexpected_reply};
pub use requests::METADATA_WINDOW;

use framed::decode_frame;
#[cfg(test)]
use {
    crate::control::encode,
    framed::{check_barrier_bound, is_pong},
    multistream::MUX_FRAME_CHANNEL,
    phux_protocol::wire::framing,
};

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
    /// adds per-Terminal streams (docs/protocol.md#quic-stream-routing).
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

// ----- QUIC per-Terminal streams ------------------------------------------

impl Session {
    /// Whether this negotiated QUIC session carries per-Terminal streams:
    /// a plain QUIC link (never UDS, WebSocket, or a relayed end-to-end
    /// session, docs/protocol.md#quic-stream-routing) whose `HELLO_OK` advertised `QUIC_STREAMS`.
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

#[cfg(test)]
mod newer_server_tests;
