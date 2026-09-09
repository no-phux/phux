//! The `AgentSession` engine: a producer-fed record stream (ADR-0103).
//!
//! The second [`ResourceKind`]. Where the Terminal engine reads its output
//! from a PTY, this one is *fed*: a harness shim appends
//! `AgentEventsJsonlV1` records through `APPEND_RESOURCE_OUTPUT` and the
//! engine validates them, stamps the sequence and time it owns, retains a
//! bounded ring, and fans the stamped records out to subscribers. There is
//! no process, no grid, and no input channel.
//!
//! The engine is deliberately thin. It owns the ring, the sequence, and the
//! `session_end` latch, and it computes each append's
//! [`StreamEvidence`] — but publishing that evidence
//! is the runtime's job (`runtime::resource_commands`), because the arbiter
//! that ranks it and the parent Terminal whose `phux.agent/v1` record it
//! lands on both live outside any one engine.
//!
//! Lifecycle is the shared one: cancelling the resource's token ends the
//! run loop, which fires the core's exit notification, which the per-resource
//! exit watcher turns into `TERMINAL_CLOSED` and a reap. A session therefore
//! closes through exactly the path a Terminal does, with the
//! [`CloseReason`](phux_protocol::wire::frame::CloseReason) the closer
//! recorded.

use std::sync::Arc;

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::{
    ControlRequest, DEFAULT_OUTPUT_BROADCAST, PaneOutput, ResourceCore, ResourceCoreChannels,
    ResourceFacetHandle, ResourceHandle, ResourceId, ResourceKind,
};

pub mod record;
pub mod ring;

pub use record::{RecordError, StreamAsk, StreamEvidence, ValidRecord};
pub use ring::RecordRing;

/// Depth of the engine's append and bootstrap mailboxes.
///
/// Producers are short-lived hook processes issuing one append apiece, so a
/// backlog this deep means the engine is not draining; refusing further
/// appends with `OVERFLOW` is the honest answer, and it is exactly what
/// ADR-0103 §3 names a full outbound queue.
const AGENT_SESSION_MAILBOX: usize = 64;

/// One `APPEND_RESOURCE_OUTPUT` payload handed to the engine.
#[derive(Debug)]
pub struct AppendRequest {
    /// The producer's raw record bytes, already bounded at 64 KiB by the
    /// command decoder.
    pub bytes: Bytes,
    /// Where the stamped header, or the refusal, is returned.
    pub reply: oneshot::Sender<Result<AppendAccepted, AppendRejection>>,
}

/// What the server stamped onto an accepted append.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendAccepted {
    /// Sequence of the append's FIRST record. A batch's later records
    /// follow contiguously, so a producer that emitted one record learns
    /// its sequence and one that emitted several learns where its run
    /// begins.
    pub first_seq: u64,
    /// Sequence of the append's last record; also the sequence carried by
    /// the live output frame the records shipped in.
    pub last_seq: u64,
    /// The wall-clock milliseconds stamped on every record in this append.
    pub ts_ms: u64,
    /// What the append's records say about the session's state, if
    /// anything. The last state-bearing record in the batch wins.
    pub evidence: Option<StreamEvidence>,
    /// The pending question the append's last question-bearing record
    /// carried, for the ask ladder (ADR-0036). Independent of
    /// [`Self::evidence`]: the two ledgers retract on different rules.
    pub ask: Option<StreamAsk>,
}

/// Why an append was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendRejection {
    /// A record failed codec validation; nothing was appended.
    Invalid(RecordError),
    /// The engine's mailbox is full or its sequence space is exhausted.
    Overflow(String),
    /// The engine is gone (the session closed under the caller).
    Closed,
}

/// A request for the stream's bootstrap: everything the ring retains.
#[derive(Debug)]
pub struct BootstrapRequest {
    /// Where the retained records are returned.
    pub reply: oneshot::Sender<AgentSessionBootstrap>,
}

/// The replayable state of one agent-session stream at a cut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionBootstrap {
    /// The stream sequence the cut includes; live output resumes at
    /// `base_seq + 1`.
    pub base_seq: u64,
    /// Retained records, oldest first, each a complete stamped JSONL line.
    pub records: Vec<Bytes>,
    /// How many records retention has evicted over the session's life. A
    /// non-zero count means the replay starts mid-session, which a reader
    /// cannot otherwise tell from a session that started there.
    pub dropped: u64,
}

