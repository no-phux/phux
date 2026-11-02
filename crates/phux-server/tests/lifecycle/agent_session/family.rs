//! A session's parent: spawn refusals, watchers, and cascading close.

use super::*;

#[test]
fn spawn_with_unknown_or_session_parent_earns_typed_refusals() {
    phux_server_testkit::run_local(async {
        let (server, mut owner) = start(|_| {}).await;
        let result = spawn_session(&mut owner, 1, ResourceId::local(999_999)).await;
        assert_eq!(result, SpawnResult::Err(SpawnError::ParentNotFound));
        let (_parent, session) = parent_and_session(&mut owner, FOREVER).await;
        let result = spawn_session(&mut owner, 4, session).await;
        assert_eq!(result, SpawnResult::Err(SpawnError::ParentKindMismatch));

        drop(owner);
        server.stop().await;
    });
}

/// A watcher scoped to the pane (plus server-wide: one subscription entry,
/// one frame) hears the child's spawn and close exactly once each, though
/// the events name the child it could not have subscribed to (ADR-0104 §2).
#[test]
fn a_childs_spawn_and_close_reach_the_parents_watchers_once() {
    phux_server_testkit::run_local(async {
        let (server, mut owner) = start(|_| {}).await;
        let parent = spawned(&mut owner, 1, sh(FOREVER)).await;
        let mut watcher = server.connect().await;
        send_frame(
            &mut watcher,
            &FrameKind::SubscribeEvents {
                terminal: None,
                after_seq: None,
            },
        )
        .await;
        subscribe(&mut watcher, 100, Some(parent.clone()), None).await;

        let SpawnResult::Ok(session) = spawn_session(&mut owner, 2, parent).await else {
            panic!("spawn session failed");
        };
        let kill = Command::KillResource {
            terminal_id: session.clone(),
            operation_id: None,
        };
        assert_eq!(command(&mut owner, 3, kill).await, CommandResult::Ok);

        // Count to a barrier on the watcher's own connection.
        send_frame(
            &mut watcher,
            &FrameKind::Command {
                request_id: 101,
                command: Command::GetState {
                    scope: phux_protocol::wire::frame::StateScope::Server,
                },
            },
        )
        .await;
        let (mut spawned_n, mut closed_n) = (0, 0);
        wait_frame(&mut watcher, "barrier", |frame| match frame {
            FrameKind::Event {
                terminal: Some(id),
                event,
                ..
            } if id == session => {
                match event {
                    AgentEvent::ResourceSpawned { kind, parent } => {
                        assert_eq!(kind, ResourceKind::AgentSession);
                        assert!(parent.is_some());
                        spawned_n += 1;
                    }
                    AgentEvent::ResourceClosed { .. } => closed_n += 1,
                    _ => {}
                }
                None
            }
            FrameKind::CommandResult {
                request_id: 101, ..
            } => Some(()),
            _ => None,
        })
        .await;
        assert_eq!((spawned_n, closed_n), (1, 1));

        drop((owner, watcher));
        server.stop().await;
    });
}

/// Killing the parent and its PTY exiting both cascade the child with
/// `ParentClosed`, to an `ATTACH_RESOURCE` watcher as well as the owner.
#[test]
fn parent_kill_and_pty_exit_cascade_the_child_with_parent_closed() {
    phux_server_testkit::run_local(async {
        let (server, mut owner) = start(|_| {}).await;
        let (parent, session) = parent_and_session(&mut owner, FOREVER).await;
        let mut watcher = server.connect().await;
        attach_resource(&mut watcher, 100, &session).await;
        let kill = Command::KillResource {
            terminal_id: parent.clone(),
            operation_id: None,
        };
        assert_eq!(command(&mut owner, 3, kill).await, CommandResult::Ok);
        let seen = closes(&mut watcher, std::slice::from_ref(&session)).await;
        assert_eq!(seen[&session].0, CloseReason::ParentClosed);
        let seen = closes(&mut owner, &[parent.clone(), session.clone()]).await;
        assert_eq!(seen[&session].0, CloseReason::ParentClosed);
        assert_eq!(seen[&parent].0, CloseReason::Killed);

        let parent = spawned(&mut owner, 10, sh("read _line; exit 5")).await;
        let SpawnResult::Ok(session) = spawn_session(&mut owner, 11, parent.clone()).await else {
            panic!("spawn session failed");
        };
        crate::common::release(&mut owner, &parent).await;
        let seen = closes(&mut owner, &[parent.clone(), session.clone()]).await;
        assert_eq!(seen[&session].0, CloseReason::ParentClosed);
        assert_eq!(seen[&parent].0, CloseReason::Exited);

        drop((owner, watcher));
        server.stop().await;
    });
}

/// `KILL_RESOURCES` naming a parent, its child, and a bystander closes each
/// exactly once (the child is not double-closed by the cascade).
#[test]
fn kill_resources_over_a_mixed_set_closes_each_once() {
    phux_server_testkit::run_local(async {
        let (server, mut owner) = start(|_| {}).await;
        let (parent, session) = parent_and_session(&mut owner, FOREVER).await;
        let unrelated = spawned(&mut owner, 3, sh(FOREVER)).await;
        let targets = [parent, session, unrelated];
        let kill = Command::KillResources {
            ids: targets.to_vec(),
            operation_id: None,
        };
        assert_eq!(command(&mut owner, 4, kill).await, CommandResult::Ok);
        let mut seen = closes(&mut owner, &targets).await;
        // Everything the cascade emitted is now queued ahead of this reply,
        // so a duplicate close would be counted before it.
        send_frame(
            &mut owner,
            &FrameKind::Command {
                request_id: 5,
                command: Command::GetState {
                    scope: phux_protocol::wire::frame::StateScope::Server,
                },
            },
        )
        .await;
        wait_frame(&mut owner, "barrier", |frame| match frame {
            FrameKind::ResourceClosed {
                terminal_id,
                reason,
                ..
            } => {
                seen.entry(terminal_id).or_insert((reason, 0)).1 += 1;
                None
            }
            FrameKind::CommandResult { request_id: 5, .. } => Some(()),
            _ => None,
        })
        .await;
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert!(seen.values().all(|(_, n)| *n == 1), "{seen:?}");

        drop(owner);
        server.stop().await;
    });
}
