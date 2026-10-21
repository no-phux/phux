use super::onboarding::{decode_reply, destination, valid_launch};
use super::*;
mod focus;
use gpui::{AppContext as _, Entity, TestAppContext};
use phux_protocol::{
    caps::{
        BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ServerCapabilities,
        ServerFeatureExtSet,
    },
    ids::{BootstrapId, ClientId, SessionId, StreamId, WindowId},
    wire::{
        frame::FrameKind,
        info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo},
    },
};

fn key() -> SessionKey {
    SessionKey::new([42; 16]).unwrap()
}

fn snapshot() -> SessionSnapshot {
    SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(7))
        .with_sessions(vec![
            SessionInfo::new(SessionId::new(1), "remote-work").with_key(Some(key())),
        ])
        .with_windows(vec![WindowInfo::new(
            WindowId::new(1),
            SessionId::new(1),
            "shell",
        )])
        .with_resources(vec![
            ResourceInfo::new(ResourceId::local(7), WindowId::new(1), 80, 24)
                .with_cwd(Some("/remote/repo with spaces".into())),
        ])
}

fn connection(supported: bool) -> Connection {
    let mut connection =
        Connection::connect("/native-onboarding-test.sock".into(), "remote-work".into()).unwrap();
    let client = &connection.client;
    client.with_control(phux_client_runtime::control::ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client
        .feed(FrameKind::HelloOk {
            protocol_major: phux_protocol::PROTOCOL_VERSION.major,
            protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
            protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new()
                .with_features(phux_protocol::caps::ServerFeatureSet::with(&[
                    phux_protocol::caps::ServerFeature::SpawnIdempotency,
                ]))
                .with_features_ext(ServerFeatureExtSet::with(if supported {
                    &[
                        ServerFeatureExt::NativeAgent,
                        ServerFeatureExt::NativeAgentLaunch,
                    ]
                } else {
                    &[]
                })),
            server_id: vec![1; 16],
            selected_profile: BootstrapProfile::SynthesizedVtRaw,
            bootstrap_limits: BootstrapLimits::default(),
        })
        .unwrap();
    let attach_id = outbound(client)
        .iter()
        .find_map(|frame| match frame {
            FrameKind::Attach { attach_id, .. } => Some(*attach_id),
            _ => None,
        })
        .unwrap();
    let terminal_id = ResourceId::local(7);
    let stream_id = StreamId::new(1).unwrap();
    let bootstrap_id = BootstrapId::new(1).unwrap();
    for frame in [
        FrameKind::Attached {
            attach_id,
            snapshot: snapshot(),
            initial_client_id: ClientId::new(1),
        },
        FrameKind::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq: 0,
        },
        FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
        FrameKind::AttachReady { attach_id },
    ] {
        client.feed(frame).unwrap();
    }
    connection.drain();
    let _ = connection.client.take_outbound();
    connection
}

fn outbound(client: &phux_client_runtime::Client) -> Vec<FrameKind> {
    client
        .take_outbound()
        .iter()
        .map(|bytes| FrameKind::decode(bytes).unwrap().0)
        .collect()
}

fn fixture(cx: &mut TestAppContext, supported: bool) -> (gpui::AnyWindowHandle, Entity<Desk>) {
    let (handle, desk) = crate::app::overlays::palette_tests::fixture(cx);
    let mut connection = connection(supported);
    let mut workspace = crate::workspace::Workspace::new(
        "remote-work".into(),
        crate::workspace::WorkspaceOptions::default(),
    );
    workspace.restore(None);
    let events = connection.drain();
    workspace.handle(&events, &connection);
    workspace.settle(&connection);
    desk.update(cx, |desk, _| {
        desk.remote = Some("remote-host".into());
        desk.session = "remote-work".into();
        desk.link = Some(crate::app::desk::Link {
            connection,
            workspace,
        });
        desk.native_launch.destination = desk.native_destination().ok();
        desk.native_launch.integrations = vec![NativeIntegration {
            integration: "opencode".into(),
            display_name: Some("OpenCode".into()),
            native_available: true,
            native_detail: None,
            auth_command: Some(vec!["opencode".into(), "auth".into(), "login".into()]),
        }];
    });
    (handle, desk)
}

#[test]
fn destination_is_the_focused_hosts_persistent_session_and_absolute_directory() {
    let mut topology = Topology::from_snapshot(&snapshot());
    let target = destination(&topology, topology.panes.first()).unwrap();
    assert_eq!(target.session, key());
    assert_eq!(target.cwd, "/remote/repo with spaces");
    assert!(destination(&topology, None).is_err());
    topology.panes[0].cwd = Some("relative".into());
    assert!(destination(&topology, topology.panes.first()).is_err());
    topology.panes[0].cwd = Some("/remote/repo".into());
    topology.sessions[0].key = None;
    assert!(destination(&topology, topology.panes.first()).is_err());
    topology.sessions[0].key = Some(key());
    topology.panes[0].terminal_id =
        ResourceId::satellite(phux_protocol::ids::SatelliteHost::new("sat"), 7);
    assert!(destination(&topology, topology.panes.first()).is_err());
}

