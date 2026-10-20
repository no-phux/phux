//! Pure projection from an observed agent and its terminal into a task state.
//!
//! No I/O. The keeper applies the result; replaying the same observation
//! writes nothing because the operation id is derived from the inputs.

use phux_work::{ObjectiveState, OpaqueId, OperationId};

/// What the agent record said, collapsed to the states this projection uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentReport {
    /// The agent is working.
    Working,
    /// The agent is blocked on a person.
    Blocked,
    /// The agent reported done.
    Done,
    /// Idle, unknown, or any other record.
    Other,
}

/// What level state says about the bound terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalFact {
    /// The process is running or frozen.
    Running,
    /// The process exited. `None` means the status was not reported.
    Exited {
        /// Process exit status.
        status: Option<i32>,
    },
    /// A complete level read no longer lists the terminal.
    Gone,
}

/// Whether the task's run is waiting, in progress, or finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPhase {
    /// Waiting for an agent slot.
    Queued,
    /// A worker is bound or being launched.
    Running,
    /// The run has ended.
    Ended,
}

/// One observation. The keeper fills this from level state or a live event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectionInput {
    /// Stored task state.
    pub current: ObjectiveState,
    /// The run this observation belongs to.
    pub run: RunPhase,
    /// Agent record, when one was read.
    pub agent: Option<AgentReport>,
    /// Terminal fact, when the task has a binding and level state was read.
    pub terminal: Option<TerminalFact>,
    /// The store has a binding row for this run.
    pub binding_present: bool,
    /// An unresolved `credential_lost` signal is open.
    pub credential_lost: bool,
    /// The keeper is admitting this queued run now.
    pub launching: bool,
}

/// The next stored state, and the event kind when the state changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Projection {
    /// State to store.
    pub next: ObjectiveState,
    /// `None` when `next` is already `current` and nothing should be written.
    pub event_kind: Option<&'static str>,
}

/// Project `input` into the next task state.
///
/// `done` is never the result. An unchanged state carries no event kind.
#[must_use]
pub fn project(input: &ProjectionInput) -> Projection {
    if input.current == ObjectiveState::Done {
        return stay(input.current);
    }
    if input.credential_lost {
        return change(input.current, ObjectiveState::NeedsYou, "needs_you");
    }
    if input.launching {
        return change(input.current, ObjectiveState::Working, "launched");
    }
    if !input.binding_present || input.run == RunPhase::Ended {
        return stay(input.current);
    }
    if input.terminal == Some(TerminalFact::Gone)
        && matches!(
            input.current,
            ObjectiveState::Working | ObjectiveState::NeedsYou
        )
    {
        return change(input.current, ObjectiveState::Failed, "terminal_lost");
    }
    if let Some(TerminalFact::Exited { status }) = input.terminal {
        let clean = status == Some(0);
        let saw_done = matches!(input.agent, Some(AgentReport::Done))
            || input.current == ObjectiveState::DoneUnchecked;
        if clean && saw_done {
            return change(
                input.current,
                ObjectiveState::DoneUnchecked,
                "done_unchecked",
            );
        }
        return change(input.current, ObjectiveState::Failed, "failed");
    }
    if matches!(
        input.current,
        ObjectiveState::DoneUnchecked | ObjectiveState::Failed
    ) {
        return stay(input.current);
    }
    match input.agent {
        Some(AgentReport::Working) => {
            change(input.current, ObjectiveState::Working, "agent_working")
        }
        Some(AgentReport::Blocked) => change(input.current, ObjectiveState::NeedsYou, "needs_you"),
        Some(AgentReport::Done) => change(
            input.current,
            ObjectiveState::DoneUnchecked,
            "done_unchecked",
        ),
        Some(AgentReport::Other) | None => stay(input.current),
    }
}

/// Operation id for one observation. The same inputs always yield the same id.
#[must_use]
pub fn observation_operation(
    objective: &OpaqueId,
    run: &OpaqueId,
    from: ObjectiveState,
    revision: u64,
    kind: &str,
    seq: u64,
) -> OperationId {
    let material = format!(
        "{}\n{}\n{}\n{revision}\n{kind}\n{seq}",
        objective.to_hex(),
        run.to_hex(),
        from.as_str(),
    );
    OperationId::derive_bytes(material.as_bytes())
}

const fn stay(current: ObjectiveState) -> Projection {
    Projection {
        next: current,
        event_kind: None,
    }
}

