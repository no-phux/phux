//! Live satellite reload (phux-lpn7.2, L3 §3.8): a running hub re-reads its
//! `[[satellites]]` registry when the `phux.config.reload/v1` doorbell rings,
//! dialing added satellites and dropping removed ones with no restart, while
//! unchanged links, their subscriptions, and the hub's own panes survive.

use std::sync::{Arc, Mutex};

use super::*;
use phux_protocol::wire::frame::CONFIG_RELOAD_KEY;
use phux_server::SatelliteSource;

type Registry = Arc<Mutex<Vec<SatelliteConfigEntry>>>;

/// A hub that starts with an empty registry, pre-seeds `hub-session`, and
/// re-reads `registry` on every reload.
fn spawn_reloading_hub(
    socket_path: PathBuf,
    registry: &Registry,
) -> (oneshot::Sender<()>, ServerTask) {
    let (tx, rx) = oneshot::channel::<()>();
    let cfg = ServerConfig {
        socket_path,
        pre_seeded_session: Some("hub-session".to_owned()),
        seed_with_pty: false,
        seed_command: None,
        ..ServerConfig::with_default_socket()
    };
    let source_registry = Arc::clone(registry);
    let source = SatelliteSource::new(move || Ok(source_registry.lock().unwrap().clone()));
    let handle = tokio::task::spawn_local(async move {
        ServerRuntime::new(cfg)
            .hub(Vec::new())
            .hub_reload(source)
            .run_async(async move {
                let _ = rx.await;
            })
            .await
    });
    (tx, handle)
}

/// Ring the doorbell, then read the key back: the hub handles one
/// connection's frames in order, so the reply proves the reload ran.
async fn ring_reload(hub: &mut UnixStream, request_id: u32, nonce: &str) {
    send_frame(
        hub,
        &FrameKind::SetMetadata {
            request_id,
            scope: Scope::Global,
            key: CONFIG_RELOAD_KEY.to_owned(),
            value: nonce.as_bytes().to_vec(),
        },
    )
    .await;
    send_frame(
        hub,
        &FrameKind::GetMetadata {
            request_id: request_id + 1,
            scope: Scope::Global,
            key: CONFIG_RELOAD_KEY.to_owned(),
        },
    )
    .await;
    tokio::time::timeout(STEP_DEADLINE, async {
        loop {
            if let FrameKind::MetadataValue {
                request_id: got, ..
            } = recv_typed(hub).await.1
                && got == request_id + 1
            {
                return;
            }
        }
    })
    .await
    .expect("doorbell read-back never answered");
}

/// The hub's own seeded pane, which every reload must leave alone.
async fn local_pane(hub: &mut UnixStream, request_id: u32) -> u32 {
    let (result, _) = get_state_via_hub(hub, request_id).await;
    let CommandResult::OkWith(CommandValue::State(snapshot)) = result else {
        panic!("hub GET_STATE must succeed, got {result:?}");
    };
    snapshot
        .focused_resource
        .local_id()
        .expect("the hub's seeded pane is local")
}

async fn assert_local_pane_alive(hub: &mut UnixStream, request_id: u32, pane: u32) {
    assert_eq!(
        local_pane(hub, request_id).await,
        pane,
        "local pane survives"
    );
    assert!(matches!(
        get_screen_via_hub(hub, request_id + 1, ResourceId::local(pane)).await,
        CommandResult::OkWith(_)
    ));
}

