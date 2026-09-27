//! The HELLO authorization seam and policy postures (ADR-0072,
//! `docs/spec/workload-auth.md` §8): a denying engine refuses HELLO with
//! `PermissionDenied` before the close; the owner socket keeps all six verbs
//! at Global under both the default and the `paired` engine; `local` mode
//! beside a remote listener refuses to start. Posture resolution and the
//! paired engine's remote refusals are unit-tested in `policy::tests`.

use std::sync::Arc;

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, ColorSupport, LayerSet};
use phux_protocol::policy::PeerIdentity;
use phux_protocol::wire::frame::{ErrorCode, FrameKind, Scope, WHOAMI_KEY};
use phux_server::ServerError;
use phux_server::auth::AuthenticatedCredential;
use phux_server::policy::{GrantFuture, PolicyEngine, PolicyError, PostureError, ScopedPolicy};
use phux_server::runtime::{ServerConfig, ServerRuntime};
use phux_server::workload::ReloadingWorkloadRegistry;
use tempfile::TempDir;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, recv_typed, recv_until, run_local, send_frame, wait_for_raw_socket,
};

/// A policy engine that refuses every HELLO. Stands in for a workload that
/// presents no valid paired credential.
#[derive(Debug)]
struct DenyAllPolicy;

impl PolicyEngine for DenyAllPolicy {
    fn authorize_hello<'a>(
        &'a self,
        _peer_identity: &'a PeerIdentity,
        _credential: Option<&'a AuthenticatedCredential>,
    ) -> GrantFuture<'a> {
        Box::pin(async move {
            Err(PolicyError::Unauthorized(
                "no paired workload credential".to_owned(),
            ))
        })
    }
}

fn hello() -> FrameKind {
    FrameKind::Hello {
        client_name: "policy-deny-test".to_owned(),
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        client_caps: ClientCapabilities::new()
            .with_color_support(ColorSupport::TrueColor)
            .with_layers(LayerSet::all()),
    }
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

/// Start a server on `cfg`, run `client` against its socket, then stop it.
async fn with_server<F, Fut>(cfg: ServerConfig, client: F)
where
    F: FnOnce(std::path::PathBuf) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let socket_path = cfg.socket_path.clone();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::task::spawn_local(async move {
        ServerRuntime::new(cfg)
            .run_async(async move {
                let _ = rx.await;
            })
            .await
    });
    client(socket_path).await;
    let _ = tx.send(());
    let _ = handle.await;
}

#[test]
fn a_denying_engine_refuses_hello_with_permission_denied() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let cfg = cfg(socket_path, Some(Arc::new(DenyAllPolicy)));
        with_server(cfg, |socket_path| async move {
            let mut stream = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
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
        })
        .await;
    });
}

/// The owner's whoami record over a real socket.
async fn owner_whoami(socket_path: std::path::PathBuf) -> serde_json::Value {
    let mut stream = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(&mut stream, &hello()).await;
    send_frame(
        &mut stream,
        &FrameKind::GetMetadata {
            request_id: 1,
            scope: Scope::Global,
            key: WHOAMI_KEY.to_owned(),
        },
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

fn all_six_at_global() -> serde_json::Value {
    serde_json::json!([{
        "verbs": ["inventory", "observe", "create", "bind", "input", "signal"],
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
            let cfg = cfg(tmp.path().join(format!("{name}.sock")), engine);
            with_server(cfg, |socket_path| async move {
                let record = owner_whoami(socket_path).await;
                assert_eq!(record["auth_route"], "uds", "{name}");
                assert_eq!(record["grant"], all_six_at_global(), "{name}");
            })
            .await;
        }
    });
}

#[test]
fn local_mode_with_a_remote_listener_refuses_to_start() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let cfg = ServerConfig {
            policy_mode: Some(phux_config::PolicyMode::Local),
            ..cfg(socket_path.clone(), None)
        };
        let started = ServerRuntime::new(cfg)
            .listen_quic("127.0.0.1:0".parse().unwrap())
            .run_async(async {})
            .await;
        assert!(
            matches!(
                started,
                Err(ServerError::Policy(PostureError::LocalWithRemoteListener))
            ),
            "{started:?}"
        );
        assert!(!socket_path.exists(), "nothing was bound");
    });
}
