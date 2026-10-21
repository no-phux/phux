//! Agent-event frames, stamps, and unknown-tag tolerance.

use proptest::prelude::*;

use super::{
    AgentEvent, ClientId, ControlAction, FrameKind, ResourceId, ResourceLifecycle,
    arb_resource_kind, arb_terminal_id, assert_round_trip, framed_tlv, tlv_field,
};

pub(super) fn arb_actor() -> impl Strategy<Value = phux_protocol::wire::frame::ActorRef> {
    (
        any::<u32>(),
        proptest::option::of(".{0,16}"),
        proptest::option::of(".{0,16}"),
    )
        .prop_map(|(client, credential_id, client_name)| {
            phux_protocol::wire::frame::ActorRef::new(ClientId::new(client))
                .with_credential_id(credential_id)
                .with_client_name(client_name)
        })
}

/// A journal stamp (ADR-0123) over the full `u64` range, with and without
/// an actor and an operation id; an all-zero key draw means no key.
pub(super) fn arb_event_stamp() -> impl Strategy<Value = phux_protocol::wire::frame::EventStamp> {
    (
        any::<u64>(),
        any::<u64>(),
        proptest::option::of(arb_actor()),
        proptest::option::of(any::<[u8; 16]>()),
    )
        .prop_map(|(seq, ts_ms, actor, key)| {
            phux_protocol::wire::frame::EventStamp::new(seq, ts_ms)
                .with_actor(actor)
                .with_operation_id(key.and_then(phux_protocol::ids::IdempotencyKey::new))
        })
}

pub(super) fn arb_agent_event() -> impl Strategy<Value = AgentEvent> {
    prop_oneof![
        (any::<u64>(), any::<u64>()).prop_map(|(first_missing, last_missing)| {
            AgentEvent::JournalGap {
                first_missing,
                last_missing,
            }
        }),
        any::<u64>().prop_map(|dropped| AgentEvent::SourceGap { dropped }),
        Just(AgentEvent::CommandStarted),
        proptest::option::of(any::<i32>())
            .prop_map(|exit_code| AgentEvent::CommandFinished { exit_code }),
        ".{0,128}".prop_map(|title| AgentEvent::TitleChanged { title }),
        ".{0,64}".prop_map(|origin| AgentEvent::InputActivity { origin }),
        Just(AgentEvent::Bell),
        (arb_resource_kind(), proptest::option::of(arb_terminal_id()))
            .prop_map(|(kind, parent)| AgentEvent::ResourceSpawned { kind, parent }),
        proptest::option::of(any::<i32>())
            .prop_map(|exit_status| AgentEvent::ResourceClosed { exit_status }),
        Just(AgentEvent::Dirty),
        Just(AgentEvent::Idle),
        // ADR-0033 TerminalControl: exercise the full lifecycle × action
        // space plus both `Option<ClientId>` slots.
        (
            0u8..3,
            proptest::option::of(any::<i32>()),
            proptest::option::of(any::<u32>()),
            0u8..10,
            proptest::option::of(any::<u32>()),
        )
            .prop_map(
                |(lc, exit_status, holder, ac, actor)| AgentEvent::TerminalControl {
                    lifecycle: ResourceLifecycle::from_u8(lc).unwrap(),
                    exit_status,
                    input_holder: holder.map(ClientId::new),
                    action: ControlAction::from_u8(ac).unwrap(),
                    actor: actor.map(ClientId::new),
                }
            ),
        (
            ".{0,64}",
            ".{0,256}",
            proptest::collection::vec(".{0,64}", 0..4),
            proptest::option::of(any::<u64>()),
        )
            .prop_map(
                |(id, question, suggestions, elapsed_seconds)| AgentEvent::Asked {
                    id,
                    question,
                    suggestions,
                    elapsed_seconds,
                }
            ),
    ]
}

/// Named EVENT fixtures the proptest taxonomy doesn't pin by name: `Asked`
/// with every field populated and with the minimal shape (empty suggestions,
/// no elapsed counter), `CwdChanged` (phux-foz.4, tag 0x0a), and `Unknown` —
/// a relay that decodes an unknown event and re-encodes it MUST produce
/// byte-identical output (lossless passthrough).
#[test]
fn event_fixture_variants_round_trip() {
    for (terminal, event) in [
        (
            None,
            AgentEvent::Asked {
                id: "q-7f3a".to_string(),
                question: "Which transport should the bridge use?".to_string(),
                suggestions: vec![
                    "WebSocket".to_string(),
                    "gRPC".to_string(),
                    "raw TCP".to_string(),
                ],
                elapsed_seconds: Some(42),
            },
        ),
        (
            None,
            AgentEvent::Asked {
                id: "q-0".to_string(),
                question: "Proceed?".to_string(),
                suggestions: Vec::new(),
                elapsed_seconds: None,
            },
        ),
        (
            Some(ResourceId::local(7)),
            AgentEvent::CwdChanged {
                cwd: "/Users/phall/workspace/phux".to_string(),
            },
        ),
        (
            Some(ResourceId::local(4)),
            AgentEvent::InputActivity {
                origin: "ada".to_string(),
            },
        ),
        (
            None,
            AgentEvent::Unknown {
                tag: 0x55,
                body: vec![1, 2, 3, 4, 5],
            },
        ),
    ] {
        assert_round_trip(&FrameKind::Event {
            terminal,
            event,
            stamp: None,
        });
    }
}

/// Forward-compat: an `EVENT` whose tag this version does not know (just
/// past the highest allocated, and far past it) decodes as
/// `AgentEvent::Unknown` with its body verbatim instead of failing the frame.
#[test]
fn event_unknown_tag_decodes_as_unknown_and_skips() {
    for tag in [0x10u8, 0x7F] {
        let body = [0xDEu8, 0xAD, 0xBE, 0xEF];
        let mut agent_event = vec![tag];
        agent_event.extend_from_slice(&u32::try_from(body.len()).unwrap().to_be_bytes());
        agent_event.extend_from_slice(&body);
        let mut fields = Vec::new();
        tlv_field(&mut fields, 2, &agent_event); // field::event::EVENT
        let bytes = framed_tlv(0xB3, &fields);
        let (decoded, tail) = FrameKind::decode(&bytes).unwrap();
        assert_eq!(
            decoded,
            FrameKind::Event {
                terminal: None,
                event: AgentEvent::Unknown {
                    tag,
                    body: body.to_vec(),
                },
                stamp: None,
            }
        );
        assert!(tail.is_empty());
    }
}
