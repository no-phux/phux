//! Snapshot tests for the organization commands, results, and events
//! (`docs/organization.md`): one golden hex dump per shape, so a byte change
//! to any of them surfaces in review; `wire_snapshots.rs` checks them. The
//! frames outside §5.7 keep their goldens in `frame_wire_snapshots.rs`.

#![allow(
    clippy::unwrap_used,
    clippy::missing_const_for_fn,
    reason = "fixtures: unwrap a known-good key; helpers stay plain fns"
)]

use crate::common::dump_frame;
use commands::command_fixtures;
use events::event_fixtures;
use phux_protocol::ids::{
    ActivationId, ArrangementKey, AuthorityId, ClientId, IdempotencyKey, PrincipalId, ResourceId,
    ResourceKey, Revision, SatelliteHost, ServerInstance, SessionKey,
};
use phux_protocol::wire::frame::{
    ActorRef, AgentEvent, ArrangementDoc, ArrangementGroup, ArrangementNode, Command,
    CommandResult, CommandValue, EntryRef, EntryTarget, FrameKind, LaunchSpec, LiveTarget,
    OrganizationPage, SplitDirection,
};
use results::result_fixtures;

#[test]
fn intake_api_aliases_keep_legacy_wire_bytes() {
    let legacy = command(Command::InboxResolve {
        operation: op(),
        session: session(),
        expected: rev(4),
    });
    let preferred = command(Command::intake_resolve(op(), session(), rev(4)));
    assert_eq!(dump_frame(&preferred), dump_frame(&legacy));

    let page = OrganizationPage::new(AuthorityId::new([0xAA; 16]).unwrap(), rev(40), 77);
    let legacy = page.clone().with_inbox(Some(other_session()));
    let preferred = page.with_intake(Some(other_session()));
    assert_eq!(preferred.intake(), legacy.inbox);
    let frame = |page| {
        result(CommandResult::OkWith(CommandValue::OrganizationPage(
            Box::new(page),
        )))
    };
    assert_eq!(dump_frame(&frame(preferred)), dump_frame(&frame(legacy)));
}

fn op() -> IdempotencyKey {
    IdempotencyKey::new([0x0A; 16]).unwrap()
}
fn session() -> SessionKey {
    SessionKey::new([0x5E; 16]).unwrap()
}
fn other_session() -> SessionKey {
    SessionKey::new([0x5F; 16]).unwrap()
}
fn resource() -> ResourceKey {
    ResourceKey::new([0x7E; 16]).unwrap()
}
fn arrangement() -> ArrangementKey {
    ArrangementKey::new([0xA7; 16]).unwrap()
}
const fn rev(n: u64) -> Revision {
    Revision::new(n)
}
fn target() -> LiveTarget {
    LiveTarget {
        resource: ResourceId::local(0x2A),
        instance: ServerInstance::new([0x11; 16]),
        activation: ActivationId::new(3),
    }
}
fn launch() -> LaunchSpec {
    LaunchSpec::new(vec!["zsh".to_owned(), "-l".to_owned()])
        .with_cwd(Some("/src".to_owned()))
        .with_env(vec![("LANG".to_owned(), "C".to_owned())])
}
fn document() -> ArrangementDoc {
    ArrangementDoc {
        groups: vec![ArrangementGroup {
            locked: false,
            id: [0x61; 16],
            name: Some("main".to_owned()),
            tree: ArrangementNode::Split {
                dir: SplitDirection::Horizontal,
                ratio: 0.5,
                first: Box::new(ArrangementNode::Leaf(EntryRef {
                    entry: [0xE1; 16],
                    target: EntryTarget::Member(resource()),
                })),
                second: Box::new(ArrangementNode::Leaf(EntryRef {
                    entry: [0xE2; 16],
                    target: EntryTarget::Satellite {
                        host: SatelliteHost::new("edge"),
                        id: 9,
                    },
                })),
            },
        }],
    }
}
fn holder() -> ActorRef {
    ActorRef::new(ClientId::new(3)).with_principal(PrincipalId::new([0x91; 16]))
}
const fn command(command: Command) -> FrameKind {
    FrameKind::Command {
        request_id: 0x21,
        command,
    }
}
const fn result(result: CommandResult) -> FrameKind {
    FrameKind::CommandResult {
        request_id: 0x21,
        result,
    }
}
const fn event(event: AgentEvent) -> FrameKind {
    FrameKind::Event {
        terminal: None,
        event,
        stamp: None,
    }
}

mod commands;
mod events;
mod results;

/// Every organization fixture: commands, then results, then events.
#[expect(
    clippy::redundant_pub_crate,
    reason = "`pub` alone trips `unreachable_pub`; `wire_snapshots.rs` checks these"
)]
pub(crate) fn fixtures() -> Vec<(&'static str, FrameKind)> {
    let mut fixtures = command_fixtures();
    fixtures.extend(result_fixtures());
    fixtures.extend(event_fixtures());
    fixtures
}
