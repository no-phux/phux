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
    ActivationId, ArrangementKey, AuthorityId, ClientId, IdempotencyKey, KEY_TEXT_LEN, LeaseEpoch,
    PrincipalId, ResourceId, ResourceKey, ResourceKind, Revision, SatelliteHost, ServerInstance,
    SessionId, SessionKey, WindowId, WorkspaceKey,
};
use phux_protocol::kinds::Verb;
use phux_protocol::wire::frame::{
    ActivationInfo, ActorRef, AgentEvent, ArrangementDoc, ArrangementGroup, ArrangementNode,
    ArrangementOwner, ArrangementValue, Command, CommandResult, CommandValue, Committed,
    Delegation, EntryRef, EntryTarget, EventStamp, ExecState, ExitPolicy, FrameKind, LaunchMode,
    LaunchSpec, LiveTarget, MoveItem, OrgFilter, OrganizationPage, Outcome, RecordRevision,
    Refusal, ResourceRecord, SessionLifetime, SessionRecord, SplitDirection, TerminateMode,
};
use phux_protocol::wire::info::{
    ResourceInfo, SessionInfo, SessionSnapshot, TUI_LAYOUT_KEY_PREFIX, tui_layout_key,
    tui_layout_key_session,
};
use proptest::prelude::*;

use crate::common::{assert_round_trip, decode, encode};

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

#[allow(
    clippy::too_many_lines,
    reason = "one arm per organization command, kept as one flat table"
)]
fn org_command() -> impl Strategy<Value = Command> {
    let filter = prop_oneof![
        Just(OrgFilter::All),
        Just(OrgFilter::Intake),
        session_key().prop_map(OrgFilter::Session),
        nonzero16().prop_map(|b| OrgFilter::Workspace(WorkspaceKey::new(b).unwrap())),
    ];
    prop_oneof![
        (
            filter,
            any::<u32>(),
            proptest::option::of(proptest::collection::vec(any::<u8>(), 0..32))
        )
            .prop_map(|(filter, page_size, cursor)| Command::GetOrganization {
                filter,
                page_size,
                cursor,
            }),
        (
            op(),
            name(),
            lifetime(),
            pairs(),
            proptest::option::of(launch())
        )
            .prop_map(|(operation, name, lifetime, attrs, launch)| {
                Command::SessionCreate {
                    operation,
                    name,
                    lifetime,
                    attrs,
                    launch: launch.map(Box::new),
                }
            }),
        (op(), session_key(), rev(), name()).prop_map(|(operation, session, expected, name)| {
            Command::SessionRename {
                operation,
                session,
                expected,
                name,
            }
        }),
        (op(), session_key(), rev(), lifetime()).prop_map(
            |(operation, session, expected, lifetime)| Command::SessionSetLifetime {
                operation,
                session,
                expected,
                lifetime,
            }
        ),
        (op(), session_key(), rev()).prop_map(|(operation, session, expected)| {
            Command::SessionDelete {
                operation,
                session,
                expected,
            }
        }),
        (
            op(),
            session_key(),
            kind(),
            proptest::option::of(launch()),
            proptest::option::of(name()),
            any::<bool>(),
            any::<bool>()
        )
            .prop_map(
                |(operation, session, kind, launch, name, retain, deferred)| {
                    Command::ResourceCreate {
                        operation,
                        session,
                        kind,
                        launch: launch.map(Box::new),
                        name,
                        on_exit: if retain {
                            ExitPolicy::Retain
                        } else {
                            ExitPolicy::Remove
                        },
                        launch_mode: if deferred {
                            LaunchMode::Deferred
                        } else {
                            LaunchMode::Immediate
                        },
                    }
                }
            ),
        (
            op(),
            resource_key(),
            session_key(),
            rev(),
            session_key(),
            rev()
        )
            .prop_map(
                |(operation, resource, from, from_expected, to, to_expected)| {
                    Command::ResourceMove {
                        operation,
                        resource,
                        from,
                        from_expected,
                        to,
                        to_expected,
                    }
                }
            ),
        (
            op(),
            proptest::collection::vec(
                (resource_key(), session_key(), rev()).prop_map(|(resource, from, e)| {
                    MoveItem {
                        resource,
                        from,
                        from_expected: e,
                    }
                }),
                0..4
            ),
            session_key(),
            rev()
        )
            .prop_map(
                |(operation, items, to, to_expected)| Command::ResourceMoveMany {
                    operation,
                    items,
                    to,
                    to_expected,
                }
            ),
        (op(), resource_key(), rev(), proptest::option::of(name())).prop_map(
            |(operation, resource, expected, name)| Command::ResourceRename {
                operation,
                resource,
                expected,
                name,
            }
        ),
        (op(), live_target(), any::<bool>()).prop_map(|(operation, target, force)| {
            Command::ResourceTerminate {
                operation,
                target,
                mode: if force {
                    TerminateMode::Force
                } else {
                    TerminateMode::Graceful
                },
            }
        }),
        (op(), resource_key(), rev()).prop_map(|(operation, resource, expected)| {
            Command::ResourceRemove {
                operation,
                resource,
                expected,
            }
        }),
        (op(), resource_key(), rev()).prop_map(|(operation, resource, expected)| {
            Command::ResourceRelaunch {
                operation,
                resource,
                expected,
            }
        }),
        (op(), resource_key(), rev(), any::<u64>()).prop_map(
            |(operation, resource, expected, a)| Command::ResourceAbandon {
                operation,
                resource,
                expected,
                activation: ActivationId::new(a),
            }
        ),
        (op(), session_key(), rev()).prop_map(|(operation, session, expected)| {
            Command::InboxResolve {
                operation,
                session,
                expected,
            }
        }),
        (op(), op(), proptest::collection::vec(pin(), 0..4)).prop_map(
            |(operation, undo, expected)| Command::OrganizationUndo {
                operation,
                undo,
                expected,
            }
        ),
        arrangement_key().prop_map(|arrangement| Command::ArrangementGet { arrangement }),
        (op(), arrangement_key(), rev(), document()).prop_map(
            |(operation, arrangement, expected, document)| Command::ArrangementReplace {
                operation,
                arrangement,
                expected,
                document,
            }
        ),
    ]
}

