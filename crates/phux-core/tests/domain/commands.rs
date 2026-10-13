//! [`phux_core::apply`]: each command's effect and each of its refusals.
//! The harness asserts that every refusal leaves the organization
//! unchanged and that every success preserves all invariants.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use phux_core::{
    ActivationId, Actor, ArrangementOwner, Attrs, Command, Effect, EntryRef, Event, ExecState,
    ExitOutcome, Group, LaunchMode, LayoutNode, NewResource, Op, Outcome, ProcIdentity, Refusal,
    Reply, ResourceKey, ResourceKind, Revision, SessionKey, SessionLifetime, SourcePin, SplitDir,
    TerminateMode, Verb,
};

use crate::support::{Harness, created, job, shell};

const A1: ActivationId = ActivationId::FIRST;

const fn ended(resource: ResourceKey, outcome: Outcome) -> Op {
    Op::ActivationEnded {
        resource,
        activation: A1,
        state: ExecState::Completed,
        outcome,
    }
}

const fn exited() -> Outcome {
    Outcome::Exit(ExitOutcome::exited(0))
}

fn create(name: &str, lifetime: SessionLifetime, resource: Option<NewResource>) -> Op {
    Op::SessionCreate {
        name: name.to_owned(),
        lifetime,
        attrs: Attrs::default(),
        resource,
    }
}

// ---- sessions --------------------------------------------------------------

#[test]
fn session_create_commits_session_resource_and_launch_together() {
    let mut h = Harness::new();
    let applied = h
        .run(create("work", SessionLifetime::Persistent, Some(shell())))
        .expect("create");
    let (s, r) = created(&applied);
    let (s, r) = (s.expect("session"), r.expect("resource"));
    assert_eq!(h.session(s).members, vec![r]);
    assert_eq!(h.session(s).revision, Revision::FIRST);
    assert!(matches!(
        applied.effects.as_slice(),
        [Effect::Launch { resource, activation, .. }] if *resource == r && *activation == A1
    ));
    assert!(matches!(
        applied.transition.events.as_slice(),
        [
            Event::SessionCreated { .. },
            Event::ResourceCreated { .. },
            Event::ActivationChanged {
                state: ExecState::Starting,
                ..
            },
            Event::ArrangementChanged { .. }
        ]
    ));
    assert_eq!(h.resource(r).provenance.operation, applied.operation);
    // The arrangement comes with the session, its first member placed.
    let ak = h.session(s).arrangement.expect("arranged at creation");
    let arrangement = h.reg.org().arrangement(&ak).expect("committed").clone();
    assert_eq!(arrangement.revision, Revision::FIRST);
    assert_eq!(arrangement.groups.len(), 1);
    assert_eq!(arrangement.groups[0].name, None);
    assert_eq!(arrangement.resources(), vec![r]);
    assert!(matches!(
        applied.reply,
        Reply::Ok { created, .. } if created.arrangement == Some(ak)
    ));

    // A bare create launches nothing and arranges nothing.
    let bare = h
        .run(create("empty", SessionLifetime::Persistent, None))
        .expect("bare");
    assert!(bare.effects.is_empty());
    let (empty, _) = created(&bare);
    let ak = h
        .session(empty.expect("session"))
        .arrangement
        .expect("arranged at creation");
    assert!(
        h.reg
            .org()
            .arrangement(&ak)
            .expect("committed")
            .groups
            .is_empty()
    );
}

#[test]
fn a_first_group_gives_every_member_an_even_share() {
    let members: Vec<_> = (1..=3_u8)
        .map(|n| ResourceKey::from_bytes([n; 16]))
        .collect();
    let mut keys = phux_core::SequentialKeys::new(*b"groupkey");
    let group = phux_core::first_group(&members, &mut keys).expect("a group");
    let LayoutNode::Split {
        ratio, ref left, ..
    } = group.tree
    else {
        panic!("three members split: {:?}", group.tree);
    };
    assert!((ratio - 2.0 / 3.0).abs() < f32::EPSILON);
    assert!(matches!(
        **left,
        LayoutNode::Split { ratio, .. } if (ratio - 0.5).abs() < f32::EPSILON
    ));
    let leaves: Vec<ResourceKey> = group
        .tree
        .leaves()
        .into_iter()
        .filter_map(EntryRef::resource)
        .collect();
    assert_eq!(leaves, members);
    assert!(phux_core::first_group(&[], &mut keys).is_none());
}

