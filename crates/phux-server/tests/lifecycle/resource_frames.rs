//! Reply-less frame admission through `handle_client`, not just app handlers.

use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, SpawnResource, SpawnResult,
};
use phux_server_testkit::{Spawn, ascii_key, command, send_frame, spawn_resource};
use phux_terminal_api::launch::LaunchCommand as CommandBuilder;
use tokio::net::UnixStream;

use crate::common::{Server, attach, sh, spawned, wait_frame};

const FOREVER: &str = "while :; do sleep 3600; done";

fn frames(resource: &ResourceId) -> Vec<FrameKind> {
    vec![
        FrameKind::InputKey {
            terminal_id: resource.clone(),
            event: ascii_key('a', PhysicalKey::A),
        },
        FrameKind::InputMouse {
            terminal_id: resource.clone(),
            event: MouseEvent {
                action: MouseAction::Motion,
                button: MouseButton::Unknown,
                mods: ModSet::empty(),
                x: 0.0,
                y: 0.0,
            },
        },
        FrameKind::InputFocus {
            terminal_id: resource.clone(),
            event: FocusEvent::Lost,
        },
        FrameKind::InputPaste {
            terminal_id: resource.clone(),
            event: PasteEvent {
                trust: PasteTrust::Trusted,
                data: b"x".to_vec(),
            },
        },
        FrameKind::InputTerminalReply {
            terminal_id: resource.clone(),
            bytes: bytes::Bytes::from_static(b"reply"),
        },
        FrameKind::ResizeTerminal {
            terminal_id: resource.clone(),
            cols: 80,
            rows: 24,
            cell_px: None,
        },
        FrameKind::FrameAck {
            terminal_id: resource.clone(),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq: 1,
        },
        FrameKind::HistoryRequest {
            terminal_id: resource.clone(),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            cursor: bytes::Bytes::from_static(b"cursor"),
            max_bytes: 1024,
            max_rows: 10,
        },
    ]
}

/// PONG fences dispatch, proving silence without a timing-based negative read.
async fn assert_error(stream: &mut UnixStream, frame: &FrameKind, expected: Option<ErrorCode>) {
    send_frame(stream, frame).await;
    send_frame(stream, &FrameKind::Ping { nonce: 9876 }).await;
    let mut errors = Vec::new();
    wait_frame(
        stream,
        "frame refusal followed by PONG",
        |frame| match frame {
            FrameKind::Error {
                request_id, code, ..
            } => {
                assert_eq!(request_id, None);
                errors.push(code);
                None
            }
            FrameKind::Pong { nonce: 9876 } => Some(()),
            _ => None,
        },
    )
    .await;
    assert_eq!(
        errors,
        expected.into_iter().collect::<Vec<_>>(),
        "{frame:?}"
    );
}

#[test]
fn agent_session_stream_frames_keep_subscription_and_refusal_order() {
    phux_server_testkit::run_local(async {
        let server = start();
        let mut owner = server.connect().await;
        attach(&mut owner, "demo").await;
        let parent = spawned(&mut owner, 1, sh(FOREVER)).await;
        let session = spawn(&mut owner, SpawnResource::agent_session(parent, "claude")).await;
        let mut outsider = server.connect().await;
        let wrong = Some(ErrorCode::WrongResourceKind);
        let malformed = Some(ErrorCode::MalformedMessage);
        let owner_errors = [wrong, wrong, wrong, wrong, wrong, wrong, malformed, wrong];
        let outsider_errors = [None, None, None, None, None, wrong, malformed, wrong];
        for ((frame, owner_error), outsider_error) in frames(&session)
            .iter()
            .zip(owner_errors)
            .zip(outsider_errors)
        {
            assert_error(&mut owner, frame, owner_error).await;
            assert_error(&mut outsider, frame, outsider_error).await;
        }
        drop((owner, outsider));
        server.stop().await;
    });
}

#[test]
fn document_stream_frames_keep_their_existing_wrong_kind_refusals() {
    phux_server_testkit::run_local(async {
        let server = start();
        let mut owner = server.connect().await;
        attach(&mut owner, "demo").await;
        let parent = spawned(&mut owner, 1, sh(FOREVER)).await;
        let mut outsider = server.connect().await;
        for request in [
            SpawnResource::accessibility_tree(parent.clone(), "window"),
            SpawnResource::pixel_surface(parent, "window"),
        ] {
            let document = spawn(&mut owner, request).await;
            for frame in frames(&document) {
                assert_error(&mut owner, &frame, Some(ErrorCode::WrongResourceKind)).await;
                assert_error(&mut outsider, &frame, Some(ErrorCode::WrongResourceKind)).await;
            }
        }
        drop((owner, outsider));
        server.stop().await;
    });
}

#[test]
fn unknown_history_keeps_its_codec_first_tombstone_without_a_default_app() {
    phux_server_testkit::run_local(async {
        let server = start();
        let mut owner = server.connect().await;
        let unknown = ResourceId::local(u32::MAX);
        assert_eq!(
            command(
                &mut owner,
                90,
                Command::PublishResourceState {
                    terminal_id: unknown.clone(),
                    bytes: b"{}".to_vec(),
                }
            )
            .await,
            CommandResult::Error {
                code: ErrorCode::InvalidCommand,
                message: "command not supported by this server".to_owned(),
            }
        );
        for frame in frames(&unknown).into_iter().take(7) {
            // The default test client advertised terminal replies.
            assert_error(&mut owner, &frame, None).await;
        }
        let history = frames(&unknown).pop().unwrap();
        send_frame(&mut owner, &history).await;
        wait_frame(
            &mut owner,
            "unknown history tombstone",
            |frame| match frame {
                FrameKind::HistoryTombstone {
                    terminal_id,
                    reason,
                    cursor,
                    ..
                } => {
                    assert_eq!(terminal_id, unknown);
                    assert_eq!(cursor.as_ref(), b"cursor");
                    assert_eq!(
                        reason,
                        phux_protocol::wire::frame::HistoryTombstoneReason::CodecFailure
                    );
                    Some(())
                }
                _ => None,
            },
        )
        .await;
        drop(owner);
        server.stop().await;
    });
}

fn start() -> Server {
    Server::start(Some("demo"), |cfg| {
        let mut seed = CommandBuilder::new("/bin/sh");
        seed.args(["-c", FOREVER]);
        phux_server_testkit::seed_pty(cfg, seed);
    })
}

async fn spawn(owner: &mut UnixStream, request: SpawnResource) -> ResourceId {
    let SpawnResult::Ok(resource) = spawn_resource(
        owner,
        2,
        Spawn {
            resource: Some(Box::new(request)),
            ..Spawn::default()
        },
    )
    .await
    else {
        panic!("spawn failed");
    };
    resource
}
