//! A session's parent: spawn refusals, watchers, and cascading close.

use super::*;
use phux_protocol::ids::{ArrangementKey, OperationId};
use phux_protocol::wire::frame::{
    ArrangementGroup, ArrangementNode, CommandValue, EntryRef, EntryTarget, KillConditions,
    KillPrecondition, LiveTarget, OrgFilter, OrganizationPage, Refusal, TerminateMode,
};

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
            &phux_server_testkit::subscribe_events(None, None),
        )
        .await;
        subscribe(&mut watcher, 100, Some(parent.clone()), None).await;

        let SpawnResult::Ok(session) = spawn_session(&mut owner, 2, parent).await else {
            panic!("spawn session failed");
        };
        let kill = phux_server_testkit::kill(session.clone());
        assert_eq!(command(&mut owner, 3, kill).await, CommandResult::Ok);

        // Count to a barrier on the watcher's own connection.
        phux_server_testkit::send_command(&mut watcher, 101, phux_server_testkit::get_state())
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
        let kill = phux_server_testkit::kill(parent.clone());
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

async fn organization(owner: &mut UnixStream, request: u32) -> OrganizationPage {
    let list = Command::GetOrganization {
        filter: OrgFilter::All,
        page_size: 0,
        cursor: None,
    };
    match command(owner, request, list).await {
        CommandResult::OkWith(CommandValue::OrganizationPage(page)) => *page,
        other => panic!("GET_ORGANIZATION: {other:?}"),
    }
}

async fn install_child_group(
    owner: &mut UnixStream,
    page: &OrganizationPage,
    child: &ResourceId,
) -> (ArrangementKey, [u8; 16]) {
    let record = page
        .resources
        .iter()
        .find(|r| r.live.as_ref() == Some(child))
        .unwrap();
    let session = page
        .sessions
        .iter()
        .find(|s| s.key == record.session)
        .unwrap();
    let key = session.arrangement.expect("the session has an arrangement");
    let CommandResult::OkWith(CommandValue::Arrangement(layout)) =
        command(owner, 5, Command::ArrangementGet { arrangement: key }).await
    else {
        panic!("arrangement not found");
    };
    let id = [9; 16];
    let mut document = layout.document;
    document.groups.push(ArrangementGroup {
        locked: false,
        id,
        name: Some("protected child".into()),
        tree: ArrangementNode::Leaf(EntryRef::new([8; 16], EntryTarget::Member(record.key))),
    });
    let replace = Command::ArrangementReplace {
        operation: OperationId::new([1; 16]).unwrap(),
        arrangement: key,
        expected: layout.revision,
        document,
        slot_tabs: false,
    };
    assert!(matches!(
        command(owner, 6, replace).await,
        CommandResult::OkWith(CommandValue::Committed(_))
    ));
    (key, id)
}

