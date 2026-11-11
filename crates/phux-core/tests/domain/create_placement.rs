//! Creation is one organization transition, not spawn + layout + compensating kill.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use crate::support::{Harness, INSTANCE, created, shell};
use phux_core::{
    Actor, Command, CreatePlacement, Effect, ExecState, InstanceId, LaunchMode, LaunchSpec,
    NewResource, Op, Refusal, Reply, SessionLifetime, SplitDir, TerminalLaunch,
};

fn first(h: &mut Harness) -> (phux_core::SessionKey, phux_core::ResourceKey, [u8; 16]) {
    let session = h.create_session("work", SessionLifetime::Persistent);
    let result = h
        .run(Op::ResourceCreate {
            session: Some(session),
            resource: shell(),
            placement: Some(CreatePlacement::NewGroup),
        })
        .unwrap();
    let Reply::Ok { created, .. } = result.reply else {
        panic!("committed")
    };
    (session, created.resource.unwrap(), created.entry.unwrap())
}
const fn beside(resource: phux_core::ResourceKey, entry: [u8; 16]) -> CreatePlacement {
    CreatePlacement::Beside {
        entry,
        resource,
        instance: INSTANCE,
        activation: phux_core::ActivationId::FIRST,
        dir: SplitDir::Horizontal,
        ratio: 0.5,
    }
}
#[test]
fn placed_remove_does_not_record_a_durable_result() {
    let mut h = Harness::new();
    let session = h.create_session("work", SessionLifetime::Persistent);
    let applied = h
        .run(Op::ResourceCreate {
            session: Some(session),
            resource: shell(),
            placement: Some(CreatePlacement::NewGroup),
        })
        .unwrap();
    let key = created(&applied).1.unwrap();
    assert!(
        !h.resource(key).durable_result,
        "a placed Remove resource is not a durable result"
    );
}

#[test]
fn placed_retain_records_a_durable_result() {
    let mut h = Harness::new();
    let session = h.create_session("work", SessionLifetime::Persistent);
    let applied = h
        .run(Op::ResourceCreate {
            session: Some(session),
            resource: crate::support::job(),
            placement: Some(CreatePlacement::NewGroup),
        })
        .unwrap();
    let key = created(&applied).1.unwrap();
    assert!(h.resource(key).durable_result);
    let plain = h.create_resource(session, crate::support::job());
    assert!(!h.resource(plain).durable_result);
}

#[test]
fn beside_refuses_an_entry_showing_a_different_resource() {
    let mut h = Harness::new();
    let (session, anchor, entry) = first(&mut h);
    let other = h
        .run(Op::ResourceCreate {
            session: Some(session),
            resource: shell(),
            placement: Some(CreatePlacement::NewGroup),
        })
        .unwrap();
    let other = created(&other).1.unwrap();
    let refused = h.run(Op::ResourceCreate {
        session: Some(session),
        resource: shell(),
        placement: Some(beside(other, entry)),
    });
    assert!(
        matches!(
            refused,
            Err(Refusal::InvalidArgument { field: "placement", ref reason })
                if reason.contains("observed resource")
        ),
        "{refused:?}"
    );
    assert_eq!(h.resource(anchor).session, session);
}

#[test]
fn beside_refuses_an_anchor_with_no_activation_as_stale() {
    let mut h = Harness::new();
    let session = h.create_session("work", SessionLifetime::Persistent);
    let applied = h
        .run(Op::ResourceCreate {
            session: Some(session),
            resource: NewResource {
                launch_mode: LaunchMode::Deferred,
                ..shell()
            },
            placement: Some(CreatePlacement::NewGroup),
        })
        .unwrap();
    let Reply::Ok { created, .. } = applied.reply else {
        panic!("committed")
    };
    let resource = created.resource.unwrap();
    let entry = created.entry.unwrap();
    assert!(h.resource(resource).activation.is_none());
    let refused = h.run(Op::ResourceCreate {
        session: Some(session),
        resource: shell(),
        placement: Some(beside(resource, entry)),
    });
    assert!(
        matches!(refused, Err(Refusal::Stale { current: None })),
        "{refused:?}"
    );
}

