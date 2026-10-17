//! Server-held approvals (ADR-0128) against a real server: a `?signal`
//! workload's kill is held, the owner's runtime lists it, approves it once,
//! and a second decision on the same id comes back stale, not retried.

use super::*;
use std::sync::atomic::AtomicBool;

use phux_client_runtime::approvals::{Decision, DecisionOutcome};
use phux_client_runtime::session::{Dial, Session, control_caps};
use phux_protocol::ids::ApprovalId;
use phux_protocol::policy::PeerIdentity;
use phux_protocol::scope::TerminalScopeSet;
use phux_protocol::wire::frame::{Command, CommandResult};
use phux_server::auth::AuthenticatedCredential;
use phux_server::policy::{ConnectionGrant, GrantFuture, PolicyEngine};

/// The owner's grant until armed; a held-`signal` grant for every later HELLO.
#[derive(Debug, Default)]
struct HeldSignal(AtomicBool);

impl PolicyEngine for HeldSignal {
    fn authorize_hello<'a>(
        &'a self,
        _peer_identity: &'a PeerIdentity,
        _credential: Option<&'a AuthenticatedCredential>,
    ) -> GrantFuture<'a> {
        let held = self.0.load(Ordering::SeqCst);
        Box::pin(async move {
            Ok(if held {
                let set = TerminalScopeSet::parse_all(&["observe,bind,?signal@global".to_owned()])
                    .unwrap();
                ConnectionGrant::scoped(set, None).unwrap()
            } else {
                ConnectionGrant::owner()
            })
        })
    }
}

async fn decision_outcome(client: &Client, id: ApprovalId) -> DecisionOutcome {
    wait_for_event(client, "the decision's outcome", |event| match event {
        Event::ApprovalDecision {
            id: decided,
            outcome,
            ..
        } if *decided == id => Some(outcome.clone()),
        _ => None,
    })
    .await
}

#[test]
fn a_held_kill_is_listed_approved_once_and_a_second_decision_is_stale() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let policy = Arc::new(HeldSignal::default());
        let engine: Arc<dyn PolicyEngine> = policy.clone();
        let (shutdown, server) =
            phux_server_testkit::spawn_server_with(socket.clone(), Some("main"), move |cfg| {
                cfg.policy_engine = Some(engine);
            });
        let owner = Runtime::connect(Target::uds(&socket), options()).unwrap();
        wait_for_status(&owner, Status::Attached).await;
        owner.watch_approvals();
        let victim = spawn_cat(&owner).await;
        assert!(owner.approvals_supported());
        assert!(owner.approvals().is_empty());

        policy.0.store(true, Ordering::SeqCst);
        let mut requester = Session::connect(
            &Dial::uds(&socket),
            "held-requester".to_owned(),
            control_caps(),
        )
        .await
        .unwrap();
        requester
            .send(&FrameKind::Command {
                request_id: 7,
                command: Command::KillResource {
                    terminal_id: victim.clone(),
                    operation_id: None,
                },
            })
            .await
            .unwrap();

        wait_for_event(&owner, "the held kill listed", |event| {
            matches!(event, Event::ApprovalsChanged).then_some(())
        })
        .await;
        let listed = owner.approvals();
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].method, "KILL_RESOURCE");
        assert_eq!(
            listed[0].requester.client_name.as_deref(),
            Some("held-requester")
        );
        let id = ApprovalId::parse(&listed[0].id).unwrap();

        assert!(owner.decide_approval(id, Decision::Approve));
        assert_eq!(decision_outcome(&owner, id).await, DecisionOutcome::Decided);
        assert!(owner.approvals().is_empty());
        let result = tokio::time::timeout(DEADLINE, async {
            loop {
                if let FrameKind::CommandResult {
                    request_id: 7,
                    result,
                } = requester.recv().await.unwrap()
                {
                    return result;
                }
            }
        })
        .await
        .expect("the approved kill answers its requester");
        assert!(
            !matches!(result, CommandResult::Error { .. }),
            "the approved kill ran: {result:?}"
        );

        assert!(owner.decide_approval(id, Decision::Deny));
        assert_eq!(
            decision_outcome(&owner, id).await,
            DecisionOutcome::Stale,
            "a second decision on a decided approval is stale"
        );
        assert!(owner.approvals().is_empty());

        requester.shutdown().await;
        owner.close();
        drop(shutdown);
        server.await.unwrap().unwrap();
    });
}
