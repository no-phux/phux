//! The real CLI decision path under a grant without Global observation.

use super::*;
use phux_client::attach::connection::Connection;
use phux_protocol::ids::OperationId;
use phux_protocol::policy::PeerIdentity;
use phux_protocol::scope::TerminalScopeSet;
use phux_protocol::wire::frame::{Command, CommandResult, ErrorCode, Scope};
use phux_server::auth::AuthenticatedCredential;
use phux_server::policy::{ConnectionGrant, GrantFuture, PolicyEngine};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// First admission observes the held request, second asks, later ones decide.
#[derive(Debug, Default)]
struct Grants(AtomicUsize);

impl PolicyEngine for Grants {
    fn authorize_hello<'a>(
        &'a self,
        _peer: &'a PeerIdentity,
        _credential: Option<&'a AuthenticatedCredential>,
    ) -> GrantFuture<'a> {
        let admission = self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if admission == 0 {
                return Ok(ConnectionGrant::owner());
            }
            let scope = if admission == 1 {
                "?terminate@global"
            } else {
                "terminate@global"
            };
            Ok(
                ConnectionGrant::scoped(TerminalScopeSet::parse_all(&[scope]).unwrap(), None)
                    .unwrap(),
            )
        })
    }
}

async fn start_server(
    socket: PathBuf,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<(), phux_server::ServerError>>,
) {
    let cfg = phux_server::ServerConfig {
        socket_path: socket,
        pre_seeded_session: Some("held".to_owned()),
        seed_with_pty: false,
        seed_command: None,
        policy_engine: Some(Arc::new(Grants::default())),
        ..phux_server::ServerConfig::with_default_socket()
    };
    let (ready, listening) = tokio::sync::oneshot::channel();
    let (shutdown, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = phux_server::ServerRuntime::new(cfg, Arc::new(phux_terminal::TerminalEngine))
        .on_listening(move |_| {
            let _ = ready.send(());
        });
    let server = tokio::task::spawn_local(server.run_async(async {
        let _ = stopped.await;
    }));
    tokio::time::timeout(Duration::from_secs(15), listening)
        .await
        .unwrap()
        .unwrap();
    (shutdown, server)
}

async fn pending_approval(owner: &mut Connection) -> Approval {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Some(record) = phux_client::approvals::list(owner)
                .await
                .unwrap()
                .into_iter()
                .next()
            {
                break record;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn noncredential_yes_approves_without_global_observation() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("least-privilege.sock");
            let (shutdown, server) = start_server(socket.clone()).await;
            let mut owner = Connection::connect(&socket).await.unwrap();
            let state = phux_client::state::get_state_on(&mut owner).await.unwrap();
            let session = state.snapshot().sessions[0].key.unwrap();
            let mut requester = Connection::connect(&socket).await.unwrap();
            let held = tokio::task::spawn_local(async move {
                requester
                    .request(
                        7,
                        Command::SessionKill {
                            operation: OperationId::new([1; 16]).unwrap(),
                            session,
                        },
                    )
                    .await
                    .unwrap()
            });
            let approval = pending_approval(&mut owner).await;
            assert!(approval.credential.is_none());
            let id = ApprovalId::parse(&approval.id).unwrap();
            let mut scoped = Connection::connect(&socket).await.unwrap();
            let refusal = scoped
                .request_metadata(1, Scope::Global, id.record_key())
                .await
                .unwrap()
                .into_parts()
                .0
                .unwrap_err();
            assert_eq!(refusal.code, ErrorCode::PermissionDenied);
            scoped.shutdown().await;
            let approved = tokio::task::spawn_blocking(move || {
                run_approve(&approval.id, true, false, Some(socket))
            });
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(15), approved)
                    .await
                    .unwrap()
                    .unwrap(),
                ExitCode::SUCCESS,
            );
            let result = tokio::time::timeout(Duration::from_secs(15), held)
                .await
                .unwrap()
                .unwrap()
                .into_parts()
                .0;
            assert!(
                !matches!(
                    result,
                    CommandResult::Error { .. } | CommandResult::Refused(_)
                ),
                "{result:?}"
            );
            assert!(
                phux_client::approvals::list(&mut owner)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert!(
                phux_client::state::get_state_on(&mut owner)
                    .await
                    .unwrap()
                    .snapshot()
                    .sessions
                    .is_empty(),
                "the held session kill actually ran",
            );
            owner.shutdown().await;
            drop(shutdown);
            tokio::time::timeout(Duration::from_secs(15), server)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        })
        .await;
}
