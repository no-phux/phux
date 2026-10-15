//! A paired grant whose `OBSERVE` and `INVENTORY` sets differ, against a real
//! server: `ATTACHED` and `GET_STATE` are different views of one live graph
//! (workload-auth §6), and neither closes what the other lists.

use super::*;
use std::sync::Mutex;

use phux_protocol::policy::PeerIdentity;
use phux_protocol::scope::TerminalScopeSet;
use phux_server::auth::AuthenticatedCredential;
use phux_server::policy::{ConnectionGrant, GrantFuture, PolicyEngine};

/// The owner's grant until `scope` is set; that scope for every later HELLO.
#[derive(Debug, Default)]
struct Switchable(Mutex<Option<TerminalScopeSet>>);

impl PolicyEngine for Switchable {
    fn authorize_hello<'a>(
        &'a self,
        _peer_identity: &'a PeerIdentity,
        _credential: Option<&'a AuthenticatedCredential>,
    ) -> GrantFuture<'a> {
        let scope = self.0.lock().unwrap().clone();
        Box::pin(async move {
            Ok(scope.map_or_else(ConnectionGrant::owner, |set| {
                ConnectionGrant::scoped(set, None).unwrap()
            }))
        })
    }
}

fn local_id(id: &ResourceId) -> u32 {
    match id {
        ResourceId::Local { id } => *id,
        other @ ResourceId::Satellite { .. } => {
            panic!("a local server spawns local terminals, got {other:?}")
        }
    }
}

#[test]
fn an_inventory_narrower_than_observe_never_closes_an_attached_pane() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("phux.sock");
        let policy = Arc::new(Switchable::default());
        let engine: Arc<dyn PolicyEngine> = policy.clone();
        let (shutdown, server) =
            phux_server_testkit::spawn_server_with(socket.clone(), Some("main"), move |cfg| {
                cfg.policy_engine = Some(engine);
            });
        let owner = Runtime::connect(Target::uds(&socket), options()).unwrap();
        wait_for_status(&owner, Status::Attached).await;
        let listed = owner.topology().unwrap().panes[0].terminal_id.clone();
        let hidden = spawn_cat(&owner).await;

        *policy.0.lock().unwrap() = Some(
            TerminalScopeSet::parse_all(&[
                "bind,observe@global".to_owned(),
                format!("inventory@terminal:{}", local_id(&listed)),
            ])
            .unwrap(),
        );
        let scoped = Runtime::connect(Target::uds(&socket), options()).unwrap();
        wait_for_status(&scoped, Status::Attached).await;
        let attached = scoped.topology().unwrap();
        assert!(attached.pane(&listed).is_some() && attached.pane(&hidden).is_some());
        let _ = scoped.take_events();

        scoped.refresh_topology().unwrap();
        wait_until("the INVENTORY view", || {
            scoped
                .topology()
                .is_some_and(|topology| topology.pane(&hidden).is_none())
        })
        .await;
        assert!(
            !scoped.take_events().iter().any(|event| matches!(
                event,
                Event::TerminalClosed { terminal_id, .. } if *terminal_id == hidden
            )),
            "a pane the grant observes but cannot inventory is alive"
        );
        assert!(!scoped.is_closed(&hidden));
        assert!(scoped.input_ready(&hidden));

        scoped.close();
        owner.close();
        drop(shutdown);
        server.await.unwrap().unwrap();
    });
}
