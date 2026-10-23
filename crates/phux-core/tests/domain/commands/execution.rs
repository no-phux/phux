//! Execution: extracted from the shared module.
use super::*;

// ---- resources -------------------------------------------------------------

#[test]
pub(super) fn resource_create_defaults_to_inbox_and_validates_kind() {
    let mut h = Harness::new();
    let applied = h
        .run(Op::ResourceCreate {
            placement: None,
            session: None,
            resource: shell(),
        })
        .expect("create");
    let r = created(&applied).1.expect("resource");
    assert_eq!(h.resource(r).session, h.inbox());

    let missing = SessionKey::from_bytes([7; 16]);
    assert_eq!(
        h.run(Op::ResourceCreate {
            placement: None,
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
            placement: None,
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
            placement: None,
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
pub(super) fn a_deferred_create_is_dormant_until_relaunched() {
    let mut h = Harness::new();
    let s = h.create_session("work", SessionLifetime::Persistent);
    let applied = h
        .run(Op::ResourceCreate {
            placement: None,
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
            launch: None,
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
            placement: None,
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
pub(super) fn resource_rename_checks_name_and_last_writer_wins() {
    let mut h = Harness::new();
    let r = h.shell_in(h.inbox());
    h.run(Op::ResourceRename {
        resource: r,
        name: Some("x".to_owned()),
    })
    .expect("first rename");
    assert!(matches!(
        h.run(Op::ResourceRename {
            resource: r,
            name: Some(String::new()),
        }),
        Err(Refusal::InvalidArgument { field: "name", .. })
    ));
    h.run(Op::ResourceRename {
        resource: r,
        name: Some("build".to_owned()),
    })
    .expect("rename");
    assert_eq!(h.resource(r).name.as_deref(), Some("build"));
}

#[test]
pub(super) fn activation_lifecycle_terminate_and_exit_policy() {
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
            diagnostic: None,
            outcome: exited(),
        }),
        Err(Refusal::InvalidArgument { field: "state", .. })
    ));
}

#[test]
pub(super) fn an_unobserved_end_keeps_even_a_remove_shell() {
    let mut h = Harness::new();
    let sh = h.shell_in(h.inbox());
    h.report(ended(sh, Outcome::Unobserved)).expect("ended");
    assert_eq!(
        h.resource(sh).activation.and_then(|a| a.outcome),
        Some(Outcome::Unobserved)
    );
}

#[test]
pub(super) fn remove_and_relaunch_refuse_current_and_lost_execution() {
    let mut h = Harness::new();
    let r = h.create_resource(h.inbox(), job());
    let rev = h.resource(r).revision;
    for op in [
        Op::ResourceRemove { resource: r },
        Op::ResourceRelaunch {
            resource: r,
            expected: rev,
            launch: None,
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
        h.run(Op::ResourceRemove { resource: r }).err(),
        Some(Refusal::ExecutionUncertain)
    );
    assert_eq!(
        h.run(Op::ResourceRelaunch {
            resource: r,
            expected: rev,
            launch: None,
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
            launch: None,
        })
        .expect("relaunch");
    assert!(matches!(
        applied.effects.as_slice(),
        [Effect::Launch { activation, .. }] if *activation == A1.next()
    ));
    assert_eq!(h.resource(r).activation.map(|a| a.id), Some(A1.next()));
}

#[test]
pub(super) fn explicit_dormant_relaunch_replaces_only_launch_and_preserves_arrangement_and_lifetime()
 {
    let mut h = Harness::new();
    let session = h.create_session("native", SessionLifetime::Persistent);
    let resource = h.create_resource(
        session,
        NewResource {
            launch_mode: LaunchMode::Deferred,
            ..job()
        },
    );
    let before = h.resource(resource);
    let arrangement = h
        .reg
        .org()
        .arrangement(&h.session(session).arrangement.unwrap())
        .unwrap()
        .clone();
    let launch = phux_core::LaunchSpec::Terminal(phux_core::TerminalLaunch {
        argv: vec!["/private/dev/phux".into(), "agent".into(), "host".into()],
        ..Default::default()
    });
    let applied = h
        .run(Op::ResourceRelaunch {
            resource,
            expected: before.revision,
            launch: Some(launch.clone()),
        })
        .unwrap();
    let after = h.resource(resource);
    assert_eq!(after.key, before.key);
    assert_eq!(after.session, before.session);
    assert_eq!(after.name, before.name);
    assert_eq!(after.on_exit, before.on_exit);
    assert_eq!(after.launch, Some(launch.clone()));
    assert_eq!(
        h.reg.org().arrangement(&arrangement.key),
        Some(&arrangement)
    );
    assert!(matches!(applied.effects.as_slice(), [Effect::Launch { spec, .. }] if spec == &launch));
    assert!(matches!(
        h.run(Op::ResourceRelaunch {
            resource,
            expected: after.revision,
            launch: Some(launch)
        }),
        Err(Refusal::InvalidArgument {
            field: "resource",
            ..
        })
    ));
}

#[test]
pub(super) fn launch_reports_are_system_only_exact_activation_and_never_execute_or_change_lifetime()
{
    let mut h = Harness::new();
    let resource = h.create_resource(h.inbox(), job());
    let before = h.resource(resource);
    let launch = phux_core::LaunchSpec::Terminal(phux_core::TerminalLaunch {
        argv: vec!["phux".into(), "agent".into(), "host".into()],
        env: vec![(
            "PHUX_NATIVE_HOST_SPEC".into(),
            "{\"native_id\":\"actual-sdk-session\"}".into(),
        )],
        ..Default::default()
    });
    let report = Op::ResourceLaunchReported {
        resource,
        activation: A1,
        launch: launch.clone(),
    };
    assert_eq!(
        h.run(report.clone()).err(),
        Some(Refusal::Unauthorized { verb: Verb::Create })
    );
    let applied = h.report(report).unwrap();
    let after = h.resource(resource);
    assert_eq!(after.activation, before.activation);
    assert_eq!(after.session, before.session);
    assert_eq!(after.on_exit, before.on_exit);
    assert_eq!(after.launch, Some(launch));
    assert!(applied.effects.is_empty());
    assert!(applied.transition.events.is_empty());
    assert!(matches!(
        h.report(Op::ResourceLaunchReported {
            resource,
            activation: A1.next(),
            launch: before.launch.unwrap(),
        }),
        Err(Refusal::Stale { .. })
    ));
}