/// The delegation commands (ADR-0163): field-tagged like the organization
/// commands, but not organization commands.
fn delegation_command() -> impl Strategy<Value = Command> {
    prop_oneof![
        (op(), session_key(), any::<u32>()).prop_map(|(operation, session, ttl_secs)| {
            Command::DelegateAgent {
                operation,
                session,
                ttl_secs,
            }
        }),
        (op(), "[a-z0-9:]{1,64}").prop_map(|(operation, credential_id)| {
            Command::RevokeDelegation {
                operation,
                credential_id,
            }
        }),
    ]
}

fn refusal() -> impl Strategy<Value = Refusal> {
    prop_oneof![
        proptest::sample::select(Verb::ALL.to_vec())
            .prop_map(|verb| Refusal::Unauthorized { verb }),
        Just(Refusal::NotFound),
        rev().prop_map(|current| Refusal::Conflict { current }),
        proptest::option::of(any::<u64>()).prop_map(|current| Refusal::Stale {
            current: current.map(ActivationId::new),
        }),
        any::<u32>().prop_map(|members| Refusal::NotEmpty { members }),
        name().prop_map(|capability| Refusal::Unsupported { capability }),
        (name(), name()).prop_map(|(field, reason)| Refusal::InvalidArgument { field, reason }),
        Just(Refusal::NameTaken),
        Just(Refusal::ExecutionUncertain),
        actor().prop_map(|holder| Refusal::LeaseHeld { holder }),
        Just(Refusal::DuplicateOperation),
    ]
}