#[test]
fn session_create_refusals() {
    let mut h = Harness::new();
    h.create_session("work", SessionLifetime::Persistent);
    assert_eq!(
        h.run(create("work", SessionLifetime::Persistent, None))
            .err(),
        Some(Refusal::NameTaken)
    );
    assert_eq!(
        h.run(create("inbox", SessionLifetime::Persistent, None))
            .err(),
        Some(Refusal::NameTaken)
    );
    for bad in ["", "   ", "tab\there", &"x".repeat(257)] {
        assert!(matches!(
            h.run(create(bad, SessionLifetime::Persistent, None)),
            Err(Refusal::InvalidArgument { field: "name", .. })
        ));
    }
    assert!(matches!(
        h.run(create("quick", SessionLifetime::Ephemeral, None)),
        Err(Refusal::InvalidArgument {
            field: "resource",
            ..
        })
    ));
    // A failed first resource refuses the whole create.
    let no_launch = NewResource {
        launch: None,
        ..shell()
    };
    assert!(matches!(
        h.run(create("x", SessionLifetime::Persistent, Some(no_launch))),
        Err(Refusal::InvalidArgument {
            field: "launch",
            ..
        })
    ));
    assert!(h.reg.org().session_by_name("x").is_none());
}

#[test]
fn session_rename_and_lifetime() {
    let mut h = Harness::new();
    let s = h.create_session("a", SessionLifetime::Persistent);
    h.create_session("b", SessionLifetime::Persistent);
    let r1 = h.session(s).revision;

    assert_eq!(
        h.run(Op::SessionRename {
            session: s,
            expected: r1.next(),
            name: "c".to_owned(),
        })
        .err(),
        Some(Refusal::Conflict { current: r1 })
    );
    assert_eq!(
        h.run(Op::SessionRename {
            session: s,
            expected: r1,
            name: "b".to_owned(),
        })
        .err(),
        Some(Refusal::NameTaken)
    );
    assert_eq!(
        h.run(Op::SessionRename {
            session: SessionKey::from_bytes([7; 16]),
            expected: r1,
            name: "z".to_owned(),
        })
        .err(),
        Some(Refusal::NotFound)
    );
    h.run(Op::SessionRename {
        session: s,
        expected: r1,
        name: "c".to_owned(),
    })
    .expect("rename");
    assert_eq!(h.session(s).name, "c");
    assert_eq!(h.session(s).revision, r1.next());

    // Ephemeral on an empty session deletes it.
    let r2 = h.session(s).revision;
    h.run(Op::SessionSetLifetime {
        session: s,
        expected: r2,
        lifetime: SessionLifetime::Ephemeral,
    })
    .expect("ephemeral");
    assert!(h.reg.org().session(&s).is_none());

    let inbox = h.inbox();
    let ri = h.session(inbox).revision;
    assert!(matches!(
        h.run(Op::SessionSetLifetime {
            session: inbox,
            expected: ri,
            lifetime: SessionLifetime::Ephemeral,
        }),
        Err(Refusal::InvalidArgument {
            field: "lifetime",
            ..
        })
    ));
}

#[test]
fn session_delete_refuses_members_before_revisions_and_never_deletes_inbox() {
    let mut h = Harness::new();
    let s = h.create_session("work", SessionLifetime::Persistent);
    let seen = h.session(s).revision;
    let t = h.create_resource(s, job());
    assert_eq!(
        h.run(Op::SessionDelete {
            session: s,
            expected: seen,
        })
        .err(),
        Some(Refusal::NotEmpty { members: 1 }),
        "work added after the caller looked survives"
    );
    h.report(ended(t, exited())).expect("ended");
    let rt = h.resource(t).revision;
    h.run(Op::ResourceRemove {
        resource: t,
        expected: rt,
    })
    .expect("remove");
    let now = h.session(s).revision;
    assert_eq!(
        h.run(Op::SessionDelete {
            session: s,
            expected: seen,
        })
        .err(),
        Some(Refusal::Conflict { current: now })
    );
    h.run(Op::SessionDelete {
        session: s,
        expected: now,
    })
    .expect("delete");
    assert!(h.reg.org().session(&s).is_none());

    let inbox = h.inbox();
    let ri = h.session(inbox).revision;
    assert!(matches!(
        h.run(Op::SessionDelete {
            session: inbox,
            expected: ri,
        }),
        Err(Refusal::InvalidArgument { .. })
    ));
}

