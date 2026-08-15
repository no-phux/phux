//! One scripted server for every client-side test, so a fake cannot lie
//! about the protocol.
//!
//! # Why this module exists
//!
//! Before it, every module that needed a server stood up its own fake, and
//! each fake encoded what its author *believed* the server does. That is the
//! same belief that produced the bug the test is supposed to catch, so the
//! fake catches nothing. The concrete failure: seventeen `phux rec` unit
//! tests passed against a completely non-functional feature, because the
//! local fake answered `COMMAND_RESULT` *before* the priming bootstrap
//! transcript while the reference server does the opposite. Every capture
//! against a real server came back as a 0x0 grid with nothing playable in it,
//! and the suite was green throughout.
//!
//! The fix is not "write better fakes". It is to have exactly one place
//! where the server's frame *order* is written down — the private
//! `reference_reply` function below — and to make every test route through it.
//!
//! # What is owned here versus supplied by the test
//!
//! The harness owns **order**. A test supplies **payloads**:
//!
//! - the priming snapshot's geometry and replay bytes, but never whether it
//!   precedes the ack (it always does);
//! - the `GET_STATE` snapshot, but never whether a hub's degradation
//!   `ERROR`s precede the ack (they always do);
//! - metadata values, but never their correlation.
//!
//! That split is the whole point: a test can still describe any scenario,
//! and cannot describe an ordering the reference server would not produce.
//!
//! # The orderings encoded, with their reference-server citations
//!
//! - `ATTACH_TERMINAL` — `handle_attach_terminal`
//!   (`crates/phux-server/src/runtime/commands.rs`) pushes the authoritative
//!   bootstrap transcript before the acknowledgement and never re-sends it.
//!   [`ScriptSpec::priming_snapshot`] is therefore **mandatory** for any script
//!   whose client attaches a Terminal: a script without one panics rather than
//!   silently modelling a
//!   server that does not exist.
//! - `GET_STATE` — `handle_get_state_federated` emits one uncorrelated
//!   `ERROR` per unreachable satellite *ahead of* the merged snapshot's ack,
//!   deliberately ("observable degradation, not silence").
//! - `SUBSCRIBE_EVENTS` — `handle_subscribe_events`
//!   (`crates/phux-server/src/runtime/client.rs`) registers the subscription
//!   and sends **no reply at all**. A fake that acked it would teach the
//!   client to wait for a frame that never comes.
//! - `SUBSCRIBE_METADATA` — `handle_subscribe_metadata`
//!   (`crates/phux-server/src/runtime/client.rs`) likewise registers and
//!   replies with nothing, and captures the connection's mailbox so fanout
//!   reaches a consumer that never attached. A script carrying a
//!   `METADATA_CHANGED` therefore waits for *this* frame rather than
//!   `SUBSCRIBE_EVENTS` before it plays.
//! - `HELLO` — answered with `HELLO_OK` echoing this build's
//!   [`PROTOCOL_VERSION`].
//! - `GET_METADATA` / `SET_METADATA` — correlated `METADATA_VALUE` /
//!   `COMMAND_RESULT` on the caller's `request_id`.
//! - `SPAWN_TERMINAL` — `handle_spawn_terminal`
//!   (`crates/phux-server/src/runtime/client.rs`) answers with
//!   `TERMINAL_SPAWNED` on the caller's `request_id`, behind any frames
//!   already queued for the connection. [`ScriptSpec::spawn_result`] is
//!   therefore **mandatory** for a script whose client spawns.
//!   [`ScriptSpec::refuse_spawn`] models the other legal answer: the
//!   correlated `ERROR` a hub surfaces when a satellite refuses the relayed
//!   spawn (`crates/phux-server/src/hub/relay.rs`, `handle_inbound`).
//!
//! # Scope
//!
//! Deliberately a *client-side* harness: it speaks the server half of a
//! `UnixStream` with [`phux_protocol`]'s codec and nothing else. It is not a
//! second implementation of the server — for behaviour (PTY, layout,
//! federation) the integration tests drive the real `ServerRuntime`. This
//! exists for the unit tests that must not pay for a PTY, and whose only
//! previous alternative was a hand-written fake.

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
    BootstrapCapabilities, BootstrapStreamProfile, ServerCapabilities, select_bootstrap_profile,
};
use phux_protocol::ids::{BootstrapId, StreamId, TerminalId};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, ErrorCode, FrameKind, Scope, SpawnResult,
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
    /// Keep serving until the client sends `DETACH_TERMINAL` or hangs up.
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
    /// Frames pushed ahead of the *next* command ack — the hub degradation
    /// notices and any foreign-correlation acks the test wants interleaved.
    pre_ack: Vec<FrameKind>,
    /// Answers `GET_METADATA` for keys the session has not itself written;
    /// `None` answers every such key with no value.
    metadata: Option<MetadataResponder>,
    /// Keys this session stored via `SET_METADATA`. A later `GET_METADATA`
    /// reads them back, which is what `handle_set_metadata` /
    /// `handle_get_metadata` do — and what a read-modify-write caller (the
    /// layout CAS) depends on being true.
    metadata_store: Vec<(Scope, String, Vec<u8>)>,
    /// When set, every metadata request is refused with a *correlated*
    /// `ERROR` instead of answered.
    metadata_error: Option<(ErrorCode, String)>,
    /// The `TERMINAL_SPAWNED` payload a `SPAWN_TERMINAL` is answered with.
    spawn: Option<SpawnResult>,
    /// When set, every `SPAWN_TERMINAL` is refused with a *correlated*
    /// `ERROR` instead of a `TERMINAL_SPAWNED`.
    spawn_error: Option<(ErrorCode, String)>,
    /// The client count a `DetachClients` command acks with. `None` defaults
    /// to `0` (nobody was attached) — `handle_detach_clients`
    /// (`crates/phux-server/src/runtime/commands.rs`) always answers
    /// `OkWith(Json(count))` and never refuses, so this needs no error twin.
    detach_result: Option<u64>,
    /// Pushed once the client's `SUBSCRIBE_EVENTS` registers.
    script: Vec<FrameKind>,
    /// phux-k0cw: frames released only when the client subscribes to that
    /// exact `(scope, key)` — the fanout rule a real server applies, and the
    /// only way to assert a client subscribed to the RIGHT key.
    keyed_script: Vec<(Scope, String, Vec<FrameKind>)>,
    /// What to do after the script.
    end: EndOfScript,
}

