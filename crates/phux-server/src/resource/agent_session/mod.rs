//! The `AgentSession` engine: a producer-fed record stream (ADR-0103).
//!
//! A harness shim appends `AgentEventsJsonlV1` records via
//! `APPEND_RESOURCE_OUTPUT`; the engine validates, stamps sequence and time,
//! keeps a bounded ring, and fans records out. No process, grid, or input.
//! It computes each append's [`StreamEvidence`], but publishing it is the
//! runtime's job. Lifecycle and close follow the same path as a Terminal.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::{
    ControlRequest, PaneOutput, ResourceCore, ResourceCoreChannels, ResourceFacetHandle,
    ResourceHandle, ResourceId, ResourceKind,
};

pub mod record;
pub mod ring;

pub use record::{RecordError, StreamAsk, StreamEvidence, ValidRecord};
pub use ring::RecordRing;

/// Publication identity verified by the dedicated native broker.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NativeAppendIdentity {
    /// Host-monotonic publication id.
    pub id: u64,
    /// SHA-256 of the exact normalized update.
    pub fingerprint: String,
}
/// Acceptance stored atomically with the native record append.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NativeAppendAccepted {
    /// Original identity and payload fingerprint.
    pub identity: NativeAppendIdentity,
    /// Original stamps and state/ask evidence.
    pub accepted: AppendAccepted,
}

/// Depth of the append and bootstrap mailboxes; a full one refuses appends
/// with `OVERFLOW` (ADR-0103 §3).
const AGENT_SESSION_MAILBOX: usize = 64;

/// One `APPEND_RESOURCE_OUTPUT` payload handed to the engine.
#[derive(Debug)]
pub struct AppendRequest {
    /// The producer's raw record bytes, already bounded at 64 KiB by the
    /// command decoder.
    pub bytes: Bytes,
    /// Where the stamped header, or the refusal, is returned.
    pub reply: oneshot::Sender<Result<AppendAccepted, AppendRejection>>,
    /// `Some` only after exclusive controller authorization.
    pub publication: Option<NativeAppendIdentity>,
}

/// What the server stamped onto an accepted append.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AppendAccepted {
    /// Sequence of the append's first record; the rest follow contiguously.
    pub first_seq: u64,
    /// Sequence of the append's last record; also the sequence carried by
    /// the live output frame the records shipped in.
    pub last_seq: u64,
    /// The wall-clock milliseconds stamped on every record in this append.
    pub ts_ms: u64,
    /// What the append's records say about the session's state, if
    /// anything. The last state-bearing record in the batch wins.
    pub evidence: Option<StreamEvidence>,
    /// The last question-bearing record's question, for the ask ladder
    /// (ADR-0036).
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

/// A request for an atomic stream cut, optionally resuming retained records.
#[derive(Debug)]
pub struct BootstrapRequest {
    /// Where the retained records are returned.
    pub reply: oneshot::Sender<AgentSessionBootstrap>,
    /// Last committed consumer record; `None` requests the full retained ring.
    /// Upgrade seal/unseal requests always use `None`.
    pub after_seq: Option<u64>,
    /// Graceful upgrade only (ADR-0032): `Some(true)` seals the stream at
    /// this cut, so no append can be acked and then lost before the re-exec;
    /// `Some(false)` lifts the seal when the upgrade does not happen. `None`
    /// for an ordinary consumer bootstrap.
    pub seal: Option<bool>,
}

/// The replayable state of one agent-session stream at a cut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionBootstrap {
    /// The stream sequence the cut includes; live output resumes at
    /// `base_seq + 1`.
    pub base_seq: u64,
    /// `Some` means `records` is a contiguous suffix after this cursor;
    /// `None` means a full retained replacement, including on eviction.
    pub resume_after_seq: Option<u64>,
    /// Selected records, oldest first, each a complete stamped JSONL line.
    pub records: Vec<Bytes>,
    /// Records retention has evicted; non-zero means the replay starts
    /// mid-session.
    pub dropped: u64,
    /// Whether the stream already accepted `session_end`; a graceful upgrade
    /// carries it so the resumed engine keeps refusing later records.
    pub ended: bool,
    /// Bounded native publication receipts, independent of transcript eviction.
    pub native_publications: Vec<NativeAppendAccepted>,
    /// Old ids below this cursor can never append again.
    pub native_publication_high_water: u64,
}