// ---- resources -------------------------------------------------------------

#[test]
fn resource_create_defaults_to_inbox_and_validates_kind() {
    let mut h = Harness::new();
    let applied = h
        .run(Op::ResourceCreate {
            session: None,
            resource: shell(),
        })
        .expect("create");
    let r = created(&applied).1.expect("resource");
    assert_eq!(h.resource(r).session, h.inbox());

    let missing = SessionKey::from_bytes([7; 16]);
    assert_eq!(
        h.run(Op::ResourceCreate {
            session: Some(missing),
            resource: shell(),
        })
        .err(),
        Some(Refusal::NotFound)
    );
    let orphan_agent = NewResource {
        kind: ResourceKind::AgentSession,
        launch: None,
        ..shell()
    };
    assert!(matches!(
        h.run(Op::ResourceCreate {
            session: None,
            resource: orphan_agent,
        }),
        Err(Refusal::InvalidArgument {
            field: "parent",
            ..
        })
    ));
    let wrong_spec = NewResource {
        kind: ResourceKind::AgentSession,
        parent: Some(r),
        ..shell()
    };
    assert!(matches!(
        h.run(Op::ResourceCreate {
            session: None,
            resource: wrong_spec,
        }),
        Err(Refusal::InvalidArgument {
            field: "launch",
            ..
        })
    ));
}

#[test]
fn a_deferred_create_is_dormant_until_relaunched() {
    let mut h = Harness::new();
    let s = h.create_session("work", SessionLifetime::Persistent);
    let applied = h
        .run(Op::ResourceCreate {
            session: Some(s),
            resource: NewResource {
                launch_mode: LaunchMode::Deferred,
                ..job()
            },
        })
        .expect("deferred create");
    assert!(applied.effects.is_empty(), "nothing launches");
    assert!(matches!(applied.reply, Reply::Ok { .. }));
    assert!(
        !applied
            .transition
            .events
            .iter()
            .any(|e| matches!(e, Event::ActivationChanged { .. })),
        "no activation event: {:?}",
        applied.transition.events
    );
    let r = created(&applied).1.expect("resource");
    let record = h.resource(r);
    assert!(record.activation.is_none(), "dormant: no activation");
    assert!(record.launch.is_some(), "the launch spec is stored");
    assert!(h.session(s).members.contains(&r));

    let relaunched = h
        .run(Op::ResourceRelaunch {
            resource: r,
            expected: record.revision,
        })
        .expect("relaunch a dormant record");
    assert!(matches!(
        relaunched.effects.as_slice(),
        [Effect::Launch { activation, .. }] if *activation == A1
    ));
    let a = h.resource(r).activation.expect("activation");
    assert_eq!((a.id, a.state), (A1, ExecState::Starting));

    let without_spec = NewResource {
        kind: ResourceKind::AgentSession,
        launch: None,
        parent: Some(r),
        launch_mode: LaunchMode::Deferred,
        ..shell()
    };
    assert!(matches!(
        h.run(Op::ResourceCreate {
            session: Some(s),
            resource: without_spec,
        }),
        Err(Refusal::InvalidArgument {
            field: "launch_mode",
            ..
        })
    ));
}

#[test]
fn resource_rename_checks_revision_and_name() {
    let mut h = Harness::new();
    let r = h.shell_in(h.inbox());
    let rev = h.resource(r).revision;
    assert_eq!(
        h.run(Op::ResourceRename {
            resource: r,
            expected: rev.next(),
            name: Some("x".to_owned()),
        })
        .err(),
        Some(Refusal::Conflict { current: rev })
    );
    assert!(matches!(
        h.run(Op::ResourceRename {
            resource: r,
            expected: rev,
            name: Some(String::new()),
        }),
        Err(Refusal::InvalidArgument { field: "name", .. })
    ));
    h.run(Op::ResourceRename {
        resource: r,
        expected: rev,
        name: Some("build".to_owned()),
    })
    .expect("rename");
    assert_eq!(h.resource(r).name.as_deref(), Some("build"));
}

