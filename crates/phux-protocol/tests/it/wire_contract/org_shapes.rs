//! Additive wire shapes of the organization model (`docs/spec/L1.md` §5.7):
//! the organization commands, results, and events round-trip for arbitrary
//! values; the trailing `ActorRef` principals and the snapshot's session and
//! lifetime extension fields leave every older shape byte-identical; and an
//! organization event an older decoder cannot read stays opaque.

#![allow(
    clippy::unwrap_used,
    clippy::missing_const_for_fn,
    reason = "fixtures: unwrap a known-good key; helpers stay plain fns"
)]

use phux_protocol::ids::{
    ActivationId, ArrangementKey, ClientId, IdempotencyKey, PrincipalId, ResourceId, ResourceKey,
    ResourceKind, Revision, SatelliteHost, ServerInstance, SessionKey, WorkspaceKey,
};
use phux_protocol::wire::frame::{
    ActorRef, ArrangementDoc, ArrangementGroup, ArrangementNode, ArrangementOwner, Command,
    EntryRef, EntryTarget, EventStamp, ExecState, FrameKind, LaunchSpec, LiveTarget, Outcome,
    RecordRevision, SessionLifetime, SplitDirection,
};
use proptest::prelude::*;

use crate::common::assert_round_trip;

use commands::{delegation_command, org_command};
use events::org_event;
use results::org_result;

mod commands;
mod events;
mod older_shapes;
mod results;

// -----------------------------------------------------------------------------
// Strategies.
// -----------------------------------------------------------------------------

fn nonzero16() -> impl Strategy<Value = [u8; 16]> {
    any::<[u8; 16]>().prop_map(|mut bytes| {
        bytes[0] |= 1;
        bytes
    })
}

