//! A document reports whether its producer is connected
//! (`docs/resource-kinds.md#supervision`): connected while the opening
//! connection lives, gone after it disconnects, and the document and its
//! newest frame stay readable.

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{Command, CommandResult, FrameKind, SpawnResource, SpawnResult};
use phux_protocol::wire::info::SessionSnapshot;
use phux_server::LaunchCommand as CommandBuilder;
use phux_server_testkit::{Spawn, command, send_frame, spawn_resource};

use crate::common::{Server, attach, find, sh, spawned, wait_for_state, wait_frame};

const FOREVER: &str = "while :; do sleep 3600; done";
const DOC: &[u8] = br#"{"role":"window"}"#;

fn producer(snapshot: &SessionSnapshot, doc: &ResourceId) -> Option<bool> {
    find(snapshot, doc).and_then(|info| info.producer_connected)
}

#[test]
fn a_document_reports_its_producer_connected_then_gone() {
    phux_server_testkit::run_local(async {
        let server = Server::start(Some("demo"), |cfg| {
            let mut seed = CommandBuilder::new("/bin/sh");
            seed.args(["-c", FOREVER]);
            phux_server_testkit::seed_pty(cfg, seed);
        });
        let mut owner = server.connect().await;
        attach(&mut owner, "demo").await;
        let parent = spawned(&mut owner, 1, sh(FOREVER)).await;
        let spawn = Spawn {
            resource: Some(Box::new(SpawnResource::accessibility_tree(
                parent.clone(),
                "app:Test",
            ))),
            ..Spawn::default()
        };
        let SpawnResult::Ok(tree) = spawn_resource(&mut owner, 2, spawn).await else {
            panic!("spawn tree failed");
        };
        let publish = Command::PublishResourceState {
            terminal_id: tree.clone(),
            bytes: DOC.to_vec(),
        };
        let taken = command(&mut owner, 3, publish).await;
        assert!(matches!(taken, CommandResult::OkWith(_)), "{taken:?}");

        let mut viewer = server.connect().await;
        let up = wait_for_state(&mut viewer, 10, "producer connected", |s| {
            producer(s, &tree) == Some(true)
        })
        .await;
        assert_eq!(find(&up, &parent).and_then(|i| i.producer_connected), None);

        drop(owner);
        wait_for_state(&mut viewer, 100, "producer gone", |s| {
            producer(s, &tree) == Some(false)
        })
        .await;

        send_frame(
            &mut viewer,
            &FrameKind::Command {
                request_id: 500,
                command: Command::attach_terminal(tree.clone(), None),
            },
        )
        .await;
        let mut chunks = Vec::new();
        wait_frame(&mut viewer, "tree bootstrap", |frame| match frame {
            FrameKind::BootstrapChunk {
                terminal_id,
                payload,
                ..
            } if terminal_id == tree => {
                chunks.extend_from_slice(&payload);
                None
            }
            FrameKind::BootstrapReady { terminal_id, .. } if terminal_id == tree => Some(()),
            _ => None,
        })
        .await;
        assert_eq!(chunks, DOC, "the last frame outlives its producer");

        drop(viewer);
        server.stop().await;
    });
}
