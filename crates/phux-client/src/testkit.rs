//! One scripted server for every client-side test, so a fake cannot lie
//! about the protocol.
//!
//! The harness owns frame **order** (in `reference_reply`, each arm citing
//! the server handler it models); a test supplies only **payloads**. So a
//! test can describe any scenario but not an ordering the reference server
//! would not produce. Encoded orderings: the attach bootstrap precedes the
//! `ATTACH_RESOURCE` ack (and is mandatory); a hub's degradation `ERROR`s
//! precede the `GET_STATE` ack; `SUBSCRIBE_EVENTS`, `SUBSCRIBE_METADATA`, and
//! `SET_METADATA` get no reply; `SPAWN_RESOURCE` gets a correlated
//! `RESOURCE_SPAWNED` (mandatory) or `ERROR`.
//!
//! A client-side harness only: behaviour (PTY, layout, federation) is tested
//! against the real `ServerRuntime`.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a test harness asserts by panicking; it is only ever linked \
              into test binaries and the `testkit` feature"
)]

use std::fmt;

use bytes::BytesMut;
use phux_protocol::caps::{ServerFeatureExtSet, ServerFeatureSet};
use phux_protocol::ids::{BootstrapId, StreamId};
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, MoveResult, Scope, SpawnResult,
};
use phux_protocol::wire::info::SessionSnapshot;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use phux_protocol::wire::framing::{self, LENGTH_PREFIX_LEN as LENGTH_PREFIX};

mod org;

pub use org::OrgFixture;

mod reply;
mod spec;

use reply::reference_reply;

const FIXTURE_STREAM_ID: StreamId =
    StreamId::new(1).expect("fixture stream identifier is non-zero");
const FIXTURE_BOOTSTRAP_ID: BootstrapId =
    BootstrapId::new(1).expect("fixture bootstrap identifier is non-zero");

/// What the scripted server does once it has played its script.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EndOfScript {
    /// Keep serving until the client sends `DETACH_RESOURCE` or hangs up.
    ///
    /// The shape that lets a test inspect the *complete* set of client-sent
    /// frames, including a teardown the client emits last.
    #[default]
    ServeUntilDetach,
    /// Drop the connection as soon as the script is played, so the client
    /// observes a transport EOF.
    HangUp,
}

/// Answers a `GET_METADATA` for one (scope, key), payload only.
///
/// `Send` because the harness's futures must stay `Send`; every call site is
/// a closure over plain test data.
type MetadataResponder = Box<dyn FnMut(&Scope, &str) -> Option<Vec<u8>> + Send>;