#[test]
fn activation_lifecycle_terminate_and_exit_policy() {
    let mut h = Harness::new();
    let s = h.create_session("work", SessionLifetime::Persistent);
    let sh = h.shell_in(s);
    let jb = h.create_resource(s, job());

    let proc = ProcIdentity {
        pid: 10,
        pgid: 10,
        start_raw: 99,
        boot_id: [3; 16],
    };
    assert_eq!(
        h.run(Op::ActivationStarted {
            resource: sh,
            activation: A1,
            proc: Some(proc),
        })
        .err(),
        Some(Refusal::Unauthorized { verb: Verb::Create }),
        "only the server reports execution"
    );
    h.report(Op::ActivationStarted {
        resource: sh,
        activation: A1,
        proc: Some(proc),
    })
    .expect("started");
    assert_eq!(
        h.report(Op::ActivationStarted {
            resource: sh,
            activation: A1,
            proc: None,
        })
        .err(),
        Some(Refusal::Stale { current: Some(A1) })
    );

    // Terminate: stale activation first, then accepted with an effect.
    assert_eq!(
        h.run(Op::ResourceTerminate {
            resource: sh,
            activation: A1.next(),
            mode: TerminateMode::Graceful,
        })
        .err(),
        Some(Refusal::Stale { current: Some(A1) })
    );
    let applied = h
        .run(Op::ResourceTerminate {
            resource: sh,
            activation: A1,
            mode: TerminateMode::Force,
        })
        .expect("terminate");
    assert!(matches!(applied.reply, Reply::Accepted { .. }));
    assert_eq!(
        applied.effects,
        vec![Effect::Terminate {
            resource: sh,
            activation: A1,
            mode: TerminateMode::Force,
            proc: Some(proc),
        }]
    );
    assert_eq!(
        h.resource(sh).activation.map(|a| a.state),
        Some(ExecState::Stopping)
    );

    // An observed exit removes a Remove shell and retains a Retain job.
    h.report(ended(sh, Outcome::Exit(ExitOutcome::signaled(9))))
        .expect("ended");
    assert!(h.reg.org().resource(&sh).is_none());
    h.report(ended(jb, Outcome::Exit(ExitOutcome::exited(3))))
        .expect("ended");
    let kept = h.resource(jb).activation.expect("activation");
    assert_eq!(kept.state, ExecState::Completed);
    assert_eq!(kept.outcome, Some(Outcome::Exit(ExitOutcome::exited(3))));
    assert_eq!(
        h.run(Op::ResourceTerminate {
            resource: jb,
            activation: A1,
            mode: TerminateMode::Graceful,
        })
        .err(),
        Some(Refusal::Stale { current: Some(A1) }),
        "a completed activation cannot be terminated"
    );
    assert!(matches!(
        h.report(Op::ActivationEnded {
            resource: jb,
            activation: A1,
            state: ExecState::Running,
            outcome: exited(),
        }),
        Err(Refusal::InvalidArgument { field: "state", .. })
    ));
}

#[test]
fn an_unobserved_end_keeps_even_a_remove_shell() {
    let mut h = Harness::new();
    let sh = h.shell_in(h.inbox());
    h.report(ended(sh, Outcome::Unobserved)).expect("ended");
    assert_eq!(
        h.resource(sh).activation.and_then(|a| a.outcome),
        Some(Outcome::Unobserved)
    );
}