impl fmt::Debug for ScriptSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScriptSpec")
            .field("priming", &self.priming)
            .field("keyed_script", &self.keyed_script.len())
            .field("state", &self.state)
            .field("pre_ack", &self.pre_ack)
            .field("metadata", &self.metadata.is_some())
            .field("metadata_store", &self.metadata_store)
            .field("metadata_error", &self.metadata_error)
            .field("spawn", &self.spawn)
            .field("spawn_error", &self.spawn_error)
            .field("detach_result", &self.detach_result)
            .field("script", &self.script)
            .field("end", &self.end)
            .finish()
    }
}

impl ScriptSpec {
    /// An empty script: handshake answered, every command acked `Ok`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The synthesized raw-VT bootstrap the server pushes before the
    /// `ATTACH_TERMINAL` acknowledgement.
    #[must_use]
    pub fn priming_snapshot(
        mut self,
        terminal: &TerminalId,
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

    /// The snapshot a `GET_STATE` ack carries.
    #[must_use]
    pub fn state(mut self, snapshot: SessionSnapshot) -> Self {
        self.state = Some(snapshot);
        self
    }

    /// A hub's per-satellite degradation notice: an *uncorrelated* `ERROR`
    /// pushed ahead of the next command ack.
    ///
    /// `handle_get_state_federated` emits one of these per unreachable
    /// satellite on purpose. It is not any command's answer (`proto.md` §9),
    /// so a client that treats it as one reports a partial fleet view as
    /// complete.
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
    /// `SET_METADATA`'d it.
    ///
    /// Prefer this over [`Self::metadata`] whenever the client will *write*
    /// the key too: the store is read back on the next `GET_METADATA`, so a
    /// read-modify-write round trip behaves the way it does against the real
    /// server instead of replaying a canned answer.
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

    /// Refuse every metadata request with a *correlated* `ERROR`.
    ///
    /// L1 §5 lets a server answer a request it will not serve (a foreign
    /// Group, an unimplemented command) with `ERROR { request_id: Some(..) }`
    /// rather than a reply frame; the correlation is what stops the caller
    /// waiting forever.
    #[must_use]
    pub fn refuse_metadata(mut self, code: ErrorCode, message: &str) -> Self {
        self.metadata_error = Some((code, message.to_owned()));
        self
    }

    /// The `TERMINAL_SPAWNED` payload a `SPAWN_TERMINAL` is answered with.
    ///
    /// Mandatory for any script whose client spawns: without it the harness
    /// panics rather than modelling a server that answers a spawn with
    /// silence.
    #[must_use]
    pub fn spawn_result(mut self, result: SpawnResult) -> Self {
        self.spawn = Some(result);
        self
    }

    /// Refuse every `SPAWN_TERMINAL` with a *correlated* `ERROR`.
    ///
    /// A satellite MAY answer a relayed spawn this way instead of with
    /// `TERMINAL_SPAWNED` — `crates/phux-server/src/hub/relay.rs`
    /// (`handle_inbound`) normalizes exactly that shape on the return leg, so
    /// it is a shape a hub's own clients can see. The correlation is what
    /// stops the caller waiting forever.
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

    /// Append frames released only once the client has subscribed to THIS
    /// exact `(scope, key)` pair (phux-k0cw).
    ///
    /// The unkeyed [`Self::push`] releases its whole script on the first
    /// `SUBSCRIBE_METADATA` of any shape. That is fine while a client has one
    /// metadata subscription, but it cannot express "the server fans this out
    /// to subscribers of THIS key" — so a test written against it passes
    /// against a client that subscribed to the WRONG key, which is precisely
    /// the bug worth catching once a client watches several.
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
        // A keyed script is by definition released by a SUBSCRIBE_METADATA,
        // so it must also hold the hang-up open past SUBSCRIBE_EVENTS —
        // otherwise the fixture closes before the client has asked for the
        // key, and every keyed assertion would vacuously fail (phux-k0cw).
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

    /// Accept one connection on `listener` and serve `spec` on it — the
    /// on-disk-socket shape, for the paths that dial with
    /// `Connection::connect` rather than a stream pair.
    ///
    /// Borrowed rather than owned so one listener can serve a sequence of
    /// client connections (`phux resize` reads back over a second dial); the
    /// `'static` future `tokio::spawn` wants is an `async move` block that
    /// captures the listener and lends it here.
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
    /// Under the default [`EndOfScript::ServeUntilDetach`] the future only
    /// resolves once the client's side of the socket is gone, exactly like a
    /// real server: a test that keeps its `Connection` in scope and then
    /// joins this task will hang. Drop the connection first.
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
            // The script stands in for the pane actor's fanout, which only
            // reaches a client once its subscription is registered. Playing
            // it earlier would emit EVENT frames to a client the server does
            // not yet know is listening.
            //
            // Which subscription unlocks it depends on what the script
            // carries. `METADATA_CHANGED` is fanned out to metadata
            // subscribers only (`broadcast_metadata_changed`), and a `watch`
            // client sends `SUBSCRIBE_EVENTS` *then* `SUBSCRIBE_METADATA`,
            // so a script with a metadata push must wait for the second one
            // or it would model a server pushing L3 to a client that had not
            // asked for it.
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
            // phux-k0cw: a keyed push models the real fanout rule — a
            // METADATA_CHANGED reaches only the clients subscribed to that
            // (scope, key). Released on the matching subscribe and never
            // before, so a client that watched the wrong key sees nothing.
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
                    command: Command::DetachTerminal { .. },
                    ..
                }
            );
            seen.push(frame);
            if detached {
                break;
            }
            if subscribed && self.spec.end == EndOfScript::HangUp {
                // Half-close the server's write side before draining the
                // client. Dropping a Unix socket with unread client frames
                // can surface as ECONNRESET instead of the clean EOF this
                // fixture promises (a terminal-scoped watch sends its
                // metadata subscription immediately after SUBSCRIBE_EVENTS).
                // The client observes EOF while this side gives the one
                // immediately-following frame a bounded chance to land.
                // Do not wait for the client's final close: some harness
                // callers keep their Connection alive while joining us.
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

/// The frames the reference server emits in response to one client frame,
/// **in the exact order it emits them**.
///
/// This function is the contract. It is the only place in the workspace's
/// test code that decides what precedes an ack, and every scripted server
/// routes through it. Extending it means citing the server handler that
/// justifies the new ordering, the way the existing arms do.
///
/// # Panics
///
/// On `ATTACH_TERMINAL` when the spec declared no priming snapshot — see
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
                server_caps: ServerCapabilities::new(),
                server_id: Vec::new(),
                selected_profile,
                bootstrap_limits,
            }]
        }
        FrameKind::Command {
            request_id,
            command,
        } => command_reply(*request_id, command, spec),
        // `handle_subscribe_events` registers the subscription and replies
        // with nothing at all. Spelled out rather than folded into the
        // wildcard because "no reply" is the load-bearing fact here: a fake
        // that acked SUBSCRIBE_EVENTS would teach a client to wait forever.
        #[allow(
            clippy::match_same_arms,
            reason = "documents an ordering fact, not a fallthrough"
        )]
        FrameKind::SubscribeEvents { .. } => Vec::new(),
        // `handle_subscribe_metadata` (`crates/phux-server/src/runtime/client.rs`)
        // likewise registers the subscription and replies with nothing —
        // and, since it captures the connection's mailbox, does so whether
        // or not the client ever attached.
        #[allow(
            clippy::match_same_arms,
            reason = "documents an ordering fact, not a fallthrough"
        )]
        FrameKind::SubscribeMetadata { .. } => Vec::new(),
        FrameKind::GetMetadata {
            request_id,
            scope,
            key,
        } => {
            let mut out = std::mem::take(&mut spec.pre_ack);
            out.push(metadata_reply(*request_id, scope, key, spec));
            out
        }
        // `SET_METADATA` is **fire-and-forget**: `handle_set_metadata`
        // (`crates/phux-server/src/runtime/client.rs`) is not even handed the
        // outbound sender, and says so in prose — "SET_METADATA has no reply
        // frame to carry an error", so a malformed write is a silent no-op.
        // Acking it here would be precisely the class of lie this module
        // exists to prevent: a client written against the ack would wait for
        // a frame the server never sends. The write is still *stored*, so a
        // read-modify-write caller sees its own value on the confirming GET,
        // which is how the layout CAS actually closes.
        FrameKind::SetMetadata {
            request_id,
            scope,
            key,
            value,
        } => {
            if let Some((code, message)) = spec.metadata_error.clone() {
                return vec![FrameKind::Error {
                    request_id: Some(*request_id),
                    code,
                    message,
                }];
            }
            spec.metadata_store
                .retain(|(s, k, _)| !(s == scope && k == key));
            spec.metadata_store
                .push((scope.clone(), key.clone(), value.clone()));
            Vec::new()
        }
        // `handle_spawn_terminal` (`crates/phux-server/src/runtime/client.rs`)
        // answers with `TERMINAL_SPAWNED` on the caller's `request_id`, after
        // whatever this connection already had queued. A hub relaying to a
        // satellite may instead surface the satellite's *correlated* `ERROR`
        // (`crates/phux-server/src/hub/relay.rs`, `handle_inbound`), which is
        // what [`ScriptSpec::refuse_spawn`] models.
        FrameKind::SpawnTerminal { request_id, .. } => {
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
                "a scripted server whose client sends SPAWN_TERMINAL must declare an \
                 outcome: handle_spawn_terminal always answers. Call \
                 ScriptSpec::spawn_result or ScriptSpec::refuse_spawn.",
            );
            out.push(FrameKind::TerminalSpawned {
                request_id: *request_id,
                result,
            });
            out
        }
        // Request-shaped frames the reference server *does* answer but this
        // harness has not modelled yet. Refusing loudly beats replying with
        // nothing: a silent no-reply wedges the client until its transport
        // dies, and the test that added the call would time out with no clue
        // why.
        FrameKind::Attach { .. } | FrameKind::ListMetadata { .. } => {
            panic!(
                "the scripted server has no reference ordering for {frame:?} yet. The                  real server answers it (ATTACHED / METADATA_LIST); add the arm to                  `reference_reply` with the handler citation rather than hand-rolling a                  fake for one test."
            )
        }
        // Everything else is genuinely fire-and-forget on this wire —
        // ROUTE_INPUT, VIEWPORT_RESIZE, FRAME_ACK, DETACH, PING's pong aside.
        _ => Vec::new(),
    }
}