/// The `AgentSession` facet. `provider` and `native_id` are immutable, so
/// the handle carries them for lock-held inventory reads.
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
    /// The engine's existing end flag, shared with lock-held source arbitration.
    pub(crate) ended: Arc<AtomicBool>,
}

/// An [`AgentSessionActor`] and everything the runtime needs to publish it.
#[derive(Debug)]
pub struct AgentSessionBundle {
    /// The engine future's owner; run it on the `LocalSet`.
    pub actor: AgentSessionActor,
    /// Cross-task handle registered in the resource table.
    pub handle: ResourceHandle,
    /// Fires when the engine's run loop ends. A session has no process, so
    /// the outcome is always unknown.
    pub exit_notify: oneshot::Receiver<phux_core::process::ExitOutcome>,
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
    ended: Arc<AtomicBool>,
    /// `true` while a graceful upgrade holds this stream's cut: appends are
    /// refused with `OVERFLOW` rather than acked and lost.
    sealed: bool,
    /// Inbound producer appends.
    append_rx: mpsc::Receiver<AppendRequest>,
    /// Inbound bootstrap cuts.
    bootstrap_rx: mpsc::Receiver<BootstrapRequest>,
    native_publications: std::collections::VecDeque<NativeAppendAccepted>,
    native_publication_high_water: u64,
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
            crate::resource::output_broadcast_capacity(),
        );
        let ResourceCoreChannels {
            output,
            control,
            exit_notify,
        } = channels;
        let (append_tx, append_rx) = mpsc::channel(AGENT_SESSION_MAILBOX);
        let (bootstrap_tx, bootstrap_rx) = mpsc::channel(AGENT_SESSION_MAILBOX);
        let ended = Arc::new(AtomicBool::new(false));
        let facet = AgentSessionHandle {
            provider: Arc::from(provider),
            native_id: native_id.map(Arc::from),
            append: append_tx,
            bootstrap: bootstrap_tx,
            ended: Arc::clone(&ended),
        };
        // Consumer, ack, and upgrade channels are Terminal-only; drop their
        // receivers so a stray send fails at once.
        let handle = ResourceHandle {
            kind: ResourceKind::AgentSession,
            parent: Some(parent),
            output,
            consumer_attach: mpsc::channel(1).0,
            consumer_detach: mpsc::channel(1).0,
            consumer_ack: mpsc::channel(1).0,
            upgrade: mpsc::channel(1).0,
            control,
            facet: ResourceFacetHandle::AgentSession(facet),
        };
        AgentSessionBundle {
            actor: Self {
                core,
                ring: RecordRing::new(log_bytes as usize),
                ended,
                sealed: false,
                append_rx,
                bootstrap_rx,
                native_publications: std::collections::VecDeque::new(),
                native_publication_high_water: 0,
            },
            handle,
            exit_notify,
        }
    }

    /// Rebuild a session from the cut a graceful upgrade carried (ADR-0032):
    /// the record counter continues at `carried.base_seq + 1`, the retained
    /// records replay to new consumers (re-pruned to `log_bytes`), the
    /// eviction toll carries over, and an ended stream stays ended.
    #[must_use]
    pub fn restore(
        parent: ResourceId,
        provider: &str,
        native_id: Option<&str>,
        token: CancellationToken,
        log_bytes: u32,
        carried: AgentSessionBootstrap,
    ) -> AgentSessionBundle {
        let mut bundle = Self::build(parent, provider, native_id, token, log_bytes);
        let actor = &mut bundle.actor;
        actor.core.seq = carried.base_seq;
        actor.ended.store(carried.ended, Ordering::Relaxed);
        actor.ring.add_dropped(carried.dropped);
        actor.native_publications = carried.native_publications.into();
        actor.native_publication_high_water = carried.native_publication_high_water;
        for record in carried.records {
            actor.ring.push(record);
        }
        bundle
    }

    /// Drive the engine until cancelled or every producer and observer is
    /// gone; the exit notification fires either way.
    pub async fn run(mut self) {
        let token = self.core.token.clone();
        loop {
            tokio::select! {
                // `biased` and not by accident: seal/unseal is the control
                // plane, and `AgentSessionSeal::drop` can only queue its
                // unseal. An append queued behind that unseal in the same
                // wake-up must see the unsealed stream, never the seal.
                biased;
                () = token.cancelled() => break,
                request = self.bootstrap_rx.recv() => match request {
                    Some(request) => self.handle_bootstrap(request),
                    None => break,
                },
                request = self.append_rx.recv() => match request {
                    Some(request) => self.handle_append(request),
                    None => break,
                },
                request = self.core.control_rx.recv() => {
                    if let Some(request) = request {
                        Self::refuse_control(request);
                    }
                }
            }
        }
        self.core
            .notify_exit(phux_core::process::ExitOutcome::UNKNOWN);
    }

    /// Apply the request's seal, if any, and answer with the cut; both in one
    /// step, so no append lands between them.
    fn handle_bootstrap(&mut self, request: BootstrapRequest) {
        if let Some(seal) = request.seal {
            self.sealed = seal;
        }
        // An upgrade must carry the full ring, even if a caller supplied a
        // consumer cursor together with a seal/unseal request.
        let after_seq = if request.seal.is_none() {
            request.after_seq
        } else {
            None
        };
        let _ = request.reply.send(self.bootstrap(after_seq));
    }

    /// Select the suffix directly by sequence arithmetic. The ring is a
    /// contiguous tail ending at `base_seq`, even after byte-budget pruning;
    /// no JSON parsing or traversal of records preceding the cursor is needed.
    fn bootstrap(&self, after_seq: Option<u64>) -> AgentSessionBootstrap {
        let base_seq = self.core.seq();
        let retained = self.ring.len();
        let suffix_len = after_seq
            .and_then(|after_seq| base_seq.checked_sub(after_seq))
            .and_then(|count| usize::try_from(count).ok())
            .filter(|count| *count <= retained);
        let resume_after_seq = suffix_len.and(after_seq);
        let start = suffix_len.map_or(0, |count| retained - count);
        AgentSessionBootstrap {
            base_seq,
            resume_after_seq,
            records: self.ring.records_from(start).cloned().collect(),
            dropped: self.ring.dropped(),
            ended: self.ended.load(Ordering::Relaxed),
            native_publications: self.native_publications.iter().cloned().collect(),
            native_publication_high_water: self.native_publication_high_water,
        }
    }

    /// Resolve a native identity before seal and record validation: acknowledged
    /// retries return their original stamps without projecting evidence again.
    fn native_append_replay(
        &self,
        identity: &NativeAppendIdentity,
    ) -> Option<Result<AppendAccepted, AppendRejection>> {
        if let Some(prior) = self
            .native_publications
            .iter()
            .find(|prior| prior.identity.id == identity.id)
        {
            return Some(if prior.identity == *identity {
                Ok(prior.accepted.clone())
            } else {
                Err(AppendRejection::Overflow(
                    "native publication identity conflicts with its payload".into(),
                ))
            });
        }
        (identity.id <= self.native_publication_high_water).then(|| {
            Err(AppendRejection::Overflow(
                "native publication receipt expired; identity cannot be replayed".into(),
            ))
        })
    }

    /// Validate, stamp, retain, and broadcast one append, all-or-nothing.
    /// Records share the append's timestamp and ship in one
    /// [`PaneOutput::Live`] sequenced by the last record.
    fn handle_append(&mut self, request: AppendRequest) {
        let AppendRequest {
            bytes,
            reply,
            publication,
        } = request;
        if let Some(result) = publication
            .as_ref()
            .and_then(|identity| self.native_append_replay(identity))
        {
            let _ = reply.send(result);
            return;
        }
        if self.sealed {
            let _ = reply.send(Err(AppendRejection::Overflow(
                "the server is upgrading; retry the append".to_owned(),
            )));
            return;
        }
        let records = match record::validate(&bytes, self.ended.load(Ordering::Relaxed)) {
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
        let ts_ms = crate::state::now_unix_ms();
        // Stamping adds server-owned fields and may expand escaped data.
        // One buffer holds the batch. Each retained line is a slice of it,
        // so the broadcast and the ring share the bytes. An evicted line
        // does not free that buffer until the batch's last line leaves;
        // an append is already bounded by the command limit. Validate every
        // canonical line before consuming any sequence or publishing state:
        // even the last record can refuse the whole batch.
        let (payload, ranges) =
            match record::stamp_batch(&records, first_seq, ts_ms, bytes.len() + records.len() * 48)
            {
                Ok(stamped) => stamped,
                Err(error) => {
                    let _ = reply.send(Err(AppendRejection::Invalid(error)));
                    return;
                }
            };
        let mut evidence = None;
        let mut ask = None;
        for (entry, range) in records.iter().zip(ranges) {
            // The entire range was checked above, before any mutation.
            let _ = self.core.next_seq();
            self.ring.push(payload.slice(range));
            if let Some(found) = entry.evidence() {
                evidence = Some(found);
            }
            if let Some(question) = entry.ask() {
                ask = Some(question);
            }
            if entry.ends_session() {
                self.ended.store(true, Ordering::Relaxed);
            }
        }
        let _ = self.core.output_tx.send(PaneOutput::Live {
            seq: last_seq,
            bytes: payload,
            at: std::time::Instant::now(),
        });
        let accepted = AppendAccepted {
            first_seq,
            last_seq,
            ts_ms,
            evidence,
            ask,
        };
        if let Some(identity) = publication {
            self.native_publication_high_water = identity.id;
            self.native_publications.push_back(NativeAppendAccepted {
                identity,
                accepted: accepted.clone(),
            });
            while self.native_publications.len()
                > phux_protocol::native_agent::MAX_NATIVE_OPERATIONS
            {
                self.native_publications.pop_front();
            }
        }
        let _ = reply.send(Ok(accepted));
    }

    /// Refuse Terminal-only supervisory requests (the runtime already
    /// refuses them with `WRONG_RESOURCE_KIND`).
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
            | ControlRequest::BindAgentSession { .. }
            | ControlRequest::Retire => {}
        }
    }
}