#[test]
fn destination_follows_a_foreign_focused_session_not_the_home_session() {
    let mut topology = Topology::from_snapshot(&snapshot());
    let mut session = topology.sessions[0].clone();
    session.id = 2;
    session.name = "other-session".into();
    let other_key = SessionKey::new([99; 16]).unwrap();
    session.key = Some(other_key);
    topology.sessions.push(session);
    let mut focused = topology.panes[0].clone();
    focused.session_id = 2;
    focused.session_name = "other-session".into();
    focused.cwd = Some("/other/checkout".into());
    let target = destination(&topology, Some(&focused)).unwrap();
    assert_eq!(target.session, other_key);
    assert_eq!(target.name, "other-session");
    assert_eq!(target.cwd, "/other/checkout");
}

#[gpui::test]
fn remote_launch_uses_connected_server_not_desktop_cli_and_pins_cwd(cx: &mut TestAppContext) {
    let _dial = crate::connection::DeterministicDial::arm();
    let (_, desk) = fixture(cx, true);
    desk.read_with(cx, |desk, _| {
        assert!(!desk.phux.exists(), "fixture has no local CLI to proxy through");
        let launch = desk.queue_native_launch("opencode").unwrap();
        let client = &desk.link.as_ref().unwrap().connection.client;
        let frames = outbound(client);
        assert!(frames.iter().any(|frame| matches!(frame,
            FrameKind::Command { request_id, command: Command::NativeAgent { request } }
                if *request_id == launch.flight.id && matches!(request.request(),
                    NativeRequest::LaunchIntegration { integration, session, cwd }
                    if integration == "opencode" && *session == key() && cwd == "/remote/repo with spaces"
                )
        )), "{frames:?}");
        assert!(frames.iter().all(|frame| !matches!(frame, FrameKind::SpawnResource { .. })), "native launch cannot fall back to a TUI");
    });
}

#[gpui::test]
fn changed_folder_and_legacy_server_refuse_before_transmitting(cx: &mut TestAppContext) {
    let _dial = crate::connection::DeterministicDial::arm();
    let (_, desk) = fixture(cx, false);
    desk.update(cx, |desk, _| {
        assert!(
            desk.queue_native_launch("opencode")
                .unwrap_err()
                .contains("Update phux")
        );
        assert!(outbound(&desk.link.as_ref().unwrap().connection.client).is_empty());
        desk.native_launch.destination.as_mut().unwrap().cwd = "/another/folder".into();
        assert!(
            desk.queue_native_launch("opencode")
                .unwrap_err()
                .contains("changed")
        );
        assert!(outbound(&desk.link.as_ref().unwrap().connection.client).is_empty());
    });
}

#[gpui::test]
fn connection_loss_never_replays_launch_or_adopts_a_late_receipt(cx: &mut TestAppContext) {
    let _dial = crate::connection::DeterministicDial::arm();
    let (_, desk) = fixture(cx, true);
    desk.update(cx, |desk, cx| {
        let launch = desk.queue_native_launch("opencode").unwrap();
        let id = launch.flight.id;
        desk.native_launch.launch = Some(launch);
        desk.link.as_mut().unwrap().connection.status = Status::Connecting;
        desk.native_launch_receipts(
            &[Event::CommandResult {
                request_id: id,
                result: CommandResult::OkWith(CommandValue::Json(
                    serde_json::to_string(&spawned()).unwrap(),
                )),
            }],
            cx,
        );
        assert!(desk.native_launch.launch.is_none());
        assert!(desk.native_launch.pending.is_none());
        assert_eq!(desk.native_launch.error.as_deref(), Some(UNKNOWN));
        let frames = outbound(&desk.link.as_ref().unwrap().connection.client);
        assert_eq!(
            frames
                .iter()
                .filter(|frame| matches!(
                    frame,
                    FrameKind::Command {
                        command: Command::NativeAgent { .. },
                        ..
                    }
                ))
                .count(),
            1
        );
    });
}

#[gpui::test]
fn signin_is_explicit_remote_argv_and_does_not_claim_authentication(cx: &mut TestAppContext) {
    let _dial = crate::connection::DeterministicDial::arm();
    let (handle, desk) = fixture(cx, true);
    desk.update(cx, |desk, _| {
        desk.native_launch.integrations[0].auth_command = Some(vec![
            "/remote/tools/opencode".into(),
            "auth".into(),
            "login".into(),
        ])
    });
    cx.update_window(handle, |_, window, cx| {
        desk.update(cx, |desk, cx| desk.login_opencode(window, cx))
    })
    .unwrap();
    desk.read_with(cx, |desk, _| {
        assert!(desk.native_launch.login.is_some());
        let frames = outbound(&desk.link.as_ref().unwrap().connection.client);
        assert!(
            frames.iter().any(
                |frame| matches!(frame, FrameKind::SpawnResource { command, cwd, resource: None, .. }
            if command.as_ref().is_some_and(|argv| argv == &["/remote/tools/opencode", "auth", "login"])
                        && cwd.as_deref() == Some("/remote/repo with spaces")
                )
            ),
            "{frames:?}"
        );
    });
}

