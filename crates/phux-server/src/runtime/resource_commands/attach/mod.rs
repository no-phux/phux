//! Bootstrapping a consumer onto an `AgentSession` stream: one atomic cut,
//! then live records.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private submodule re-exported by resource_commands at crate visibility"
)]

use bytes::Bytes;
use phux_protocol::caps::{BootstrapLimits, BootstrapStreamProfile};
use phux_protocol::wire::frame::{CommandResult, ErrorCode, FrameKind};
use phux_resource::agent_session::AgentSessionResource as _;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::DEFAULT_AGENT_CHUNK_BYTES;
use crate::mailbox::Outbound;
use crate::resource::agent_session::{AgentSessionBootstrap, BootstrapRequest};
use crate::state::{ClientId, SharedState};
/// Bootstrap an `AgentSession` at one atomic cut: a full retained replacement
/// or a cursor-anchored suffix, followed by live records. The live subscription
/// precedes the cut so nothing appended between them is lost.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
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
        return CommandResult::Error {
            code: ErrorCode::WrongResourceKind,
            message: "not an agent session".to_owned(),
        };
    };
    let Some(bootstrap_id) = state.with_mut(|s| s.next_attach_terminal_bootstrap_id(client_id))
    else {
        return CommandResult::Error {
            code: ErrorCode::ResourceExhausted,
            message: "ATTACH_RESOURCE bootstrap id space exhausted".to_owned(),
        };
    };
    let stream_id = crate::runtime::attach::stream_id_from(client_id.0);
    let live = handle.output.subscribe();
    // Await the old task's exit before BEGIN: no prior generation may leak
    // output between this replacement's bootstrap frames.
    crate::runtime::pump::stop_output(state, client_id, core).await;

    let (reply, rx) = oneshot::channel();
    if session
        .bootstrap
        .send(BootstrapRequest {
            reply,
            seal: None,
            after_seq,
        })
        .await
        .is_err()
    {
        return CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: "the session closed before its bootstrap was cut".to_owned(),
        };
    }
    let Ok(cut) = rx.await else {
        return CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: "the session closed before its bootstrap was cut".to_owned(),
        };
    };
    if cut.dropped > 0 {
        debug!(
            session = %terminal_id,
            dropped = cut.dropped,
            "ATTACH_RESOURCE: the replay starts after evicted records"
        );
    }

    let (cancel, done, last_seq, prior) =
        state.with_mut(|s| s.replace_attach_terminal_pump(client_id, core, bootstrap_id));
    let pump_done = done.clone().drop_guard();
    if let Some((prior_id, prior_done, prior_seq)) = prior {
        prior_done.cancelled().await;
        let tombstone = FrameKind::BootstrapTombstone {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id: prior_id,
            reason: phux_protocol::wire::frame::TombstoneReason::ExplicitReattach,
            last_valid_seq: prior_seq.load(std::sync::atomic::Ordering::Acquire),
        };
        if out_tx.send(Outbound::Frame(tombstone)).await.is_err() {
            return CommandResult::Error {
                code: ErrorCode::ResourceExhausted,
                message: "the client went away before its replacement bootstrap".to_owned(),
            };
        }
    }
    for frame in bootstrap_frames(terminal_id, stream_id, bootstrap_id, &cut, limits) {
        if out_tx.send(Outbound::Frame(frame)).await.is_err() {
            return CommandResult::Error {
                code: ErrorCode::ResourceExhausted,
                message: "the client went away mid-bootstrap".to_owned(),
            };
        }
    }
    last_seq.store(cut.base_seq, std::sync::atomic::Ordering::Release);

    let pump = live::LivePump {
        state: state.clone(),
        client_id,
        core,
        bootstrap: session.bootstrap.clone(),
        out_tx: out_tx.clone(),
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        limits,
        cancel,
        last_seq,
    };
    let connection_cancel = connection_token.clone();
    let handle = tokio::task::spawn_local(async move {
        let _done = pump_done;
        tokio::select! {
            () = connection_cancel.cancelled() => {}
            () = pump.run(live, cut.base_seq) => {}
        }
    });
    state.with_mut(|s| {
        s.track_terminal_output_pump(client_id, core, handle.abort_handle(), done, None);
    });
    CommandResult::Ok
}

/// The bootstrap frame run for one cut: BEGIN, the retained records in
/// chunks, then READY.
fn bootstrap_frames(
    terminal_id: &phux_protocol::ids::ResourceId,
    stream_id: phux_protocol::ids::StreamId,
    bootstrap_id: phux_protocol::ids::BootstrapId,
    cut: &AgentSessionBootstrap,
    limits: BootstrapLimits,
) -> impl Iterator<Item = FrameKind> {
    let budget = usize::try_from(limits.max_chunk_bytes()).unwrap_or(DEFAULT_AGENT_CHUNK_BYTES);
    let begin = FrameKind::BootstrapBegin {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        profile: cut
            .resume_after_seq
            .map_or(BootstrapStreamProfile::AgentEventsJsonlV1, |after_seq| {
                BootstrapStreamProfile::AgentEventsJsonlResumeV1 { after_seq }
            }),
        cols: 0,
        rows: 0,
        base_seq: cut.base_seq,
    };
    // A negotiated budget can be smaller than one record. Bound payloads
    // even then, and produce frames lazily instead of allocating one frame
    // per byte for a peer that advertises a tiny budget.
    let mut records = cut.records.iter().peekable();
    let mut offset = 0;
    let mut chunk_seq = 0u32;
    let chunks = std::iter::from_fn(move || {
        let mut payload = Vec::new();
        while let Some(record) = records.peek() {
            let remaining = &record[offset..];
            // Preserve whole-record chunks whenever a record fits the
            // budget, including for older consumers' default-size offers.
            if !payload.is_empty() && payload.len() + remaining.len() > budget {
                break;
            }
            let take = remaining.len().min(budget - payload.len());
            payload.extend_from_slice(&remaining[..take]);
            offset += take;
            if offset == record.len() {
                records.next();
                offset = 0;
            }
            if payload.len() == budget {
                break;
            }
        }
        if payload.is_empty() {
            return None;
        }
        let frame = FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload: Bytes::from(payload),
        };
        chunk_seq = chunk_seq.saturating_add(1);
        Some(frame)
    });
    let ready = FrameKind::BootstrapReady {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        history_cursor: None,
    };
    std::iter::once(begin)
        .chain(chunks)
        .chain(std::iter::once(ready))
}

mod live;

#[cfg(test)]
mod tests;