#[test]
fn remove_and_relaunch_refuse_current_and_lost_execution() {
    let mut h = Harness::new();
    let r = h.create_resource(h.inbox(), job());
    let rev = h.resource(r).revision;
    for op in [
        Op::ResourceRemove {
            resource: r,
            expected: rev,
        },
        Op::ResourceRelaunch {
            resource: r,
            expected: rev,
        },
    ] {
        assert!(matches!(
            h.run(op),
            Err(Refusal::InvalidArgument {
                field: "resource",
                ..
            })
        ));
    }

    h.report(Op::ActivationLost {
        resource: r,
        activation: A1,
    })
    .expect("lost");
    let rev = h.resource(r).revision;
    assert_eq!(
        h.run(Op::ResourceRemove {
            resource: r,
            expected: rev,
        })
        .err(),
        Some(Refusal::ExecutionUncertain)
    );
    assert_eq!(
        h.run(Op::ResourceRelaunch {
            resource: r,
            expected: rev,
        })
        .err(),
        Some(Refusal::ExecutionUncertain)
    );

    // Lost can still be terminated by its recorded identity.
    let applied = h
        .run(Op::ResourceTerminate {
            resource: r,
            activation: A1,
            mode: TerminateMode::Force,
        })
        .expect("terminate orphan");
    assert_eq!(applied.effects.len(), 1);
    assert_eq!(
        h.resource(r).activation.map(|a| a.state),
        Some(ExecState::Lost)
    );

    // The orphan's end, once observed, settles it: relaunch is allowed.
    h.report(ended(r, Outcome::Exit(ExitOutcome::signaled(9))))
        .expect("orphan ended");
    let rev = h.resource(r).revision;
    let applied = h
        .run(Op::ResourceRelaunch {
            resource: r,
            expected: rev,
        })
        .expect("relaunch");
    assert!(matches!(
        applied.effects.as_slice(),
        [Effect::Launch { activation, .. }] if *activation == A1.next()
    ));
    assert_eq!(h.resource(r).activation.map(|a| a.id), Some(A1.next()));
}

// ---- membership ------------------------------------------------------------

#[test]
fn resource_move_refusals_and_ephemeral_source_deletion() {
    let mut h = Harness::new();
    let quick = h.create_session("quick", SessionLifetime::Ephemeral);
    let dest = h.create_session("dest", SessionLifetime::Persistent);
    let r = h.session(quick).members[0];
    let (rq, rd) = (h.session(quick).revision, h.session(dest).revision);

    assert_eq!(
        h.run(Op::ResourceMove {
            resource: r,
            from: dest,
            from_expected: rd,
            to: quick,
            to_expected: rq,
        })
        .err(),
        Some(Refusal::Conflict {
            current: h.resource(r).revision
        }),
        "the caller's idea of the source is stale"
    );
    assert_eq!(
        h.run(Op::ResourceMove {
            resource: r,
            from: quick,
            from_expected: rq,
            to: dest,
            to_expected: rd.next(),
        })
        .err(),
        Some(Refusal::Conflict { current: rd })
    );
    assert_eq!(
        h.run(Op::ResourceMove {
            resource: r,
            from: quick,
            from_expected: rq,
            to: SessionKey::from_bytes([7; 16]),
            to_expected: rd,
        })
        .err(),
        Some(Refusal::NotFound)
    );
    let applied = h
        .run(Op::ResourceMove {
            resource: r,
            from: quick,
            from_expected: rq,
            to: dest,
            to_expected: rd,
        })
        .expect("move");
    assert!(applied.effects.is_empty(), "a move never touches execution");
    assert!(h.reg.org().session(&quick).is_none(), "emptied ephemeral");
    assert_eq!(h.session(dest).members, vec![r]);
    assert!(
        applied
            .transition
            .events
            .iter()
            .any(|e| matches!(e, Event::SessionDeleted { key, .. } if *key == quick))
    );
}

