//! Source gaps, event filters, and the actor behind each event.

use std::time::Duration;

use phux_protocol::ids::{GroupId, ResourceId};
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, ControlAction, FrameKind, InputMode, ResourceEventType,
    SESSION_CREATE_KEY, SESSION_KEEP_EMPTY_KEY, SESSION_NAME_KEY, Scope,
};
use tempfile::TempDir;

use phux_server_testkit::{
    Spawn, join_after_shutdown, recv_until, run_local, send_frame, spawn_server_with_seed_cmd,
};

use super::{
    Seen, Server, asked_ids, command, drain_until_quiet, journal_contents, next_event,
    report_asked, spawn_pane, subscribe,
};
use crate::common::{attach_pane, connect_as, full_caps, gated_seed};

/// Hundreds of OSC-133 marks in one write overflow the 64-event sink; the
/// loss is journaled as a stamped `source_gap` scoped to that pane.
#[test]
fn sink_overflow_journals_source_gap_for_that_terminal() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let release = tmp.path().join("release");
        let marks = "\\033]133;C\\007\\033]133;D;0\\007".repeat(400);
        let seed = gated_seed(&release, &format!("printf '{marks}'; sleep 600"));
        let (shutdown, server) = spawn_server_with_seed_cmd(socket.clone(), "demo", seed);
        let (mut observer, _) = connect_as(&socket, "observer", full_caps()).await;
        let (mut attacher, _) = connect_as(&socket, "attacher", full_caps()).await;
        let pane = attach_pane(&mut attacher, "demo").await;
        let _ = subscribe(&mut observer, 1, Some(pane.clone()), None).await;
        std::fs::write(&release, b"go").unwrap();

        let reported = next_event(&mut observer, |s| {
            matches!(
                s.event,
                AgentEvent::SourceGap { .. } | AgentEvent::JournalGap { .. }
            )
        })
        .await;
        let source_gap = if matches!(reported.event, AgentEvent::SourceGap { .. }) {
            reported
        } else {
            // The observer's own mailbox overflowed first; the journal still holds it.
            let (mut replayer, _) = connect_as(&socket, "replayer", full_caps()).await;
            subscribe(&mut replayer, 2, Some(pane.clone()), Some(0))
                .await
                .into_iter()
                .find(|s| matches!(s.event, AgentEvent::SourceGap { .. }))
                .expect("the journal holds the source gap")
        };
        assert_eq!(source_gap.terminal.as_ref(), Some(&pane));
        let AgentEvent::SourceGap { dropped } = source_gap.event else {
            unreachable!()
        };
        assert!(dropped > 0);
        assert!(source_gap.stamp.is_some(), "journaled and stamped");

        drop((observer, attacher));
        join_after_shutdown(shutdown, server).await;
    });
}

/// `SUBSCRIBE_RESOURCE_EVENTS` filters are replaced by a later subscribe of
/// either verb; bell, title, and asked reach an empty-filter subscriber.
#[test]
fn resource_event_filters_are_replaced_and_empty_admits_everything() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let release = tmp.path().join("release");
        let seed = gated_seed(
            &release,
            "printf '\\007\\033]2;journal-title\\007'; sleep 600",
        );
        let (shutdown, server) = spawn_server_with_seed_cmd(socket.clone(), "demo", seed);
        let (mut a, _) = connect_as(&socket, "filtered", full_caps()).await;
        let pane = attach_pane(&mut a, "demo").await;
        let filter = |types: Vec<ResourceEventType>| Command::SubscribeResourceEvents {
            terminal_id: pane.clone(),
            event_types: types,
        };

        assert_eq!(
            command(&mut a, 1, filter(vec![ResourceEventType::CwdChanged]))
                .await
                .0,
            CommandResult::Ok
        );
        let (_, seen) = command(&mut a, 2, report_asked(&pane, "filtered-out")).await;
        assert!(
            asked_ids(&seen).is_empty(),
            "a cwd-only filter admits no asked: {seen:?}"
        );
        let _ = subscribe(&mut a, 3, Some(pane.clone()), None).await;
        let (_, seen) = command(&mut a, 4, report_asked(&pane, "unfiltered-again")).await;
        assert_eq!(
            asked_ids(&seen),
            ["unfiltered-again"],
            "SUBSCRIBE_EVENTS reset the filter"
        );

        assert_eq!(
            command(&mut a, 5, filter(vec![ResourceEventType::CwdChanged]))
                .await
                .0,
            CommandResult::Ok
        );
        assert_eq!(
            command(&mut a, 6, filter(Vec::new())).await.0,
            CommandResult::Ok
        );
        std::fs::write(&release, b"go").unwrap();
        let _ = next_event(&mut a, |s| matches!(s.event, AgentEvent::Bell)).await;
        let _ = next_event(
            &mut a,
            |s| matches!(&s.event, AgentEvent::TitleChanged { title } if title == "journal-title"),
        )
        .await;
        let (_, seen) = command(&mut a, 7, report_asked(&pane, "admitted")).await;
        assert_eq!(
            asked_ids(&seen),
            ["admitted"],
            "the empty filter replaced the old one"
        );

        drop(a);
        join_after_shutdown(shutdown, server).await;
    });
}