#[test]
fn beside_refuses_a_stale_activation_id() {
    let mut h = Harness::new();
    let (session, anchor, entry) = first(&mut h);
    let mut placement = beside(anchor, entry);
    let CreatePlacement::Beside { activation, .. } = &mut placement else {
        unreachable!()
    };
    *activation = phux_core::ActivationId::FIRST.next();
    let refused = h.run(Op::ResourceCreate {
        session: Some(session),
        resource: shell(),
        placement: Some(placement),
    });
    assert!(
        matches!(
            refused,
            Err(Refusal::Stale { current: Some(id) }) if id == phux_core::ActivationId::FIRST
        ),
        "{refused:?}"
    );
}

#[test]
fn beside_refuses_an_anchor_from_another_session() {
    let mut h = Harness::new();
    let (_session, anchor, entry) = first(&mut h);
    let other = h.create_session("other", SessionLifetime::Persistent);
    let refused = h.run(Op::ResourceCreate {
        session: Some(other),
        resource: shell(),
        placement: Some(beside(anchor, entry)),
    });
    assert!(
        matches!(
            refused,
            Err(Refusal::InvalidArgument { field: "placement", ref reason })
                if reason.contains("destination session")
        ),
        "{refused:?}"
    );
}

#[test]
fn create_and_place_commit_together_and_replay_without_effects() {
    let mut h = Harness::new();
    let (session, anchor, entry) = first(&mut h);
    let arrangement = h.session(session).arrangement.unwrap();
    let before = h.reg.org().arrangement(&arrangement).unwrap().clone();
    let command = Command {
        operation: h.next_op(),
        op: Op::ResourceCreate {
            session: Some(session),
            resource: shell(),
            placement: Some(beside(anchor, entry)),
        },
    };
    let result = h.run_cmd(&Actor::owner(), &command).unwrap();
    assert!(matches!(result.effects.as_slice(), [Effect::Launch { .. }]));
    let resource = created(&result).1.unwrap();
    let Reply::Ok { created, .. } = result.reply else {
        panic!("committed")
    };
    assert_eq!(created.arrangement, Some(arrangement));
    assert!(created.entry.is_some());
    let after = h.reg.org().arrangement(&arrangement).unwrap();
    assert_eq!(after.revision, before.revision.next());
    assert_eq!(after.groups.len(), 1);
    assert_eq!(after.groups[0].tree.leaves().len(), 2);
    assert_eq!(after.groups[0].tree.leaves()[0].entry, entry);
    assert_eq!(after.groups[0].tree.leaves()[1].resource(), Some(resource));
    let snapshot = h.reg.org().clone();
    let replay = h.run_cmd(&Actor::owner(), &command).unwrap();
    assert!(replay.replayed);
    assert!(replay.effects.is_empty());
    assert_eq!(replay.reply, result.reply);
    assert_eq!(h.reg.org(), &snapshot);
    let mut changed = command;
    let Op::ResourceCreate { placement, .. } = &mut changed.op else {
        unreachable!()
    };
    *placement = Some(CreatePlacement::NewGroup);
    assert_eq!(
        h.run_cmd(&Actor::owner(), &changed),
        Err(Refusal::DuplicateOperation)
    );
}
#[test]
fn invalid_missing_moved_and_replaced_anchors_refuse_without_launch_or_records() {
    let mut h = Harness::new();
    let (session, resource, entry) = first(&mut h);
    let mut bad_ratio = beside(resource, entry);
    let CreatePlacement::Beside { ratio, .. } = &mut bad_ratio else {
        unreachable!()
    };
    *ratio = f32::NAN;
    for placement in [
        bad_ratio,
        beside(resource, [99; 16]),
        CreatePlacement::Beside {
            entry,
            resource,
            instance: InstanceId::from_bytes([8; 16]),
            activation: phux_core::ActivationId::FIRST,
            dir: SplitDir::Vertical,
            ratio: 0.5,
        },
    ] {
        assert!(
            h.run(Op::ResourceCreate {
                session: Some(session),
                resource: shell(),
                placement: Some(placement)
            })
            .is_err()
        );
    }
    let other = h.create_session("other", SessionLifetime::Persistent);
    assert!(
        h.run(Op::ResourceCreate {
            session: Some(other),
            resource: shell(),
            placement: Some(beside(resource, entry))
        })
        .is_err()
    );
    h.report(Op::ActivationEnded {
        resource,
        activation: phux_core::ActivationId::FIRST,
        state: ExecState::Failed,
        diagnostic: None,
        outcome: phux_core::Outcome::Unobserved,
    })
    .unwrap();
    assert!(
        h.run(Op::ResourceCreate {
            session: Some(session),
            resource: shell(),
            placement: Some(beside(resource, entry))
        })
        .is_err()
    );
}
#[test]
fn receipt_env_values_are_hashed_and_retries_still_match() {
    use phux_core::{Actor, Command, LaunchSpec, TerminalLaunch};
    let mut h = Harness::new();
    let session = h.create_session("work", SessionLifetime::Persistent);
    let mut spec = shell();
    spec.launch = Some(LaunchSpec::Terminal(TerminalLaunch {
        env: vec![("SECRET".to_owned(), "super-secret-value".to_owned())],
        ..TerminalLaunch::default()
    }));
    let op = Op::ResourceCreate {
        session: Some(session),
        resource: spec,
        placement: None,
    };
    let operation = h.next_op();
    let applied = h
        .run_cmd(
            &Actor::owner(),
            &Command {
                operation,
                op: op.clone(),
            },
        )
        .unwrap();
    assert!(!applied.replayed);
    let receipt = h.reg.org().receipt(&operation).unwrap();
    let stored = serde_json::to_string(receipt).unwrap();
    assert!(!stored.contains("super-secret-value"));
    assert!(stored.contains("sha256:"));
    let key = created(&applied).1.unwrap();
    let LaunchSpec::Terminal(launch) = h.resource(key).launch.unwrap() else {
        panic!("terminal launch")
    };
    assert_eq!(launch.env[0].1, "super-secret-value");
    let replay = h
        .run_cmd(&Actor::owner(), &Command { operation, op })
        .unwrap();
    assert!(replay.replayed);
}

