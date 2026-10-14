//! [`phux_core::Registry`]: live handles over committed records.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use phux_core::{
    AgentLaunch, ExecState, ExitOutcome, ExitPolicy, LaunchSpec, NewResource, Op, Org, Outcome,
    Registry, ResourceId, ResourceKind, SessionId, SessionLifetime, Stamp, TerminalLaunch,
};

use crate::support::{Harness, INSTANCE, job, shell};

fn agent_of(parent: phux_core::ResourceKey) -> NewResource {
    NewResource {
        kind: ResourceKind::AgentSession,
        name: None,
        launch: Some(LaunchSpec::Agent(AgentLaunch {
            provider: "claude".to_owned(),
            native_id: Some("abc".to_owned()),
        })),
        on_exit: ExitPolicy::Retain,
        parent: Some(parent),
        launch_mode: phux_core::LaunchMode::Immediate,
    }
}

#[test]
fn a_new_registry_indexes_the_inbox_and_handles_round_trip() {
    let h = Harness::new();
    let inbox = h.inbox();
    let id = h.reg.session_id(&inbox).expect("inbox has a handle");
    assert_eq!(h.reg.session_key(id), Some(inbox));
    assert_eq!(h.reg.session(id).map(|s| s.name.as_str()), Some("intake"));
    assert_eq!(h.reg.session_by_name("intake"), Some(id));
    assert_eq!(h.reg.session_count(), 1);
    assert!(h.reg.session(SessionId::default()).is_none());
    assert!(h.reg.resource(ResourceId::default()).is_none());
}

#[test]
fn authority_retains_legacy_serialized_designation_key() {
    let h = Harness::new();
    let authority = h.reg.org().authority();
    let value = serde_json::to_value(authority).unwrap();
    assert!(value.get("inbox_session").is_some());
    assert!(value.get("intake_session").is_none());
    let restored: phux_core::Authority = serde_json::from_value(value).unwrap();
    assert_eq!(restored, *authority);
    assert_eq!(restored.intake_session(), h.inbox());
}

#[test]
fn creation_issues_handles_and_seeds_facets_from_the_launch_spec() {
    let mut h = Harness::new();
    let s = h.create_session("work", SessionLifetime::Persistent);
    let t = h.create_resource(
        s,
        NewResource {
            launch: Some(LaunchSpec::Terminal(TerminalLaunch {
                argv: vec!["vim".to_owned()],
                cwd: Some("/repo".into()),
                env: Vec::new(),
                restore: None,
            })),
            ..shell()
        },
    );
    let second = h.shell_in(s);

    let sid = h.reg.session_id(&s).expect("session handle");
    let tid = h.reg.resource_id(&t).expect("resource handle");
    let desc = h.reg.resource(tid).expect("descriptor");
    assert_eq!((desc.key, desc.kind), (t, ResourceKind::Terminal));
    let facet = h.reg.terminal(tid).expect("terminal facet");
    assert_eq!(facet.dims, (80, 24));
    assert_eq!(facet.cwd, std::path::PathBuf::from("/repo"));
    assert_eq!(h.reg.session_of(tid), Some(sid));
    assert_eq!(
        h.reg.members(sid),
        vec![tid, h.reg.resource_id(&second).expect("handle")]
    );
    assert_eq!(h.reg.terminal_count(), 2);
    let record = h.reg.record(tid).expect("record");
    assert_eq!(record.session, s);
    assert_eq!(
        record.activation.as_ref().map(|a| (a.state, a.instance)),
        Some((ExecState::Starting, INSTANCE))
    );

    h.reg.terminal_mut(tid).expect("facet").dims = (120, 40);
    assert_eq!(h.reg.terminal(tid).map(|f| f.dims), Some((120, 40)));
}

#[test]
fn agent_conversations_bind_to_a_terminal_and_outlive_it_as_records() {
    let mut h = Harness::new();
    let s = h.create_session("work", SessionLifetime::Persistent);
    let t = h.create_resource(s, job());
    let a = h.create_resource(s, agent_of(t));
    let (tid, aid) = (
        h.reg.resource_id(&t).expect("handle"),
        h.reg.resource_id(&a).expect("handle"),
    );
    let desc = h.reg.resource(aid).expect("descriptor");
    assert_eq!(desc.parent, Some(tid));
    assert_eq!(desc.agent().map(|f| f.provider.as_str()), Some("claude"));
    assert!(h.reg.terminal(aid).is_none());
    assert_eq!(h.reg.children(tid), vec![aid]);

    // End and remove the terminal: the conversation stays, unbound.
    h.report(Op::ActivationEnded {
        resource: t,
        activation: phux_core::ActivationId::FIRST,
        state: ExecState::Completed,
        outcome: Outcome::Exit(ExitOutcome::exited(0)),
    })
    .expect("ended");
    h.run(Op::ResourceRemove { resource: t }).expect("removed");
    assert!(h.reg.resource(tid).is_none());
    assert_eq!(h.reg.resource(aid).and_then(|d| d.parent), None);
    assert_eq!(
        h.reg.record(aid).map(|r| r.provenance.parent),
        Some(Some(t))
    );
}

