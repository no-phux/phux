//! Bootstrapping a consumer onto an `AgentSession` stream: one atomic cut,
//! then live records.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private module of the agent session app"
)]

use bytes::Bytes;
use phux_agent_session::session::AgentSessionResource as _;
use phux_protocol::caps::{BootstrapLimits, BootstrapStreamProfile};
use phux_protocol::wire::frame::{CommandResult, ErrorCode, FrameKind};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::DEFAULT_AGENT_CHUNK_BYTES;
use crate::mailbox::Outbound;
use crate::runtime::apps::producer_stream::{self, StreamAttach, StreamCut, StreamIds};
use crate::state::{ClientId, SharedState};
use phux_agent_session::session::{AgentSessionBootstrap, BootstrapRequest};

fn validate_cut(cut: &AgentSessionBootstrap) -> Result<(), String> {
    let mut ended = false;
    for record in &cut.records {
        let records = phux_agent_session::session::record::validate(record, ended)
            .map_err(|error| error.to_string())?;
        ended = records
            .last()
            .is_some_and(phux_agent_session::session::record::ValidRecord::ends_session);
    }
    Ok(())
}

fn session_closed() -> CommandResult {
    CommandResult::error(
        ErrorCode::TerminalNotFound,
        "the session closed before its bootstrap was cut",
    )
}

/// The same validated cut serves explicit subscriptions and lag recovery.
async fn capture_cut(
    bootstrap: &mpsc::Sender<BootstrapRequest>,
    after_seq: Option<u64>,
) -> Result<AgentSessionBootstrap, CommandResult> {
    let (reply, rx) = oneshot::channel();
    bootstrap
        .send(BootstrapRequest {
            reply,
            seal: None,
            after_seq,
        })
        .await
        .map_err(|_| session_closed())?;
    let cut = rx.await.map_err(|_| session_closed())?;
    validate_cut(&cut).map_err(|message| CommandResult::Error {
        code: ErrorCode::CodecUnavailable,
        message,
    })?;
    Ok(cut)
}

/// An explicit subscription's cut: [`capture_cut`], noting a replay that
/// starts after evicted records.
async fn attach_cut(
    bootstrap: &mpsc::Sender<BootstrapRequest>,
    terminal_id: &phux_protocol::ids::ResourceId,
    after_seq: Option<u64>,
) -> Result<AgentSessionBootstrap, CommandResult> {
    let cut = capture_cut(bootstrap, after_seq).await?;
    if cut.dropped > 0 {
        debug!(
            session = %terminal_id,
            dropped = cut.dropped,
            "ATTACH_RESOURCE: the replay starts after evicted records"
        );
    }
    Ok(cut)
}

/// Bootstrap an `AgentSession` at one atomic cut: a full retained replacement
/// or a cursor-anchored suffix, followed by live records. The live subscription
/// precedes the cut so nothing appended between them is lost.
#[allow(
    clippy::too_many_arguments,
    reason = "one ordered subscription/cut/retirement/publication transaction with negotiated context"
)]
pub(crate) async fn attach_agent_session(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    core: phux_core::ids::ResourceId,
    handle: &crate::resource::ResourceHandle,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    limits: BootstrapLimits,
    connection_token: &CancellationToken,
    after_seq: Option<u64>,
) -> CommandResult {
    let Ok(session) = handle.agent_session() else {
        return CommandResult::error(ErrorCode::WrongResourceKind, "not an agent session");
    };
    let attach = StreamAttach {
        state,
        client_id,
        terminal_id,
        core,
        handle,
        out_tx,
        limits,
        connection_token,
    };
    let recovery = live::ReplayThenRepublish {
        bootstrap: session.bootstrap.clone(),
    };
    let cut = attach_cut(&session.bootstrap, terminal_id, after_seq);
    producer_stream::attach_stream(attach, cut, recovery).await
}

/// A cut announces a full retained replacement, or a suffix resuming after
/// a cursor; its payload is the retained records, packed whole into chunks
/// whenever a record fits the budget.
impl StreamCut for AgentSessionBootstrap {
    fn base_seq(&self) -> u64 {
        self.base_seq
    }

    fn profile(&self) -> BootstrapStreamProfile {
        self.resume_after_seq
            .map_or(BootstrapStreamProfile::AgentEventsJsonlV1, |after_seq| {
                BootstrapStreamProfile::AgentEventsJsonlResumeV1 { after_seq }
            })
    }

    fn chunks(&self, ids: StreamIds, limits: BootstrapLimits) -> impl Iterator<Item = FrameKind> {
        let budget = usize::try_from(limits.max_chunk_bytes()).unwrap_or(DEFAULT_AGENT_CHUNK_BYTES);
        let mut packer = RecordPacker {
            records: self.records.iter().peekable(),
            offset: 0,
            budget,
        };
        let mut chunk_seq = 0u32;
        std::iter::from_fn(move || {
            let payload = packer.next_payload()?;
            let frame = ids.chunk(chunk_seq, payload);
            chunk_seq = chunk_seq.saturating_add(1);
            Some(frame)
        })
    }
}

/// Packs records into chunk payloads of at most `budget` bytes. A negotiated
/// budget can be smaller than one record: payloads stay bounded even then,
/// produced lazily instead of allocating one frame per byte for a peer that
/// advertises a tiny budget.
struct RecordPacker<'a> {
    records: std::iter::Peekable<std::slice::Iter<'a, Bytes>>,
    /// How much of the next record earlier chunks already carried.
    offset: usize,
    budget: usize,
}

impl RecordPacker<'_> {
    fn next_payload(&mut self) -> Option<Bytes> {
        let mut payload = Vec::new();
        while let Some(record) = self.records.peek() {
            let remaining = &record[self.offset..];
            // Preserve whole-record chunks whenever a record fits the
            // budget, including for older consumers' default-size offers.
            if !payload.is_empty() && payload.len() + remaining.len() > self.budget {
                break;
            }
            let take = remaining.len().min(self.budget - payload.len());
            payload.extend_from_slice(&remaining[..take]);
            self.offset += take;
            if self.offset == record.len() {
                self.records.next();
                self.offset = 0;
            }
            if payload.len() == self.budget {
                break;
            }
        }
        (!payload.is_empty()).then(|| Bytes::from(payload))
    }
}

/// The bootstrap frame run for one cut: BEGIN, the retained records in
/// chunks, then READY.
#[cfg(test)]
fn bootstrap_frames(
    terminal_id: &phux_protocol::ids::ResourceId,
    stream_id: phux_protocol::ids::StreamId,
    bootstrap_id: phux_protocol::ids::BootstrapId,
    cut: &AgentSessionBootstrap,
    limits: BootstrapLimits,
) -> impl Iterator<Item = FrameKind> {
    let ids = StreamIds {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
    };
    producer_stream::bootstrap_frames(cut, ids, limits)
}

mod live;

#[cfg(test)]
mod tests;