#[test]
fn moving_out_prunes_the_source_arrangement() {
    let mut h = Harness::new();
    let a = h.create_session("a", SessionLifetime::Persistent);
    let b = h.create_session("b", SessionLifetime::Persistent);
    let r1 = h.shell_in(a);
    let r2 = h.shell_in(a);
    let tree = LayoutNode::Split {
        dir: SplitDir::Horizontal,
        ratio: 0.5,
        left: Box::new(LayoutNode::Leaf(EntryRef::member([1; 16], r1))),
        right: Box::new(LayoutNode::Leaf(EntryRef::member([2; 16], r2))),
    };
    h.run(Op::ArrangementReplace {
        owner: ArrangementOwner::Session(a),
        expected: Revision::FIRST,
        groups: vec![Group {
            id: [5; 16],
            name: None,
            tree,
        }],
    })
    .expect("arrangement");
    let ak = h.session(a).arrangement.expect("linked");
    let (ra, rb) = (h.session(a).revision, h.session(b).revision);
    h.run(Op::ResourceMove {
        resource: r1,
        from: a,
        from_expected: ra,
        to: b,
        to_expected: rb,
    })
    .expect("move");
    let arrangement = h.reg.org().arrangement(&ak).expect("kept");
    assert_eq!(arrangement.resources(), vec![r2]);
    assert_eq!(arrangement.revision, Revision(3));
}

#[test]
fn undoing_a_move_restores_the_source_placement() {
    let mut h = Harness::new();
    let a = h.create_session("a", SessionLifetime::Persistent);
    let b = h.create_session("b", SessionLifetime::Persistent);
    let r1 = h.shell_in(a);
    let r2 = h.shell_in(a);
    let groups = vec![Group {
        id: [5; 16],
        name: None,
        tree: LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.3,
            left: Box::new(LayoutNode::Leaf(EntryRef::member([1; 16], r1))),
            right: Box::new(LayoutNode::Leaf(EntryRef::member([2; 16], r2))),
        },
    }];
    h.run(Op::ArrangementReplace {
        owner: ArrangementOwner::Session(a),
        expected: Revision::FIRST,
        groups: groups.clone(),
    })
    .expect("arrangement");
    let ak = h.session(a).arrangement.expect("linked");
    let (ra, rb) = (h.session(a).revision, h.session(b).revision);
    let moved = h
        .run(Op::ResourceMove {
            resource: r1,
            from: a,
            from_expected: ra,
            to: b,
            to_expected: rb,
        })
        .expect("move");
    h.run(Op::OrganizationUndo {
        original: moved.operation,
        expected: vec![],
    })
    .expect("undo");
    assert_eq!(h.resource(r1).session, a);
    let arrangement = h.reg.org().arrangement(&ak).expect("kept");
    assert_eq!(arrangement.groups, groups, "the pane is back where it was");
}

#[test]
fn undoing_a_move_out_of_a_deleted_session_refuses() {
    let mut h = Harness::new();
    let quick = h.create_session("quick", SessionLifetime::Ephemeral);
    let dest = h.create_session("dest", SessionLifetime::Persistent);
    let r = h.session(quick).members[0];
    let (rq, rd) = (h.session(quick).revision, h.session(dest).revision);
    let moved = h
        .run(Op::ResourceMove {
            resource: r,
            from: quick,
            from_expected: rq,
            to: dest,
            to_expected: rd,
        })
        .expect("move");
    assert!(
        h.reg.org().session(&quick).is_none(),
        "the empty ephemeral went"
    );
    assert_eq!(
        h.run(Op::OrganizationUndo {
            original: moved.operation,
            expected: vec![],
        })
        .err(),
        Some(Refusal::InvalidArgument {
            field: "undo",
            reason: "the original session no longer exists".to_owned(),
        })
    );
    assert_eq!(h.resource(r).session, dest, "nothing is resurrected");
}

