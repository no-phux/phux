use bytes::Bytes;
use phux_protocol::caps::{BootstrapStreamProfile, ServerFeatureExtSet, ServerFeatureSet};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{
    CommandResult, ErrorCode, FrameKind, MoveResult, Scope, SpawnResult,
};
use phux_protocol::wire::info::SessionSnapshot;

#[cfg(doc)]
use super::ScriptedServer;
use super::{EndOfScript, FIXTURE_BOOTSTRAP_ID, FIXTURE_STREAM_ID, OrgFixture, ScriptSpec};

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

    /// An agent bootstrap carrying exact JSONL bytes (possibly malformed).
    /// Frame order stays fixed: BEGIN, CHUNK, then READY, or `RESOURCE_CLOSED`
    /// when `closed` models a resource ending during its bootstrap.
    #[must_use]
    pub fn agent_log_bootstrap_bytes(
        mut self,
        session: &ResourceId,
        bytes: &[u8],
        closed: bool,
    ) -> Self {
        self = self.agent_log_bootstrap(session, &[]);
        let ready = self.priming.pop();
        self.priming.push(FrameKind::BootstrapChunk {
            terminal_id: session.clone(),
            stream_id: FIXTURE_STREAM_ID,
            bootstrap_id: FIXTURE_BOOTSTRAP_ID,
            chunk_seq: 0,
            payload: Bytes::copy_from_slice(bytes),
        });
        if closed {
            self.priming.push(FrameKind::ResourceClosed {
                terminal_id: session.clone(),
                exit_status: None,
                reason: phux_protocol::wire::frame::CloseReason::Unknown,
                signal: None,
            });
        } else {
            self.priming.extend(ready);
        }
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

    /// The extended features (`HELLO_OK` word 1) the server advertises.
    #[must_use]
    pub const fn server_features_ext(mut self, features: ServerFeatureExtSet) -> Self {
        self.server_features_ext = features;
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

    /// Refuse every `UPGRADE` the way `handle_upgrade` reports a failed
    /// preparation: a `COMMAND_RESULT` error carrying `message`.
    #[must_use]
    pub fn refuse_upgrade(mut self, message: &str) -> Self {
        self.upgrade_error = Some(message.to_owned());
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

    /// Answer every organization command (`docs/organization.md`) from
    /// `fixture`, and `GET_STATE` too when no snapshot payload is set.
    /// Advertise `features_ext.ORGANIZATION` with
    /// [`Self::server_features_ext`] for a client to send them.
    #[must_use]
    pub fn organization(mut self, fixture: OrgFixture) -> Self {
        self.organization = Some(fixture);
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
    pub(super) fn script_needs_metadata_subscription(&self) -> bool {
        // A keyed script must hold the hang-up open past SUBSCRIBE_EVENTS.
        !self.keyed_script.is_empty()
            || self
                .script
                .iter()
                .any(|frame| matches!(frame, FrameKind::MetadataChanged { .. }))
    }
}
