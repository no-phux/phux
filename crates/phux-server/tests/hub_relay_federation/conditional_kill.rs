//! `KILL_RESOURCE_IF` through the real UDS -> hub -> WebSocket -> satellite
//! path (ADR-0109, `docs/spec/L1.md` §9.1).
//!
//! The hub relays the precondition unchanged and the satellite evaluates it:
//! a bound spawn carries the satellite's own token back through the hub, an
//! untouched pane is killed, and a stale token or a satellite-local attach
//! is refused by the satellite. Every hub consumer is one connection on the
//! satellite, so an attach or send-keys-style input by a second hub consumer
//! is refused by the hub, and so is a kill whose token the hub's record of
//! the spawn does not carry.

use super::*;
use phux_protocol::ids::ServerInstance;
use phux_protocol::wire::frame::{KillPrecondition, SpawnResource};
use phux_server_testkit::await_command_result;

/// A satellite and a hub linked to it, with one consumer on each.
struct Topology {
    _tmp: TempDir,
    hub_path: PathBuf,
    satellite: UnixStream,
    hub: UnixStream,
    sat_shutdown: oneshot::Sender<()>,
    sat_task: JoinHandle<Result<(), ServerError>>,
    hub_shutdown: oneshot::Sender<()>,
    hub_task: JoinHandle<Result<(), ServerError>>,
}

impl Topology {
    /// Boot both servers and wait until the hub's link answers.
    async fn boot() -> Self {
        let tmp = TempDir::new().unwrap();
        let ws_port = free_port();
        let (sat_shutdown, sat_task) = spawn_satellite(tmp.path().join("sat.sock"), ws_port);
        let seed = discover_satellite_pane(ws_port).await;
        let satellite = wait_for_socket(&tmp.path().join("sat.sock"), STEP_DEADLINE).await;
        let hub_path = tmp.path().join("hub.sock");
        let (hub_shutdown, hub_task) =
            spawn_hub(hub_path.clone(), vec![satellite_entry("sat", ws_port)]);
        let mut hub = wait_for_socket(&hub_path, STEP_DEADLINE).await;
        assert!(matches!(
            get_screen_until_ok(&mut hub, seed).await,
            CommandResult::OkWith(_)
        ));
        Self {
            _tmp: tmp,
            hub_path,
            satellite,
            hub,
            sat_shutdown,
            sat_task,
            hub_shutdown,
            hub_task,
        }
    }

    async fn shutdown(self) {
        drop(self.hub);
        drop(self.satellite);
        drop(self.hub_shutdown);
        self.hub_task.await.unwrap().unwrap();
        drop(self.sat_shutdown);
        self.sat_task.await.unwrap().unwrap();
    }
}

/// Spawn `/bin/cat` asking for binding, on `satellite` through a hub or on
/// the server `stream` talks to; return the bound id and token.
async fn spawn_bound(
    stream: &mut UnixStream,
    request_id: u32,
    satellite: Option<&str>,
) -> (ResourceId, ServerInstance) {
    send_frame(
        stream,
        &FrameKind::SpawnResource {
            request_id,
            group: GroupId::new(1),
            command: Some(vec!["/bin/cat".to_owned()]),
            cwd: None,
            env: None,
            term: None,
            satellite: satellite.map(SatelliteHost::new),
            owner_terminal: None,
            agent_session: None,
            initial_size: None,
            resource: Some(Box::new(SpawnResource::default().with_bind_instance(true))),
        },
    )
    .await;
    let result = tokio::time::timeout(STEP_DEADLINE, async {
        loop {
            if let (
                _,
                FrameKind::ResourceSpawned {
                    request_id: got,
                    result,
                },
            ) = recv_typed(stream).await
                && got == request_id
            {
                return result;
            }
        }
    })
    .await
    .expect("correlated spawn reply");
    let SpawnResult::OkBound { id, instance } = result else {
        panic!("a bind request must be answered bound: {result:?}");
    };
    (id, instance)
}

async fn command(stream: &mut UnixStream, request_id: u32, command: Command) -> CommandResult {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command,
        },
    )
    .await;
    await_command_result(stream, request_id).await
}

async fn kill_if(
    stream: &mut UnixStream,
    request_id: u32,
    pane: &ResourceId,
    instance: ServerInstance,
) -> CommandResult {
    let kill = Command::KillResourceIf {
        terminal_id: pane.clone(),
        precondition: KillPrecondition::spawned_and_unattached(instance),
    };
    command(stream, request_id, kill).await
}

/// The refusal's message, asserting it is `PRECONDITION_FAILED`.
fn refusal_message(result: CommandResult) -> String {
    let CommandResult::Error {
        code: ErrorCode::PreconditionFailed,
        message,
    } = result
    else {
        panic!("expected PRECONDITION_FAILED, got {result:?}");
    };
    message
}

