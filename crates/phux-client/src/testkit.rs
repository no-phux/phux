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

use bytes::{Bytes, BytesMut};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapStreamProfile, ServerCapabilities, ServerFeatureSet,
    select_bootstrap_profile,
};
use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, ErrorCode, FrameKind, MoveResult, Scope, SpawnResult,
};
use phux_protocol::wire::info::SessionSnapshot;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use phux_protocol::wire::framing::{self, LENGTH_PREFIX_LEN as LENGTH_PREFIX};

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

impl ScriptSpec {
    /// An empty script: handshake answered, every command acked `Ok`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The synthesized raw-VT bootstrap the server pushes before the
    /// `ATTACH_RESOURCE` acknowledgement.
    #[must_use]
    pub fn priming_snapshot(
        mut self,
        terminal: &ResourceId,
        cols: u16,
        rows: u16,
        replay: &[u8],
    ) -> Self {
        let stream_id = FIXTURE_STREAM_ID;
        let bootstrap_id = FIXTURE_BOOTSTRAP_ID;
        self.priming = vec![
            FrameKind::BootstrapBegin {
                terminal_id: terminal.clone(),
                stream_id,
                bootstrap_id,
                profile: BootstrapStreamProfile::SynthesizedVtRaw,
                cols,
                rows,
                base_seq: 0,
            },
            FrameKind::BootstrapChunk {
                terminal_id: terminal.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: Bytes::copy_from_slice(replay),
            },
            FrameKind::BootstrapReady {
                terminal_id: terminal.clone(),
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
        ];
        self
    }

    /// The retained `AgentEventsJsonlV1` transcript pushed before the
    /// `ATTACH_RESOURCE` acknowledgement of an `AgentSession` resource.
    ///
    /// A 0x0 JSONL-profile `BOOTSTRAP_BEGIN`, one `BOOTSTRAP_CHUNK` per
    /// line, then `READY`.
    ///
    /// # Panics
    ///
    /// On more than `u32::MAX` lines, which no fixture has.
    #[must_use]
    pub fn agent_log_bootstrap(mut self, session: &ResourceId, lines: &[&str]) -> Self {
        let stream_id = FIXTURE_STREAM_ID;
        let bootstrap_id = FIXTURE_BOOTSTRAP_ID;
        self.priming = vec![FrameKind::BootstrapBegin {
            terminal_id: session.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::AgentEventsJsonlV1,
            cols: 0,
            rows: 0,
            base_seq: 0,
        }];
        for (index, line) in lines.iter().enumerate() {
            let mut payload = line.as_bytes().to_vec();
            payload.push(b'\n');
            self.priming.push(FrameKind::BootstrapChunk {
                terminal_id: session.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: u32::try_from(index).expect("fixture chunk count fits u32"),
                payload: Bytes::from(payload),
            });
        }
        self.priming.push(FrameKind::BootstrapReady {
            terminal_id: session.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        });
        self
    }

    /// The `COMMAND_RESULT` every `APPEND_RESOURCE_OUTPUT` is answered with:
    /// `OkWith(Json({"seq", "ts_ms"}))` for the stamped header, or
    /// `Error { code, .. }` for one of the typed refusals.
    #[must_use]
    pub fn append_result(mut self, result: CommandResult) -> Self {
        self.append_result = Some(result);
        self
    }

    /// The additive features `HELLO_OK` advertises.
    #[must_use]
    pub const fn server_features(mut self, features: ServerFeatureSet) -> Self {
        self.server_features = features;
        self
    }

    /// The `HELLO_OK.server_id` the scripted server names itself with: the
    /// incarnation a journal cursor belongs to (ADR-0123). Empty by default,
    /// which names no incarnation, so no cursor is issued.
    #[must_use]
    pub fn server_id(mut self, server_id: Vec<u8>) -> Self {
        self.server_id = server_id;
        self
    }

    /// Frames pushed right after the next `GET_STATE` ack (one batch per
    /// call): an event that happened after the snapshot was cut.
    #[must_use]
    pub fn push_after_state(mut self, frames: Vec<FrameKind>) -> Self {
        self.after_state.push_back(frames);
        self
    }

    /// The JSON a `GET_TERMINAL_STATE` is answered with (else a bare `Ok`:
    /// no process facet).
    #[must_use]
    pub fn terminal_state(mut self, json: &serde_json::Value) -> Self {
        self.terminal_state = Some(json.to_string());
        self
    }

    /// The snapshot a `GET_STATE` ack carries.
    #[must_use]
    pub fn state(mut self, snapshot: SessionSnapshot) -> Self {
        self.state = Some(snapshot);
        self
    }

    /// Snapshots returned in order by successive `GET_STATE` commands.
    #[must_use]
    pub fn states(mut self, snapshots: impl IntoIterator<Item = SessionSnapshot>) -> Self {
        self.states = snapshots.into_iter().collect();
        self
    }

    /// A hub's per-satellite degradation notice: an uncorrelated `ERROR`
    /// pushed ahead of the next command ack.
    #[must_use]
    pub fn degradation_notice(mut self, message: &str) -> Self {
        self.pre_ack.push(FrameKind::Error {
            request_id: None,
            code: ErrorCode::UnsupportedSatelliteRoute,
            message: message.to_owned(),
        });
        self
    }

    /// A `COMMAND_RESULT` for some *other* pipelined request, pushed ahead
    /// of the next ack. Belongs to that request's correlation and must
    /// survive this one's wait.
    #[must_use]
    pub fn foreign_ack(mut self, request_id: u32) -> Self {
        self.pre_ack.push(FrameKind::CommandResult {
            request_id,
            result: CommandResult::Ok,
        });
        self
    }

    /// A `METADATA_VALUE` for some *other* pipelined request, pushed ahead
    /// of the next correlated reply — the metadata-path twin of
    /// [`Self::foreign_ack`].
    #[must_use]
    pub fn foreign_metadata_value(mut self, request_id: u32) -> Self {
        self.pre_ack.push(FrameKind::MetadataValue {
            request_id,
            value: None,
        });
        self
    }

    /// Seed the stored value for one (scope, key), as if a prior writer had
    /// set it. Prefer this over [`Self::metadata`] when the client also
    /// writes the key.
    #[must_use]
    pub fn stored_metadata(mut self, scope: Scope, key: &str, value: Vec<u8>) -> Self {
        self.metadata_store.push((scope, key.to_owned(), value));
        self
    }

    /// Answer `GET_METADATA` with `responder(scope, key)` for keys the
    /// session has not itself stored.
    #[must_use]
    pub fn metadata(
        mut self,
        responder: impl FnMut(&Scope, &str) -> Option<Vec<u8>> + Send + 'static,
    ) -> Self {
        self.metadata = Some(Box::new(responder));
        self
    }

    /// Refuse every metadata request with a correlated `ERROR` (L1 §5).
    #[must_use]
    pub fn refuse_metadata(mut self, code: ErrorCode, message: &str) -> Self {
        self.metadata_error = Some((code, message.to_owned()));
        self
    }

    /// Make `SET_METADATA` on this `(scope, key)` a silent no-op, as an
    /// oversized value or a racing writer looks from this session. Pair
    /// with [`Self::stored_metadata`] to seed what a confirming read sees.
    #[must_use]
    pub fn drop_metadata_writes(mut self, scope: Scope, key: &str) -> Self {
        self.dropped_metadata_writes.insert((scope, key.to_owned()));
        self
    }

    /// The `RESOURCE_SPAWNED` payload a `SPAWN_RESOURCE` is answered with;
    /// mandatory for a script whose client spawns.
    #[must_use]
    pub fn spawn_result(mut self, result: SpawnResult) -> Self {
        self.spawn = Some(result);
        self
    }

    /// The result returned for every `MOVE_RESOURCE` request.
    #[must_use]
    pub fn move_result(mut self, result: MoveResult) -> Self {
        self.move_result = Some(result);
        self
    }

    /// Refuse every `GET_STATE` with a *correlated* `ERROR`: a scoped
    /// workload's inventory read that dispatch denies (workload-auth §7).
    #[must_use]
    pub fn refuse_state(mut self, code: ErrorCode, message: &str) -> Self {
        self.state_error = Some((code, message.to_owned()));
        self
    }

    /// Refuse every `SPAWN_RESOURCE` with a correlated `ERROR`, as a hub
    /// relays a satellite's refusal.
    #[must_use]
    pub fn refuse_spawn(mut self, code: ErrorCode, message: &str) -> Self {
        self.spawn_error = Some((code, message.to_owned()));
        self
    }

    /// The client count a `DetachClients` command acks with. Defaults to `0`
    /// when unset, matching a real server that matched nobody.
    #[must_use]
    pub const fn detach_result(mut self, count: u64) -> Self {
        self.detach_result = Some(count);
        self
    }

    /// Never answer `GET_SCREEN`: a deliberately wedged peer a deadline must
    /// survive. Everything else is still answered.
    #[must_use]
    pub const fn wedge_screen_reads(mut self) -> Self {
        self.wedge_screen_reads = true;
        self
    }

    /// The screen a `GET_SCREEN` is answered with (else a bare `Ok`, which
    /// the client refuses).
    ///
    /// # Panics
    ///
    /// If `screen` does not serialize.
    #[must_use]
    pub fn screen(mut self, screen: &phux_core::screen::ScreenState) -> Self {
        self.screen = Some(serde_json::to_string(screen).expect("ScreenState serializes"));
        self
    }

    /// Acknowledge the first `acked` `ROUTE_INPUT`s, then go silent: a
    /// command line that lands without its Enter.
    #[must_use]
    pub const fn wedge_input_after(mut self, acked: usize) -> Self {
        self.input_acks_left = Some(acked);
        self
    }

    /// Append one frame to the stream pushed after `SUBSCRIBE_EVENTS`.
    #[must_use]
    pub fn push(mut self, frame: FrameKind) -> Self {
        self.script.push(frame);
        self
    }

    /// Append several frames to the post-subscription stream.
    #[must_use]
    pub fn extend(mut self, frames: impl IntoIterator<Item = FrameKind>) -> Self {
        self.script.extend(frames);
        self
    }

    /// Append frames released only once the client subscribes to this exact
    /// `(scope, key)`, so a client watching the wrong key sees nothing.
    #[must_use]
    pub fn push_after_subscribe(
        mut self,
        scope: Scope,
        key: impl Into<String>,
        frames: Vec<FrameKind>,
    ) -> Self {
        self.keyed_script.push((scope, key.into(), frames));
        self
    }

    /// What the server does once the script is played.
    #[must_use]
    pub const fn end(mut self, end: EndOfScript) -> Self {
        self.end = end;
        self
    }

    /// Whether the pushed script contains a frame the reference server only
    /// fans out to a metadata subscriber, so [`ScriptedServer::run`] knows
    /// which `SUBSCRIBE_*` unlocks it.
    fn script_needs_metadata_subscription(&self) -> bool {
        // A keyed script must hold the hang-up open past SUBSCRIBE_EVENTS.
        !self.keyed_script.is_empty()
            || self
                .script
                .iter()
                .any(|frame| matches!(frame, FrameKind::MetadataChanged { .. }))
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

/// The frames the reference server emits in response to one client frame,
/// **in the exact order it emits them**.
///
/// The only place test code decides what precedes an ack; a new arm cites
/// the server handler that justifies its ordering.
///
/// # Panics
///
/// On `ATTACH_RESOURCE` when the spec declared no priming snapshot — see
/// [`ScriptedServer::run`].
fn reference_reply(frame: &FrameKind, spec: &mut ScriptSpec) -> Vec<FrameKind> {
    match frame {
        FrameKind::Hello { client_caps, .. } => {
            let (selected_profile, bootstrap_limits) =
                select_bootstrap_profile(client_caps, &BootstrapCapabilities::new())
                    .expect("default scripted server and client share a bootstrap profile");
            vec![FrameKind::HelloOk {
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                server_caps: ServerCapabilities::new().with_features(spec.server_features),
                server_id: spec.server_id.clone(),
                selected_profile,
                bootstrap_limits,
            }]
        }
        FrameKind::Command {
            request_id,
            command,
        } => command_reply(*request_id, command, spec),
        // `handle_subscribe_events` / `handle_subscribe_metadata` register
        // and reply with nothing; an ack would teach a client to wait forever.
        #[allow(
            clippy::match_same_arms,
            reason = "documents an ordering fact, not a fallthrough"
        )]
        FrameKind::SubscribeEvents { .. } | FrameKind::SubscribeMetadata { .. } => Vec::new(),
        FrameKind::GetMetadata {
            request_id,
            scope,
            key,
        } => {
            let mut out = std::mem::take(&mut spec.pre_ack);
            out.push(metadata_reply(*request_id, scope, key, spec));
            out
        }
        FrameKind::SetMetadata {
            request_id,
            scope,
            key,
            value,
        } => set_metadata_reply(*request_id, scope, key, value, spec),
        FrameKind::DeleteMetadata { scope, key, .. } => {
            spec.metadata_store
                .retain(|(stored_scope, stored_key, _)| stored_scope != scope || stored_key != key);
            Vec::new()
        }
        // `handle_spawn_terminal`: `RESOURCE_SPAWNED` after anything queued,
        // or a hub-relayed correlated `ERROR`.
        FrameKind::SpawnResource { request_id, .. } => {
            let mut out = std::mem::take(&mut spec.pre_ack);
            if let Some((code, message)) = spec.spawn_error.clone() {
                out.push(FrameKind::Error {
                    request_id: Some(*request_id),
                    code,
                    message,
                });
                return out;
            }
            let result = spec.spawn.clone().expect(
                "a scripted server whose client sends SPAWN_RESOURCE must declare an \
                 outcome: handle_spawn_terminal always answers. Call \
                 ScriptSpec::spawn_result or ScriptSpec::refuse_spawn.",
            );
            out.push(FrameKind::ResourceSpawned {
                request_id: *request_id,
                result,
            });
            out
        }
        FrameKind::MoveResource { request_id, .. } => vec![FrameKind::ResourceMoved {
            request_id: *request_id,
            result: spec.move_result.clone().expect(
                "a scripted server whose client sends MOVE_RESOURCE must declare an outcome",
            ),
        }],
        // Answered by the real server but not modelled yet: fail loudly
        // rather than wedge the client with silence.
        FrameKind::Attach { .. } | FrameKind::ListMetadata { .. } => {
            panic!(
                "the scripted server has no reference ordering for {frame:?} yet; add an \
                 arm to `reference_reply` citing the server handler"
            )
        }
        // Everything else is fire-and-forget on this wire.
        _ => Vec::new(),
    }
}

/// `SET_METADATA` has no reply (`handle_set_metadata`); the write is stored
/// unless pinned by [`ScriptSpec::drop_metadata_writes`], so a confirming
/// read sees it.
fn set_metadata_reply(
    request_id: u32,
    scope: &Scope,
    key: &str,
    value: &[u8],
    spec: &mut ScriptSpec,
) -> Vec<FrameKind> {
    if let Some((code, message)) = spec.metadata_error.clone() {
        return vec![FrameKind::Error {
            request_id: Some(request_id),
            code,
            message,
        }];
    }
    if spec
        .dropped_metadata_writes
        .contains(&(scope.clone(), key.to_owned()))
    {
        return Vec::new();
    }
    spec.metadata_store
        .retain(|(s, k, _)| !(s == scope && k == key));
    spec.metadata_store
        .push((scope.clone(), key.to_owned(), value.to_vec()));
    Vec::new()
}

/// The reply sequence for one `COMMAND`, pre-ack pushes first.
fn command_reply(request_id: u32, command: &Command, spec: &mut ScriptSpec) -> Vec<FrameKind> {
    // Every command ack is preceded by whatever was queued.
    let mut out = std::mem::take(&mut spec.pre_ack);
    match command {
        Command::AttachResource { .. } => {
            assert!(
                !spec.priming.is_empty(),
                "a scripted server whose client sends ATTACH_RESOURCE must declare \
                 priming bootstrap frames. Call ScriptSpec::priming_snapshot."
            );
            out.extend(spec.priming.clone());
            out.push(FrameKind::CommandResult {
                request_id,
                result: CommandResult::Ok,
            });
        }
        Command::GetState { .. } => {
            let snapshot = spec.states.pop_front().or_else(|| spec.state.clone());
            let result = match (&spec.state_error, snapshot) {
                (Some((code, message)), _) => CommandResult::Error {
                    code: *code,
                    message: message.clone(),
                },
                (None, snapshot) => snapshot.map_or(CommandResult::Ok, |snapshot| {
                    CommandResult::OkWith(CommandValue::State(snapshot))
                }),
            };
            out.push(FrameKind::CommandResult { request_id, result });
            // An event that happened after the snapshot was cut: see
            // `ScriptSpec::push_after_state`.
            if let Some(batch) = spec.after_state.pop_front() {
                out.extend(batch);
            }
        }
        // `handle_get_terminal_state` answers `OK_WITH(JSON(..))`.
        Command::GetTerminalState { .. } if spec.terminal_state.is_some() => {
            let json = spec.terminal_state.clone().unwrap_or_default();
            out.push(FrameKind::CommandResult {
                request_id,
                result: CommandResult::OkWith(CommandValue::Json(json)),
            });
        }
        // `handle_detach_clients` always answers `OkWith(Json(count))`.
        Command::DetachClients { .. } => {
            let count = spec.detach_result.unwrap_or(0);
            out.push(FrameKind::CommandResult {
                request_id,
                result: CommandResult::OkWith(CommandValue::Json(count.to_string())),
            });
        }
        // `APPEND_RESOURCE_OUTPUT`: the test supplies the result.
        Command::AppendResourceOutput { .. } => {
            let result = spec.append_result.clone().unwrap_or(CommandResult::Ok);
            out.push(FrameKind::CommandResult { request_id, result });
        }
        // A wedged server, on purpose: see `ScriptSpec::wedge_screen_reads`.
        Command::GetScreen { .. } if spec.wedge_screen_reads => {}
        // `handle_get_screen` answers `OK_WITH(JSON(ScreenState))`.
        Command::GetScreen { .. } if spec.screen.is_some() => {
            let json = spec.screen.clone().unwrap_or_default();
            out.push(FrameKind::CommandResult {
                request_id,
                result: CommandResult::OkWith(CommandValue::Json(json)),
            });
        }
        // A wedged server, on purpose: see `ScriptSpec::wedge_input_after`.
        Command::RouteInput { .. } if spec.input_acks_left == Some(0) => {}
        Command::RouteInput { .. } => {
            if let Some(left) = spec.input_acks_left.as_mut() {
                *left -= 1;
            }
            out.push(FrameKind::CommandResult {
                request_id,
                result: CommandResult::Ok,
            });
        }
        _ => out.push(FrameKind::CommandResult {
            request_id,
            result: CommandResult::Ok,
        }),
    }
    out
}

/// The reply to one `GET_METADATA`: a refusal, this session's own stored
/// write, or the spec's canned answer — in that precedence.
fn metadata_reply(request_id: u32, scope: &Scope, key: &str, spec: &mut ScriptSpec) -> FrameKind {
    if let Some((code, message)) = spec.metadata_error.clone() {
        return FrameKind::Error {
            request_id: Some(request_id),
            code,
            message,
        };
    }
    let stored = spec
        .metadata_store
        .iter()
        .find(|(s, k, _)| s == scope && k == key)
        .map(|(_, _, value)| value.clone());
    let value = stored.or_else(|| {
        spec.metadata
            .as_mut()
            .and_then(|responder| responder(scope, key))
    });
    FrameKind::MetadataValue { request_id, value }
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
