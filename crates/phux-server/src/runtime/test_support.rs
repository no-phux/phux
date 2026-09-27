//! Wire-level fixtures shared by the authorization conformance suites
//! (`scope_matrix`, `approval_matrix`, `revocation_conformance`): each drives
//! the real client loop over an in-memory transport.

#![allow(clippy::expect_used, reason = "tests")]

use std::cell::{Cell, RefCell};
use std::io;
use std::rc::Rc;

use bytes::BytesMut;
use chrono::Utc;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, LayerSet};
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::wire::frame::FrameKind;
use tokio::sync::mpsc;

use crate::auth::{AuthenticatedCredential, ConnectionIdentity};
use crate::transport::{FrameReader, FrameWriter};

/// Frames a test feeds one connection. When every sender is gone the peer
/// either hangs up (`eof_on_close`) or goes silent.
pub(super) struct Feed {
    rx: mpsc::UnboundedReceiver<BytesMut>,
    eof_on_close: bool,
    _script: Option<mpsc::UnboundedSender<BytesMut>>,
}

impl Feed {
    /// A feed the test writes to; dropping the sender is silence.
    pub(super) const fn silent_on_close(rx: mpsc::UnboundedReceiver<BytesMut>) -> Self {
        Self {
            rx,
            eof_on_close: false,
            _script: None,
        }
    }

    /// A feed the test writes to; dropping the sender is the peer's EOF.
    pub(super) const fn eof_on_close(rx: mpsc::UnboundedReceiver<BytesMut>) -> Self {
        Self {
            rx,
            eof_on_close: true,
            _script: None,
        }
    }

    /// `frames`, then silence.
    pub(super) fn scripted(frames: Vec<BytesMut>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        for frame in frames {
            tx.send(frame).expect("receiver alive");
        }
        Self {
            rx,
            eof_on_close: false,
            _script: Some(tx),
        }
    }
}

impl FrameReader for Feed {
    async fn read_frame(&mut self) -> io::Result<Option<BytesMut>> {
        match self.rx.recv().await {
            Some(frame) => Ok(Some(frame)),
            None if self.eof_on_close => Ok(None),
            None => std::future::pending().await,
        }
    }
}

/// Everything the server writes to one connection, decoded, and whether it
/// closed.
#[derive(Clone, Default)]
pub(super) struct Wire {
    pub(super) frames: Rc<RefCell<Vec<FrameKind>>>,
    pub(super) closed: Rc<Cell<bool>>,
}

#[allow(
    clippy::unused_async_trait_impl,
    reason = "the recording test writer implements the production async transport trait without I/O"
)]
impl FrameWriter for Wire {
    async fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        let decoded = FrameKind::decode(frame).expect("server frame").0;
        self.frames.borrow_mut().push(decoded);
        Ok(())
    }

    async fn close(&mut self) -> io::Result<()> {
        self.closed.set(true);
        Ok(())
    }
}

pub(super) fn encode(frame: &FrameKind) -> BytesMut {
    let mut out = BytesMut::new();
    frame.encode(&mut out);
    out
}

pub(super) fn hello(client_name: &str) -> FrameKind {
    FrameKind::Hello {
        client_name: client_name.to_owned(),
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        client_caps: ClientCapabilities::new().with_layers(LayerSet::all()),
    }
}

/// The owner's socket: the serving uid over the Unix socket.
pub(super) fn owner_peer() -> PeerIdentity {
    PeerIdentity {
        uid: nix::unistd::geteuid().as_raw(),
        pid: None,
        exe_path: None,
        mcp_host_key: None,
        transport: TransportType::UnixSocket,
        source_addr: None,
    }
}

/// A workload's QUIC connection with its certificate verified. `cached`
/// is what the transport cached on the credential; the grant must always
/// come from the engine instead.
pub(super) fn workload_identity(id: &str, cached: &[&str]) -> ConnectionIdentity {
    ConnectionIdentity {
        peer: PeerIdentity {
            uid: 0,
            pid: None,
            exe_path: None,
            mcp_host_key: Some(id.to_owned()),
            transport: TransportType::Quic,
            source_addr: None,
        },
        credential: Some(AuthenticatedCredential {
            id: id.to_owned(),
            principal: id.to_owned(),
            scopes: cached.iter().map(|scope| (*scope).to_owned()).collect(),
            issued_at: Utc::now(),
            expires_at: None,
            generation: 1,
            registry_instance: None,
        }),
        ssh_origin: None,
        bearer: None,
    }
}
