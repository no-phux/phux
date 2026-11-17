//! Wire-level fixtures shared by the authorization conformance suites
//! (`scope_matrix`, `approval_matrix`, `revocation_conformance`,
//! `actor_conformance`): each drives the real client loop over an in-memory
//! transport.

use std::cell::{Cell, RefCell};
use std::io;
use std::rc::Rc;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use chrono::Utc;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, LayerSet};
use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::wire::frame::{
    AttachTarget, Command, CommandResult, FrameKind, InputMode, ViewportInfo,
};
use tokio::sync::mpsc;
use tokio::task::{JoinHandle, LocalSet};
use tokio_util::sync::CancellationToken;

use crate::auth::{AuthenticatedCredential, ConnectionIdentity};
use crate::runtime::apps::InputLane;
use crate::runtime::connections::handle_client;
use crate::state::{ClientId, SharedState};
use crate::transport::{FrameReader, FrameWriter};

/// A session whose seed pane runs no process.
pub(super) use crate::runtime::apps::terminal::commands::seed::seed_session_with_actor;
/// The server's input lane alone, without the rest of the apps' startup.
pub(super) use crate::runtime::apps::terminal::input_lane::start as start_input_lane;
/// Server state with the Terminal engine installed.
pub(super) use crate::runtime::apps::terminal::test_support::terminal_state;

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

    /// `frames`, then the peer's EOF.
    pub(super) fn scripted_then_eof(frames: Vec<BytesMut>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        for frame in frames {
            tx.send(frame).expect("receiver alive");
        }
        Self::eof_on_close(rx)
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

/// `COMMAND { request_id, command }`.
pub(super) const fn command(request_id: u32, command: Command) -> FrameKind {
    FrameKind::Command {
        request_id,
        command,
    }
}

pub(super) fn encode(frame: &FrameKind) -> BytesMut {
    let mut out = BytesMut::new();
    frame.encode(&mut out);
    out
}

pub(in crate::runtime) fn hello(client_name: &str) -> FrameKind {
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
            subject: None,
            managed: None,
        }),
        ssh_origin: None,
        bearer: None,
    }
}

/// How long any one expectation may take before the case fails.
pub(super) const PATIENCE: Duration = Duration::from_secs(5);

/// One live connection a test drives through the real client loop.
pub(super) struct Connection {
    pub(super) client: ClientId,
    feed: Option<mpsc::UnboundedSender<BytesMut>>,
    pub(super) wire: Wire,
    pub(super) token: CancellationToken,
    task: JoinHandle<io::Result<()>>,
}

/// Start `client`'s loop on `state` under `root`, without its HELLO. With
/// `eof_on_close`, [`Connection::disconnect`] is the peer hanging up rather
/// than going silent.
pub(super) fn spawn_connection(
    state: &SharedState,
    client: ClientId,
    transport: TransportType,
    root: &CancellationToken,
    lane: Option<InputLane>,
    eof_on_close: bool,
) -> Connection {
    let (feed, rx) = mpsc::unbounded_channel();
    let feed_rx = if eof_on_close {
        Feed::eof_on_close(rx)
    } else {
        Feed::silent_on_close(rx)
    };
    let wire = Wire::default();
    let token = root.child_token();
    let task = tokio::task::spawn_local(handle_client(
        feed_rx,
        wire.clone(),
        state.clone(),
        client,
        token.clone(),
        root.clone(),
        lane,
        transport,
        false,
    ));
    Connection {
        client,
        feed: Some(feed),
        wire,
        token,
        task,
    }
}

impl Connection {
    pub(super) fn send(&self, frame: &FrameKind) {
        if let Some(feed) = &self.feed {
            let _ = feed.send(encode(frame));
        }
    }

    pub(super) fn frames(&self) -> Vec<FrameKind> {
        self.wire.frames.borrow().clone()
    }

    /// Whether `done` holds of the frames within [`PATIENCE`].
    pub(super) async fn until(&self, done: impl Fn(&[FrameKind]) -> bool) -> bool {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if done(&self.wire.frames.borrow()) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        false
    }

    pub(super) async fn wait_for(&self, what: &str, found: impl Fn(&FrameKind) -> bool) {
        assert!(
            self.until(|frames| frames.iter().any(&found)).await,
            "no {what} within {PATIENCE:?}: {:?}",
            self.frames()
        );
    }

    pub(super) async fn hello(&self) {
        self.send(&hello("conformance"));
        self.wait_for("HELLO_OK", |frame| {
            matches!(frame, FrameKind::HelloOk { .. })
        })
        .await;
    }