/// The reply sequence for one `COMMAND`, pre-ack pushes first.
fn command_reply(request_id: u32, command: &Command, spec: &mut ScriptSpec) -> Vec<FrameKind> {
    // Every command ack is preceded by whatever the server had queued for
    // this connection. Draining here (rather than per-command) is what makes
    // the interleave unavoidable for the first ack of any session.
    let mut out = std::mem::take(&mut spec.pre_ack);
    match command {
        Command::AttachTerminal { .. } => {
            assert!(
                !spec.priming.is_empty(),
                "a scripted server whose client sends ATTACH_TERMINAL must declare \
                 priming bootstrap frames. Call ScriptSpec::priming_snapshot."
            );
            out.extend(spec.priming.clone());
            out.push(FrameKind::CommandResult {
                request_id,
                result: CommandResult::Ok,
            });
        }
        Command::GetState { .. } => {
            let result = spec.state.clone().map_or(CommandResult::Ok, |snapshot| {
                CommandResult::OkWith(CommandValue::State(snapshot))
            });
            out.push(FrameKind::CommandResult { request_id, result });
        }
        // `handle_detach_clients` (`crates/phux-server/src/runtime/commands.rs`)
        // always answers `OkWith(Json(count))`, never a bare `Ok` — modeled
        // explicitly rather than falling into the generic wildcard below,
        // which would teach a client under test the wrong reply shape.
        Command::DetachClients { .. } => {
            let count = spec.detach_result.unwrap_or(0);
            out.push(FrameKind::CommandResult {
                request_id,
                result: CommandResult::OkWith(CommandValue::Json(count.to_string())),
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

/// Length-prefixed frame I/O over the server half of a `UnixStream`.
///
/// The same SPEC §5 framing `Connection` speaks, minus the coalescing
/// buffer: a harness reads one frame at a time by construction, and the
/// hand-rolled copies of this that used to live in four test modules are
/// exactly what this replaces.
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
        // A read that ends at a frame boundary is the client hanging up; any
        // other short read is a genuinely truncated stream and should fail
        // the test loudly rather than look like a tidy close.
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
