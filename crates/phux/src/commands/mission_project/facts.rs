//! Translating phux observations into the mission projection's inputs.

use super::{AgentReport, OpenRun, RunPhase, RunState, TerminalFact, WatchItem};
use phux_client::{
    agent_meta::AgentMetaState,
    attach::connection::{Answer, Connection},
    resource::show::show_key_level_on,
};
use phux_protocol::{
    api::{ExecutionObservation, ExecutionOutcome, ExecutionRef, ExecutionState},
    ids::{ResourceId, ResourceKind},
    wire::frame::{AgentEvent, ResourceLifecycle},
};

pub(super) const fn phase(run: &OpenRun) -> RunPhase {
    match run.run_state {
        RunState::Queued => RunPhase::Queued,
        RunState::Running => RunPhase::Running,
        RunState::Ended => RunPhase::Ended,
    }
}

pub(super) const fn report_of(state: AgentMetaState) -> AgentReport {
    match state {
        AgentMetaState::Working => AgentReport::Working,
        AgentMetaState::Blocked => AgentReport::Blocked,
        AgentMetaState::Done => AgentReport::Done,
        AgentMetaState::Idle | AgentMetaState::Unknown => AgentReport::Other,
    }
}

const fn seen_exit(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::TerminalControl {
            lifecycle: ResourceLifecycle::Exited,
            ..
        } | AgentEvent::ResourceClosed { .. }
    )
}

pub(super) struct ExitFact {
    pub(super) terminal: ResourceId,
    pub(super) seq: u64,
}

pub(super) fn exit_of(item: &WatchItem) -> Option<ExitFact> {
    let WatchItem::Event(event) = item else {
        return None;
    };
    if !seen_exit(&event.event) {
        return None;
    }
    Some(ExitFact {
        terminal: event.terminal.clone()?,
        seq: event.stamp.as_ref().map_or(0, |stamp| stamp.seq),
    })
}

pub(super) const fn is_gap(item: &WatchItem) -> bool {
    let WatchItem::Event(event) = item else {
        return false;
    };
    matches!(
        event.event,
        AgentEvent::JournalGap { .. } | AgentEvent::SourceGap { .. }
    )
}

pub(super) const fn is_spawn(item: &WatchItem) -> bool {
    let WatchItem::Event(event) = item else {
        return false;
    };
    matches!(
        event.event,
        AgentEvent::ResourceSpawned {
            kind: ResourceKind::Terminal,
            ..
        }
    )
}

fn terminal_fact(observation: &ExecutionObservation) -> Option<TerminalFact> {
    if observation.persistence_pending {
        return None;
    }
    Some(match observation.state {
        ExecutionState::Starting | ExecutionState::Running | ExecutionState::Stopping => {
            TerminalFact::Running
        }
        ExecutionState::Completed => match observation.outcome {
            Some(ExecutionOutcome::Exited { code }) => TerminalFact::Exited { status: Some(code) },
            Some(ExecutionOutcome::Signaled { .. }) => TerminalFact::Exited { status: None },
            _ => TerminalFact::Uncertain,
        },
        ExecutionState::Failed
            if observation
                .diagnostic
                .is_some_and(|diagnostic| diagnostic.stage.before_requested_exec()) =>
        {
            TerminalFact::Exited { status: None }
        }
        ExecutionState::Failed => TerminalFact::Uncertain,
        ExecutionState::Lost | ExecutionState::Abandoned | ExecutionState::Unknown => {
            TerminalFact::Uncertain
        }
    })
}

/// Read through the exact binding, never through an event's restart-local
/// handle. Agent metadata is only used while two reads bracket it with the
/// same durable resource and activation on this authenticated connection.
pub(super) async fn observe(
    conn: &mut Connection,
    pin: &ExecutionRef,
) -> Result<Option<(Option<AgentReport>, TerminalFact)>, String> {
    let (authority, key, _) = pin.validated().map_err(str::to_owned)?;
    let (actual, first) = show_key_level_on(conn, key)
        .await
        .map_err(|error| error.to_string())?;
    if actual != authority {
        return Ok(None);
    }
    let Some(first) = first else {
        return Ok(Some((None, TerminalFact::Uncertain)));
    };
    if first.execution_ref.as_ref() != Some(pin) {
        return Ok(Some((None, TerminalFact::Uncertain)));
    }
    let Some(fact) = first.execution.as_ref().and_then(terminal_fact) else {
        return Ok(None);
    };
    let Some(id) = first.terminal_id else {
        return Ok(Some((None, fact)));
    };
    let (record, _) = phux_client::agent_record::get_record(conn, &ResourceId::local(id), 2)
        .await
        .map_err(|error| error.to_string())?;
    let Answer::Ok(Some(record)) = record else {
        return Ok(Some((None, fact)));
    };
    let (actual, latest) = show_key_level_on(conn, key)
        .await
        .map_err(|error| error.to_string())?;
    if actual != authority {
        return Ok(None);
    }
    let Some(latest) = latest else {
        return Ok(Some((None, TerminalFact::Uncertain)));
    };
    if latest.execution_ref.as_ref() != Some(pin) {
        return Ok(Some((None, TerminalFact::Uncertain)));
    }
    let Some(latest_fact) = latest.execution.as_ref().and_then(terminal_fact) else {
        return Ok(None);
    };
    let report = (latest.terminal_id == Some(id)).then(|| report_of(record.state));
    Ok(Some((report, latest_fact)))
}
