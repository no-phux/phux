//! Strategies for the organization and delegation commands.

use super::{
    arrangement_key, document, kind, launch, lifetime, live_target, name, nonzero16, op, pairs,
    pin, resource_key, rev, session_key,
};
use phux_protocol::ids::WorkspaceKey;
use phux_protocol::wire::frame::{
    Command, ExitPolicy, LaunchMode, MoveItem, OrgFilter, TerminateMode,
};
use proptest::prelude::*;

#[allow(
    clippy::too_many_lines,
    reason = "one arm per organization command, kept as one flat table"
)]
pub(super) fn org_command() -> impl Strategy<Value = Command> {
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
        (op(), session_key(), name()).prop_map(|(operation, session, name)| {
            Command::SessionRename {
                operation,
                session,
                name,
            }
        }),
        (op(), session_key(), proptest::option::of(name())).prop_map(
            |(operation, session, project)| Command::SessionSetProject {
                operation,
                session,
                project,
            }
        ),
        (op(), session_key(), lifetime()).prop_map(|(operation, session, lifetime)| {
            Command::SessionSetLifetime {
                operation,
                session,
                lifetime,
            }
        }),
        (op(), session_key())
            .prop_map(|(operation, session)| { Command::SessionDelete { operation, session } }),
        (op(), session_key())
            .prop_map(|(operation, session)| { Command::SessionKill { operation, session } }),
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
                        placement: None,
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
        (op(), resource_key(), proptest::option::of(name())).prop_map(
            |(operation, resource, name)| Command::ResourceRename {
                operation,
                resource,
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
        (op(), resource_key()).prop_map(|(operation, resource)| {
            Command::ResourceRemove {
                operation,
                resource,
            }
        }),
        (op(), resource_key(), rev()).prop_map(|(operation, resource, expected)| {
            Command::ResourceRelaunch {
                operation,
                resource,
                expected,
            }
        }),
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
pub(super) fn delegation_command() -> impl Strategy<Value = Command> {
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