/// Metadata writes (plain, intercepted rename/keep-empty, session create)
/// name the writer as their actor.
#[test]
fn metadata_writes_name_their_actor() {
    run_local(async {
        const KEY: &str = "phux.tui.layout/v1";
        let server = Server::start(None);
        let mut watcher = server.connect("watcher").await;
        let group = Scope::Group(GroupId::new(1));
        for (scope, key) in [
            (group.clone(), KEY),
            (Scope::Global, SESSION_NAME_KEY),
            (Scope::Global, SESSION_KEEP_EMPTY_KEY),
        ] {
            send_frame(
                &mut watcher,
                &FrameKind::SubscribeMetadata {
                    scope,
                    key: key.to_owned(),
                },
            )
            .await;
        }
        let _ = subscribe(&mut watcher, 1, None, None).await;

        let mut writer = server.connect("writer").await;
        let writes = [
            (group, KEY, b"layout".to_vec()),
            (Scope::Global, SESSION_NAME_KEY, b"demo\0renamed".to_vec()),
            (
                Scope::Global,
                SESSION_KEEP_EMPTY_KEY,
                // The named seed is persistent: making it ephemeral is the change.
                phux_protocol::wire::frame::encode_session_keep_empty("renamed", false),
            ),
        ];
        for (n, (scope, key, value)) in writes.into_iter().enumerate() {
            send_frame(
                &mut writer,
                &FrameKind::SetMetadata {
                    request_id: u32::try_from(n).unwrap(),
                    scope,
                    key: key.to_owned(),
                    value,
                },
            )
            .await;
            let actor = recv_until(&mut watcher, |_, frame| match frame {
                FrameKind::MetadataChanged {
                    key: changed,
                    actor,
                    ..
                } if changed == key => Some(actor),
                _ => None,
            })
            .await;
            assert_eq!(
                actor.expect(key).client_name.as_deref(),
                Some("writer"),
                "{key}"
            );
        }

        send_frame(
            &mut writer,
            &FrameKind::SetMetadata {
                request_id: 9,
                scope: Scope::Global,
                key: SESSION_CREATE_KEY.to_owned(),
                value: br#"{"name":"made-by-writer"}"#.to_vec(),
            },
        )
        .await;
        let spawned = next_event(&mut watcher, |s| {
            matches!(s.event, AgentEvent::ResourceSpawned { .. })
        })
        .await;
        assert_eq!(
            spawned.actor_name(),
            Some("writer"),
            "a created session's seed pane"
        );

        drop((watcher, writer));
        server.stop().await;
    });
}

/// Input-lease take and give are journaled naming the driver, in stamp and body.
#[test]
fn terminal_control_events_are_journaled_with_actor() {
    run_local(async {
        let server = Server::start(None);
        let mut watcher = server.connect("watcher").await;
        let _ = subscribe(&mut watcher, 1, None, None).await;
        let (mut writer, pane) = server.producer("writer").await;
        let acquire = Command::AcquireInput {
            terminal_id: pane.clone(),
            mode: InputMode::Cooperative,
            ttl_ms: 0,
        };
        assert_eq!(command(&mut writer, 20, acquire).await.0, CommandResult::Ok);
        let control = |action: ControlAction| move |s: &Seen| matches!(s.event, AgentEvent::TerminalControl { action: got, .. } if got == action);
        let taken = next_event(&mut watcher, control(ControlAction::Acquired)).await;
        let release = Command::ReleaseInput {
            terminal_id: pane.clone(),
        };
        assert_eq!(command(&mut writer, 21, release).await.0, CommandResult::Ok);
        let given = next_event(&mut watcher, control(ControlAction::Released)).await;
        for seen in [&taken, &given] {
            assert_eq!(seen.terminal.as_ref(), Some(&pane));
            assert_eq!(seen.actor_name(), Some("writer"));
            let AgentEvent::TerminalControl { actor, .. } = &seen.event else {
                unreachable!()
            };
            let stamped = seen.stamp.as_ref().unwrap().actor.as_ref().unwrap();
            assert_eq!(Some(stamped.client), *actor, "stamp and body agree");
        }
        assert!(taken.seq() < given.seq());

        drop((watcher, writer));
        server.stop().await;
    });
}