#[allow(
    clippy::too_many_lines,
    reason = "one strategy per organization result"
)]
fn org_result() -> impl Strategy<Value = CommandResult> {
    let session_record = (
        session_key(),
        name(),
        lifetime(),
        any::<bool>(),
        proptest::option::of(arrangement_key()),
        pairs(),
        any::<u32>(),
        rev(),
        proptest::option::of(any::<u32>()),
    )
        .prop_map(
            |(key, n, l, awaiting, arrangement, attrs, members, r, live)| {
                SessionRecord::new(key, n, r)
                    .with_lifetime(l)
                    .with_awaiting_organization(awaiting)
                    .with_arrangement(arrangement)
                    .with_attrs(attrs)
                    .with_members(members)
                    .with_live(live.map(SessionId::new))
            },
        );
    let activation = (
        any::<u64>(),
        any::<[u8; 16]>(),
        exec_state(),
        proptest::option::of(outcome()),
    )
        .prop_map(|(id, instance, state, outcome)| {
            ActivationInfo::new(ActivationId::new(id), ServerInstance::new(instance), state)
                .with_outcome(outcome)
        });
    let resource_record = (
        resource_key(),
        session_key(),
        proptest::option::of(name()),
        kind(),
        proptest::option::of(activation),
        rev(),
        proptest::option::of(resource_id()),
    )
        .prop_map(|(key, session, n, k, activation, r, live)| {
            ResourceRecord::new(key, session, r)
                .with_name(n)
                .with_kind(k)
                .with_on_exit(ExitPolicy::Retain)
                .with_activation(activation)
                .with_live(live)
        });
    let page = (
        nonzero16(),
        proptest::option::of(session_key()),
        proptest::collection::vec(session_record, 0..3),
        proptest::collection::vec(resource_record, 0..3),
        any::<(u32, u32, u32)>(),
        rev(),
        any::<u64>(),
        proptest::option::of(proptest::collection::vec(any::<u8>(), 0..16)),
    )
        .prop_map(
            |(authority, inbox, sessions, resources, totals, r, seq, cursor)| {
                CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(
                    OrganizationPage::new(AuthorityId::new(authority).unwrap(), r, seq)
                        .with_inbox(inbox)
                        .with_sessions(sessions)
                        .with_resources(resources)
                        .with_totals(totals.0, totals.1, totals.2)
                        .with_next_cursor(cursor),
                )))
            },
        );
    let committed = (
        op(),
        rev(),
        proptest::option::of(session_key()),
        proptest::option::of(resource_key()),
        proptest::option::of(live_target()),
        proptest::collection::vec(pin(), 0..3),
        proptest::option::of(any::<bool>()),
    )
        .prop_map(|(operation, r, session, resource, target, pins, prior)| {
            CommandResult::OkWith(CommandValue::Committed(Box::new(
                Committed::new(operation, r)
                    .with_session(session)
                    .with_resource(resource)
                    .with_target(target)
                    .with_revisions(pins)
                    .with_prior_awaiting(prior),
            )))
        });
    let arrangement = (arrangement_key(), owner(), rev(), document()).prop_map(|(k, o, r, d)| {
        CommandResult::OkWith(CommandValue::Arrangement(Box::new(ArrangementValue::new(
            k, o, r, d,
        ))))
    });
    let delegation = (
        session_key(),
        "[a-z0-9:]{1,64}",
        "/[a-z0-9/._-]{0,80}",
        any::<u64>(),
    )
        .prop_map(|(session, id, path, expires)| {
            CommandResult::OkWith(CommandValue::Delegation(Box::new(Delegation::new(
                id, session, path, expires,
            ))))
        });
    prop_oneof![
        refusal().prop_map(CommandResult::Refused),
        op().prop_map(|operation| CommandResult::Accepted { operation }),
        page,
        committed,
        arrangement,
        delegation,
    ]
}