/// Enroll a satellite into a running hub's registry and ring the doorbell:
/// the hub routes to it without a restart. A no-op reload leaves the live
/// link and its event subscription intact; removing the entry stops routing.
#[test]
fn reload_dials_added_satellites_and_drops_removed_ones_without_restart() {
    phux_server_testkit::run_local(async {
        let tmp = TempDir::new().unwrap();
        let registry: Registry = Arc::default();
        let (hub_shutdown, hub_task) = spawn_reloading_hub(tmp.path().join("hub.sock"), &registry);
        let mut hub = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;
        let pane = local_pane(&mut hub, 1).await;

        // Before enrollment the hub has no route to `sat`.
        let (ws_port, sat_shutdown, sat_task) = spawn_satellite(tmp.path().join("sat.sock")).await;
        let seed = discover_satellite_pane(ws_port).await;
        let sat_id = ResourceId::satellite("sat", seed);
        assert_error(
            &get_screen_via_hub(&mut hub, 2, sat_id.clone()).await,
            ErrorCode::UnsupportedSatelliteRoute,
        );

        // Enroll and ring: the same hub process now dials and routes.
        registry
            .lock()
            .unwrap()
            .push(satellite_entry("sat", ws_port));
        ring_reload(&mut hub, 10, "enroll-1").await;
        assert!(matches!(
            get_screen_until_ok(&mut hub, seed).await,
            CommandResult::OkWith(_)
        ));
        assert_local_pane_alive(&mut hub, 20, pane).await;

        // An unchanged registry touches no link: a subscription opened
        // before the reload still delivers afterwards.
        send_frame(
            &mut hub,
            &FrameKind::SubscribeEvents {
                terminal: Some(sat_id.clone()),
                after_seq: None,
            },
        )
        .await;
        ring_reload(&mut hub, 30, "noop-2").await;
        let ask = Command::ReportAsked {
            terminal_id: sat_id.clone(),
            id: "after-reload".to_owned(),
            question: "still linked?".to_owned(),
            suggestions: Vec::new(),
            elapsed_seconds: None,
        };
        let (result, mut frames) = command_via_hub(&mut hub, 40, ask).await;
        assert_eq!(result, CommandResult::Ok, "the link survived the reload");
        assert!(
            !frames.iter().any(|frame| matches!(
                frame,
                FrameKind::Error {
                    code: ErrorCode::SatelliteUnreachable,
                    ..
                }
            )),
            "a no-op reload must not tear the link down: {frames:?}"
        );
        let asked = |frame: &FrameKind| matches!(frame, FrameKind::Event { event: AgentEvent::Asked { id, .. }, .. } if id == "after-reload");
        if !frames.iter().any(asked) {
            frames.push(next_event(&mut hub, asked).await);
        }

        // Forget the satellite: routing stops, the hub's pane stays.
        registry.lock().unwrap().clear();
        ring_reload(&mut hub, 50, "remove-3").await;
        assert_error(
            &get_screen_via_hub(&mut hub, 60, sat_id).await,
            ErrorCode::UnsupportedSatelliteRoute,
        );
        assert_local_pane_alive(&mut hub, 70, pane).await;

        drop(hub);
        stop(hub_shutdown, hub_task).await;
        stop(sat_shutdown, sat_task).await;
    });
}

/// A registry that fails to validate keeps the running satellites.
#[test]
fn reload_with_a_broken_registry_keeps_the_running_links() {
    phux_server_testkit::run_local(async {
        let tmp = TempDir::new().unwrap();
        let registry: Registry = Arc::default();
        let (hub_shutdown, hub_task) = spawn_reloading_hub(tmp.path().join("hub.sock"), &registry);
        let mut hub = wait_for_socket(&tmp.path().join("hub.sock"), STEP_DEADLINE).await;
        let (ws_port, sat_shutdown, sat_task) = spawn_satellite(tmp.path().join("sat.sock")).await;
        let seed = discover_satellite_pane(ws_port).await;

        registry
            .lock()
            .unwrap()
            .push(satellite_entry("sat", ws_port));
        ring_reload(&mut hub, 1, "enroll-1").await;
        assert!(matches!(
            get_screen_until_ok(&mut hub, seed).await,
            CommandResult::OkWith(_)
        ));

        // A duplicate name fails validation: the table is kept whole.
        registry
            .lock()
            .unwrap()
            .push(satellite_entry("sat", ws_port));
        ring_reload(&mut hub, 10, "broken-2").await;
        assert!(matches!(
            get_screen_via_hub(&mut hub, 20, ResourceId::satellite("sat", seed)).await,
            CommandResult::OkWith(_)
        ));

        drop(hub);
        stop(hub_shutdown, hub_task).await;
        stop(sat_shutdown, sat_task).await;
    });
}
