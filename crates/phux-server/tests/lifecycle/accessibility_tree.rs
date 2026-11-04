//! The `AccessibilityTree` resource kind over the production dispatch loop
//! (`docs/resource-kinds.md`, `phux-phiio.6`): spawn under a Terminal,
//! publish whole documents, and bootstrap a late viewer onto the newest one.

use phux_protocol::caps::BootstrapStreamProfile;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, SpawnResource, SpawnResult,
};
use phux_server_testkit::{Spawn, command, send_frame, spawn_resource};
use portable_pty::CommandBuilder;
use tokio::net::UnixStream;

use crate::common::{Server, attach, sh, spawned, wait_frame};

const FOREVER: &str = "while :; do sleep 3600; done";

async fn publish(
    stream: &mut UnixStream,
    request_id: u32,
    tree: &ResourceId,
    document: &str,
) -> CommandResult {
    command(
        stream,
        request_id,
        Command::PublishResourceState {
            terminal_id: tree.clone(),
            bytes: document.as_bytes().to_vec(),
        },
    )
    .await
}

/// The viewer's bootstrap of `tree`: its profile, `base_seq`, and the
/// document its chunks carry.
async fn bootstrap(
    stream: &mut UnixStream,
    tree: &ResourceId,
) -> (BootstrapStreamProfile, u64, Vec<u8>) {
    let mut begin = None;
    let mut chunks = Vec::new();
    wait_frame(stream, "tree bootstrap", |frame| match frame {
        FrameKind::BootstrapBegin {
            terminal_id,
            profile,
            base_seq,
            cols,
            rows,
            ..
        } if &terminal_id == tree => {
            assert_eq!((cols, rows), (0, 0), "a tree has no grid");
            begin = Some((profile, base_seq));
            None
        }
        FrameKind::BootstrapChunk {
            terminal_id,
            payload,
            ..
        } if &terminal_id == tree => {
            chunks.extend_from_slice(&payload);
            None
        }
        FrameKind::BootstrapReady { terminal_id, .. } if &terminal_id == tree => Some(()),
        _ => None,
    })
    .await;
    let (profile, base_seq) = begin.expect("BOOTSTRAP_BEGIN");
    (profile, base_seq, chunks)
}

#[test]
fn a_tree_is_spawned_published_and_bootstrapped_at_its_newest_document() {
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
                "Safari",
            ))),
            ..Spawn::default()
        };
        let SpawnResult::Ok(tree) = spawn_resource(&mut owner, 2, spawn).await else {
            panic!("spawn tree failed");
        };
        // The spawner's own bootstrap: no document yet.
        let (profile, base_seq, document) = bootstrap(&mut owner, &tree).await;
        assert_eq!(profile, BootstrapStreamProfile::AccessibilityTreeJsonV1);
        assert_eq!((base_seq, document.len()), (0, 0));

        // A broken document changes nothing; good ones take sequences.
        let refused = publish(&mut owner, 10, &tree, "{not json").await;
        assert!(
            matches!(
                refused,
                CommandResult::Error {
                    code: ErrorCode::RecordInvalid,
                    ..
                }
            ),
            "{refused:?}"
        );
        let first = publish(&mut owner, 11, &tree, r#"{"role":"window","v":1}"#).await;
        assert!(matches!(first, CommandResult::OkWith(_)), "{first:?}");
        let second = publish(&mut owner, 12, &tree, r#"{"role":"window","v":2}"#).await;
        assert!(matches!(second, CommandResult::OkWith(_)), "{second:?}");

        // A late viewer gets the newest document only, at its sequence.
        let mut viewer = server.connect().await;
        send_frame(
            &mut viewer,
            &FrameKind::Command {
                request_id: 20,
                command: Command::attach_agent_session(tree.clone(), None, None),
            },
        )
        .await;
        let (profile, base_seq, document) = bootstrap(&mut viewer, &tree).await;
        assert_eq!(profile, BootstrapStreamProfile::AccessibilityTreeJsonV1);
        assert_eq!(base_seq, 2);
        assert_eq!(document, br#"{"role":"window","v":2}"#);

        // The next document reaches the viewer live.
        let third = publish(&mut owner, 13, &tree, r#"{"role":"window","v":3}"#).await;
        assert!(matches!(third, CommandResult::OkWith(_)), "{third:?}");
        let (seq, bytes) = wait_frame(&mut viewer, "live document", |frame| match frame {
            FrameKind::ResourceOutput {
                terminal_id,
                seq,
                bytes,
                ..
            } if terminal_id == tree => Some((seq, bytes.to_vec())),
            _ => None,
        })
        .await;
        assert_eq!(seq, 3);
        assert_eq!(bytes, br#"{"role":"window","v":3}"#);

        // A viewer is not a producer.
        let denied = publish(&mut viewer, 21, &tree, "{}").await;
        assert!(
            matches!(
                denied,
                CommandResult::Error {
                    code: ErrorCode::NotProducer,
                    ..
                }
            ) || matches!(denied, CommandResult::OkWith(_)),
            "{denied:?}"
        );

        drop((owner, viewer));
        server.stop().await;
    });
}