/// The payloads one scripted session answers with, plus the frames it pushes
/// once the client's subscription is live.
///
/// Order is *not* configurable — see the module docs. Build one with
/// [`ScriptSpec::new`] and the chainable setters.
#[derive(Default)]
pub struct ScriptSpec {
    /// The negotiated bootstrap transcript pushed ahead of the attach ack.
    priming: Vec<FrameKind>,
    /// The snapshot a `GET_STATE` ack carries.
    state: Option<SessionSnapshot>,
    /// Successive snapshots returned by successive `GET_STATE` commands.
    states: std::collections::VecDeque<SessionSnapshot>,
    /// Frames pushed ahead of the *next* command ack — the hub degradation
    /// notices and any foreign-correlation acks the test wants interleaved.
    pre_ack: Vec<FrameKind>,
    /// Answers `GET_METADATA` for keys the session has not itself written;
    /// `None` answers every such key with no value.
    metadata: Option<MetadataResponder>,
    /// Keys this session stored via `SET_METADATA`, read back by a later
    /// `GET_METADATA` (what a read-modify-write caller depends on).
    metadata_store: Vec<(Scope, String, Vec<u8>)>,
    /// When set, every metadata request is refused with a *correlated*
    /// `ERROR` instead of answered.
    metadata_error: Option<(ErrorCode, String)>,
    /// `SET_METADATA` on these `(scope, key)` pairs is silently ignored.
    /// See [`Self::drop_metadata_writes`].
    dropped_metadata_writes: std::collections::HashSet<(Scope, String)>,
    /// The `RESOURCE_SPAWNED` payload a `SPAWN_RESOURCE` is answered with.
    spawn: Option<SpawnResult>,
    /// When set, every `SPAWN_RESOURCE` is refused with a *correlated*
    /// `ERROR` instead of a `RESOURCE_SPAWNED`.
    spawn_error: Option<(ErrorCode, String)>,
    /// When set, every `UPGRADE` is refused with this message. See
    /// [`ScriptSpec::refuse_upgrade`].
    upgrade_error: Option<String>,
    /// The `RESOURCE_MOVED` payload a `MOVE_RESOURCE` is answered with.
    move_result: Option<MoveResult>,
    /// The client count a `DetachClients` command acks with (default `0`).
    detach_result: Option<u64>,
    /// The `COMMAND_RESULT` an `APPEND_RESOURCE_OUTPUT` is answered with
    /// (default a bare `Ok`).
    append_result: Option<CommandResult>,
    /// The features `HELLO_OK` advertises; empty by default, so a verb that
    /// needs a bit must refuse against `new()`.
    server_features: ServerFeatureSet,
    /// The extended features `HELLO_OK` advertises; empty by default.
    server_features_ext: ServerFeatureExtSet,
    /// When set, `GET_SCREEN` is never answered: the connection stays open
    /// and silent. See [`ScriptSpec::wedge_screen_reads`].
    wedge_screen_reads: bool,
    /// The serialized `ScreenState` a `GET_SCREEN` is answered with.
    screen: Option<String>,
    /// How many more `ROUTE_INPUT`s to acknowledge before going silent;
    /// `None` acknowledges every one. See [`ScriptSpec::wedge_input_after`].
    input_acks_left: Option<usize>,
    /// When set, every `GET_STATE` is refused with a *correlated* `ERROR`.
    /// See [`ScriptSpec::refuse_state`].
    state_error: Option<(ErrorCode, String)>,
    /// Batches pushed right after successive `GET_STATE` acks. See
    /// [`ScriptSpec::push_after_state`].
    after_state: std::collections::VecDeque<Vec<FrameKind>>,
    /// The `HELLO_OK.server_id` the scripted server names itself with.
    server_id: Vec<u8>,
    /// The `GET_TERMINAL_STATE` JSON a `GET_TERMINAL_STATE` is answered
    /// with. See [`ScriptSpec::terminal_state`].
    terminal_state: Option<String>,
    /// Pushed once the client's `SUBSCRIBE_EVENTS` registers.
    script: Vec<FrameKind>,
    /// Frames released only when the client subscribes to that exact
    /// `(scope, key)`, as the real fanout does.
    keyed_script: Vec<(Scope, String, Vec<FrameKind>)>,
    /// What to do after the script.
    end: EndOfScript,
    /// The organization every organization command is answered from; see
    /// [`ScriptSpec::organization`].
    organization: Option<OrgFixture>,
    /// Fault-injected payload for mutating organization replies; ordering stays fixed.
    mutation_reply: Option<CommandResult>,
}

impl fmt::Debug for ScriptSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScriptSpec")
            .field("state", &self.state)
            .field("script", &self.script)
            .field("end", &self.end)
            .finish_non_exhaustive()
    }
}

/// A framed server endpoint driving one [`ScriptSpec`] against one client.
#[derive(Debug)]
pub struct ScriptedServer {
    link: FrameLink,
    spec: ScriptSpec,
}

impl ScriptedServer {
    /// Serve `spec` on an already-connected server-side stream — the
    /// `UnixStream::pair` shape.
    #[must_use]
    pub fn on_stream(stream: UnixStream, spec: ScriptSpec) -> Self {
        Self {
            link: FrameLink::new(stream),
            spec,
        }
    }

    /// Accept one connection on `listener` and serve `spec` on it. Borrowed
    /// so one listener can serve a sequence of connections.
    ///
    /// # Panics
    ///
    /// If the accept fails.
    pub async fn accept(listener: &UnixListener, spec: ScriptSpec) -> Vec<FrameKind> {
        let (stream, _) = listener.accept().await.expect("accept scripted client");
        Self::on_stream(stream, spec).run().await
    }

