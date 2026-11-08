//! A satellite's document resources relay through the hub: the link
//! negotiated a terminal profile, but a tree's stream passes regardless.

use super::*;
use phux_protocol::caps::BootstrapStreamProfile;

#[test]
fn a_satellite_tree_streams_through_the_hub() {
    phux_server_testkit::run_local(async {
        let fed = Fed::boot().await;
        let mut sat = wait_for_socket(&fed.tmp.path().join("sat.sock"), STEP_DEADLINE).await;
        let spawn = Spawn {
            resource: Some(Box::new(SpawnResource::accessibility_tree(
                ResourceId::local(fed.seed),
                "Smoke",
            ))),
            ..Spawn::default()
        };
        let SpawnResult::Ok(ResourceId::Local { id: tree }) =
            spawn_resource(&mut sat, 1, spawn).await
        else {
            panic!("spawn tree on the satellite failed");
        };
        let document = br#"{"role":"window","title":"Smoke"}"#.to_vec();
        let published = phux_server_testkit::command(
            &mut sat,
            2,
            Command::PublishResourceState {
                terminal_id: ResourceId::local(tree),
                bytes: document.clone(),
            },
        )
        .await;
        assert!(
            matches!(published, CommandResult::OkWith(_)),
            "publish on the satellite: {published:?}"
        );

        let mut hub = fed.linked_hub().await;
        let sat_tree = ResourceId::satellite("sat", tree);
        let frames = attach_terminal_until_ok(&mut hub, &sat_tree).await;
        let mut frames = frames.into_iter();
        let profile = frames.by_ref().find_map(|frame| match frame {
            FrameKind::BootstrapBegin {
                terminal_id,
                profile,
                ..
            } if terminal_id == sat_tree => Some(profile),
            _ => None,
        });
        assert_eq!(
            profile,
            Some(BootstrapStreamProfile::AccessibilityTreeJsonV1),
            "the tree's own profile reached the hub's client"
        );
        let mut received = Vec::new();
        for frame in frames {
            if let FrameKind::BootstrapChunk {
                terminal_id,
                payload,
                ..
            } = frame
                && terminal_id == sat_tree
            {
                received.extend_from_slice(&payload);
            }
        }
        if received.is_empty() {
            let deadline = Instant::now() + STEP_DEADLINE;
            while received.is_empty() && Instant::now() < deadline {
                if let FrameKind::BootstrapChunk {
                    terminal_id,
                    payload,
                    ..
                } = recv_typed(&mut hub).await.1
                    && terminal_id == sat_tree
                {
                    received.extend_from_slice(&payload);
                }
            }
        }
        assert_eq!(received, document, "the newest document crossed both hops");

        drop(hub);
        fed.shutdown().await;
    });
}

/// `SEND_DOCUMENT_INPUT` to a satellite document relays through the hub to
/// the satellite producer, and the classes a sender stamps cross the link:
/// the satellite, which knows the document's class, refuses one outside
/// them (`docs/resource-kinds.md#authority`).
#[test]
fn input_to_a_satellite_document_reaches_its_producer_through_the_hub() {
    use phux_protocol::wire::frame::{ControlClass, ControlClasses, DocumentInput};
    phux_server_testkit::run_local(async {
        let fed = Fed::boot().await;
        let mut sat = wait_for_socket(&fed.tmp.path().join("sat.sock"), STEP_DEADLINE).await;
        let surface = SpawnResource::pixel_surface(ResourceId::local(fed.seed), "Smoke")
            .with_control(Some(ControlClass::Window));
        let spawn = Spawn {
            resource: Some(Box::new(surface)),
            ..Spawn::default()
        };
        let SpawnResult::Ok(ResourceId::Local { id }) = spawn_resource(&mut sat, 1, spawn).await
        else {
            panic!("spawn surface on the satellite failed");
        };
        let mut hub = fed.linked_hub().await;
        let send = |control| Command::SendDocumentInput {
            terminal_id: ResourceId::satellite("sat", id),
            intent: DocumentInput::Click { x: 3, y: 4 },
            control,
        };
        let display = Some(ControlClasses::EMPTY.with(ControlClass::Display));
        let refused = phux_server_testkit::command(&mut hub, 2, send(display)).await;
        assert!(
            matches!(
                refused,
                CommandResult::Error {
                    code: ErrorCode::PermissionDenied,
                    ..
                }
            ),
            "{refused:?}"
        );
        let taken = phux_server_testkit::command(&mut hub, 3, send(None)).await;
        assert!(matches!(taken, CommandResult::Ok), "{taken:?}");
        let deadline = Instant::now() + STEP_DEADLINE;
        let mut got = None;
        while got.is_none() && Instant::now() < deadline {
            if let FrameKind::Event {
                terminal: Some(ResourceId::Local { id: at }),
                event: AgentEvent::DocumentInput { intent },
                ..
            } = recv_typed(&mut sat).await.1
                && at == id
            {
                got = Some(intent);
            }
        }
        assert_eq!(got, Some(DocumentInput::Click { x: 3, y: 4 }));

        drop(hub);
        fed.shutdown().await;
    });
}