#[gpui::test]
fn custom_profile_without_login_action_never_runs_bare_opencode(cx: &mut TestAppContext) {
    let _dial = crate::connection::DeterministicDial::arm();
    let (handle, desk) = fixture(cx, true);
    desk.update(cx, |desk, _| {
        desk.native_launch.integrations[0].auth_command = None
    });
    cx.update_window(handle, |_, window, cx| {
        desk.update(cx, |desk, cx| desk.login_opencode(window, cx))
    })
    .unwrap();
    desk.read_with(cx, |desk, _| {
        assert!(desk.native_launch.login.is_none());
        assert!(
            desk.native_launch
                .error
                .as_deref()
                .unwrap()
                .contains("no documented sign-in")
        );
        assert!(outbound(&desk.link.as_ref().unwrap().connection.client).is_empty());
    });
}

#[gpui::test]
fn picker_keeps_opencode_start_and_signin_inside_the_native_panel(cx: &mut TestAppContext) {
    let _dial = crate::connection::DeterministicDial::arm();
    let (handle, desk) = fixture(cx, true);
    desk.update(cx, |desk, cx| {
        desk.modal = Modal::NativeLaunch;
        cx.notify();
    });
    let cx = &mut gpui::VisualTestContext::from_window(handle, cx);
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
    }
    let panel = cx.debug_bounds("native-launch-panel").unwrap();
    let viewport = cx.update(|window, _| window.viewport_size());
    assert!(
        panel.origin.y >= gpui::px(0.) && panel.origin.y + panel.size.height <= viewport.height
    );
    for selector in ["native-launch-opencode", "opencode-sign-in"] {
        let button = cx.debug_bounds(selector).unwrap();
        assert!(
            button.origin.x >= panel.origin.x && button.origin.y >= panel.origin.y,
            "{selector}"
        );
        assert!(
            button.origin.x + button.size.width <= panel.origin.x + panel.size.width,
            "{selector}"
        );
        assert!(
            button.origin.y + button.size.height <= panel.origin.y + panel.size.height,
            "{selector}"
        );
    }
}

#[gpui::test]
fn fresh_reconnect_inventory_retires_missing_guardian_without_relaunch(cx: &mut TestAppContext) {
    let _dial = crate::connection::DeterministicDial::arm();
    let (_, desk) = fixture(cx, true);
    desk.update(cx, |desk, cx| {
        let connection = &desk.link.as_ref().unwrap().connection;
        desk.native_launch.pending = Some(Pending {
            owner: Arc::downgrade(&connection.client),
            epoch: connection.connection_epoch().wrapping_sub(1),
            session: "remote-work".into(),
            session_key: Some(key()),
            terminal: ResourceId::local(9),
            agent: ResourceId::local(10),
            activation: "original-activation".into(),
            revealed: false,
        });
        desk.native_launch_receipts(&[Event::TopologyChanged], cx);
        assert!(
            desk.native_launch.busy(),
            "cached inventory cannot prove disappearance"
        );
        desk.native_launch_receipts(
            &[Event::TopologySnapshot {
                attach_id: Some(1),
                snapshot: snapshot(),
            }],
            cx,
        );
        assert!(
            desk.native_launch.busy(),
            "home-session attach is not full inventory"
        );
        desk.native_launch_receipts(
            &[Event::TopologySnapshot {
                attach_id: None,
                snapshot: snapshot(),
            }],
            cx,
        );
        assert!(desk.native_launch.pending.is_none());
        assert!(!desk.native_launch.busy());
        assert!(
            desk.native_launch
                .error
                .as_deref()
                .unwrap()
                .contains("no longer listed")
        );
        assert!(
            outbound(&desk.link.as_ref().unwrap().connection.client).is_empty(),
            "reconciliation must not start a replacement"
        );
    });
}

fn spawned() -> SpawnedNativeAgent {
    SpawnedNativeAgent {
        parent: ResourceId::local(9),
        agent: ResourceId::local(10),
        activation: "activation-1".into(),
    }
}

#[test]
fn malformed_or_missing_receipts_never_become_success() {
    assert!(
        decode_reply::<NativeIntegrationCatalog>(&CommandResult::OkWith(CommandValue::Json(
            " ".repeat(phux_protocol::native_agent::MAX_NATIVE_PAYLOAD_BYTES + 1)
        )))
        .is_err()
    );
    assert!(decode_reply::<SpawnedNativeAgent>(&CommandResult::Ok).is_err());
    assert!(
        decode_reply::<SpawnedNativeAgent>(&CommandResult::OkWith(CommandValue::Json("{}".into())))
            .is_err()
    );
    let mut invalid = spawned();
    invalid.activation.clear();
    assert!(valid_launch(invalid).is_err());
    let mut invalid = spawned();
    invalid.agent = invalid.parent.clone();
    assert!(valid_launch(invalid).is_err());
    let mut invalid = spawned();
    invalid.parent = ResourceId::local(0);
    assert!(valid_launch(invalid).is_err());
    assert!(valid_launch(spawned()).is_ok());
}
