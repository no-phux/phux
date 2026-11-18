use super::*;
use crate::launch::Resolved;
use phux_client_runtime::control::{ControlOptions, ControlPlane};
use phux_protocol::{
    PROTOCOL_VERSION,
    caps::{
        BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ServerCapabilities,
        ServerFeature, ServerFeatureExt, ServerFeatureExtSet, ServerFeatureSet,
    },
    ids::{
        ActivationId, AuthorityId, BootstrapId, ClientId, ResourceKey, Revision, ServerInstance,
        SessionId, SessionKey, StreamId, WindowId,
    },
    wire::{
        frame::{
            ActivationInfo, AttachTarget, Command, CommandResult, CommandValue, ExecState,
            FrameKind, OrganizationPage, ResourceRecord,
        },
        info::{ResourceInfo, SessionSnapshot},
    },
};

fn desk() -> Desk {
    Desk::new(&Resolved::offline("not launched"))
}

fn connected(desk: &mut Desk) {
    let client = Arc::new(phux_client_runtime::Runtime::embedded(ControlOptions {
        attach: Some(AttachTarget::ByName("work".into())),
        viewport: (80, 24),
        ..ControlOptions::default()
    }));
    client.with_control(ControlPlane::connection_opened);
    let _ = client.take_outbound();
    client
        .feed(FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps: ServerCapabilities::new()
                .with_features(ServerFeatureSet::with(&[
                    ServerFeature::FileUpload,
                    ServerFeature::AcknowledgedInput,
                ]))
                .with_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization])),
            server_id: vec![1; 16],
            selected_profile: Some(BootstrapProfile::SynthesizedVtRaw),
            bootstrap_limits: BootstrapLimits::default(),
        })
        .unwrap();
    let frames: Vec<_> = client
        .take_outbound()
        .iter()
        .map(|bytes| FrameKind::decode(bytes).unwrap().0)
        .collect();
    let request = frames
        .iter()
        .find_map(|frame| match frame {
            FrameKind::Command {
                request_id,
                command: Command::GetOrganization { .. },
            } => Some(*request_id),
            _ => None,
        })
        .unwrap();
    let attach = frames
        .iter()
        .find_map(|frame| match frame {
            FrameKind::Attach { attach_id, .. } => Some(*attach_id),
            _ => None,
        })
        .unwrap();
    let session = SessionKey::new([1; 16]).unwrap();
    let records = [1, 3]
        .into_iter()
        .map(|n| {
            ResourceRecord::new(
                ResourceKey::new([n; 16]).unwrap(),
                session,
                Revision::new(1),
            )
            .with_live(Some(ResourceId::local(u32::from(n))))
            .with_activation(Some(ActivationInfo::new(
                ActivationId::new(1),
                ServerInstance::new([1; 16]),
                ExecState::Running,
            )))
        })
        .collect();
    client
        .feed(FrameKind::CommandResult {
            request_id: request,
            result: CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(
                OrganizationPage::new(AuthorityId::new([1; 16]).unwrap(), Revision::new(1), 0)
                    .with_resources(records),
            ))),
        })
        .unwrap();
    client
        .feed(FrameKind::Attached {
            attach_id: attach,
            snapshot: SessionSnapshot::new(
                SessionId::new(1),
                WindowId::new(1),
                ResourceId::local(1),
            )
            .with_resources(
                [1, 3]
                    .into_iter()
                    .map(|n| {
                        ResourceInfo::new(ResourceId::local(n), WindowId::new(1), 80, 24)
                            .with_session(Some(SessionId::new(1)))
                    })
                    .collect(),
            ),
            initial_client_id: ClientId::new(1),
        })
        .unwrap();
    for n in [1, 3] {
        bootstrap(&client, n);
    }
    client
        .feed(FrameKind::AttachReady { attach_id: attach })
        .unwrap();
    desk.connection.client = Some(client);
}

fn bootstrap(client: &phux_client_runtime::Client, n: u32) {
    let stream_id = StreamId::new(u64::from(n)).unwrap();
    let bootstrap_id = BootstrapId::new(1).unwrap();
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: ResourceId::local(n),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: ResourceId::local(n),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: b"test fixture".to_vec().into(),
        },
        FrameKind::BootstrapReady {
            terminal_id: ResourceId::local(n),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ] {
        client.feed(frame).unwrap();
    }
}

fn taken(desk: &mut Desk, terminal: u32, shot: Shot) {
    if desk.connection.client.is_none() {
        connected(desk);
    }
    let destination = desk
        .connection
        .client
        .as_ref()
        .unwrap()
        .with_control(|control| {
            control
                .private_upload_destination(&ResourceId::local(terminal))
                .unwrap()
        });
    desk.connection.capture = Some(Capture {
        terminal: ResourceId::local(terminal),
        server: desk.link.server,
        destination,
        done: Arc::new(Mutex::new(Some(shot))),
        picker: screenshot::Picker::default(),
        worker: None,
    });
}

#[test]
fn a_screenshot_uploads_for_the_terminal_it_was_taken_for() {
    let mut desk = desk();
    connected(&mut desk);
    taken(&mut desk, 3, Shot::Taken(b"\x89PNG".to_vec()));
    assert!(desk.finish_screenshot());
    assert!(desk.connection.capture.is_none());
    assert_eq!(desk.connection.uploads.len(), 1);
    assert_eq!(desk.connection.uploads[0].1, ResourceId::local(3));
}

#[test]
fn a_cancel_says_nothing() {
    let mut desk = desk();
    connected(&mut desk);
    taken(&mut desk, 1, Shot::Cancelled);
    assert!(!desk.finish_screenshot());
    assert!(desk.connection.capture.is_none(), "the next one may start");
    assert!(desk.notice.is_none());
    assert!(desk.connection.uploads.is_empty());
}

