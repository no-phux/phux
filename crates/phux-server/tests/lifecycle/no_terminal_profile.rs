//! A client that offers no Terminal bootstrap profile at `HELLO` is admitted
//! (`docs/resource-kinds.md`, `phux-phiio.3`): it consumes other kinds, and a
//! Terminal stream is refused when it binds, with the connection kept.

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapProfileSet, ClientCapabilities, LayerSet,
};
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, SpawnResource, SpawnResult,
};
use phux_server::LaunchCommand as CommandBuilder;
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, Spawn, attach_by_name, command, recv_typed, send_frame,
    spawn_resource, wait_for_raw_socket,
};
use tokio::net::UnixStream;

use crate::common::{Server, attach, sh, spawned, wait_frame};

const FOREVER: &str = "while :; do sleep 3600; done";

/// A connection whose `HELLO` withholds every Terminal profile; returns it
/// once `HELLO_OK` has confirmed no profile was selected.
async fn connect_without_terminal_profile(server: &Server) -> UnixStream {
    let mut viewer = wait_for_raw_socket(&server.socket, SOCKET_CONNECT_DEADLINE).await;
    send_frame(
        &mut viewer,
        &FrameKind::Hello {
            client_name: "no-terminal-profile".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::new()
                .with_layers(LayerSet::all())
                .with_bootstrap(
                    BootstrapCapabilities::new().with_profiles(BootstrapProfileSet::new()),
                ),
        },
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

/// A parent Terminal and an agent session under it, spawned by `owner`.
async fn parent_and_session(owner: &mut UnixStream) -> (ResourceId, ResourceId) {
    let parent = spawned(owner, 1, sh(FOREVER)).await;
    let spawn = Spawn {
        resource: Some(Box::new(SpawnResource::agent_session(
            parent.clone(),
            "claude",
        ))),
        ..Spawn::default()
    };
    let SpawnResult::Ok(session) = spawn_resource(owner, 2, spawn).await else {
        panic!("spawn session failed");
    };
    (parent, session)
}

#[test]
fn a_client_with_no_terminal_profile_is_admitted_and_refused_only_at_a_terminal_bind() {
    phux_server_testkit::run_local(async {
        let server = Server::start(Some("demo"), |cfg| {
            let mut seed = CommandBuilder::new("/bin/sh");
            seed.args(["-c", FOREVER]);
            phux_server_testkit::seed_pty(cfg, seed);
        });
        let mut owner = server.connect().await;
        attach(&mut owner, "demo").await;
        let (parent, session) = parent_and_session(&mut owner).await;
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
        assert!(
            matches!(
                result,
                CommandResult::Error {
                    code: ErrorCode::CodecUnavailable,
                    ..
                }
            ),
            "{result:?}"
        );

        // The agent session's stream needs no Terminal profile.
        send_frame(
            &mut viewer,
            &FrameKind::Command {
                request_id: 11,
                command: Command::attach_agent_session(session.clone(), None, None),
            },
        )
        .await;
        wait_frame(&mut viewer, "agent bootstrap READY", |frame| match frame {
            FrameKind::BootstrapReady { terminal_id, .. } if terminal_id == session => Some(()),
            _ => None,
        })
        .await;
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
        wait_frame(&mut viewer, "agent record", |frame| match frame {
            FrameKind::ResourceOutput { terminal_id, .. } if terminal_id == session => Some(()),
            _ => None,
        })
        .await;

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
        let server = Server::start(Some("demo"), |cfg| {
            let mut seed = CommandBuilder::new("/bin/sh");
            seed.args(["-c", FOREVER]);
            phux_server_testkit::seed_pty(cfg, seed);
        });
        let mut owner = server.connect().await;
        attach(&mut owner, "demo").await;
        let parent = spawned(&mut owner, 1, sh(FOREVER)).await;
        let mut viewer = connect_without_terminal_profile(&server).await;

        let children = [
            SpawnResource::agent_session(parent.clone(), "claude"),
            SpawnResource::document(parent.clone(), "text/plain", None),
        ];
        for (child, request_id) in children.into_iter().zip(20..) {
            let spawn = Spawn {
                resource: Some(Box::new(child.clone())),
                ..Spawn::default()
            };
            let SpawnResult::Ok(id) = spawn_resource(&mut viewer, request_id, spawn).await else {
                panic!("{:?} spawn refused without a Terminal profile", child.kind);
            };
            wait_frame(&mut viewer, "the spawned stream", |frame| match frame {
                FrameKind::BootstrapReady { terminal_id, .. } if terminal_id == id => Some(()),
                _ => None,
            })
            .await;
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
