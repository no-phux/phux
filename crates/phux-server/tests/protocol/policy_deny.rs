//! The HELLO authorization seam is real, not decoration (ADR-0072).
//!
//! `phux-server` ships exactly one policy engine and it allows everything,
//! so nothing in the normal test suite ever exercises the refusal path. The
//! audit that produced bead phux-3djs called that path unreachable, and it
//! was right about the *shipped* configuration. It is not right about the
//! seam: `ServerConfig::policy_engine` is the documented injection point
//! phux-pjc5 will use, and these two tests pin its contract so a later
//! refactor cannot quietly sever it.
//!
//! * A denying engine refuses HELLO with `ERROR { PermissionDenied }` and
//!   closes the connection — the refusal reaches the wire before the close.
//! * The default (no engine configured) still completes HELLO, so the
//!   permissive default is not silently tightened.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, ColorSupport, LayerSet};
use phux_protocol::policy::{Capability, PeerIdentity};
use phux_protocol::wire::frame::{ErrorCode, FrameKind};
use phux_server::policy::{PolicyEngine, PolicyError};
use phux_server::runtime::{ServerConfig, ServerRuntime};
use tempfile::TempDir;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, recv_typed, run_local, send_frame, wait_for_raw_socket,
};

/// A policy engine that refuses every HELLO. Stands in for whatever
/// phux-pjc5 installs when a workload presents no valid paired credential.
#[derive(Debug)]
struct DenyAllPolicy;

impl PolicyEngine for DenyAllPolicy {
    fn authorize_hello<'a>(
        &'a self,
        _peer_identity: &'a PeerIdentity,
        _requested_caps: Vec<Capability>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Capability>, PolicyError>> + Send + 'a>> {
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

#[test]
fn a_denying_engine_refuses_hello_with_permission_denied() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, handle) = {
            let cfg = cfg(socket_path.clone(), Some(Arc::new(DenyAllPolicy)));
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            let handle = tokio::task::spawn_local(async move {
                ServerRuntime::new(cfg)
                    .run_async(async move {
                        let _ = rx.await;
                    })
                    .await
            });
            (tx, handle)
        };

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

        let _ = shutdown_tx.send(());
        let _ = handle.await;
    });
}

#[test]
fn the_default_engine_still_admits_a_local_client() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, handle) = {
            let cfg = cfg(socket_path.clone(), None);
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            let handle = tokio::task::spawn_local(async move {
                ServerRuntime::new(cfg)
                    .run_async(async move {
                        let _ = rx.await;
                    })
                    .await
            });
            (tx, handle)
        };

        let mut stream = wait_for_raw_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;
        send_frame(&mut stream, &hello()).await;
        let (_type_byte, frame) = recv_typed(&mut stream).await;
        assert!(
            matches!(frame, FrameKind::HelloOk { .. }),
            "the shipped default must stay permissive; a tightened default \
             would lock every existing local client out (got {frame:?})",
        );

        let _ = shutdown_tx.send(());
        let _ = handle.await;
    });
}
