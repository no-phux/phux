//! A client that offers no Terminal bootstrap profile at `HELLO` is admitted
//! (`docs/resource-kinds.md`, `phux-phiio.3`): it consumes other kinds, and a
//! Terminal stream is refused when it binds, with the connection kept.

use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapProfileSet, ClientCapabilities, LayerSet,
};
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, FrameKind, SpawnResource};
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, Spawn, assert_command_error, attach_by_name, command, recv_typed,
    send_frame, wait_for_raw_socket,
};
use tokio::net::UnixStream;

use crate::common::{Server, bootstrap_ready, demo_parent, next_output, send_command, spawn_kind};

/// A connection whose `HELLO` withholds every Terminal profile; returns it
/// once `HELLO_OK` has confirmed no profile was selected.
async fn connect_without_terminal_profile(server: &Server) -> UnixStream {
    let mut viewer = wait_for_raw_socket(&server.socket, SOCKET_CONNECT_DEADLINE).await;
    send_frame(
        &mut viewer,
        &phux_server_testkit::hello(
            "no-terminal-profile",
            ClientCapabilities::new()
                .with_layers(LayerSet::all())
                .with_bootstrap(
                    BootstrapCapabilities::new().with_profiles(BootstrapProfileSet::new()),
                ),
        ),
    )
    .await;
    let (_, hello_ok) = recv_typed(&mut viewer).await;
    let FrameKind::HelloOk {
        selected_profile, ..
    } = hello_ok
    else {
        panic!("expected HELLO_OK, got {hello_ok:?}");
    };
    assert_eq!(
        selected_profile, None,
        "admitted without a Terminal profile"
    );
    viewer
}

#[test]
fn a_client_with_no_terminal_profile_is_admitted_and_refused_only_at_a_terminal_bind() {
    phux_server_testkit::run_local(async {
        let (server, mut owner, parent) = demo_parent().await;
        let agent = SpawnResource::agent_session(parent.clone(), "claude");
        let session = spawn_kind(&mut owner, 2, agent).await;
        let mut viewer = connect_without_terminal_profile(&server).await;

        // A session ATTACH is a Terminal stream: refused, connection kept.
        send_frame(&mut viewer, &attach_by_name("demo")).await;
        let (_, refusal) = recv_typed(&mut viewer).await;
        assert!(
            matches!(
                &refusal,
                FrameKind::Error {
                    code: ErrorCode::CodecUnavailable,
                    ..
                }
            ),
            "{refusal:?}"
        );
        send_frame(&mut viewer, &FrameKind::Ping { nonce: 7 }).await;
        assert!(matches!(
            recv_typed(&mut viewer).await.1,
            FrameKind::Pong { nonce: 7 }
        ));

        // ATTACH_RESOURCE on a Terminal: refused in the command result.
        let result = command(&mut viewer, 10, Command::attach_terminal(parent, None)).await;
        assert_command_error(&result, ErrorCode::CodecUnavailable);

        // The agent session's stream needs no Terminal profile.
        let attach = Command::attach_agent_session(session.clone(), None, None);
        send_command(&mut viewer, 11, attach).await;
        bootstrap_ready(&mut viewer, &session).await;
        let result = command(
            &mut owner,
            3,
            Command::AppendResourceOutput {
                terminal_id: session.clone(),
                bytes: b"{\"type\":\"tool_end\"}\n".to_vec(),
            },
        )
        .await;
        assert!(matches!(result, CommandResult::OkWith(_)), "{result:?}");
        next_output(&mut viewer, &session).await;

        drop((owner, viewer));
        server.stop().await;
    });
}

/// A connection with no Terminal profile spawns, and is streamed, the kinds
/// whose stream needs none; only a Terminal spawn is refused, with the
/// connection kept.
#[test]
fn a_client_with_no_terminal_profile_spawns_every_kind_but_a_terminal() {
    phux_server_testkit::run_local(async {
        let (server, owner, parent) = demo_parent().await;
        let mut viewer = connect_without_terminal_profile(&server).await;

        let children = [
            SpawnResource::agent_session(parent.clone(), "claude"),
            SpawnResource::document(parent.clone(), "text/plain", None),
        ];
        for (child, request_id) in children.into_iter().zip(20..) {
            let id = spawn_kind(&mut viewer, request_id, child).await;
            bootstrap_ready(&mut viewer, &id).await;
        }

        send_frame(&mut viewer, &Spawn::default().frame(30)).await;
        let (_, refusal) = recv_typed(&mut viewer).await;
        assert!(
            matches!(
                &refusal,
                FrameKind::Error {
                    request_id: Some(30),
                    code: ErrorCode::CodecUnavailable,
                    ..
                }
            ),
            "{refusal:?}"
        );
        send_frame(&mut viewer, &FrameKind::Ping { nonce: 8 }).await;
        assert!(matches!(
            recv_typed(&mut viewer).await.1,
            FrameKind::Pong { nonce: 8 }
        ));

        drop((owner, viewer));
        server.stop().await;
    });
}