/// The `AgentSession` facet: the channels only this engine serves.
///
/// `provider` and `native_id` are immutable for the resource's lifetime, so
/// they are carried on the handle rather than fetched: the inventory path
/// reads them under the state lock without a round trip to the engine.
#[derive(Debug, Clone)]
pub struct AgentSessionHandle {
    /// Harness that produces the session's records, e.g. `claude`.
    pub provider: Arc<str>,
    /// The provider's own opaque session id, when it supplied one.
    pub native_id: Option<Arc<str>>,
    /// Producer-fed appends (`APPEND_RESOURCE_OUTPUT`).
    pub append: mpsc::Sender<AppendRequest>,
    /// Bootstrap cut requests, served from the retained ring.
    pub bootstrap: mpsc::Sender<BootstrapRequest>,
}

/// An [`AgentSessionActor`] and everything the runtime needs to publish it.
#[derive(Debug)]
pub struct AgentSessionBundle {
    /// The engine future's owner; run it on the `LocalSet`.
    pub actor: AgentSessionActor,
    /// Cross-task handle registered in the resource table.
    pub handle: ResourceHandle,
    /// Fires when the engine's run loop ends.
    pub exit_notify: oneshot::Receiver<Option<i32>>,
}

/// The producer-fed agent-session engine.
pub struct AgentSessionActor {
    /// The generic half: sequence, output broadcast, event subscribers,
    /// lifecycle, control mailbox.
    core: ResourceCore,
    /// Retained records, bounded by `defaults.agent-log-bytes`.
    ring: RecordRing,
    /// `true` once a `session_end` record has been accepted; every later
    /// record is `RECORD_INVALID`.
    ended: bool,
    /// Inbound producer appends.
    append_rx: mpsc::Receiver<AppendRequest>,
    /// Inbound bootstrap cuts.
    bootstrap_rx: mpsc::Receiver<BootstrapRequest>,
}

impl std::fmt::Debug for AgentSessionActor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentSessionActor")
            .field("core", &self.core)
            .field("ring", &self.ring)
            .field("ended", &self.ended)
            .finish_non_exhaustive()
    }
}

impl AgentSessionActor {
    /// Build a session bound to `parent`, retaining at most `log_bytes` of
    /// records, whose run loop watches `token`.
    #[must_use]
    pub fn build(
        parent: ResourceId,
        provider: &str,
        native_id: Option<&str>,
        token: CancellationToken,
        log_bytes: u32,
    ) -> AgentSessionBundle {
        let (core, channels) = ResourceCore::new(
            ResourceKind::AgentSession,
            Some(parent),
            token,
            DEFAULT_OUTPUT_BROADCAST,
        );
        let ResourceCoreChannels {
            output,
            subscribe_to_events,
            unsubscribe_from_events,
            control,
            exit_notify,
        } = channels;
        let (append_tx, append_rx) = mpsc::channel(AGENT_SESSION_MAILBOX);
        let (bootstrap_tx, bootstrap_rx) = mpsc::channel(AGENT_SESSION_MAILBOX);
        let facet = AgentSessionHandle {
            provider: Arc::from(provider),
            native_id: native_id.map(Arc::from),
            append: append_tx,
            bootstrap: bootstrap_tx,
        };
        // The consumer-lifecycle, ack, and upgrade channels exist for every
        // kind but are served only by the Terminal engine: per-consumer
        // `StateSync` bookkeeping is a grid concern, `FRAME_ACK` is refused
        // on this stream's raw profile, and a session has no PTY for a
        // re-exec'd image to re-adopt. Their receivers are dropped here, so
        // a send that should never happen fails immediately instead of
        // queueing against a mailbox nobody reads.
        let handle = ResourceHandle {
            kind: ResourceKind::AgentSession,
            parent: Some(parent),
            output,
            consumer_attach: mpsc::channel(1).0,
            consumer_detach: mpsc::channel(1).0,
            consumer_ack: mpsc::channel(1).0,
            subscribe_to_events,
            unsubscribe_from_events,
            upgrade: mpsc::channel(1).0,
            control,
            facet: ResourceFacetHandle::AgentSession(facet),
        };
        AgentSessionBundle {
            actor: Self {
                core,
                ring: RecordRing::new(log_bytes as usize),
                ended: false,
                append_rx,
                bootstrap_rx,
            },
            handle,
            exit_notify,
        }
    }

    /// Drive the engine until its token is cancelled or every producer and
    /// observer has gone away.
    ///
    /// The exit notification fires on the way out however the loop ended,
    /// so the per-resource exit watcher closes and reaps the session on the
    /// same path a Terminal takes.
    pub async fn run(mut self) {
        let token = self.core.token.clone();
        loop {
            tokio::select! {
                () = token.cancelled() => break,
                request = self.append_rx.recv() => match request {
                    Some(request) => self.handle_append(request),
                    None => break,
                },
                request = self.bootstrap_rx.recv() => match request {
                    Some(request) => {
                        let _ = request.reply.send(self.bootstrap());
                    }
                    None => break,
                },
                request = self.core.subscribe_to_events_rx.recv() => {
                    if let Some(request) = request {
                        self.core.subscribe_events(request);
                    }
                }
                request = self.core.unsubscribe_from_events_rx.recv() => {
                    if let Some(request) = request {
                        self.core.unsubscribe_events(&request);
                    }
                }
                request = self.core.control_rx.recv() => {
                    if let Some(request) = request {
                        Self::refuse_control(request);
                    }
                }
            }
        }
        self.core.notify_exit(None);
    }

