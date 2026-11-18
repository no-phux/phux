use super::*;

fn id(n: u32) -> ResourceId {
    ResourceId::local(n)
}

fn record(state: &str) -> Event {
    Event::AgentMetadata {
        terminal_id: id(1),
        value: Some(format!(r#"{{"name":"claude","state":"{state}"}}"#).into_bytes()),
    }
}

#[test]
fn a_question_is_kept_until_the_level_retracts_it() {
    let mut a = Activities::default();
    let asked = Event::AgentAsked {
        terminal_id: id(1),
        question_id: "q1".to_owned(),
        text: "Ship it?".to_owned(),
        suggestions: vec!["yes".to_owned(), "no".to_owned()],
        waiting_seconds: Some(3),
    };
    a.apply(&asked, &[]);
    assert_eq!(a.ask(&id(1)).map(|ask| ask.id.as_str()), Some("q1"));
    assert!(a.mark(&id(1)).needs_you());
    a.apply(
        &Event::AgentAskedState {
            terminal_id: id(1),
            asked: false,
        },
        &[],
    );
    assert!(a.ask(&id(1)).is_none());
    assert!(!a.mark(&id(1)).needs_you());
}

#[test]
fn a_blocked_agent_needs_you_until_it_moves_on() {
    let mut a = Activities::default();
    a.apply(&record("blocked"), &[]);
    assert_eq!(a.mark(&id(1)), Mark::Blocked);
    assert!(a.mark(&id(1)).needs_you());
    a.apply(&record("working"), &[]);
    assert_eq!(a.mark(&id(1)), Mark::Working);
    assert!(a.mark(&id(1)).live());
    assert_eq!(a.agent_name(&id(1)), Some("claude"));
}

#[test]
fn finishing_off_screen_is_held_until_seen() {
    let mut a = Activities::default();
    a.apply(&record("working"), &[]);
    a.apply(&record("done"), &[]);
    assert_eq!(a.mark(&id(1)), Mark::Done);
    a.seen(&id(1));
    assert_eq!(a.mark(&id(1)), Mark::Idle);
}

#[test]
fn the_shown_terminal_collects_no_marks() {
    let mut a = Activities::default();
    a.apply(&record("working"), &[id(1)]);
    a.apply(&record("done"), &[id(1)]);
    a.apply(&Event::Bell { terminal_id: id(1) }, &[id(1)]);
    assert_eq!(a.mark(&id(1)), Mark::Idle);
}

#[test]
fn a_bell_and_a_question_mark_the_row() {
    let mut a = Activities::default();
    a.apply(&Event::Bell { terminal_id: id(1) }, &[]);
    assert_eq!(a.mark(&id(1)), Mark::Bell);
    a.apply(
        &Event::AgentAskedState {
            terminal_id: id(1),
            asked: true,
        },
        &[],
    );
    assert_eq!(a.mark(&id(1)), Mark::Blocked);
}

#[test]
fn a_restated_record_changes_nothing() {
    let mut a = Activities::default();
    assert!(a.apply(&record("working"), &[]));
    let revision = a.revision();
    assert!(!a.apply(&record("working"), &[]));
    // A working agent's commands sit under its own mark.
    assert!(!a.apply(&Event::CommandStarted { terminal_id: id(1) }, &[]));
    let finished = Event::CommandFinished {
        terminal_id: id(1),
        exit_code: Some(0),
    };
    assert!(!a.apply(&finished, &[]));
    assert_eq!(a.revision(), revision);
}

#[test]
fn a_question_announced_again_keeps_its_wait() {
    let asked = |seconds| Event::AgentAsked {
        terminal_id: id(1),
        question_id: "q1".to_owned(),
        text: "Ship it?".to_owned(),
        suggestions: Vec::new(),
        waiting_seconds: Some(seconds),
    };
    let mut a = Activities::default();
    assert!(a.apply(&asked(1), &[]));
    assert!(!a.apply(&asked(900), &[]));
}

#[test]
fn a_mark_is_dated_when_it_changes() {
    let mut a = Activities::default();
    a.apply(&record("blocked"), &[]);
    let first = a.changed_at(&id(1));
    assert!(first.is_some());
    a.apply(&record("blocked"), &[]);
    assert_eq!(a.changed_at(&id(1)), first);
}

#[test]
fn a_reconnect_settles_commands_only_on_an_answer() {
    let mut a = Activities::default();
    for n in 1..=3 {
        a.apply(&Event::CommandStarted { terminal_id: id(n) }, &[]);
    }
    let ids = [id(1), id(2)];
    assert!(a.reconnected(&ids));
    assert_eq!(a.mark(&id(3)), Mark::None);
    // No answer yet: no replica, or the alternate screen.
    assert!(!a.prove(&ids, |_| None));
    assert_eq!(a.mark(&id(1)), Mark::Running);
    assert!(a.unproven());
    // Ended in the gap, and still running.
    assert!(a.prove(&ids, |t| Some(*t == id(2))));
    assert_eq!(a.mark(&id(1)), Mark::None);
    assert_eq!(a.mark(&id(2)), Mark::Running);
    assert!(!a.unproven());
}

#[test]
fn a_doubted_command_still_reports_its_finish() {
    let mut a = Activities::default();
    a.apply(&Event::CommandStarted { terminal_id: id(1) }, &[]);
    let long_ago = Instant::now().checked_sub(LONG_COMMAND);
    a.terminals.get_mut(&id(1)).unwrap().command_since = long_ago;
    a.reconnected(&[id(1)]);
    a.prove(&[id(1)], |_| None);
    let finished = Event::CommandFinished {
        terminal_id: id(1),
        exit_code: Some(1),
    };
    a.apply(&finished, &[]);
    assert_eq!(a.mark(&id(1)), Mark::Failed);
    assert!(!a.unproven());
}

#[test]
fn a_reconnect_keeps_unseen_marks() {
    let mut a = Activities::default();
    a.apply(&Event::Bell { terminal_id: id(1) }, &[]);
    assert!(!a.reconnected(&[id(1)]));
    assert_eq!(a.mark(&id(1)), Mark::Bell);
}

#[test]
fn only_acp_questions_refuse_typing() {
    let ask = |id: &str| Ask {
        id: id.to_owned(),
        text: String::new(),
        suggestions: Vec::new(),
        since: None,
    };
    assert!(ask("q1").typed());
    assert!(!ask("acp-question:1").typed());
}

#[test]
fn a_running_command_shows_and_a_retracted_record_clears() {
    let mut a = Activities::default();
    a.apply(&Event::CommandStarted { terminal_id: id(1) }, &[]);
    assert_eq!(a.mark(&id(1)), Mark::Running);
    a.apply(
        &Event::CommandFinished {
            terminal_id: id(1),
            exit_code: Some(1),
        },
        &[],
    );
    // Short commands leave no mark behind.
    assert_eq!(a.mark(&id(1)), Mark::None);
    a.apply(&record("working"), &[]);
    a.apply(
        &Event::AgentMetadata {
            terminal_id: id(1),
            value: None,
        },
        &[],
    );
    assert_eq!(a.mark(&id(1)), Mark::None);
}

#[test]
fn an_idle_agent_with_work_in_the_background_is_told_apart() {
    let mut a = Activities::default();
    let background = |running| Event::BackgroundWork {
        terminal_id: id(1),
        running,
    };
    a.apply(&record("idle"), &[]);
    assert!(a.apply(&background(true), &[]));
    assert_eq!(a.mark(&id(1)), Mark::Background);
    assert!(!a.mark(&id(1)).needs_you() && !a.mark(&id(1)).live());
    // Working outranks it; it shows again once the agent settles.
    a.apply(&record("working"), &[]);
    assert_eq!(a.mark(&id(1)), Mark::Working);
    a.apply(&record("idle"), &[]);
    assert_eq!(a.mark(&id(1)), Mark::Background);
    a.apply(&background(false), &[]);
    assert_eq!(a.mark(&id(1)), Mark::Idle);
}

/// The brand's `tone_of` vectors through Rune's own path, facts to
/// `Activity` to ladder state, for the facts Rune can see (§1.3.1): an
/// agent, its attention, a question, and whether its result was seen.
#[test]
fn the_tone_of_vectors_hold_through_rune() {
    let vectors: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../crates/phux-design/tokens/derive-vectors.subset.json"
    )))
    .unwrap();
    let mut ran = 0;
    for case in vectors["tone_of"].as_array().unwrap() {
        let input = &case["input"];
        if ["exit", "link", "crashed"]
            .iter()
            .any(|k| !input[k].is_null())
        {
            continue;
        }
        let state = match input["agent"].as_str() {
            Some("Working") => Some(AgentState::Working),
            Some("Blocked") => Some(AgentState::Blocked),
            Some("Idle") => Some(AgentState::Idle),
            Some("Done") => Some(AgentState::Done),
            Some(_) => Some(AgentState::Unknown),
            None => None,
        };
        let attention = if input["attention"].as_bool().unwrap_or(false) {
            AgentAttention::High
        } else {
            AgentAttention::Normal
        };
        let seen = input["seen"].as_bool().unwrap_or(false);
        let activity = Activity {
            agent: state.map(|state| Agent {
                name: "agent".to_owned(),
                kind: None,
                state,
                attention,
            }),
            asked: input["asked"].as_bool().unwrap_or(false),
            done_unseen: state == Some(AgentState::Done) && !seen,
            ..Activity::default()
        };
        assert_eq!(
            activity.state().key(),
            case["expect"].as_str().unwrap(),
            "{input}"
        );
        ran += 1;
    }
    assert!(ran >= 8, "only {ran} vectors ran");
}

/// A command that succeeded off screen is a result not yet seen: `done`.
#[test]
fn a_command_that_succeeded_off_screen_is_done() {
    let activity = Activity {
        finished: Some(Some(0)),
        ..Activity::default()
    };
    assert_eq!(activity.state(), phux_design::State::Done);
    assert_eq!(activity.mark(), Mark::Succeeded);
    assert_eq!(Mark::Succeeded.state(), Some(phux_design::State::Done));
}