#[test]
fn a_relayed_conditional_kill_is_evaluated_on_the_satellite() {
    phux_server_testkit::run_local(async {
        let mut t = Topology::boot().await;

        // The satellite's token, learned directly, is the one the hub hands
        // back for a satellite spawn: relayed unchanged.
        let (_direct, sat_instance) = spawn_bound(&mut t.satellite, 8000, None).await;
        let (pane, instance) = spawn_bound(&mut t.hub, 8001, Some("sat")).await;
        assert!(matches!(pane, ResourceId::Satellite { .. }), "{pane:?}");
        assert_eq!(instance, sat_instance, "the satellite's own token");

        // Untouched: the satellite kills it.
        let killed = kill_if(&mut t.hub, 8002, &pane, instance).await;
        assert!(matches!(killed, CommandResult::Ok), "{killed:?}");

        // A stale token with the attachment condition is refused by the hub,
        // whose record is bound to the token the spawn came back with (after
        // a satellite restart the same id would name a new pane).
        let (second, _) = spawn_bound(&mut t.hub, 8003, Some("sat")).await;
        let mut stale = *instance.as_bytes();
        stale[0] ^= 0xFF;
        let stale = ServerInstance::new(stale);
        let message = refusal_message(kill_if(&mut t.hub, 8004, &second, stale).await);
        assert!(message.contains("this hub"), "the hub's refusal: {message}");
        // Without the condition the hub has nothing to vouch for; the
        // satellite, which alone knows its current token, refuses.
        let instance_only = Command::KillResourceIf {
            terminal_id: second.clone(),
            precondition: KillPrecondition {
                instance: Some(stale),
                conditions: phux_protocol::wire::frame::KillConditions::NONE,
            },
        };
        let message = refusal_message(command(&mut t.hub, 8009, instance_only).await);
        assert!(
            message.contains("instance token"),
            "the satellite's refusal: {message}"
        );

        // A client local to the satellite attaches the session the pane was
        // placed in. The hub never saw it; the satellite refuses.
        send_frame(
            &mut t.satellite,
            &phux_server_testkit::attach_by_name("sat-session"),
        )
        .await;
        tokio::time::timeout(STEP_DEADLINE, async {
            loop {
                if let (_, FrameKind::Attached { .. }) = recv_typed(&mut t.satellite).await {
                    return;
                }
            }
        })
        .await
        .expect("satellite-local attach");
        let message = refusal_message(kill_if(&mut t.hub, 8005, &second, instance).await);
        assert!(
            message.contains("spawner"),
            "the satellite's refusal: {message}"
        );
        assert!(matches!(
            get_screen_via_hub(&mut t.hub, 8006, second).await,
            CommandResult::OkWith(_)
        ));

        t.shutdown().await;
    });
}

#[test]
fn another_hub_consumer_attaching_refuses_the_kill_at_the_hub() {
    phux_server_testkit::run_local(async {
        let mut t = Topology::boot().await;
        let (pane, instance) = spawn_bound(&mut t.hub, 8100, Some("sat")).await;

        // A second hub consumer attaches the pane. On the satellite that is
        // the hub's link again, the connection that spawned it, so only the
        // hub can tell the two consumers apart.
        let mut other = wait_for_socket(&t.hub_path, STEP_DEADLINE).await;
        let attach = Command::AttachResource {
            terminal_id: pane.clone(),
        };
        let attached = command(&mut other, 8101, attach).await;
        assert!(
            matches!(attached, CommandResult::Ok | CommandResult::OkWith(_)),
            "{attached:?}"
        );

        let message = refusal_message(kill_if(&mut t.hub, 8102, &pane, instance).await);
        assert!(
            message.contains("hub consumer"),
            "the hub's refusal: {message}"
        );
        assert!(matches!(
            get_screen_via_hub(&mut t.hub, 8103, pane).await,
            CommandResult::OkWith(_)
        ));

        // Driving a pane without attaching counts the same: the second
        // consumer sends send-keys-style input to a fresh pane.
        let (typed_into, typed_instance) = spawn_bound(&mut t.hub, 8104, Some("sat")).await;
        let send_keys = Command::RouteInput {
            terminal_id: typed_into.clone(),
            event: phux_protocol::input::InputEvent::Key(phux_server_testkit::ascii_key(
                'a',
                phux_protocol::input::key::PhysicalKey::A,
            )),
        };
        let typed = command(&mut other, 8105, send_keys).await;
        assert!(matches!(typed, CommandResult::Ok), "{typed:?}");
        let message = refusal_message(kill_if(&mut t.hub, 8106, &typed_into, typed_instance).await);
        assert!(
            message.contains("hub consumer"),
            "the hub's refusal: {message}"
        );

        drop(other);
        t.shutdown().await;
    });
}