fn op() -> impl Strategy<Value = IdempotencyKey> {
    nonzero16().prop_map(|b| IdempotencyKey::new(b).unwrap())
}
fn session_key() -> impl Strategy<Value = SessionKey> {
    nonzero16().prop_map(|b| SessionKey::new(b).unwrap())
}
fn resource_key() -> impl Strategy<Value = ResourceKey> {
    nonzero16().prop_map(|b| ResourceKey::new(b).unwrap())
}
fn arrangement_key() -> impl Strategy<Value = ArrangementKey> {
    nonzero16().prop_map(|b| ArrangementKey::new(b).unwrap())
}
fn principal() -> impl Strategy<Value = PrincipalId> {
    nonzero16().prop_map(|b| PrincipalId::new(b).unwrap())
}
fn rev() -> impl Strategy<Value = Revision> {
    any::<u64>().prop_map(Revision::new)
}
fn name() -> impl Strategy<Value = String> {
    "[a-z0-9 ._-]{0,40}"
}
fn resource_id() -> impl Strategy<Value = ResourceId> {
    prop_oneof![
        any::<u32>().prop_map(ResourceId::local),
        ("[a-z]{1,8}", any::<u32>()).prop_map(|(h, id)| ResourceId::satellite(h, id)),
    ]
}
fn kind() -> impl Strategy<Value = ResourceKind> {
    any::<u8>().prop_map(ResourceKind::from_wire)
}
fn lifetime() -> impl Strategy<Value = SessionLifetime> {
    prop_oneof![
        Just(SessionLifetime::Persistent),
        Just(SessionLifetime::Ephemeral)
    ]
}
fn exec_state() -> impl Strategy<Value = ExecState> {
    (0_u8..=6).prop_map(|b| ExecState::from_u8(b).unwrap())
}
fn outcome() -> impl Strategy<Value = Outcome> {
    prop_oneof![
        any::<i32>().prop_map(Outcome::Exited),
        any::<i32>().prop_map(Outcome::Signaled),
        Just(Outcome::Unobserved),
    ]
}
fn pairs() -> impl Strategy<Value = Vec<(String, String)>> {
    proptest::collection::vec((name(), name()), 0..4)
}
fn launch() -> impl Strategy<Value = LaunchSpec> {
    (
        proptest::collection::vec(name(), 0..4),
        proptest::option::of(name()),
        pairs(),
        proptest::option::of(name()),
        proptest::option::of(proptest::collection::vec(any::<u8>(), 0..32)),
    )
        .prop_map(|(command, cwd, env, term, restore)| {
            LaunchSpec::new(command)
                .with_cwd(cwd)
                .with_env(env)
                .with_term(term)
                .with_restore(restore)
        })
}
fn live_target() -> impl Strategy<Value = LiveTarget> {
    (resource_id(), any::<[u8; 16]>(), any::<u64>()).prop_map(|(resource, instance, a)| {
        LiveTarget {
            resource,
            instance: ServerInstance::new(instance),
            activation: ActivationId::new(a),
        }
    })
}
fn pin() -> impl Strategy<Value = RecordRevision> {
    (nonzero16(), rev()).prop_map(|(key, revision)| RecordRevision { key, revision })
}
fn actor() -> impl Strategy<Value = ActorRef> {
    (
        any::<u32>(),
        proptest::option::of(name()),
        proptest::option::of(name()),
        proptest::option::of(principal()),
        proptest::option::of(principal()),
    )
        .prop_map(|(client, cred, client_name, p, obo)| {
            ActorRef::new(ClientId::new(client))
                .with_credential_id(cred)
                .with_client_name(client_name)
                .with_principal(p)
                .with_on_behalf_of(obo)
        })
}
fn entry_target() -> impl Strategy<Value = EntryTarget> {
    prop_oneof![
        resource_key().prop_map(EntryTarget::Member),
        ("[a-z]{1,8}", any::<u32>()).prop_map(|(host, id)| EntryTarget::Satellite {
            host: SatelliteHost::new(host),
            id,
        }),
    ]
}
fn node() -> impl Strategy<Value = ArrangementNode> {
    let leaf = (any::<[u8; 16]>(), entry_target())
        .prop_map(|(entry, target)| ArrangementNode::Leaf(EntryRef { entry, target }));
    leaf.prop_recursive(4, 16, 2, |inner| {
        (any::<bool>(), 1_u16..=999, inner.clone(), inner).prop_map(|(h, r, a, b)| {
            ArrangementNode::Split {
                dir: if h {
                    SplitDirection::Horizontal
                } else {
                    SplitDirection::Vertical
                },
                ratio: f32::from(r) / 1000.0,
                first: Box::new(a),
                second: Box::new(b),
            }
        })
    })
}
fn document() -> impl Strategy<Value = ArrangementDoc> {
    proptest::collection::vec(
        (any::<[u8; 16]>(), proptest::option::of(name()), node())
            .prop_map(|(id, name, tree)| ArrangementGroup { id, name, tree }),
        0..3,
    )
    .prop_map(|groups| ArrangementDoc { groups })
}
fn owner() -> impl Strategy<Value = ArrangementOwner> {
    prop_oneof![
        session_key().prop_map(ArrangementOwner::Session),
        nonzero16().prop_map(|b| ArrangementOwner::Workspace(WorkspaceKey::new(b).unwrap())),
    ]
}

proptest! {
    #[test]
    fn organization_commands_round_trip(request_id in any::<u32>(), command in org_command()) {
        prop_assert!(command.is_organization());
        if !matches!(command, Command::GetOrganization { .. } | Command::ArrangementGet { .. }) {
            prop_assert!(command.idempotency_key().is_some());
        }
        assert_round_trip(&FrameKind::Command { request_id, command });
    }

    #[test]
    fn delegation_commands_round_trip(request_id in any::<u32>(), command in delegation_command()) {
        prop_assert!(!command.is_organization());
        assert_round_trip(&FrameKind::Command { request_id, command });
    }

    #[test]
    fn organization_results_round_trip(request_id in any::<u32>(), result in org_result()) {
        assert_round_trip(&FrameKind::CommandResult { request_id, result });
    }

    #[test]
    fn organization_events_round_trip(
        terminal in proptest::option::of(resource_id()),
        event in org_event(),
        actor in proptest::option::of(actor()),
    ) {
        let stamp = Some(Box::new(EventStamp::new(1, 2).with_actor(actor)));
        assert_round_trip(&FrameKind::Event { terminal, event, stamp });
    }
}