    /// The current bootstrap cut: the retained ring plus the sequence it
    /// reaches.
    fn bootstrap(&self) -> AgentSessionBootstrap {
        AgentSessionBootstrap {
            base_seq: self.core.seq(),
            records: self.ring.records().cloned().collect(),
            dropped: self.ring.dropped(),
        }
    }

    /// Validate, stamp, retain, and broadcast one append.
    ///
    /// All-or-nothing: a batch whose third record is malformed appends none
    /// of the first two, so a producer never has to reason about a partial
    /// commit. Every record in one append shares that append's timestamp
    /// and takes its own sequence, and the whole run ships in a single
    /// [`PaneOutput::Live`] carrying complete records, sequenced by the last
    /// one — which is what the bootstrap's `base_seq` is measured against.
    fn handle_append(&mut self, request: AppendRequest) {
        let AppendRequest { bytes, reply } = request;
        let records = match record::validate(&bytes, self.ended) {
            Ok(records) => records,
            Err(error) => {
                let _ = reply.send(Err(AppendRejection::Invalid(error)));
                return;
            }
        };
        // The sequence space is claimed before anything is retained: a run
        // that would exhaust `u64` leaves the ring exactly as it was.
        let Some(first_seq) = self.core.seq().checked_add(1) else {
            let _ = reply.send(Err(AppendRejection::Overflow(
                "the session's sequence space is exhausted".to_owned(),
            )));
            return;
        };
        let Some(last_seq) = first_seq.checked_add(records.len() as u64 - 1) else {
            let _ = reply.send(Err(AppendRejection::Overflow(
                "the session's sequence space is exhausted".to_owned(),
            )));
            return;
        };
        let ts_ms = record::now_ms();
        let mut evidence = None;
        let mut ask = None;
        let mut payload = Vec::with_capacity(bytes.len() + records.len() * 48);
        for entry in &records {
            let Some(seq) = self.core.next_seq() else {
                break;
            };
            let line = entry.stamp(seq, ts_ms);
            payload.extend_from_slice(&line);
            self.ring.push(line);
            if let Some(found) = entry.evidence() {
                evidence = Some(found);
            }
            if let Some(question) = entry.ask() {
                ask = Some(question);
            }
            self.ended |= entry.ends_session();
        }
        let _ = self.core.output_tx.send(PaneOutput::Live {
            seq: last_seq,
            bytes: Bytes::from(payload),
        });
        let _ = reply.send(Ok(AppendAccepted {
            first_seq,
            last_seq,
            ts_ms,
            evidence,
            ask,
        }));
    }