/// A detached actor the instruction-count guard appends into.
///
/// Construction stays outside the measured region. Not a product API.
#[doc(hidden)]
#[allow(unreachable_pub, reason = "the icount bench crate is the caller")]
#[must_use]
pub fn icount_actor() -> AgentSessionActor {
    AgentSessionActor::build(
        ResourceId::default(),
        "icount",
        None,
        CancellationToken::new(),
        4 * 1024 * 1024,
    )
    .actor
}

/// Append `bytes` and replay the retained ring, returning the replayed byte count.
///
/// The instruction-count guard measures this. Production appends go through
/// the actor mailbox; `None` means the append was refused.
#[doc(hidden)]
#[allow(unreachable_pub, reason = "the icount bench crate is the caller")]
#[must_use]
pub fn icount_append_and_replay(actor: &mut AgentSessionActor, bytes: Bytes) -> Option<usize> {
    let (reply, rx) = oneshot::channel();
    actor.handle_append(AppendRequest {
        bytes,
        reply,
        publication: None,
    });
    rx.blocking_recv().ok()?.ok()?;
    let cut = actor.bootstrap(None);
    Some(cut.records.iter().map(Bytes::len).sum())
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

    #[test]
    fn one_batch_broadcast_is_the_concatenation_of_its_lines() {
        let (mut actor, _facet, handle) = engine();
        let mut subscriber = handle.output.subscribe();
        append(&mut actor, "{\"type\":\"prompt\"}\n{\"type\":\"stop\"}").expect("accepted");
        let super::PaneOutput::Live { bytes, .. } = subscriber.try_recv().expect("broadcast")
        else {
            panic!("append publishes a live batch");
        };
        let replay = actor.bootstrap(None);
        let mut joined = Vec::new();
        for record in &replay.records {
            joined.extend_from_slice(record);
        }
        assert_eq!(bytes.as_ref(), joined.as_slice());
        assert_eq!(replay.records.len(), 2);
    }

    #[test]
    fn icount_helper_appends_and_replays_the_ring() {
        let (mut actor, _facet, _handle) = engine();
        let replayed = icount_append_and_replay(
            &mut actor,
            Bytes::from_static(b"{\"type\":\"prompt\"}\n{\"type\":\"stop\"}\n"),
        )
        .expect("accepted");
        assert!(replayed > 0, "replay copies the stamped records");
        assert_eq!(actor.ring.len(), 2);
    }

    fn append(
        actor: &mut AgentSessionActor,
        payload: &str,
    ) -> Result<AppendAccepted, AppendRejection> {
        let (reply, rx) = oneshot::channel();
        actor.handle_append(AppendRequest {
            bytes: Bytes::copy_from_slice(payload.as_bytes()),
            reply,
            publication: None,
        });
        rx.blocking_recv().expect("the engine always replies")
    }

    fn cut(
        actor: &mut AgentSessionActor,
        after_seq: Option<u64>,
        seal: Option<bool>,
    ) -> AgentSessionBootstrap {
        let (reply, mut rx) = oneshot::channel();
        actor.handle_bootstrap(BootstrapRequest {
            reply,
            after_seq,
            seal,
        });
        rx.try_recv().expect("the actor replies synchronously")
    }

    #[test]
    fn resume_selects_the_first_boundary_a_batch_suffix_and_an_empty_cut() {
        let (mut actor, _facet, _handle) = engine();
        append(
            &mut actor,
            "{\"type\":\"prompt\"}\n{\"type\":\"ask\"}\n{\"type\":\"stop\"}",
        )
        .expect("accepted batch");
        let full = cut(&mut actor, None, None);
        let boundary = cut(&mut actor, Some(0), None);
        assert_eq!(boundary.resume_after_seq, Some(0));
        assert_eq!(boundary.records, full.records);
        let inside = cut(&mut actor, Some(1), None);
        assert_eq!(inside.resume_after_seq, Some(1));
        assert_eq!(inside.base_seq, 3);
        assert_eq!(inside.records, full.records[1..]);
        let at_cut = cut(&mut actor, Some(3), None);
        assert_eq!(at_cut.resume_after_seq, Some(3));
        assert_eq!(at_cut.base_seq, 3);
        assert!(at_cut.records.is_empty());
        assert_eq!(
            cut(&mut actor, Some(4), None),
            full,
            "future cursor replaces"
        );
    }

    #[test]
    fn evicted_cursor_replaces_but_the_first_retained_boundary_resumes() {
        let (mut actor, _facet, _handle) = engine();
        actor.ring = RecordRing::new(0);
        append(
            &mut actor,
            "{\"type\":\"prompt\"}\n{\"type\":\"ask\"}\n{\"type\":\"stop\"}",
        )
        .expect("accepted");
        let full = cut(&mut actor, None, None);
        assert_eq!(
            full.records.len(),
            1,
            "zero-byte budget keeps the newest record"
        );
        assert_eq!(full.dropped, 2);
        assert_eq!(cut(&mut actor, Some(1), None), full, "cursor was evicted");
        let boundary = cut(&mut actor, Some(2), None);
        assert_eq!(boundary.resume_after_seq, Some(2));
        assert_eq!(boundary.records, full.records);
        assert!(cut(&mut actor, Some(3), None).records.is_empty());
    }

    #[test]
    fn an_empty_ring_resumes_only_at_its_cut() {
        let (mut actor, _facet, _handle) = engine();
        let empty = cut(&mut actor, Some(0), None);
        assert_eq!(empty.resume_after_seq, Some(0));
        assert!(empty.records.is_empty());
        assert_eq!(cut(&mut actor, Some(1), None).resume_after_seq, None);
        actor.core.seq = 9;
        assert_eq!(cut(&mut actor, Some(9), None).resume_after_seq, Some(9));
        assert_eq!(cut(&mut actor, Some(8), None).resume_after_seq, None);
        assert_eq!(cut(&mut actor, Some(10), None).resume_after_seq, None);
    }

    #[test]
    fn upgrade_cuts_ignore_consumer_cursors_and_preserve_the_full_stream() {
        let (mut actor, _facet, _handle) = engine();
        append(&mut actor, "{\"type\":\"prompt\"}\n{\"type\":\"stop\"}").expect("accepted");
        let full = cut(&mut actor, None, None);
        let carried = cut(&mut actor, Some(2), Some(true));
        assert_eq!(carried, full, "sealing carries every retained record");
        assert!(matches!(
            append(&mut actor, "{\"type\":\"prompt\"}"),
            Err(AppendRejection::Overflow(_))
        ));
        assert_eq!(cut(&mut actor, Some(2), Some(false)), full);
        let mut restored = AgentSessionActor::restore(
            ResourceId::default(),
            "claude",
            None,
            CancellationToken::new(),
            4096,
            carried,
        )
        .actor;
        assert_eq!(cut(&mut restored, None, None), full);
        assert_eq!(
            append(&mut restored, "{\"type\":\"session_end\"}")
                .expect("accepted end")
                .first_seq,
            3
        );
        assert!(cut(&mut restored, None, None).ended);
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
        let bootstrap = actor.bootstrap(None);
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
        let bootstrap = actor.bootstrap(None);
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
    fn stamped_oversize_refuses_the_batch_before_any_mutation() {
        let (mut actor, _facet, handle) = engine();
        append(&mut actor, "{\"type\":\"prompt\"}").expect("seed");
        let before = actor.bootstrap(None);
        let mut subscriber = handle.output.subscribe();
        let prefix = "{\"type\":\"provider_raw\",\"data\":{\"text\":\"";
        let suffix = "\"}}";
        let oversized = format!(
            "{prefix}{}{suffix}",
            "x".repeat(record::MAX_RECORD_BYTES - 1 - prefix.len() - suffix.len())
        );
        assert_eq!(oversized.len() + 1, record::MAX_RECORD_BYTES);
        let batch = format!(
            "{{\"type\":\"ask\",\"data\":{{\"question\":\"continue?\"}}}}\n{oversized}\n{{\"type\":\"session_end\"}}"
        );
        assert!(matches!(
            append(&mut actor, &batch),
            Err(AppendRejection::Invalid(RecordError::Oversized { .. }))
        ));
        assert_eq!(
            actor.bootstrap(None),
            before,
            "ring, sequence and ended unchanged"
        );
        assert!(matches!(
            subscriber.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        let accepted = append(&mut actor, "{\"type\":\"stop\"}").expect("still open");
        assert_eq!((accepted.first_seq, accepted.last_seq), (2, 2));
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
        let bootstrap = actor.bootstrap(None);
        assert_eq!(bootstrap.base_seq, 8, "every record still took a sequence");
        assert!(bootstrap.dropped > 0, "the ring evicted its oldest records");
        assert_eq!(
            bootstrap.records.len() as u64 + bootstrap.dropped,
            8,
            "retained plus evicted accounts for every record"
        );
    }

    #[test]
    fn a_restored_engine_continues_the_carried_stream() {
        let (mut old, _facet, _handle) = engine();
        append(&mut old, "{\"type\":\"prompt\"}\n{\"type\":\"stop\"}").expect("accepted");
        let mut carried = old.bootstrap(None);
        carried.dropped = 5;
        let bundle = AgentSessionActor::restore(
            ResourceId::default(),
            "claude",
            Some("abc"),
            CancellationToken::new(),
            4096,
            carried.clone(),
        );
        let mut actor = bundle.actor;
        assert_eq!(actor.bootstrap(None), carried, "the cut replays unchanged");
        let next = append(&mut actor, "{\"type\":\"session_end\"}").expect("accepted");
        assert_eq!(next.first_seq, 3, "the sequence continues");

        let ended = actor.bootstrap(None);
        assert!(ended.ended);
        let bundle = AgentSessionActor::restore(
            ResourceId::default(),
            "claude",
            None,
            CancellationToken::new(),
            4096,
            ended,
        );
        let mut actor = bundle.actor;
        assert_eq!(
            append(&mut actor, "{\"type\":\"prompt\"}"),
            Err(AppendRejection::Invalid(RecordError::AfterSessionEnd)),
            "an ended stream stays ended"
        );
    }

    #[test]
    fn a_sealed_stream_refuses_appends_until_unsealed() {
        let (mut actor, _facet, _handle) = engine();
        append(&mut actor, "{\"type\":\"prompt\"}").expect("accepted");
        let (reply, mut rx) = oneshot::channel();
        actor.handle_bootstrap(BootstrapRequest {
            reply,
            seal: Some(true),
            after_seq: None,
        });
        assert_eq!(rx.try_recv().expect("the cut").base_seq, 1);
        assert!(matches!(
            append(&mut actor, "{\"type\":\"stop\"}"),
            Err(AppendRejection::Overflow(_))
        ));
        actor.handle_bootstrap(BootstrapRequest {
            reply: oneshot::channel().0,
            seal: Some(false),
            after_seq: None,
        });
        let next = append(&mut actor, "{\"type\":\"stop\"}").expect("unsealed");
        assert_eq!(next.first_seq, 2, "the refused append consumed nothing");
    }

    /// `AgentSessionSeal::drop` cannot await the actor: all it can do is queue
    /// the unseal. An append queued behind that unseal in the same wake-up must
    /// still find an unsealed stream, so `run` has to poll the bootstrap
    /// mailbox before the append mailbox. Regression for #1035.
    #[tokio::test(flavor = "current_thread")]
    async fn a_queued_unseal_is_applied_before_the_append_queued_behind_it() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let bundle = AgentSessionActor::build(
                    ResourceId::default(),
                    "claude",
                    Some("abc"),
                    CancellationToken::new(),
                    4096,
                );
                let ResourceFacetHandle::AgentSession(facet) = &bundle.handle.facet else {
                    panic!("built the wrong facet");
                };
                let bootstrap = facet.bootstrap.clone();
                let append_tx = facet.append.clone();
                tokio::task::spawn_local(bundle.actor.run());

                // Prove the actor is polling, then leave it sealed.
                let (reply, cut) = oneshot::channel();
                bootstrap
                    .send(BootstrapRequest {
                        reply,
                        seal: Some(true),
                        after_seq: None,
                    })
                    .await
                    .expect("the actor is running");
                assert_eq!(cut.await.expect("the cut").base_seq, 0);

                // Queue the unseal and an append without yielding, so the actor
                // wakes with both mailboxes ready at once.
                bootstrap
                    .try_send(BootstrapRequest {
                        reply: oneshot::channel().0,
                        seal: Some(false),
                        after_seq: None,
                    })
                    .expect("the bootstrap mailbox has room");
                let (reply, accepted) = oneshot::channel();
                append_tx
                    .try_send(AppendRequest {
                        bytes: Bytes::copy_from_slice(b"{\"type\":\"stop\"}"),
                        reply,
                        publication: None,
                    })
                    .expect("the append mailbox has room");

                let verdict = accepted.await.expect("the actor always replies");
                assert!(
                    verdict.is_ok(),
                    "an append queued behind its own unseal saw the seal: {verdict:?}"
                );
            })
            .await;
    }

    #[test]
    fn live_output_carries_whole_records_sequenced_by_the_last() {
        let (mut actor, _facet, handle) = engine();
        let mut subscriber = handle.output.subscribe();
        let accepted =
            append(&mut actor, "{\"type\":\"prompt\"}\n{\"type\":\"stop\"}").expect("accepted");
        match subscriber.try_recv().expect("one live frame") {
            PaneOutput::Live { seq, bytes, .. } => {
                assert_eq!(seq, accepted.last_seq);
                let text = String::from_utf8(bytes.to_vec()).expect("utf-8");
                assert_eq!(text.lines().count(), 2);
                assert!(text.ends_with('\n'), "every record ends its line");
            }
            other => panic!("expected live output, got {other:?}"),
        }
    }
}