    /// Attach to the session `alpha`.
    pub(super) async fn attach(&self, attach_id: u32) {
        self.send(&FrameKind::Attach {
            attach_id,
            target: AttachTarget::ByName("alpha".to_owned()),
            viewport: ViewportInfo::new(80, 24),
            request_scrollback: false,
            scrollback_limit_lines: 0,
            role_policy: None,
        });
        self.wait_for("ATTACHED", |frame| {
            matches!(frame, FrameKind::Attached { .. })
        })
        .await;
    }

    /// Every `COMMAND_RESULT` for request `id` so far.
    pub(super) fn results(&self, id: u32) -> Vec<CommandResult> {
        self.frames()
            .into_iter()
            .filter_map(|frame| match frame {
                FrameKind::CommandResult { request_id, result } if request_id == id => Some(result),
                _ => None,
            })
            .collect()
    }

    /// The first `COMMAND_RESULT` for request `id`, once it arrives.
    pub(super) async fn result(&self, id: u32) -> CommandResult {
        self.wait_for("COMMAND_RESULT", |frame| {
            matches!(frame, FrameKind::CommandResult { request_id, .. } if *request_id == id)
        })
        .await;
        self.results(id).remove(0)
    }

    pub(super) async fn command(&self, request_id: u32, command: Command) -> CommandResult {
        self.send(&FrameKind::Command {
            request_id,
            command,
        });
        self.result(request_id).await
    }

    pub(super) async fn command_ok(&self, request_id: u32, command: Command) {
        assert_eq!(self.command(request_id, command).await, CommandResult::Ok);
    }

    pub(super) async fn wait_closed(&self) {
        let deadline = Instant::now() + PATIENCE;
        while !self.wire.closed.get() {
            assert!(
                Instant::now() < deadline,
                "the transport never closed: {:?}",
                self.frames()
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    pub(super) fn is_open(&self) -> bool {
        !self.wire.closed.get()
            && !self
                .frames()
                .iter()
                .any(|frame| matches!(frame, FrameKind::Detached { .. }))
    }

    /// Drop the feed: EOF or silence, as the connection was spawned.
    pub(super) fn disconnect(&mut self) {
        self.feed = None;
    }

    pub(super) async fn finish(self) {
        self.token.cancel();
        let _ = self.task.await;
    }
}

pub(super) fn acquire(terminal_id: &WireResourceId, mode: InputMode) -> Command {
    Command::AcquireInput {
        terminal_id: terminal_id.clone(),
        mode,
        ttl_ms: 0,
    }
}

pub(super) async fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "{what} never held");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Run `case` on a current-thread runtime's `LocalSet`, as the server does.
pub(in crate::runtime) async fn local<T>(case: impl std::future::Future<Output = T>) -> T {
    LocalSet::new().run_until(case).await
}

/// Server state with the Terminal engine, `engine` installed, and one
/// session, `alpha`, whose pane has a live (no-PTY) actor.
pub(super) fn alpha_world(
    engine: std::sync::Arc<dyn crate::policy::PolicyEngine>,
    root: &CancellationToken,
) -> (SharedState, WireResourceId, phux_core::ids::ResourceId) {
    let state = terminal_state();
    let alpha_core = seed_session_with_actor(
        &state,
        "alpha",
        phux_config::ScrollbackLimits::default(),
        root,
    )
    .unwrap();
    let alpha = state.with_mut(|s| {
        s.config_mut().policy_engine = engine;
        s.idspace.intern_resource(alpha_core)
    });
    (state, alpha, alpha_core)
}

/// The code of an error `result`; `None` for any other result.
pub(super) const fn error_code(
    result: &CommandResult,
) -> Option<phux_protocol::wire::frame::ErrorCode> {
    match result {
        CommandResult::Error { code, .. } => Some(*code),
        _ => None,
    }
}

/// Run `f` with an INFO subscriber installed, returning what it logged.
pub(super) fn with_info_log<T>(f: impl FnOnce() -> T) -> (T, String) {
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log").extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> MakeWriter<'a> for Buf {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    let buf = Buf::default();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(buf.clone())
        .finish();
    let out = tracing::subscriber::with_default(subscriber, || {
        tracing::callsite::rebuild_interest_cache();
        f()
    });
    let log = String::from_utf8(buf.0.lock().expect("log").clone()).unwrap();
    (out, log)
}
