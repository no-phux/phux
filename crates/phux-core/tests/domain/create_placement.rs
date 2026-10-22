//! Creation is one organization transition, not spawn + layout + compensating kill.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use crate::support::{Harness, INSTANCE, created, shell};
use phux_core::{
    Actor, Command, CreatePlacement, Effect, ExecState, InstanceId, Op, Refusal, Reply,
    SessionLifetime, SplitDir,
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