fn org_event() -> impl Strategy<Value = AgentEvent> {
    prop_oneof![
        (session_key(), name(), lifetime(), rev()).prop_map(
            |(session, name, lifetime, revision)| {
                AgentEvent::SessionCreated {
                    session,
                    name,
                    lifetime,
                    revision,
                }
            }
        ),
        (session_key(), name(), rev()).prop_map(|(session, name, revision)| {
            AgentEvent::SessionRenamed {
                session,
                name,
                revision,
            }
        }),
        (session_key(), name(), rev()).prop_map(|(session, name, revision)| {
            AgentEvent::SessionDeleted {
                session,
                name,
                revision,
            }
        }),
        (session_key(), lifetime(), rev()).prop_map(|(session, lifetime, revision)| {
            AgentEvent::SessionLifetimeChanged {
                session,
                lifetime,
                revision,
            }
        }),
        (resource_key(), session_key(), session_key(), rev(), rev()).prop_map(
            |(resource, from, to, from_revision, to_revision)| AgentEvent::MembershipChanged {
                resource,
                from,
                to,
                from_revision,
                to_revision,
            }
        ),
        (resource_key(), session_key(), kind()).prop_map(|(resource, session, kind)| {
            AgentEvent::ResourceCreated {
                resource,
                session,
                kind,
            }
        }),
        (resource_key(), session_key(), kind()).prop_map(|(resource, session, kind)| {
            AgentEvent::ResourceRemoved {
                resource,
                session,
                kind,
            }
        }),
        (resource_key(), proptest::option::of(name()), rev()).prop_map(
            |(resource, name, revision)| AgentEvent::ResourceRenamed {
                resource,
                name,
                revision,
            }
        ),
        (
            resource_key(),
            any::<u64>(),
            exec_state(),
            proptest::option::of(outcome())
        )
            .prop_map(
                |(resource, a, state, outcome)| AgentEvent::ActivationChanged {
                    resource,
                    activation: ActivationId::new(a),
                    state,
                    outcome,
                }
            ),
        (arrangement_key(), owner(), proptest::option::of(rev())).prop_map(
            |(arrangement, owner, revision)| AgentEvent::ArrangementChanged {
                arrangement,
                owner,
                revision,
            }
        ),
        (resource_key(), any::<u64>(), proptest::option::of(actor())).prop_map(
            |(resource, epoch, holder)| AgentEvent::LeaseChanged {
                resource,
                epoch: LeaseEpoch::new(epoch),
                holder,
            }
        ),
        (session_key(), any::<bool>(), rev()).prop_map(|(session, awaiting, revision)| {
            AgentEvent::IntakeChanged {
                session,
                awaiting_organization: awaiting,
                revision,
            }
        }),
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

// -----------------------------------------------------------------------------
// Older shapes stay byte-identical.
// -----------------------------------------------------------------------------

fn changed(actor: Option<ActorRef>) -> FrameKind {
    FrameKind::MetadataChanged {
        scope: phux_protocol::wire::frame::Scope::Global,
        key: "phux.example/v1".to_owned(),
        value: None,
        actor,
    }
}

/// R09: an actor without principals writes exactly its `0.9.0` bytes, and a
/// pre-principal encoding decodes with both absent; with principals the pair
/// trails the earlier fields, which an older decoder skips because the actor
/// ends its container.
#[test]
fn actor_principals_trail_and_old_shapes_are_unchanged() {
    let plain = ActorRef::new(ClientId::new(7)).with_client_name(Some("cli".to_owned()));
    let with = plain
        .clone()
        .with_principal(PrincipalId::new([0x91; 16]))
        .with_on_behalf_of(PrincipalId::new([0x92; 16]));
    assert_round_trip(&changed(Some(with.clone())));
    let only_obo = plain
        .clone()
        .with_on_behalf_of(PrincipalId::new([0x92; 16]));
    assert_round_trip(&changed(Some(only_obo)));

    let old = encode(&changed(Some(plain.clone())));
    let new = encode(&changed(Some(with.clone())));
    // The new encoding is the old one plus `1 || 16 || 1 || 16` trailing the
    // actor (and four more length bytes in the frame header, and the field
    // length), so stripping the trailing pair reproduces the old bytes.
    assert_eq!(new.len(), old.len() + 34);
    let decoded = decode(&old).unwrap();
    let FrameKind::MetadataChanged {
        actor: Some(actor), ..
    } = decoded
    else {
        panic!("actor survives");
    };
    assert_eq!(actor, plain);
    assert_eq!(actor.principal(), None);
    assert_eq!(actor.on_behalf_of(), None);
    assert_eq!(with.principal(), PrincipalId::new([0x91; 16]));
}

/// R05/R06: a snapshot whose sessions and resources report no organization
/// state encodes exactly as before; reported state rides the extension block
/// (fields 3 and 4) and round-trips, and `with_lifetime` keeps `keep_empty`
/// equal to `lifetime == Persistent`.
#[test]
fn snapshot_session_link_and_lifetime_ride_the_extension_block() {
    let base = || {
        SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(2))
            .with_sessions(vec![
                SessionInfo::new(SessionId::new(1), "work").with_window_count(1),
            ])
            .with_resources(vec![ResourceInfo::new(
                ResourceId::local(2),
                WindowId::new(1),
                80,
                24,
            )])
    };
    let state = |snapshot| FrameKind::CommandResult {
        request_id: 1,
        result: CommandResult::OkWith(CommandValue::State(snapshot)),
    };
    let legacy = encode(&state(base()));

    let key = SessionKey::new([0x5E; 16]).unwrap();
    let mut organized = base();
    organized.sessions[0] = organized.sessions[0]
        .clone()
        .with_lifetime(Some(SessionLifetime::Ephemeral))
        .with_key(Some(key));
    organized.resources[0] = organized.resources[0]
        .clone()
        .with_session(Some(SessionId::new(1)));
    assert!(!organized.sessions[0].keep_empty);
    let persistent = organized.sessions[0]
        .clone()
        .with_lifetime(Some(SessionLifetime::Persistent));
    assert!(persistent.keep_empty);

    let frame = state(organized.clone());
    assert_round_trip(&frame);
    let bytes = encode(&frame);
    assert!(bytes.len() > legacy.len());
    assert_eq!(encode(&state(base())), legacy);
    let FrameKind::CommandResult {
        result: CommandResult::OkWith(CommandValue::State(decoded)),
        ..
    } = decode(&bytes).unwrap()
    else {
        panic!("state reply");
    };
    assert_eq!(decoded.resources[0].session, Some(SessionId::new(1)));
    assert_eq!(
        decoded.sessions[0].lifetime,
        Some(SessionLifetime::Ephemeral)
    );
    assert_eq!(decoded.sessions[0].key, Some(key));

    // keep_empty is still written as the legacy facet.
    let keep = base();
    let mut keep_sessions = keep.sessions.clone();
    keep_sessions[0] = keep_sessions[0]
        .clone()
        .with_lifetime(Some(SessionLifetime::Persistent));
    assert_round_trip(&state(keep.with_sessions(keep_sessions)));
}

/// An organization event whose body names an unknown enum value or misses a
/// required field stays opaque, like any unknown event, instead of failing the
/// frame.
#[test]
fn malformed_organization_events_are_opaque() {
    let frame = FrameKind::Event {
        terminal: None,
        event: AgentEvent::SessionLifetimeChanged {
            session: SessionKey::new([1; 16]).unwrap(),
            lifetime: SessionLifetime::Ephemeral,
            revision: Revision::new(1),
        },
        stamp: None,
    };
    let mut bytes = encode(&frame);
    // The lifetime byte (`1`) is the only `0x01` following the `06 04 01`
    // field header; set it to an undefined lifetime.
    let at = bytes
        .windows(3)
        .position(|w| w == [0x06, 0x04, 0x01])
        .unwrap();
    bytes[at + 3] = 0x09;
    let FrameKind::Event { event, .. } = decode(&bytes).unwrap() else {
        panic!("event");
    };
    assert!(
        matches!(event, AgentEvent::Unknown { tag: 0x13, .. }),
        "{event:?}"
    );
}

/// The 16-byte keys print as 26 lowercase base32 digits and parse back in
/// either case; a short prefix matches, and zero keys do not exist.
#[test]
fn keys_have_one_text_form() {
    let key = SessionKey::new([0xFF; 16]).unwrap();
    let text = key.to_string();
    assert_eq!(text.len(), KEY_TEXT_LEN);
    assert_eq!(text, "77777777777777777777777774");
    assert_eq!(SessionKey::parse(&text), Some(key));
    assert_eq!(SessionKey::parse(&text.to_uppercase()), Some(key));
    assert!(key.has_prefix("777777"));
    assert!(!key.has_prefix("abc"));
    let one = ResourceKey::new([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]).unwrap();
    assert_eq!(ResourceKey::parse(&one.to_string()), Some(one));
    assert_eq!(SessionKey::new([0; 16]), None);
    assert_eq!(SessionKey::parse(&"a".repeat(KEY_TEXT_LEN)), None);
    // The two padding bits of the last digit must be zero.
    assert_eq!(SessionKey::parse("77777777777777777777777777"), None);
    assert_eq!(SessionKey::parse("short"), None);
    let json = serde_json::to_string(&key).unwrap();
    assert_eq!(json, format!("\"{text}\""));
    assert_eq!(serde_json::from_str::<SessionKey>(&json).unwrap(), key);
}

#[test]
fn tui_layout_key_names_one_session() {
    assert_eq!(TUI_LAYOUT_KEY_PREFIX, "phux.tui.layout/v1/");
    assert_eq!(tui_layout_key(SessionId::new(7)), "phux.tui.layout/v1/7");
    assert_eq!(
        tui_layout_key_session("phux.tui.layout/v1/7"),
        Some(SessionId::new(7))
    );
    for other in [
        "phux.tui.layout/v1/",
        "phux.tui.layout/v1/07",
        "phux.tui.layout/v1/x",
        "phux.tui.layout/v12",
        "work.layout/v1/1",
    ] {
        assert_eq!(tui_layout_key_session(other), None, "{other}");
    }
}