#[test]
fn move_many_is_atomic_and_bounded() {
    let mut h = Harness::new();
    let a = h.create_session("a", SessionLifetime::Persistent);
    let b = h.create_session("b", SessionLifetime::Persistent);
    let dest = h.create_session("dest", SessionLifetime::Persistent);
    let ra = h.shell_in(a);
    let rb = h.shell_in(b);
    let pins = |h: &Harness| {
        vec![
            SourcePin {
                session: a,
                expected: h.session(a).revision,
            },
            SourcePin {
                session: b,
                expected: h.session(b).revision,
            },
        ]
    };
    let rd = h.session(dest).revision;

    assert!(matches!(
        h.run(Op::ResourceMoveMany {
            resources: vec![],
            sources: pins(&h),
            to: dest,
            to_expected: rd,
        }),
        Err(Refusal::InvalidArgument { .. })
    ));
    assert!(matches!(
        h.run(Op::ResourceMoveMany {
            resources: vec![ra, ra],
            sources: pins(&h),
            to: dest,
            to_expected: rd,
        }),
        Err(Refusal::InvalidArgument { .. })
    ));
    assert!(matches!(
        h.run(Op::ResourceMoveMany {
            resources: vec![ResourceKey::from_bytes([0; 16]); 300],
            sources: pins(&h),
            to: dest,
            to_expected: rd,
        }),
        Err(Refusal::InvalidArgument { .. })
    ));
    assert!(matches!(
        h.run(Op::ResourceMoveMany {
            resources: vec![ra, rb],
            sources: pins(&h)[..1].to_vec(),
            to: dest,
            to_expected: rd,
        }),
        Err(Refusal::InvalidArgument {
            field: "sources",
            ..
        })
    ));
    let mut stale = pins(&h);
    stale[1].expected = stale[1].expected.next();
    assert!(matches!(
        h.run(Op::ResourceMoveMany {
            resources: vec![ra, rb],
            sources: stale,
            to: dest,
            to_expected: rd,
        }),
        Err(Refusal::Conflict { .. })
    ));
    assert_eq!(
        h.run(Op::ResourceMoveMany {
            resources: vec![ra, ResourceKey::from_bytes([0; 16])],
            sources: pins(&h),
            to: dest,
            to_expected: rd,
        })
        .err(),
        Some(Refusal::NotFound),
        "one missing target refuses the whole batch"
    );

    h.run(Op::ResourceMoveMany {
        resources: vec![ra, rb],
        sources: pins(&h),
        to: dest,
        to_expected: rd,
    })
    .expect("move many");
    assert_eq!(h.session(dest).members, vec![ra, rb]);
    assert_eq!(h.session(dest).revision, rd.next(), "one bump per record");
}

// ---- intake and undo -------------------------------------------------------

#[test]
fn undo_restores_membership_only_while_revisions_match() {
    let mut h = Harness::new();
    let a = h.create_session("a", SessionLifetime::Persistent);
    let b = h.create_session("b", SessionLifetime::Persistent);
    let r = h.shell_in(a);
    let (ra, rb) = (h.session(a).revision, h.session(b).revision);
    let moved = h
        .run(Op::ResourceMove {
            resource: r,
            from: a,
            from_expected: ra,
            to: b,
            to_expected: rb,
        })
        .expect("move");
    let receipt = moved.transition.receipt.clone().expect("receipt");
    assert!(receipt.undo.is_some());

    let undone = h
        .run(Op::OrganizationUndo {
            original: moved.operation,
            expected: vec![],
        })
        .expect("undo");
    assert_eq!(h.resource(r).session, a);
    assert!(undone.effects.is_empty(), "undo never touches execution");

    // The state moved on, so undoing the move again conflicts.
    assert!(matches!(
        h.run(Op::OrganizationUndo {
            original: moved.operation,
            expected: vec![],
        }),
        Err(Refusal::Conflict { .. })
    ));
    // Undoing the undo redoes the move.
    h.run(Op::OrganizationUndo {
        original: undone.operation,
        expected: vec![],
    })
    .expect("redo");
    assert_eq!(h.resource(r).session, b);

    assert_eq!(
        h.run(Op::OrganizationUndo {
            original: phux_core::OperationId::from_bytes([0; 16]),
            expected: vec![],
        })
        .err(),
        Some(Refusal::NotFound)
    );
    let rename = h
        .run(Op::SessionRename {
            session: a,
            expected: h.session(a).revision,
            name: "renamed".to_owned(),
        })
        .expect("rename");
    assert!(matches!(
        h.run(Op::OrganizationUndo {
            original: rename.operation,
            expected: vec![],
        }),
        Err(Refusal::Unsupported { .. })
    ));
}