#[test]
fn resource_close_refuses_a_locked_descendant_without_closing_the_batch() {
    phux_server_testkit::run_local(async {
        let (server, mut owner) = start(|_| {}).await;
        let (parent, child) = parent_and_session(&mut owner, FOREVER).await;
        let bystander = spawned(&mut owner, 3, sh(FOREVER)).await;
        let page = organization(&mut owner, 4).await;
        let (key, id) = install_child_group(&mut owner, &page, &child).await;
        let lock = |n, locked| Command::ArrangementLock {
            operation: OperationId::new([n; 16]).unwrap(),
            arrangement: key,
            group: id,
            locked,
        };
        assert!(matches!(
            command(&mut owner, 7, lock(2, true)).await,
            CommandResult::OkWith(CommandValue::Committed(_))
        ));

        let batch = [parent.clone(), bystander.clone()];
        let attempts = [
            phux_server_testkit::kill(parent.clone()),
            Command::KillResourceIf {
                terminal_id: parent.clone(),
                precondition: KillPrecondition {
                    instance: None,
                    conditions: KillConditions::NONE,
                },
                operation_id: None,
            },
            phux_server_testkit::kill_all(batch.to_vec()),
            Command::CloseTabResources {
                ids: batch.to_vec(),
            },
        ];
        for (request, attempt) in (8..).zip(attempts) {
            let result = command(&mut owner, request, attempt).await;
            assert_command_error(&result, ErrorCode::PreconditionFailed);
            assert!(matches!(result, CommandResult::Error { message, .. }
                if message.contains("protected child") && message.contains("locked")));
        }
        let activation = parent_activation(&page, &parent);
        let operation = OperationId::new([4; 16]).unwrap();
        let terminate = Command::ResourceTerminate {
            operation,
            target: LiveTarget {
                resource: parent.clone(),
                instance: activation.instance,
                activation: activation.id,
            },
            mode: TerminateMode::Graceful,
        };
        let result = command(&mut owner, 12, terminate.clone()).await;
        assert!(matches!(
            result,
            CommandResult::Refused(Refusal::InvalidArgument { field, reason })
                if field == "group" && reason.contains("protected child") && reason.contains("locked")
        ));

        let unchanged = organization(&mut owner, 13).await;
        let current = parent_activation(&unchanged, &parent);
        assert_eq!(
            (current.instance, current.id),
            (activation.instance, activation.id)
        );
        assert_ne!(
            current.state,
            phux_protocol::wire::frame::ExecState::Stopping
        );
        for id in [&parent, &child, &bystander] {
            assert!(
                unchanged
                    .resources
                    .iter()
                    .any(|r| r.live.as_ref() == Some(id))
            );
        }

        assert!(matches!(
            command(&mut owner, 14, lock(3, false)).await,
            CommandResult::OkWith(CommandValue::Committed(_))
        ));
        assert_eq!(
            command(&mut owner, 15, terminate).await,
            CommandResult::Accepted { operation }
        );
        let seen = closes(&mut owner, &[parent, child.clone()]).await;
        assert_eq!(seen[&child], (CloseReason::ParentClosed, 1));

        drop(owner);
        server.stop().await;
    });
}

fn parent_activation(
    page: &OrganizationPage,
    parent: &ResourceId,
) -> phux_protocol::wire::frame::ActivationInfo {
    page.resources
        .iter()
        .find(|r| r.live.as_ref() == Some(parent))
        .and_then(|r| r.activation)
        .expect("parent activation")
}

/// `KILL_RESOURCES` naming a parent, its child, and a bystander closes each
/// exactly once (the child is not double-closed by the cascade).
#[test]
fn kill_resources_over_a_mixed_set_closes_each_once() {
    phux_server_testkit::run_local(async {
        let (server, mut owner) = start(|_| {}).await;
        let (parent, session) = parent_and_session(&mut owner, FOREVER).await;
        let unrelated = spawned(&mut owner, 3, sh(FOREVER)).await;
        // The request lists the child first. The plan must still reap and
        // announce it before its parent can discard its record.
        let targets = [session.clone(), parent.clone(), unrelated];
        let kill = phux_server_testkit::kill_all(targets.to_vec());
        assert_eq!(command(&mut owner, 4, kill).await, CommandResult::Ok);
        let mut seen = HashMap::new();
        for _ in &targets {
            let (id, reason) = wait_frame(&mut owner, "batch close", |frame| match frame {
                FrameKind::ResourceClosed {
                    terminal_id,
                    reason,
                    ..
                } => Some((terminal_id, reason)),
                _ => None,
            })
            .await;
            if id == parent {
                assert!(
                    seen.contains_key(&session),
                    "child must close before parent"
                );
            }
            assert!(seen.insert(id, (reason, 1)).is_none(), "duplicate close");
        }
        assert_eq!(seen[&session].0, CloseReason::Killed);
        // Everything the cascade emitted is now queued ahead of this reply,
        // so a duplicate close would be counted before it.
        phux_server_testkit::send_command(&mut owner, 5, phux_server_testkit::get_state()).await;
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
