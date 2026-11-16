//! Translating phux observations into the mission projection's inputs.

use super::{
    AgentEvent, AgentMetaState, AgentReport, OpenRun, ResourceId, ResourceInfo, ResourceKind,
    ResourceLifecycle, RunPhase, RunState, TerminalFact, WatchItem, terminal_key,
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

pub(super) fn fact_of(resource: &ResourceInfo) -> TerminalFact {
    if resource.lifecycle == ResourceLifecycle::Exited {
        TerminalFact::Exited {
            status: resource.exit.as_ref().and_then(|exit| exit.exit_status),
        }
    } else {
        TerminalFact::Running
    }
}

struct SeenExit {
    status: Option<i32>,
}

const fn seen_exit(event: &AgentEvent) -> Option<SeenExit> {
    match event {
        AgentEvent::TerminalControl {
            lifecycle: ResourceLifecycle::Exited,
            exit_status,
            ..
        }
        | AgentEvent::ResourceClosed { exit_status } => Some(SeenExit {
            status: *exit_status,
        }),
        _ => None,
    }
}

pub(super) struct ExitFact {
    pub(super) terminal: ResourceId,
    pub(super) status: Option<i32>,
    pub(super) seq: u64,
}

pub(super) fn exit_of(item: &WatchItem) -> Option<ExitFact> {
    let WatchItem::Event(event) = item else {
        return None;
    };
    let exit = seen_exit(&event.event)?;
    Some(ExitFact {
        terminal: event.terminal.clone()?,
        status: exit.status,
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

pub(super) fn agent_is_done(item: &WatchItem, id: &str) -> bool {
    let WatchItem::AgentState(update) = item else {
        return false;
    };
    let Some(terminal) = update.terminal.as_ref() else {
        return false;
    };
    if terminal_key(terminal).as_deref() != Some(id) {
        return false;
    }
    update
        .record
        .as_ref()
        .is_some_and(|record| report_of(record.state) == AgentReport::Done)
}
