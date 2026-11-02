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

use phux_resource::{
    Delivery, LeaseChange, ResourceCore, ResourceCoreChannels, ResourceFacet, ResourceHandle,
    ResourceId, ResourceKind, ResourceOps, ResourceOutput, WrongResourceKind,
};

pub mod record;
pub mod ring;

pub use record::{RecordError, StreamAsk, StreamEvidence, ValidRecord};
pub use ring::RecordRing;

/// Depth of the append and bootstrap mailboxes; a full one refuses appends
/// with `OVERFLOW` (ADR-0103 §3).
const AGENT_SESSION_MAILBOX: usize = 64;

mod messages;
pub use messages::{
    AgentSessionBootstrap, AppendAccepted, AppendRejection, AppendRequest, BootstrapRequest,
    NativeAppendAccepted, NativeAppendIdentity,
};

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
    pub ended: Arc<AtomicBool>,
}

impl ResourceFacet for AgentSessionHandle {
    const KIND: ResourceKind = ResourceKind::AgentSession;
}

/// A record stream has no grid to size and no process to hand off; an upgrade
/// carries it through its bootstrap mailbox instead.
impl ResourceOps for AgentSessionHandle {
    fn resize(&self, _size: (u16, u16), _cell_px: Option<(u16, u16)>) -> bool {
        false
    }

    fn upgrade_sender(&self) -> Option<mpsc::Sender<phux_resource::handoff::UpgradeHandleRequest>> {
        None
    }

    fn lease_changed(&self, _change: LeaseChange) -> Delivery {
        Box::pin(std::future::ready(()))
    }

    fn try_lease_changed(&self, _change: LeaseChange) {}

    fn retire(&self) -> Delivery {
        Box::pin(std::future::ready(()))
    }
}

/// The `AgentSession` facet of a [`ResourceHandle`].
pub trait AgentSessionResource {
    /// The `AgentSession` facet.
    ///
    /// # Errors
    ///
    /// [`WrongResourceKind`] when this resource is not an agent session.
    fn agent_session(&self) -> Result<&AgentSessionHandle, WrongResourceKind>;
}

impl AgentSessionResource for ResourceHandle {
    fn agent_session(&self) -> Result<&AgentSessionHandle, WrongResourceKind> {
        self.facet::<AgentSessionHandle>()
    }
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
    /// The generic half: sequence, output broadcast, lifecycle.
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
            phux_resource::output_broadcast_capacity(),
        );
        let ResourceCoreChannels {
            output,
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
        let handle = ResourceHandle::new(Some(parent), output, facet);
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
    /// [`ResourceOutput::Live`] sequenced by the last record.
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
        let ts_ms = phux_resource::now_unix_ms();
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
        let _ = self.core.output_tx.send(ResourceOutput::Live {
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
mod tests;