fn secret_shell(pairs: &[(&str, &str)]) -> NewResource {
    let mut spec = shell();
    spec.launch = Some(LaunchSpec::Terminal(TerminalLaunch {
        env: pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect(),
        ..TerminalLaunch::default()
    }));
    spec
}

/// Unsalted SHA-256 receipt form of two distinct environment values.
fn redacted_pairs() -> Vec<(String, String)> {
    vec![
        (
            "SECRET".to_owned(),
            "sha256:9fad4dc34768a5437a32a231b132409c96d8cf8aac141740253c5d565b958646".to_owned(),
        ),
        (
            "TOKEN".to_owned(),
            "sha256:459b126333d980ad9ecb764d78bd0001e4690a9549665be2cecde15c7f6d9602".to_owned(),
        ),
    ]
}

fn assert_receipt_hides_env(
    h: &Harness,
    operation: phux_core::OperationId,
    launch: Option<&LaunchSpec>,
) {
    let receipt = h.reg.org().receipt(&operation).expect("receipt");
    let stored = serde_json::to_string(receipt).expect("receipt json");
    assert!(!stored.contains("alpha-secret-value"), "{stored}");
    assert!(!stored.contains("beta-secret-value"), "{stored}");
    let LaunchSpec::Terminal(launch) = launch.expect("redacted launch") else {
        panic!("terminal launch")
    };
    assert_eq!(launch.env, redacted_pairs());
}

