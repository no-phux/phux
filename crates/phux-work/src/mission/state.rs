//! Pure projection from an observed agent and its terminal into a task state.
//!
//! No I/O. The keeper applies the result; replaying the same observation
//! writes nothing because the operation id is derived from the inputs.

use crate::{ObjectiveState, OpaqueId, OperationId};

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
    /// The exact result is unavailable or its ownership is uncertain.
    Uncertain,
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
    if input.terminal == Some(TerminalFact::Uncertain) {
        return match input.current {
            ObjectiveState::Working => {
                change(input.current, ObjectiveState::NeedsYou, "binding_uncertain")
            }
            state => stay(state),
        };
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
    use crate::{ObjectiveState, OpaqueId, OperationId};

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

    use AgentReport::{Blocked, Done as AgentDone, Other, Working as AgentWorking};
    use ObjectiveState::{Done, DoneUnchecked, Failed, NeedsYou, Queued, Working};
    use TerminalFact::{Exited, Running, Uncertain};

    /// One projection case: label, stored state, observed agent report,
    /// terminal fact and credential loss, then the expected state and event.
    /// An absent event means the state is unchanged (see [`apply`]).
    type Case<'a> = (
        &'a str,
        ObjectiveState,
        Option<AgentReport>,
        TerminalFact,
        bool,
        ObjectiveState,
        Option<&'a str>,
    );

    fn check(cases: &[Case<'_>]) {
        for &(label, current, agent, terminal, credential_lost, next, event) in cases {
            let got = apply(ProjectionInput {
                agent,
                terminal: Some(terminal),
                credential_lost,
                ..input(current)
            });
            assert_eq!(got.next, next, "{label}");
            assert_eq!(got.event_kind, event, "{label}");
        }
    }

    const fn exited(status: Option<i32>) -> TerminalFact {
        Exited { status }
    }

    #[test]
    fn agent_reports_map_and_identical_reports_are_no_ops() {
        #[rustfmt::skip]
        let cases: &[Case<'_>] = &[
            ("working from queued", Queued,   Some(AgentWorking), Running, false, Working,       Some("agent_working")),
            ("working again",       Working,  Some(AgentWorking), Running, false, Working,       None),
            ("blocked",             Working,  Some(Blocked),      Running, false, NeedsYou,      Some("needs_you")),
            ("resumed",             NeedsYou, Some(AgentWorking), Running, false, Working,       Some("agent_working")),
            ("done",                Working,  Some(AgentDone),    Running, false, DoneUnchecked, Some("done_unchecked")),
            ("other word",          Working,  Some(Other),        Running, false, Working,       None),
            ("no report",           Working,  None,               Running, false, Working,       None),
        ];
        check(cases);
    }

    #[test]
    fn exit_status_distinguishes_done_from_failure() {
        #[rustfmt::skip]
        let cases: &[Case<'_>] = &[
            ("done, clean exit",        Working,       Some(AgentDone), exited(Some(0)), false, DoneUnchecked, Some("done_unchecked")),
            ("clean exit after done",   DoneUnchecked, None,            exited(Some(0)), false, DoneUnchecked, None),
            ("done, dirty exit",        Working,       Some(AgentDone), exited(Some(1)), false, Failed,        Some("failed")),
            ("clean exit without done", Working,       None,            exited(Some(0)), false, Failed,        Some("failed")),
            ("unknown status",          NeedsYou,      None,            exited(None),    false, Failed,        Some("failed")),
            ("dirty exit after done",   DoneUnchecked, None,            exited(Some(9)), false, Failed,        Some("failed")),
        ];
        check(cases);
    }

    #[test]
    fn unavailable_evidence_only_requests_attention_for_work_in_progress() {
        #[rustfmt::skip]
        let cases: &[Case<'_>] = &[
            ("working",        Working,       None, Uncertain, false, NeedsYou,      Some("binding_uncertain")),
            ("needs you",      NeedsYou,      None, Uncertain, false, NeedsYou,      None),
            ("done unchecked", DoneUnchecked, None, Uncertain, false, DoneUnchecked, None),
            ("failed",         Failed,        None, Uncertain, false, Failed,        None),
        ];
        check(cases);
    }

    #[test]
    fn credential_lost_wins_and_terminal_states_do_not_reopen() {
        #[rustfmt::skip]
        let cases: &[Case<'_>] = &[
            ("lost over a dirty exit", Working,       Some(AgentWorking), exited(Some(1)), true,  NeedsYou,      Some("needs_you")),
            ("lost while needing you", NeedsYou,      None,               Running,         true,  NeedsYou,      None),
            ("lost reopens a failure", Failed,        None,               Uncertain,       true,  NeedsYou,      Some("needs_you")),
            ("working after failure",  Failed,        Some(AgentWorking), Running,         false, Failed,        None),
            ("done after done",        DoneUnchecked, Some(AgentDone),    Running,         false, DoneUnchecked, None),
        ];
        check(cases);
        let done = project(&ProjectionInput {
            credential_lost: true,
            ..input(Done)
        });
        assert_eq!(done.next, Done, "a finished objective stays done");
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