fn change(current: ObjectiveState, next: ObjectiveState, kind: &'static str) -> Projection {
    if current == next {
        stay(current)
    } else {
        Projection {
            next,
            event_kind: Some(kind),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AgentReport, ProjectionInput, RunPhase, TerminalFact, observation_operation, project,
    };
    use phux_work::{ObjectiveState, OpaqueId, OperationId};

    fn input(current: ObjectiveState) -> ProjectionInput {
        ProjectionInput {
            current,
            run: RunPhase::Running,
            agent: None,
            terminal: Some(TerminalFact::Running),
            binding_present: true,
            credential_lost: false,
            launching: false,
        }
    }

    fn apply(observed: ProjectionInput) -> super::Projection {
        let got = project(&observed);
        assert_ne!(got.next, ObjectiveState::Done);
        if got.next == observed.current {
            assert_eq!(got.event_kind, None);
        } else {
            assert!(got.event_kind.is_some());
        }
        got
    }

    #[test]
    fn launching_a_queued_task_starts_it() {
        let mut observed = input(ObjectiveState::Queued);
        observed.run = RunPhase::Queued;
        observed.terminal = None;
        observed.binding_present = false;
        observed.launching = true;
        let got = apply(observed);
        assert_eq!(got.next, ObjectiveState::Working);
        assert_eq!(got.event_kind, Some("launched"));
    }

    #[test]
    fn agent_reports_map_and_identical_reports_are_no_ops() {
        let working = apply(ProjectionInput {
            agent: Some(AgentReport::Working),
            ..input(ObjectiveState::Queued)
        });
        assert_eq!(working.next, ObjectiveState::Working);
        assert_eq!(working.event_kind, Some("agent_working"));
        assert_eq!(
            apply(ProjectionInput {
                agent: Some(AgentReport::Working),
                ..input(ObjectiveState::Working)
            })
            .event_kind,
            None
        );

        let blocked = apply(ProjectionInput {
            agent: Some(AgentReport::Blocked),
            ..input(ObjectiveState::Working)
        });
        assert_eq!(blocked.next, ObjectiveState::NeedsYou);
        assert_eq!(blocked.event_kind, Some("needs_you"));

        let resumed = apply(ProjectionInput {
            agent: Some(AgentReport::Working),
            ..input(ObjectiveState::NeedsYou)
        });
        assert_eq!(resumed.next, ObjectiveState::Working);
        assert_eq!(resumed.event_kind, Some("agent_working"));

        let done = apply(ProjectionInput {
            agent: Some(AgentReport::Done),
            ..input(ObjectiveState::Working)
        });
        assert_eq!(done.next, ObjectiveState::DoneUnchecked);
        assert_eq!(done.event_kind, Some("done_unchecked"));

        assert_eq!(
            apply(ProjectionInput {
                agent: Some(AgentReport::Other),
                ..input(ObjectiveState::Working)
            })
            .event_kind,
            None
        );
        assert_eq!(
            apply(ProjectionInput {
                agent: None,
                ..input(ObjectiveState::Working)
            })
            .event_kind,
            None
        );
    }

    #[test]
    fn exit_status_distinguishes_done_from_failure() {
        let clean = apply(ProjectionInput {
            agent: Some(AgentReport::Done),
            terminal: Some(TerminalFact::Exited { status: Some(0) }),
            ..input(ObjectiveState::Working)
        });
        assert_eq!(clean.next, ObjectiveState::DoneUnchecked);
        assert_eq!(clean.event_kind, Some("done_unchecked"));

        assert_eq!(
            apply(ProjectionInput {
                terminal: Some(TerminalFact::Exited { status: Some(0) }),
                ..input(ObjectiveState::DoneUnchecked)
            })
            .event_kind,
            None
        );

        let dirty = apply(ProjectionInput {
            agent: Some(AgentReport::Done),
            terminal: Some(TerminalFact::Exited { status: Some(1) }),
            ..input(ObjectiveState::Working)
        });
        assert_eq!(dirty.next, ObjectiveState::Failed);
        assert_eq!(dirty.event_kind, Some("failed"));

        let without_done = apply(ProjectionInput {
            terminal: Some(TerminalFact::Exited { status: Some(0) }),
            ..input(ObjectiveState::Working)
        });
        assert_eq!(without_done.next, ObjectiveState::Failed);
        assert_eq!(without_done.event_kind, Some("failed"));

        let unknown = apply(ProjectionInput {
            terminal: Some(TerminalFact::Exited { status: None }),
            ..input(ObjectiveState::NeedsYou)
        });
        assert_eq!(unknown.next, ObjectiveState::Failed);

        let after = apply(ProjectionInput {
            terminal: Some(TerminalFact::Exited { status: Some(9) }),
            ..input(ObjectiveState::DoneUnchecked)
        });
        assert_eq!(after.next, ObjectiveState::Failed);
        assert_eq!(after.event_kind, Some("failed"));
    }

    #[test]
    fn a_missing_terminal_fails_only_work_in_progress() {
        let lost = apply(ProjectionInput {
            terminal: Some(TerminalFact::Gone),
            ..input(ObjectiveState::Working)
        });
        assert_eq!(lost.next, ObjectiveState::Failed);
        assert_eq!(lost.event_kind, Some("terminal_lost"));
        assert_eq!(
            apply(ProjectionInput {
                terminal: Some(TerminalFact::Gone),
                ..input(ObjectiveState::NeedsYou)
            })
            .event_kind,
            Some("terminal_lost")
        );
        assert_eq!(
            apply(ProjectionInput {
                terminal: Some(TerminalFact::Gone),
                ..input(ObjectiveState::DoneUnchecked)
            })
            .event_kind,
            None
        );
        assert_eq!(
            apply(ProjectionInput {
                terminal: Some(TerminalFact::Gone),
                ..input(ObjectiveState::Failed)
            })
            .event_kind,
            None
        );
    }

    #[test]
    fn credential_lost_wins_and_terminal_states_do_not_reopen() {
        let lost = apply(ProjectionInput {
            credential_lost: true,
            agent: Some(AgentReport::Working),
            terminal: Some(TerminalFact::Exited { status: Some(1) }),
            ..input(ObjectiveState::Working)
        });
        assert_eq!(lost.next, ObjectiveState::NeedsYou);
        assert_eq!(lost.event_kind, Some("needs_you"));
        assert_eq!(
            apply(ProjectionInput {
                credential_lost: true,
                ..input(ObjectiveState::NeedsYou)
            })
            .event_kind,
            None
        );
        assert_eq!(
            apply(ProjectionInput {
                credential_lost: true,
                terminal: Some(TerminalFact::Gone),
                ..input(ObjectiveState::Failed)
            })
            .next,
            ObjectiveState::NeedsYou
        );
        assert_eq!(
            project(&ProjectionInput {
                credential_lost: true,
                ..input(ObjectiveState::Done)
            })
            .next,
            ObjectiveState::Done
        );
        assert_eq!(
            apply(ProjectionInput {
                agent: Some(AgentReport::Working),
                ..input(ObjectiveState::Failed)
            })
            .event_kind,
            None
        );
        assert_eq!(
            apply(ProjectionInput {
                agent: Some(AgentReport::Done),
                ..input(ObjectiveState::DoneUnchecked)
            })
            .event_kind,
            None
        );
    }

    #[test]
    fn an_unbound_task_is_unchanged_until_launch() {
        let mut queued = input(ObjectiveState::Queued);
        queued.run = RunPhase::Queued;
        queued.binding_present = false;
        queued.terminal = None;
        queued.agent = Some(AgentReport::Working);
        assert_eq!(apply(queued).event_kind, None);
        let mut ended = input(ObjectiveState::Working);
        ended.run = RunPhase::Ended;
        ended.agent = Some(AgentReport::Done);
        assert_eq!(apply(ended).event_kind, None);
    }

    #[test]
    fn observation_ids_are_stable_and_change_with_the_input() {
        let objective = OpaqueId::derive_bytes(b"objective");
        let run = OpaqueId::derive_bytes(b"run");
        let first =
            observation_operation(&objective, &run, ObjectiveState::Working, 2, "needs_you", 0);
        let again =
            observation_operation(&objective, &run, ObjectiveState::Working, 2, "needs_you", 0);
        assert_eq!(first, again);
        assert_ne!(first, OperationId::derive_bytes(&[0]));
        assert_ne!(
            first,
            observation_operation(&objective, &run, ObjectiveState::Working, 3, "needs_you", 0)
        );
        assert_ne!(
            first,
            observation_operation(
                &objective,
                &run,
                ObjectiveState::NeedsYou,
                2,
                "agent_working",
                0
            )
        );
    }
}