#[test]
fn session_create_receipt_hashes_every_env_value() {
    let mut h = Harness::new();
    let spec = secret_shell(&[
        ("SECRET", "alpha-secret-value"),
        ("TOKEN", "beta-secret-value"),
    ]);
    let operation = h.next_op();
    let op = Op::SessionCreate {
        name: "secret-session".to_owned(),
        lifetime: SessionLifetime::Persistent,
        attrs: phux_core::Attrs::default(),
        resource: Some(spec),
    };
    let applied = h
        .run_cmd(&Actor::owner(), &Command { operation, op })
        .unwrap();
    let key = created(&applied).1.unwrap();
    let LaunchSpec::Terminal(live) = h.resource(key).launch.unwrap() else {
        panic!("terminal launch")
    };
    assert_eq!(live.env[0].1, "alpha-secret-value");
    assert_eq!(live.env[1].1, "beta-secret-value");
    let receipt = h.reg.org().receipt(&operation).unwrap();
    let Op::SessionCreate { resource, .. } = &receipt.op else {
        panic!("session create receipt")
    };
    assert_receipt_hides_env(
        &h,
        operation,
        resource.as_ref().and_then(|spec| spec.launch.as_ref()),
    );
}

#[test]
fn resource_relaunch_receipt_hashes_every_env_value() {
    let mut h = Harness::new();
    let session = h.create_session("work", SessionLifetime::Persistent);
    let created_key = h.create_resource(
        session,
        NewResource {
            launch_mode: LaunchMode::Deferred,
            ..shell()
        },
    );
    let expected = h.resource(created_key).revision;
    let operation = h.next_op();
    let op = Op::ResourceRelaunch {
        resource: created_key,
        expected,
        launch: Some(LaunchSpec::Terminal(TerminalLaunch {
            env: vec![
                ("SECRET".to_owned(), "alpha-secret-value".to_owned()),
                ("TOKEN".to_owned(), "beta-secret-value".to_owned()),
            ],
            ..TerminalLaunch::default()
        })),
    };
    h.run_cmd(&Actor::owner(), &Command { operation, op })
        .unwrap();
    let LaunchSpec::Terminal(live) = h.resource(created_key).launch.unwrap() else {
        panic!("terminal launch")
    };
    assert_eq!(live.env[0].1, "alpha-secret-value");
    assert_eq!(live.env[1].1, "beta-secret-value");
    let receipt = h.reg.org().receipt(&operation).unwrap();
    let Op::ResourceRelaunch { launch, .. } = &receipt.op else {
        panic!("relaunch receipt")
    };
    assert_receipt_hides_env(&h, operation, launch.as_ref());
}

#[test]
fn same_operation_with_a_different_env_value_is_a_duplicate() {
    let mut h = Harness::new();
    let session = h.create_session("work", SessionLifetime::Persistent);
    let operation = h.next_op();
    let op = Op::ResourceCreate {
        session: Some(session),
        resource: secret_shell(&[("SECRET", "value-one"), ("OTHER", "value-two")]),
        placement: None,
    };
    let applied = h
        .run_cmd(
            &Actor::owner(),
            &Command {
                operation,
                op: op.clone(),
            },
        )
        .unwrap();
    assert!(!applied.replayed);
    let replay = h
        .run_cmd(&Actor::owner(), &Command { operation, op })
        .unwrap();
    assert!(replay.replayed);
    let changed = Op::ResourceCreate {
        session: Some(session),
        resource: secret_shell(&[("SECRET", "value-one"), ("OTHER", "value-other")]),
        placement: None,
    };
    assert_eq!(
        h.run_cmd(
            &Actor::owner(),
            &Command {
                operation,
                op: changed
            }
        ),
        Err(Refusal::DuplicateOperation)
    );
}

#[test]
fn unrelated_arrangement_edits_do_not_conflict_with_stable_entry_intent() {
    let mut h = Harness::new();
    let (session, anchor, entry) = first(&mut h);
    let intended = beside(anchor, entry);
    h.run(Op::ResourceCreate {
        session: Some(session),
        resource: shell(),
        placement: Some(CreatePlacement::NewGroup),
    })
    .unwrap();
    h.run(Op::ResourceCreate {
        session: Some(session),
        resource: shell(),
        placement: Some(intended),
    })
    .unwrap();
    let arrangement = h.session(session).arrangement.unwrap();
    let groups = &h.reg.org().arrangement(&arrangement).unwrap().groups;
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].tree.leaves().len(), 2);
}