#[test]
fn a_move_changes_membership_and_keeps_every_handle() {
    let mut h = Harness::new();
    let a = h.create_session("a", SessionLifetime::Persistent);
    let b = h.create_session("b", SessionLifetime::Persistent);
    let t = h.shell_in(a);
    let tid = h.reg.resource_id(&t).expect("handle");
    let (ra, rb) = (h.session(a).revision, h.session(b).revision);

    h.run(Op::ResourceMove {
        resource: t,
        from: a,
        from_expected: ra,
        to: b,
        to_expected: rb,
    })
    .expect("move");
    assert_eq!(h.reg.resource_id(&t), Some(tid));
    assert_eq!(h.reg.session_of(tid), h.reg.session_id(&b));
    assert!(h.reg.members(h.reg.session_id(&a).expect("a")).is_empty());
    assert_eq!(h.reg.members(h.reg.session_id(&b).expect("b")), vec![tid]);
}

#[test]
fn commit_reports_released_handles_when_an_ephemeral_session_empties() {
    let mut h = Harness::new();
    let s = h.create_session("quick", SessionLifetime::Ephemeral);
    let sid = h.reg.session_id(&s).expect("handle");
    let t = h.session(s).members[0];
    let tid = h.reg.resource_id(&t).expect("handle");

    h.report(Op::ActivationStarted {
        resource: t,
        activation: phux_core::ActivationId::FIRST,
        proc: None,
    })
    .expect("started");
    let operation = h.next_op();
    let applied = phux_core::apply(
        h.reg.org(),
        &phux_core::Command {
            operation,
            op: Op::ActivationEnded {
                resource: t,
                activation: phux_core::ActivationId::FIRST,
                state: ExecState::Completed,
                outcome: Outcome::Exit(ExitOutcome::exited(0)),
            },
        },
        &phux_core::Actor::system(),
        Stamp(100),
        &mut h.keys,
    )
    .expect("ended");
    let committed = h.reg.commit(&applied.transition);
    assert_eq!(committed.resources_removed, vec![(t, tid)]);
    assert_eq!(committed.sessions_removed, vec![(s, sid)]);
    assert!(h.reg.session(sid).is_none() && h.reg.resource(tid).is_none());
    assert!(h.reg.org().check_invariants().is_ok());
}

#[test]
fn set_recent_accepts_members_only() {
    let mut h = Harness::new();
    let a = h.create_session("a", SessionLifetime::Persistent);
    let b = h.create_session("b", SessionLifetime::Persistent);
    let t = h.shell_in(a);
    let tid = h.reg.resource_id(&t).expect("handle");
    let (sa, sb) = (
        h.reg.session_id(&a).expect("a"),
        h.reg.session_id(&b).expect("b"),
    );
    let rev = h.session(a).revision;
    assert!(h.reg.set_recent(sa, Some(tid)));
    assert_eq!(h.session(a).recent, Some(t));
    assert_eq!(h.session(a).revision, rev, "recent is unversioned");
    assert!(!h.reg.set_recent(sb, Some(tid)));
    assert!(h.reg.set_recent(sa, None));
    assert_eq!(h.session(a).recent, None);
}

#[test]
fn a_restored_org_gets_handles_for_every_record() {
    let mut h = Harness::new();
    let s = h.create_session("work", SessionLifetime::Persistent);
    let t = h.create_resource(s, job());
    let a = h.create_resource(s, agent_of(t));
    let org = h.reg.org();
    let restored = Org::restore(
        *org.authority(),
        INSTANCE,
        org.sessions().cloned(),
        org.resources().cloned(),
        org.arrangements().cloned(),
        org.receipts().cloned(),
    )
    .expect("consistent records restore");
    let reg = Registry::new(restored);
    assert_eq!(reg.session_count(), 2);
    let (tid, aid) = (
        reg.resource_id(&t).expect("terminal"),
        reg.resource_id(&a).expect("agent"),
    );
    assert_eq!(reg.resource(aid).and_then(|d| d.parent), Some(tid));

    let mut broken: Vec<_> = org.sessions().cloned().collect();
    for session in &mut broken {
        session.members.clear();
    }
    assert!(
        Org::restore(
            *org.authority(),
            INSTANCE,
            broken,
            org.resources().cloned(),
            Vec::new(),
            Vec::new(),
        )
        .is_err(),
        "a resource outside every member list is refused"
    );
}
