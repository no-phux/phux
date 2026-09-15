//! The HELLO authorization seam and the policy postures (ADR-0072,
//! `docs/spec/workload-auth.md` §8).
//!
//! * A denying engine refuses HELLO with `ERROR { PermissionDenied }` and
//!   closes the connection — the refusal reaches the wire before the close.
//! * The default (no engine, no `[policy] mode`) still completes HELLO and
//!   mints the owner's grant, all six verbs at Global, so a local server is
//!   not silently tightened.
//! * The closed modes: `local` beside a remote listener refuses to start;
//!   `paired` with no registry admits no remote connection, while the owner
//!   socket keeps its authority.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::sync::Arc;

use chrono::Utc;
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, ColorSupport, LayerSet};
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::wire::frame::{ErrorCode, FrameKind, Scope, WHOAMI_KEY};
use phux_server::ServerError;
use phux_server::auth::AuthenticatedCredential;
use phux_server::policy::{
    GrantFuture, PermissivePolicy, PolicyEngine, PolicyError, PolicyPosture, PostureError,
    ScopedPolicy,
};
use phux_server::runtime::{ServerConfig, ServerRuntime};
use phux_server::workload::ReloadingWorkloadRegistry;
use tempfile::TempDir;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, recv_typed, run_local, send_frame, wait_for_raw_socket,
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

/// A peer on `transport` running as the serving user, as the kernel would
/// report a same-user Unix-socket client.
fn peer(transport: TransportType) -> PeerIdentity {
    PeerIdentity {
        uid: nix::unistd::geteuid().as_raw(),
        pid: None,
        exe_path: None,
        mcp_host_key: None,
        transport,
        source_addr: None,
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

#[test]
fn the_default_engine_still_admits_a_local_client() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let cfg = cfg(tmp.path().join("phux.sock"), None);
        with_server(cfg, |socket_path| async move {
            let mut stream = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
            send_frame(&mut stream, &hello()).await;
            let (_type_byte, frame) = recv_typed(&mut stream).await;
            assert!(
                matches!(frame, FrameKind::HelloOk { .. }),
                "the shipped default must stay permissive; a tightened default \
                 would lock every existing local client out (got {frame:?})",
            );
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
    loop {
        match recv_typed(&mut stream).await.1 {
            FrameKind::MetadataValue {
                request_id: 1,
                value,
            } => return serde_json::from_slice(&value.expect("a whoami record")).unwrap(),
            FrameKind::Error { code, message, .. } => panic!("{code:?}: {message}"),
            _ => {}
        }
    }
}

fn all_six_at_global() -> serde_json::Value {
    serde_json::json!([{
        "verbs": ["inventory", "observe", "create", "bind", "input", "signal"],
        "selector": "global",
    }])
}

#[test]
fn local_mode_mints_all_verbs_at_global_and_existing_suite_is_unchanged() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let cfg = cfg(tmp.path().join("phux.sock"), None);
        with_server(cfg, |socket_path| async move {
            let record = owner_whoami(socket_path).await;
            assert_eq!(record["auth_route"], "uds");
            assert_eq!(record["grant"], all_six_at_global());
        })
        .await;
    });
}

#[test]
fn whoami_reports_the_effective_grant() {
    // The owner socket under the `paired` engine keeps kernel-uid authority
    // (workload-auth §3), and whoami says so; the scoped half of this
    // contract is pinned in `runtime::scope_matrix`, which can stamp a
    // workload identity.
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let registry = ReloadingWorkloadRegistry::load(tmp.path().join("workload-keys")).unwrap();
        let engine: Arc<dyn PolicyEngine> = Arc::new(ScopedPolicy::paired(Arc::new(registry)));
        let cfg = cfg(tmp.path().join("phux.sock"), Some(engine));
        with_server(cfg, |socket_path| async move {
            let record = owner_whoami(socket_path).await;
            assert_eq!(record["grant"], all_six_at_global());
        })
        .await;
    });
}

#[test]
fn paired_mode_without_registry_denies_everything() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let registry = ReloadingWorkloadRegistry::load(tmp.path().join("workload-keys")).unwrap();
        let policy = ScopedPolicy::paired(Arc::new(registry));
        let credential = AuthenticatedCredential {
            id: format!("sha256:{}", "a".repeat(64)),
            principal: "workload".to_owned(),
            scopes: vec!["*@global".to_owned()],
            issued_at: Utc::now(),
            expires_at: None,
            generation: 1,
            registry_instance: None,
        };
        for transport in [
            TransportType::Quic,
            TransportType::WebSocket,
            TransportType::WebTransport,
        ] {
            let refused = policy
                .authorize_hello(&peer(transport), Some(&credential))
                .await;
            assert!(refused.is_err(), "{transport:?} was admitted");
            assert!(
                policy
                    .authorize_hello(&peer(transport), None)
                    .await
                    .is_err(),
                "{transport:?} without a credential was admitted"
            );
        }
        let owner = policy
            .authorize_hello(&peer(TransportType::UnixSocket), None)
            .await
            .unwrap();
        assert!(owner.is_owner(), "the owner socket keeps its authority");
    });
}

#[test]
fn mode_unset_with_remote_listener_warns_and_keeps_local_grant() {
    run_local(async {
        let posture = PolicyPosture::resolve(None, false, true).unwrap();
        assert_eq!(
            posture,
            PolicyPosture::Transitional {
                remote_listener: true
            }
        );
        assert!(posture.warns_remote_owner_grant());
        assert!(!posture.requires_workload_mtls());
        // The transitional engine keeps today's behaviour for a bearer-
        // admitted remote consumer: the owner's grant.
        let grant = PermissivePolicy::INSTANCE
            .authorize_hello(&peer(TransportType::Quic), None)
            .await
            .unwrap();
        assert!(grant.is_owner());
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