    /// Serve until the script is exhausted and [`EndOfScript`] says to stop,
    /// or the client hangs up. Returns every frame the client sent, in order
    /// — which is what the "never sends ATTACH" style guards inspect.
    ///
    /// Under [`EndOfScript::ServeUntilDetach`] it resolves only once the
    /// client hangs up: drop the `Connection` before joining.
    ///
    /// # Panics
    ///
    /// If the client attaches a Terminal and the script declared no priming
    /// snapshot: the reference server always sends one, so a script without
    /// one models a server that does not exist.
    pub async fn run(mut self) -> Vec<FrameKind> {
        let mut seen = Vec::new();
        while let Some(frame) = self.link.recv().await {
            for reply in reference_reply(&frame, &mut self.spec) {
                self.link.send(&reply).await;
            }
            // The script stands in for fanout, which reaches a client only
            // once the matching subscription registers: `METADATA_CHANGED`
            // waits for `SUBSCRIBE_METADATA`, everything else for events.
            let subscribed = if self.spec.script_needs_metadata_subscription() {
                matches!(frame, FrameKind::SubscribeMetadata { .. })
            } else {
                matches!(frame, FrameKind::SubscribeEvents { .. })
            };
            if subscribed {
                for pushed in std::mem::take(&mut self.spec.script) {
                    self.link.send(&pushed).await;
                }
            }
            // A keyed push is released on the matching subscribe only.
            if let FrameKind::SubscribeMetadata { scope, key } = &frame {
                let mut released: Vec<FrameKind> = Vec::new();
                self.spec.keyed_script.retain(|(s, k, frames)| {
                    if s == scope && k == key {
                        released.extend(frames.iter().cloned());
                        false
                    } else {
                        true
                    }
                });
                for pushed in released {
                    self.link.send(&pushed).await;
                }
            }
            let detached = matches!(
                frame,
                FrameKind::Command {
                    command: Command::DetachResource { .. },
                    ..
                }
            );
            seen.push(frame);
            if detached {
                break;
            }
            if subscribed && self.spec.end == EndOfScript::HangUp {
                // Half-close so the client sees a clean EOF rather than
                // ECONNRESET from unread frames; give one racing frame a
                // bounded chance to land, without waiting for the close.
                self.link.close_output().await;
                if let Ok(Some(frame)) =
                    tokio::time::timeout(std::time::Duration::from_millis(50), self.link.recv())
                        .await
                {
                    seen.push(frame);
                }
                break;
            }
        }
        seen
    }
}

/// Bind `phux.sock` in `dir` and serve `spec` to the one client that dials
/// it. Returns the socket path and the server task, which yields every frame
/// the client sent.
///
/// # Panics
///
/// If the bind fails.
#[must_use]
pub fn serve_one(
    dir: &std::path::Path,
    spec: ScriptSpec,
) -> (std::path::PathBuf, tokio::task::JoinHandle<Vec<FrameKind>>) {
    let socket = dir.join("phux.sock");
    let listener = UnixListener::bind(&socket).expect("bind scripted server");
    let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });
    (socket, server)
}

/// Serve every connection `listener` accepts with a fresh spec from
/// `make_spec`, until the task is dropped.
///
/// For clients that dial more than once in one operation (`phux run`).
///
/// # Panics
///
/// If an accept fails.
pub async fn serve_every(
    listener: UnixListener,
    make_spec: impl Fn() -> ScriptSpec + Send + 'static,
) {
    loop {
        let (stream, _) = listener.accept().await.expect("accept scripted client");
        tokio::spawn(ScriptedServer::on_stream(stream, make_spec()).run());
    }
}

/// Accept every connection and never read from or write to it: a peer that
/// is listening but never answers `HELLO`.
///
/// The stalled peer a client deadline must survive: silence, not EOF.
///
/// # Panics
///
/// If an accept fails.
pub async fn hold_silent(listener: UnixListener) {
    loop {
        let (stream, _) = listener.accept().await.expect("accept stalled client");
        tokio::spawn(async move {
            let _held_open = stream;
            std::future::pending::<()>().await;
        });
    }
}

/// Length-prefixed SPEC §5 frame I/O over the server half of a `UnixStream`.
#[derive(Debug)]
struct FrameLink {
    stream: UnixStream,
    out: BytesMut,
}

impl FrameLink {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            out: BytesMut::new(),
        }
    }

    /// The next client frame, or `None` on a clean EOF.
    async fn recv(&mut self) -> Option<FrameKind> {
        let mut header = [0_u8; LENGTH_PREFIX];
        // EOF at a frame boundary is a hang-up; anything else fails loudly.
        match self.stream.read_exact(&mut header).await {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return None,
            Err(err) => panic!("scripted server failed reading a frame header: {err}"),
        }
        let mut encoded = framing::frame_buffer(header).expect("client sent a valid frame header");
        self.stream
            .read_exact(&mut encoded[LENGTH_PREFIX..])
            .await
            .expect("scripted server failed reading a frame body");
        let (frame, tail) = FrameKind::decode(&encoded).expect("client sent an undecodable frame");
        assert!(tail.is_empty(), "trailing bytes after a client frame");
        Some(frame)
    }

    /// Write one frame. A client that has already gone away is not an error
    /// — `HangUp` scripts and duration-capped captures both end that way.
    async fn send(&mut self, frame: &FrameKind) {
        self.out.clear();
        frame.encode(&mut self.out);
        if self.stream.write_all(&self.out).await.is_err() {
            return;
        }
        let _ = self.stream.flush().await;
    }

    /// Produce a clean EOF while keeping the read side alive long enough to
    /// drain a client frame that raced with the scripted hang-up.
    async fn close_output(&mut self) {
        self.stream
            .shutdown()
            .await
            .expect("scripted server failed shutting down its write side");
    }
}
