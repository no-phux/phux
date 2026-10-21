//! New native onboarding requests travel through the authenticated wire loop.
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, LayerSet, ServerFeatureExt, ServerFeatureExtSet};
use phux_protocol::ids::SessionKey;
use phux_protocol::native_agent::{
    NativeIntegrationCatalog, NativeRequest, NativeRequestPayload, SpawnedNativeAgent,
};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, ErrorCode, FrameKind, StateScope,
};
use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, command, join_after_shutdown, recv_typed, run_local, send_frame,
    spawn_server_with, wait_for_raw_socket,
};

fn native(request: NativeRequest) -> Command {
    Command::NativeAgent {
        request: NativeRequestPayload::new(request).unwrap(),
    }
}

fn fixture(root: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let plugin = root.join("plugin");
    std::fs::create_dir_all(plugin.join("integrations")).unwrap();
    std::fs::create_dir_all(root.join("phux")).unwrap();
    std::fs::write(
        plugin.join("phux-plugin.toml"),
        "id = 'test.native'\nname = 'Native'\nversion = '0.1.0'\nmin_phux_version = '0.0.2'\n",
    )
    .unwrap();
    std::fs::write(plugin.join("integrations/opencode.toml"), "id = 'opencode'\n[acp]\nprovider = 'opencode'\ncommand = ['sh', 'configured-acp']\nrequired_executables = ['sh']\n").unwrap();
    std::fs::write(
        root.join("phux/config.toml"),
        format!(
            "[[plugins]]\nmanifest = {:?}\n",
            plugin.join("phux-plugin.toml").to_str().unwrap()
        ),
    )
    .unwrap();
    let guardian = root.join("guardian");
    std::fs::write(&guardian, "#!/bin/sh\nexec /bin/sleep 60\n").unwrap();
    std::fs::set_permissions(&guardian, std::fs::Permissions::from_mode(0o700)).unwrap();
    guardian
}

async fn connect(socket: &std::path::Path) -> tokio::net::UnixStream {
    let mut stream = wait_for_raw_socket(socket, SOCKET_CONNECT_DEADLINE).await;
    send_frame(
        &mut stream,
        &FrameKind::Hello {
            client_name: "native-launch-test".into(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::new().with_layers(LayerSet::all()),
        },
    )
    .await;
    let (_, hello) = recv_typed(&mut stream).await;
    let FrameKind::HelloOk { server_caps, .. } = hello else {
        panic!("hello: {hello:?}")
    };
    assert!(
        server_caps
            .features_ext
            .contains(ServerFeatureExt::NativeAgentLaunch)
    );
    stream
}

async fn snapshot(
    stream: &mut tokio::net::UnixStream,
    request_id: u32,
) -> phux_protocol::wire::info::SessionSnapshot {
    let result = command(
        stream,
        request_id,
        Command::GetState {
            scope: StateScope::Server,
        },
    )
    .await;
    let CommandResult::OkWith(CommandValue::State(state)) = result else {
        panic!("state: {result:?}")
    };
    state
}

async fn invalid_directories(stream: &mut tokio::net::UnixStream, session: SessionKey) {
    for (index, cwd) in [
        "",
        "relative",
        "/definitely-not-a-phux-directory",
        "/dev/null",
    ]
    .into_iter()
    .enumerate()
    {
        let result = command(
            stream,
            u32::try_from(index).unwrap() + 10,
            native(NativeRequest::LaunchIntegration {
                integration: "opencode".into(),
                session,
                cwd: cwd.into(),
            }),
        )
        .await;
        assert!(
            matches!(
                result,
                CommandResult::Error {
                    code: ErrorCode::InvalidCommand,
                    ..
                }
            ),
            "{result:?}"
        );
    }
}

#[test]
fn native_integration_inventory_and_invalid_launches_use_real_dispatch() {
    const CHILD: &str = "PHUX_TEST_NATIVE_INTEGRATIONS_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let tmp = tempfile::tempdir().unwrap();
        fixture(tmp.path());
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "native_integrations::native_integration_inventory_and_invalid_launches_use_real_dispatch", "--nocapture"])
            .env(CHILD, "1")
            .env("XDG_CONFIG_HOME", tmp.path())
            .output().unwrap();
        assert!(
            output.status.success(),
            "isolated native wire test failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let root = std::path::PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap());
    let guardian = root.join("guardian");
    run_local(async {
        let socket = root.join("phux.sock");
        let (shutdown, server) =
            spawn_server_with(socket.clone(), Some("native-onboarding"), |cfg| {
                cfg.native_host_executable = Some(guardian);
            });
        let mut stream = connect(&socket).await;
        let state = snapshot(&mut stream, 1).await;
        let session = state
            .sessions
            .iter()
            .find(|session| session.name == "native-onboarding")
            .unwrap()
            .key
            .unwrap();
        let catalog = command(&mut stream, 2, native(NativeRequest::ListIntegrations {})).await;
        let CommandResult::OkWith(CommandValue::Json(catalog)) = catalog else {
            panic!("catalog: {catalog:?}")
        };
        let catalog: NativeIntegrationCatalog = serde_json::from_str(&catalog).unwrap();
        assert!(catalog.integrations.len() <= 64);
        assert!(
            catalog
                .integrations
                .iter()
                .all(|item| item.auth_command.is_none())
        );
        assert!(
            catalog
                .integrations
                .iter()
                .find(|item| item.integration == "opencode")
                .unwrap()
                .native_available
        );
        invalid_directories(&mut stream, session).await;
        let missing_session = command(
            &mut stream,
            20,
            native(NativeRequest::LaunchIntegration {
                integration: "opencode".into(),
                session: SessionKey::new([0xa9; 16]).unwrap(),
                cwd: "/".into(),
            }),
        )
        .await;
        assert!(matches!(missing_session, CommandResult::Error { .. }));
        let after = snapshot(&mut stream, 21).await;
        assert_eq!(state.sessions.len(), after.sessions.len());
        assert_eq!(state.resources.len(), after.resources.len());
        // A real inert guardian, not a provider echo: wire dispatch reaches the
        // existing native reservation transaction without running any model.
        let launched = command(
            &mut stream,
            22,
            native(NativeRequest::LaunchIntegration {
                integration: "opencode".into(),
                session,
                cwd: root.to_str().unwrap().into(),
            }),
        )
        .await;
        let CommandResult::OkWith(CommandValue::Json(launched)) = launched else {
            panic!("launch: {launched:?}")
        };
        let launched: SpawnedNativeAgent = serde_json::from_str(&launched).unwrap();
        assert_ne!(launched.parent, launched.agent);
        assert!(!launched.activation.is_empty());
        assert!(
            !ServerFeatureExtSet::new().contains(ServerFeatureExt::NativeAgentLaunch),
            "legacy servers must not imply launch support"
        );
        drop(stream);
        join_after_shutdown(shutdown, server).await;
    });
}
