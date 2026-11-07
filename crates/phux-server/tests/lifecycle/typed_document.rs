//! The `Document` resource kind over the production dispatch loop
//! (`docs/resource-kinds.md`): open one with a content type under a
//! Terminal, publish whole documents, and bootstrap a late viewer onto the
//! newest with the content type ahead of it.

use phux_protocol::caps::BootstrapStreamProfile;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::typed_document::split_bootstrap;
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, SpawnResource, SpawnResult,
};
use phux_server::LaunchCommand as CommandBuilder;
use phux_server_testkit::{Spawn, command, send_frame, spawn_resource};
use tokio::net::UnixStream;

use crate::common::{Server, attach, sh, spawned, wait_frame};

const FOREVER: &str = "while :; do sleep 3600; done";

async fn publish(
    stream: &mut UnixStream,
    request_id: u32,
    doc: &ResourceId,
    bytes: &[u8],
) -> CommandResult {
    command(
        stream,
        request_id,
        Command::PublishResourceState {
            terminal_id: doc.clone(),
            bytes: bytes.to_vec(),
        },
    )
    .await
}

async fn open(
    stream: &mut UnixStream,
    request_id: u32,
    parent: &ResourceId,
    content_type: &str,
) -> ResourceId {
    let spawn = Spawn {
        resource: Some(Box::new(SpawnResource::document(
            parent.clone(),
            content_type,
            Some("notes".to_owned()),
        ))),
        ..Spawn::default()
    };
    let SpawnResult::Ok(doc) = spawn_resource(stream, request_id, spawn).await else {
        panic!("spawn document failed");
    };
    doc
}

/// The viewer's bootstrap of `doc`: profile, `base_seq`, content type and
/// document.
async fn bootstrap(
    stream: &mut UnixStream,
    doc: &ResourceId,
) -> (BootstrapStreamProfile, u64, String, Vec<u8>) {
    let mut begin = None;
    let mut chunks = Vec::new();
    wait_frame(stream, "document bootstrap", |frame| match frame {
        FrameKind::BootstrapBegin {
            terminal_id,
            profile,
            base_seq,
            ..
        } if &terminal_id == doc => {
            begin = Some((profile, base_seq));
            None
        }
        FrameKind::BootstrapChunk {
            terminal_id,
            payload,
            ..
        } if &terminal_id == doc => {
            chunks.extend_from_slice(&payload);
            None
        }
        FrameKind::BootstrapReady { terminal_id, .. } if &terminal_id == doc => Some(()),
        _ => None,
    })
    .await;
    let (profile, base_seq) = begin.expect("BOOTSTRAP_BEGIN");
    let (content_type, document) = split_bootstrap(&chunks).expect("a typed bootstrap");
    (
        profile,
        base_seq,
        content_type.to_owned(),
        document.to_vec(),
    )
}

#[test]
fn a_typed_document_carries_its_content_type_to_a_late_viewer() {
    phux_server_testkit::run_local(async {
        let server = Server::start(Some("demo"), |cfg| {
            let mut seed = CommandBuilder::new("/bin/sh");
            seed.args(["-c", FOREVER]);
            phux_server_testkit::seed_pty(cfg, seed);
        });
        let mut owner = server.connect().await;
        attach(&mut owner, "demo").await;
        let parent = spawned(&mut owner, 1, sh(FOREVER)).await;
        let doc = open(&mut owner, 2, &parent, "text/markdown").await;
        let opened = bootstrap(&mut owner, &doc).await;
        assert_eq!(
            opened,
            (
                BootstrapStreamProfile::DocumentTypedV1,
                0,
                "text/markdown".to_owned(),
                Vec::new()
            )
        );
        for (request_id, text) in [(10, "# one"), (11, "# two")] {
            let taken = publish(&mut owner, request_id, &doc, text.as_bytes()).await;
            assert!(matches!(taken, CommandResult::OkWith(_)), "{taken:?}");
        }
        // Markdown is text: bytes that are not UTF-8 are refused.
        let refused = publish(&mut owner, 12, &doc, b"\xff\xfe").await;
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

        let mut viewer = server.connect().await;
        send_frame(
            &mut viewer,
            &FrameKind::Command {
                request_id: 20,
                command: Command::attach_terminal(doc.clone(), None),
            },
        )
        .await;
        let late = bootstrap(&mut viewer, &doc).await;
        assert_eq!(
            late,
            (
                BootstrapStreamProfile::DocumentTypedV1,
                2,
                "text/markdown".to_owned(),
                b"# two".to_vec()
            )
        );
        let taken = publish(&mut owner, 13, &doc, b"# three").await;
        assert!(matches!(taken, CommandResult::OkWith(_)), "{taken:?}");
        let live = wait_frame(&mut viewer, "live document", |frame| match frame {
            FrameKind::ResourceOutput {
                terminal_id,
                seq,
                bytes,
                ..
            } if terminal_id == doc => Some((seq, bytes.to_vec())),
            _ => None,
        })
        .await;
        assert_eq!(live, (3, b"# three".to_vec()));

        // A JSON document must parse.
        let json = open(&mut owner, 3, &parent, "application/json").await;
        let _ = bootstrap(&mut owner, &json).await;
        let refused = publish(&mut owner, 30, &json, b"{not json").await;
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
        let taken = publish(&mut owner, 31, &json, b"{\"ok\":true}").await;
        assert!(matches!(taken, CommandResult::OkWith(_)), "{taken:?}");

        drop((owner, viewer));
        server.stop().await;
    });
}
