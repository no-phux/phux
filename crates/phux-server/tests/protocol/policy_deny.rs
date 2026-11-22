//! The HELLO authorization seam and policy postures (ADR-0072,
//! `docs/remote.md#managing-credentials`): a denying engine refuses HELLO with
//! `PermissionDenied` and `DETACHED { AUTHENTICATION_FAILED }` before the
//! close; the owner socket keeps all six verbs
//! at Global under both the default and the `paired` engine; `local` mode,
//! or no mode at all, beside a remote listener refuses to start (ADR-0179).
//! Posture resolution and the paired engine's remote refusals are
//! unit-tested in `policy::tests`.

use std::sync::Arc;

use phux_protocol::caps::{ClientCapabilities, ColorSupport, LayerSet};
use phux_protocol::wire::frame::{DetachReason, ErrorCode, FrameKind, Scope, WHOAMI_KEY};
use phux_server::ServerError;
use phux_server::policy::{PolicyEngine, PostureError, ScopedPolicy};
use phux_server::runtime::ServerConfig;
use phux_server::workload::ReloadingWorkloadRegistry;
use tempfile::TempDir;
use tokio::io::AsyncReadExt as _;

use phux_server_testkit::{
    FnPolicy, SOCKET_CONNECT_DEADLINE, SpawnedServer, recv_typed, recv_until, run_local, send_frame,
};

fn hello() -> FrameKind {
    phux_server_testkit::hello(
        "policy-deny-test",
        ClientCapabilities::new()
            .with_color_support(ColorSupport::TrueColor)
            .with_layers(LayerSet::all()),
    )
}

fn cfg(socket_path: std::path::PathBuf, engine: Option<Arc<dyn PolicyEngine>>) -> ServerConfig {
    ServerConfig {
        socket_path,
        pre_seeded_session: Some("solo".to_owned()),
        seed_with_pty: false,
        seed_command: None,
        policy_engine: engine,
        ..ServerConfig::with_default_socket()
    }
}

/// A server seeding `solo` whose HELLOs `engine` decides (`None`: the default).
fn server(engine: Option<Arc<dyn PolicyEngine>>) -> SpawnedServer {
    SpawnedServer::builder()
        .session("solo")
        .configure(|cfg| cfg.policy_engine = engine)
        .start()
}

/// A refused HELLO is a post-HELLO authentication outcome,
/// so the goodbye is `ERROR { PERMISSION_DENIED }`, `DETACHED {
/// AUTHENTICATION_FAILED }`, and the transport closes.
#[test]
fn a_denying_engine_refuses_hello_with_permission_denied() {
    run_local(async {
        let server = server(Some(Arc::new(FnPolicy::deny(
            "no paired workload credential",
        ))));
        let mut stream = server.connect_raw().await;
        send_frame(&mut stream, &hello()).await;
        let (_type_byte, frame) = recv_typed(&mut stream).await;
        match frame {
            FrameKind::Error { code, message, .. } => {
                assert_eq!(
                    code,
                    ErrorCode::PermissionDenied,
                    "a policy refusal must reach the consumer as PermissionDenied, \
                         not as a bare disconnect",
                );
                assert!(
                    message.contains("no paired workload credential"),
                    "the engine's reason must survive to the wire; got {message:?}",
                );
            }
            other => panic!("expected ERROR after a denied HELLO, got {other:?}"),
        }
        let (_type_byte, frame) = recv_typed(&mut stream).await;
        assert!(
            matches!(
                frame,
                FrameKind::Detached {
                    reason: Some(DetachReason::AuthenticationFailed),
                    ..
                }
            ),
            "a refused HELLO ends with AUTHENTICATION_FAILED, got {frame:?}",
        );
        let mut rest = [0_u8; 1];
        let read = tokio::time::timeout(SOCKET_CONNECT_DEADLINE, stream.read(&mut rest))
            .await
            .expect("the server closes after DETACHED")
            .expect("clean close");
        assert_eq!(read, 0, "nothing follows DETACHED");
    });
}

/// The owner's whoami record over a real socket.
async fn owner_whoami(server: &SpawnedServer) -> serde_json::Value {
    let mut stream = server.connect_raw().await;
    send_frame(&mut stream, &hello()).await;
    send_frame(
        &mut stream,
        &phux_server_testkit::get_metadata(1, Scope::Global, WHOAMI_KEY),
    )
    .await;
    recv_until(&mut stream, |_, frame| match frame {
        FrameKind::MetadataValue {
            request_id: 1,
            value,
        } => Some(serde_json::from_slice(&value.expect("a whoami record")).unwrap()),
        FrameKind::Error { code, message, .. } => panic!("{code:?}: {message}"),
        _ => None,
    })
    .await
}

fn all_verbs_at_global() -> serde_json::Value {
    serde_json::json!([{
        "verbs": [
            "inventory", "observe", "create", "bind", "input", "signal", "organize", "terminate"
        ],
        "selector": "global",
    }])
}

/// The owner socket's effective grant, under no engine and under `paired`.
#[test]
fn the_owner_socket_keeps_all_verbs_at_global() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let registry = ReloadingWorkloadRegistry::load(tmp.path().join("workload-keys")).unwrap();
        let paired: Arc<dyn PolicyEngine> = Arc::new(ScopedPolicy::paired(Arc::new(registry)));
        for (name, engine) in [("default", None), ("paired", Some(paired))] {
            let server = server(engine);
            let record = owner_whoami(&server).await;
            assert_eq!(record["auth_route"], "uds", "{name}");
            assert_eq!(record["grant"], all_verbs_at_global(), "{name}");
            server.stop().await;
        }
    });
}

/// `local` mode, or no mode at all (rather than handing whoever reaches it
/// the owner's grant), beside a remote listener refuses to start (ADR-0179).
#[test]
fn a_local_or_unset_mode_with_a_remote_listener_refuses_to_start() {
    let cases = [
        (
            "local",
            Some(phux_config::PolicyMode::Local),
            PostureError::LocalWithRemoteListener,
        ),
        ("unset", None, PostureError::UnsetWithRemoteListener),
    ];
    for (label, policy_mode, refusal) in cases {
        run_local(async {
            let tmp = TempDir::new().unwrap();
            let socket_path = tmp.path().join("phux.sock");
            let cfg = ServerConfig {
                policy_mode,
                ..cfg(socket_path.clone(), None)
            };
            let started = phux_server_testkit::terminal_server(cfg)
                .listen_quic("127.0.0.1:0".parse().unwrap())
                .run_async(async {})
                .await;
            assert!(
                matches!(started, Err(ServerError::Policy(got)) if got == refusal),
                "{label}: {started:?}"
            );
            assert!(!socket_path.exists(), "{label}: nothing was bound");
        });
    }
}