#[test]
fn a_failure_is_heard() {
    let mut desk = desk();
    taken(&mut desk, 1, Shot::Failed("no picker".into()));
    assert!(desk.finish_screenshot());
    assert!(desk.notice.is_some());
}

#[test]
fn a_screenshot_never_follows_the_window_to_another_server() {
    let mut desk = desk();
    connected(&mut desk);
    taken(&mut desk, 1, Shot::Taken(b"\x89PNG".to_vec()));
    desk.left_server();
    assert!(desk.finish_screenshot());
    assert!(desk.connection.uploads.is_empty());
    assert!(desk.notice.is_some(), "the person hears why");
}

#[test]
fn an_unfinished_capture_waits() {
    let mut desk = desk();
    taken(&mut desk, 1, Shot::Cancelled);
    *desk
        .connection
        .capture
        .as_ref()
        .unwrap()
        .done
        .lock()
        .unwrap() = None;
    assert!(!desk.finish_screenshot());
    assert!(desk.connection.capture.is_some());
    desk.screenshot();
    assert!(desk.notice.is_some(), "one at a time, and said so");
}

#[test]
fn no_terminal_no_picker() {
    let mut desk = desk();
    desk.screenshot();
    assert!(desk.connection.capture.is_none());
    assert!(desk.notice.is_some());
}

#[test]
fn replacing_the_connection_drops_its_capture() {
    for policy in [
        crate::desk::link::Replacement::Retry,
        crate::desk::link::Replacement::Machine,
    ] {
        let mut desk = desk();
        taken(&mut desk, 1, Shot::Taken(b"\x89PNG".to_vec()));
        desk.replace_connection(policy);
        assert!(desk.connection.capture.is_none());
        assert!(!desk.finish_screenshot(), "nothing left to send");
        assert!(desk.connection.uploads.is_empty());
    }
}

#[test]
fn a_capture_finishing_after_disconnect_never_queues_private_bytes() {
    let mut desk = desk();
    taken(&mut desk, 1, Shot::Taken(vec![7]));
    let client = desk.connection.client.clone().unwrap();
    client.with_control(|control| control.connection_lost(None));
    let _ = client.take_outbound();
    assert!(desk.finish_screenshot());
    assert!(client.take_outbound().is_empty());
    let receipts = client.take_file_upload_receipts();
    assert_eq!(receipts.len(), 1);
    assert_eq!(
        receipts[0].outcome,
        phux_client_runtime::control::FileUploadOutcome::Refused
    );
}

#[test]
fn delayed_private_path_delivery_never_targets_another_connection() {
    let mut desk = desk();
    taken(&mut desk, 1, Shot::Cancelled);
    let capture = desk.connection.capture.take().unwrap();
    let client = desk.connection.client.clone().unwrap();
    client.with_control(|control| {
        control.connection_lost(None);
        control.connection_opened();
    });
    let _ = client.take_outbound();
    assert!(!desk.deliver_private_image(&capture.destination, "/private/image.png "));
    assert!(client.take_outbound().is_empty());
    assert!(desk.connection.pastes.is_empty());
}

#[test]
fn a_completed_upload_receipt_cannot_paste_after_the_connection_changes() {
    use phux_protocol::wire::frame::FileUploadAck;

    let mut desk = desk();
    taken(&mut desk, 1, Shot::Taken(vec![7]));
    let client = desk.connection.client.clone().unwrap();
    let _ = client.take_outbound();
    assert!(desk.finish_screenshot());
    let request = client
        .take_outbound()
        .iter()
        .find_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
            FrameKind::Command {
                request_id,
                command: Command::PutFile { .. },
            } => Some(request_id),
            _ => None,
        })
        .unwrap();
    client
        .feed(FrameKind::CommandResult {
            request_id: request,
            result: CommandResult::OkWith(CommandValue::FileUpload(FileUploadAck {
                next_offset: 1,
                path: Some("/private/image.png".into()),
            })),
        })
        .unwrap();
    client.with_control(|control| {
        control.connection_lost(None);
        control.connection_opened();
    });
    let _ = client.take_outbound();
    assert!(desk.finish_uploads(&client));
    assert!(client.take_outbound().is_empty());
    assert!(desk.connection.uploads.is_empty());
    assert!(desk.connection.pastes.is_empty());
    assert!(desk.notice.as_ref().unwrap().text.contains("not pasted"));
}

#[test]
fn a_success_types_once_to_the_original_pane_without_focus_or_replay() {
    let mut desk = desk();
    taken(&mut desk, 1, Shot::Cancelled);
    let capture = desk.connection.capture.take().unwrap();
    let client = desk.connection.client.clone().unwrap();
    let _ = client.take_outbound();
    assert!(client.input_ready(&ResourceId::local(1)));
    assert!(desk.deliver_private_image(&capture.destination, "/private/image.png "));
    let frames: Vec<_> = client
        .take_outbound()
        .iter()
        .map(|bytes| FrameKind::decode(bytes).unwrap().0)
        .collect();
    assert!(
        matches!(&frames[..], [FrameKind::InputPaste { terminal_id, .. }] if terminal_id == &ResourceId::local(1))
    );
    assert!(
        desk.connection.shown.is_none(),
        "delivery never takes focus"
    );
    assert!(
        desk.connection.pastes.is_empty(),
        "no private path in reconnect journal"
    );
    client.with_control(|control| {
        control.connection_lost(None);
        control.connection_opened();
    });
    assert!(client.take_outbound().iter().all(|bytes| !matches!(
        FrameKind::decode(bytes).unwrap().0,
        FrameKind::InputPaste { .. }
            | FrameKind::Command {
                command: Command::ApplyInput { .. },
                ..
            }
    )));
}
