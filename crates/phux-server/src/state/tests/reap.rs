//! Moving and reaping panes, and ephemeral-session cleanup.

use super::*;

/// Move `pane` into `to` through a committed `ResourceMove`.
fn move_pane(s: &mut ServerState, pane: ResourceId, to: phux_core::ids::SessionId) {
    let record = s.registry().record(pane).expect("pane").clone();
    let from = s.org().session(&record.session).expect("from").revision;
    let to_key = s.registry().session_key(to).expect("to");
    let to_revision = s.org().session(&to_key).expect("to").revision;
    s.commit(
        phux_core::Op::ResourceMove {
            resource: record.key,
            from: record.session,
            from_expected: from,
            to: to_key,
            to_expected: to_revision,
        },
        &phux_core::Actor::owner(),
    )
    .expect("move commits");
}

#[test]
fn a_move_that_empties_an_ephemeral_source_deletes_it() {
    // ADR-0160: membership moves; the process and its handle survive.
    let mut s = ServerState::new();
    let (sid_a, pid_a) = s
        .create_terminal(
            TerminalTarget::NewSession {
                name: "a",
                lifetime: phux_core::SessionLifetime::Ephemeral,
            },
            phux_core::TerminalLaunch::default(),
            phux_core::ExitPolicy::Remove,
            &phux_core::Actor::owner(),
        )
        .expect("a");
    let (sid_b, _pid_b) = s.seed_session("b");
    let attached = s.clients_mut().new_client_id();
    s.attach_default_caps(attached, "a", mk_tx()).unwrap();

    move_pane(&mut s, pid_a, sid_b);

    assert!(
        s.registry().session(sid_a).is_none(),
        "emptied ephemeral session deleted"
    );
    assert_eq!(
        s.take_killed_sessions(),
        vec![sid_a],
        "its clients are detached"
    );
    assert_eq!(
        s.clients()
            .attached_in_session(sid_a)
            .iter()
            .map(|(client, _)| *client)
            .collect::<Vec<_>>(),
        vec![attached],
        "session-attached clients remain discoverable by stable id after delete"
    );
    assert_eq!(s.registry().session_of(pid_a), Some(sid_b));

    // A persistent source survives empty.
    let (sid_c, pid_c) = s.seed_session("c");
    move_pane(&mut s, pid_c, sid_b);
    assert!(
        s.registry().session(sid_c).is_some(),
        "persistent source kept"
    );
    assert!(s.registry().terminal(pid_c).is_some());
}

#[test]
fn reaping_the_last_pane_of_an_ephemeral_session_deletes_it() {
    let mut s = ServerState::new();
    let (sid, pid) = s
        .create_terminal(
            TerminalTarget::NewSession {
                name: "quick",
                lifetime: phux_core::SessionLifetime::Ephemeral,
            },
            phux_core::TerminalLaunch::default(),
            phux_core::ExitPolicy::Remove,
            &phux_core::Actor::owner(),
        )
        .expect("quick");

    let idle = s.reap_resource(pid);

    assert!(idle, "nothing runs once the only pane is reaped");
    assert!(
        s.registry().session(sid).is_none(),
        "session deleted with it"
    );
    assert!(s.registry().terminal(pid).is_none());
}

#[test]
fn reap_clears_agent_asked_state() {
    let mut s = ServerState::new();
    let (_sid, pid) = s.seed_session("default");
    s.report_agent_asked(
        pid,
        phux_agent_session::asked::AskedSource::Hook,
        phux_agent_session::asked::AskedPayload {
            id: "hook".to_owned(),
            question: "Approve?".to_owned(),
            suggestions: Vec::new(),
            elapsed_seconds: None,
        },
    );
    assert!(s.current_agent_asked(pid).is_some());

    s.reap_resource(pid);

    assert!(s.current_agent_asked(pid).is_none());
}
