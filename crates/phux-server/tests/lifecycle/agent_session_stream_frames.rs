//! Input and stream frames on an `AgentSession` (ADR-0103 §4): each earns
//! its exact uncorrelated refusal, and the connection survives every one.

use phux_protocol::ids::{BootstrapId, StreamId};
use phux_protocol::input::key::PhysicalKey;
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, SpawnResource, SpawnResult,
};
use phux_server_testkit::{Spawn, ascii_key, command, send_frame, spawn_resource};
use portable_pty::CommandBuilder;
use tokio::net::UnixStream;

use crate::common::{Server, attach, sh, spawned, wait_frame};

const FOREVER: &str = "while :; do sleep 3600; done";
const WRONG_KIND: &str = "resource is agent-session, operation requires terminal";

/// The next uncorrelated `ERROR`, as `(code, message)`.
async fn next_error(stream: &mut UnixStream, what: &str) -> (ErrorCode, String) {
    wait_frame(stream, what, |frame| match frame {
        FrameKind::Error {
            request_id: None,
            code,
            message,
        } => Some((code, message)),
        _ => None,
    })
    .await
}

/// Each frame paired with the refusal it has always earned on a session.
fn cases(session: &phux_protocol::ids::ResourceId) -> [(FrameKind, ErrorCode, &'static str); 4] {
    let stream_id = StreamId::new(1).unwrap();
    let bootstrap_id = BootstrapId::new(1).unwrap();
    [
        (
            FrameKind::FrameAck {
                terminal_id: session.clone(),
                stream_id,
                bootstrap_id,
                seq: 1,
            },
            ErrorCode::MalformedMessage,
            "FRAME_ACK is not valid on an agent-session stream",
        ),
        (
            FrameKind::HistoryRequest {
                terminal_id: session.clone(),
                stream_id,
                bootstrap_id,
                cursor: bytes::Bytes::new(),
                max_bytes: 1024,
                max_rows: 10,
            },
            ErrorCode::WrongResourceKind,
            "an agent-session stream retains no history beyond its bootstrap",
        ),
        (
            FrameKind::ResizeTerminal {
                terminal_id: session.clone(),
                cols: 100,
                rows: 30,
                cell_px: None,
            },
            ErrorCode::WrongResourceKind,
            WRONG_KIND,
        ),
        (
            FrameKind::InputKey {
                terminal_id: session.clone(),
                event: ascii_key('a', PhysicalKey::A),
            },
            ErrorCode::WrongResourceKind,
            WRONG_KIND,
        ),
    ]
}

#[test]
fn stream_frames_on_an_agent_session_earn_exact_refusals() {
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
            resource: Some(Box::new(SpawnResource::agent_session(parent, "claude"))),
            ..Spawn::default()
        };
        let SpawnResult::Ok(session) = spawn_resource(&mut owner, 2, spawn).await else {
            panic!("spawn session failed");
        };

        for (frame, code, message) in cases(&session) {
            send_frame(&mut owner, &frame).await;
            let refusal = next_error(&mut owner, message).await;
            assert_eq!(refusal, (code, message.to_owned()), "{frame:?}");
        }
        let screen = Command::GetScreen {
            terminal_id: session,
            request_scrollback: None,
            cells: false,
            format: 0,
        };
        let result = command(&mut owner, 3, screen).await;
        assert!(
            matches!(
                result,
                CommandResult::Error {
                    code: ErrorCode::WrongResourceKind,
                    ..
                }
            ),
            "the connection survives: {result:?}"
        );

        drop(owner);
        server.stop().await;
    });
}