/// For panes that exit at once after emitting marks: `pane_spawned` precedes
/// `pane_closed`, and nothing about a pane is journaled after its close.
#[test]
fn fast_exiting_panes_journal_their_events_between_spawned_and_closed() {
    run_local(async {
        let server = Server::start(None);
        let (mut a, _) = server.producer("spawner").await;
        let _ = subscribe(&mut a, 1, None, None).await;
        let mut spawned = Vec::new();
        for n in 0..12 {
            spawned.push(
                spawn_pane(
                    &mut a,
                    500 + n,
                    "printf '\\033]133;C\\007\\033]133;D;0\\007'",
                )
                .await
                .0,
            );
        }
        let is_closed = |seen: &[Seen], pane: &ResourceId| {
            seen.iter().any(|s| {
                s.terminal.as_ref() == Some(pane)
                    && matches!(s.event, AgentEvent::ResourceClosed { .. })
            })
        };
        let mut seen = Vec::new();
        for _ in 0..60 {
            seen = journal_contents(&server.socket()).await;
            if spawned.iter().all(|pane| is_closed(&seen, pane)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        assert!(
            spawned.iter().all(|pane| is_closed(&seen, pane)),
            "{spawned:?}"
        );
        assert!(
            seen.iter()
                .any(|s| matches!(s.event, AgentEvent::CommandFinished { .. })),
            "final marks journaled"
        );
        for close in seen
            .iter()
            .filter(|s| matches!(s.event, AgentEvent::ResourceClosed { .. }))
        {
            let pane = close.terminal.as_ref().expect("a close names its pane");
            let about: Vec<&Seen> = seen
                .iter()
                .filter(|s| s.terminal.as_ref() == Some(pane))
                .collect();
            let opened = about
                .iter()
                .find(|s| matches!(s.event, AgentEvent::ResourceSpawned { .. }))
                .unwrap_or_else(|| panic!("{pane:?} closed without a pane_spawned"));
            assert!(opened.seq() < close.seq(), "{pane:?}");
            assert!(
                about.iter().all(|s| s.seq() <= close.seq()),
                "{pane:?}: {about:?}"
            );
        }

        drop(a);
        server.stop().await;
    });
}

/// A spawner that vanishes mid-publication still leaves exactly one
/// `pane_closed` for every pane that is gone, and none for a live one.
#[test]
fn a_spawn_abandoned_mid_publication_still_journals_its_close() {
    run_local(async {
        let server = Server::start(None);
        let mut watcher = server.connect("watcher").await;
        let _ = subscribe(&mut watcher, 1, None, None).await;
        for n in 0..15 {
            let (mut spawner, _) = server.producer("abandoner").await;
            send_frame(
                &mut spawner,
                &Spawn::command(&["/bin/sh", "-c", "sleep 600"]).frame(700 + n),
            )
            .await;
            drop(spawner);
        }
        let seen = drain_until_quiet(&mut watcher, Duration::from_millis(1500)).await;
        let spawned: Vec<ResourceId> = seen
            .iter()
            .filter(|s| matches!(s.event, AgentEvent::ResourceSpawned { .. }))
            .filter_map(|s| s.terminal.clone())
            .collect();
        for (n, pane) in (900..).zip(&spawned) {
            let closes = seen
                .iter()
                .filter(|s| {
                    s.terminal.as_ref() == Some(pane)
                        && matches!(s.event, AgentEvent::ResourceClosed { .. })
                })
                .count();
            let state = Command::GetTerminalState {
                terminal_id: pane.clone(),
                include_scrollback: false,
                max_scrollback_lines: 0,
            };
            let (alive, _) = command(&mut watcher, n, state).await;
            let gone = usize::from(matches!(alive, CommandResult::Error { .. }));
            assert_eq!(closes, gone, "{pane:?}");
        }

        drop(watcher);
        server.stop().await;
    });
}