#[test]
fn inbox_resolve_clears_intake_and_undo_restores_it() {
    let mut h = Harness::new();
    let s = h.create_session("imported", SessionLifetime::Persistent);
    // Mark it imported the way migration does: a committed record write.
    let mut record = h.session(s);
    record.awaiting_organization = true;
    record.revision = record.revision.next();
    h.reg.commit(&phux_core::Transition {
        deltas: vec![phux_core::Delta::PutSession(record)],
        events: vec![],
        receipt: None,
    });
    let rev = h.session(s).revision;
    assert_eq!(
        h.run(Op::InboxResolve {
            session: s,
            expected: rev.next(),
        })
        .err(),
        Some(Refusal::Conflict { current: rev })
    );
    let resolved = h
        .run(Op::InboxResolve {
            session: s,
            expected: rev,
        })
        .expect("resolve");
    assert!(!h.session(s).awaiting_organization);
    assert!(resolved.transition.events.iter().any(|e| matches!(
        e,
        Event::SessionIntakeChanged {
            awaiting_organization: false,
            ..
        }
    )));
    h.run(Op::OrganizationUndo {
        original: resolved.operation,
        expected: vec![],
    })
    .expect("undo");
    assert!(h.session(s).awaiting_organization);
}

// ---- arrangements ----------------------------------------------------------

#[test]
fn arrangement_replace_validates_members_and_revisions() {
    let mut h = Harness::new();
    // The Inbox of a bare organization has no arrangement yet, so the first
    // replace creates it.
    let a = h.inbox();
    let b = h.create_session("b", SessionLifetime::Persistent);
    let mine = h.shell_in(a);
    let theirs = h.shell_in(b);
    let group = |resource, entry| Group {
        id: [entry; 16],
        name: Some("main".to_owned()),
        tree: LayoutNode::Leaf(EntryRef::member([entry; 16], resource)),
    };
    let owner = ArrangementOwner::Session(a);

    assert!(matches!(
        h.run(Op::ArrangementReplace {
            owner,
            expected: Revision::ZERO,
            groups: vec![group(theirs, 1)],
        }),
        Err(Refusal::InvalidArgument {
            field: "groups",
            ..
        })
    ));
    assert!(matches!(
        h.run(Op::ArrangementReplace {
            owner,
            expected: Revision::ZERO,
            groups: vec![group(mine, 1), group(mine, 1)],
        }),
        Err(Refusal::InvalidArgument { .. })
    ));
    assert_eq!(
        h.run(Op::ArrangementReplace {
            owner,
            expected: Revision::FIRST,
            groups: vec![group(mine, 1)],
        })
        .err(),
        Some(Refusal::Conflict {
            current: Revision::ZERO
        })
    );
    let applied = h
        .run(Op::ArrangementReplace {
            owner,
            expected: Revision::ZERO,
            groups: vec![group(mine, 1)],
        })
        .expect("create");
    let Reply::Ok { created, revision } = applied.reply else {
        panic!("expected Ok");
    };
    assert_eq!(revision, Some(Revision::FIRST));
    assert_eq!(h.session(a).arrangement, created.arrangement);
    h.run(Op::ArrangementReplace {
        owner,
        expected: Revision::FIRST,
        groups: vec![],
    })
    .expect("replace");
    assert!(matches!(
        h.run(Op::ArrangementReplace {
            owner: ArrangementOwner::Workspace(phux_core::WorkspaceKey::from_bytes([1; 16])),
            expected: Revision::ZERO,
            groups: vec![],
        }),
        Err(Refusal::Unsupported { .. })
    ));
}

// ---- retries ---------------------------------------------------------------

#[test]
fn retries_replay_the_receipt_and_reject_a_changed_payload() {
    let mut h = Harness::new();
    let operation = h.next_op();
    let cmd = Command {
        operation,
        op: create("work", SessionLifetime::Persistent, Some(shell())),
    };
    let owner = Actor::owner();
    let first = h.run_cmd(&owner, &cmd).expect("first");
    let again = h.run_cmd(&owner, &cmd).expect("retry");
    assert!(again.replayed);
    assert_eq!(again.reply, first.reply);
    assert!(again.effects.is_empty() && again.transition.is_empty());
    assert_eq!(h.reg.org().sessions().count(), 2, "no second session");

    let changed = Command {
        operation,
        op: create("other", SessionLifetime::Persistent, None),
    };
    assert_eq!(
        h.run_cmd(&owner, &changed).err(),
        Some(Refusal::DuplicateOperation)
    );
}