    /// Answer a supervisory request that belongs to another kind.
    ///
    /// Lease changes and record invalidation are about a Terminal's input
    /// gate and detector; a signal needs a process group. None of the three
    /// exists here, so the two that carry a reply get a typed refusal and
    /// the rest are dropped. The runtime refuses these at dispatch with
    /// `WRONG_RESOURCE_KIND`; this arm is the engine holding the same line
    /// for any path that reaches it anyway.
    fn refuse_control(request: ControlRequest) {
        match request {
            ControlRequest::ReportAgentState { reply, .. }
            | ControlRequest::SynthesizeAgentStateRecord { reply, .. }
            | ControlRequest::ReportStreamState { reply, .. } => {
                let _ = reply.send(Err(
                    "an agent session derives its state from its own stream".to_owned(),
                ));
            }
            ControlRequest::Signal { reply, .. } => {
                let _ = reply.send(Err("an agent session has no process to signal".to_owned()));
            }
            ControlRequest::LeaseChanged { .. }
            | ControlRequest::AgentRecordInvalidated
            | ControlRequest::BindAgentSession { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a detached engine and drive it by hand, without the runtime.
    fn engine() -> (AgentSessionActor, AgentSessionHandle, ResourceHandle) {
        let bundle = AgentSessionActor::build(
            ResourceId::default(),
            "claude",
            Some("abc"),
            CancellationToken::new(),
            4096,
        );
        let facet = match &bundle.handle.facet {
            ResourceFacetHandle::AgentSession(facet) => facet.clone(),
            ResourceFacetHandle::Terminal(_) => panic!("built the wrong facet"),
        };
        (bundle.actor, facet, bundle.handle)
    }

    fn append(
        actor: &mut AgentSessionActor,
        payload: &str,
    ) -> Result<AppendAccepted, AppendRejection> {
        let (reply, rx) = oneshot::channel();
        actor.handle_append(AppendRequest {
            bytes: Bytes::copy_from_slice(payload.as_bytes()),
            reply,
        });
        rx.blocking_recv().expect("the engine always replies")
    }

    #[test]
    fn the_facet_resolves_and_carries_its_provenance() {
        let (_actor, facet, handle) = engine();
        assert_eq!(&*facet.provider, "claude");
        assert_eq!(facet.native_id.as_deref(), Some("abc"));
        assert_eq!(handle.kind, ResourceKind::AgentSession);
        assert!(handle.parent.is_some());
        assert!(
            handle.terminal().is_err(),
            "a session refuses the Terminal facet"
        );
        assert_eq!(
            handle.agent_session().expect("session facet").provider,
            facet.provider
        );
    }

    #[test]
    fn appends_stamp_contiguous_sequences_and_one_timestamp() {
        let (mut actor, _facet, _handle) = engine();
        let first = append(&mut actor, "{\"type\":\"session_start\"}").expect("accepted");
        assert_eq!((first.first_seq, first.last_seq), (1, 1));
        let batch = append(
            &mut actor,
            "{\"type\":\"prompt\"}\n{\"type\":\"tool_start\"}\n",
        )
        .expect("accepted");
        assert_eq!((batch.first_seq, batch.last_seq), (2, 3));
        assert_eq!(batch.evidence, Some(StreamEvidence::Working));
        let bootstrap = actor.bootstrap();
        assert_eq!(bootstrap.base_seq, 3);
        assert_eq!(bootstrap.records.len(), 3);
        assert!(
            bootstrap.records[1].starts_with(b"{\"seq\":2,\"ts_ms\":"),
            "records replay with their stamped header"
        );
    }

    #[test]
    fn an_invalid_record_commits_nothing() {
        let (mut actor, _facet, _handle) = engine();
        append(&mut actor, "{\"type\":\"prompt\"}").expect("accepted");
        let rejection = append(&mut actor, "{\"type\":\"stop\"}\n{\"type\":\"nope\"}")
            .expect_err("the batch is refused");
        assert!(matches!(rejection, AppendRejection::Invalid(_)));
        let bootstrap = actor.bootstrap();
        assert_eq!(bootstrap.base_seq, 1, "no sequence was consumed");
        assert_eq!(bootstrap.records.len(), 1);
    }

    #[test]
    fn session_end_latches_the_stream_closed() {
        let (mut actor, _facet, _handle) = engine();
        let ended = append(&mut actor, "{\"type\":\"session_end\"}").expect("accepted");
        assert_eq!(ended.evidence, Some(StreamEvidence::Retract));
        assert_eq!(
            append(&mut actor, "{\"type\":\"prompt\"}"),
            Err(AppendRejection::Invalid(RecordError::AfterSessionEnd))
        );
    }

    #[test]
    fn overflow_evicts_and_the_bootstrap_reports_the_toll() {
        let bundle = AgentSessionActor::build(
            ResourceId::default(),
            "claude",
            None,
            CancellationToken::new(),
            120,
        );
        let mut actor = bundle.actor;
        for _ in 0..8 {
            append(&mut actor, "{\"type\":\"tool_end\"}").expect("accepted");
        }
        let bootstrap = actor.bootstrap();
        assert_eq!(bootstrap.base_seq, 8, "every record still took a sequence");
        assert!(bootstrap.dropped > 0, "the ring evicted its oldest records");
        assert_eq!(
            bootstrap.records.len() as u64 + bootstrap.dropped,
            8,
            "retained plus evicted accounts for every record"
        );
    }

    #[test]
    fn live_output_carries_whole_records_sequenced_by_the_last() {
        let (mut actor, _facet, handle) = engine();
        let mut subscriber = handle.output.subscribe();
        let accepted =
            append(&mut actor, "{\"type\":\"prompt\"}\n{\"type\":\"stop\"}").expect("accepted");
        match subscriber.try_recv().expect("one live frame") {
            PaneOutput::Live { seq, bytes } => {
                assert_eq!(seq, accepted.last_seq);
                let text = String::from_utf8(bytes.to_vec()).expect("utf-8");
                assert_eq!(text.lines().count(), 2);
                assert!(text.ends_with('\n'), "every record ends its line");
            }
            other => panic!("expected live output, got {other:?}"),
        }
    }
}
