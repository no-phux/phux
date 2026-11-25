use super::*;
use crate::control::{ControlOptions, FileUploadOutcome, ServerInfo};
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, LayerSet, ServerFeature, ServerFeatureExtSet,
    ServerFeatureSet,
};
use phux_protocol::ids::Revision;
use phux_protocol::wire::frame::{
    ActivationInfo, Command, CommandResult, CommandValue, FileUploadAck, FrameKind, OrgFilter,
    OrganizationPage, ResourceRecord,
};

fn record(key: u8, session: u8, activation: u64) -> ResourceRecord {
    ResourceRecord::new(
        ResourceKey::new([key; 16]).unwrap(),
        SessionKey::new([session; 16]).unwrap(),
        Revision::new(1),
    )
    .with_activation(Some(ActivationInfo::new(
        ActivationId::new(activation),
        ServerInstance::new([1; 16]),
        ExecState::Running,
    )))
    .with_live(Some(ResourceId::local(3)))
}

fn seed(control: &mut ControlPlane, record: ResourceRecord) {
    control.organization.begin_seed(OrgFilter::All);
    control.organization.apply_page(
        OrganizationPage::new(AuthorityId::new([1; 16]).unwrap(), Revision::new(1), 0)
            .with_resources(vec![record]),
    );
}

fn connected() -> (ControlPlane, PrivateUploadDestination) {
    let mut control = ControlPlane::new(std::time::Instant::now(), ControlOptions::default());
    control.connection_opened();
    control.take_outbound();
    control.server = Some(ServerInfo {
        id: vec![1; 16],
        features: ServerFeatureSet::with(&[ServerFeature::FileUpload]),
        features_ext: ServerFeatureExtSet::default(),
        features_ext2: phux_protocol::caps::ServerFeatureExt2Set::new(),
        layers: LayerSet::default(),
        protocol: (1, 0, 0),
        profile: BootstrapProfile::SynthesizedVtRaw,
        limits: BootstrapLimits::default(),
    });
    control.handshake_ready = true;
    control.status = Status::Attached;
    seed(&mut control, record(1, 1, 1));
    let destination = control
        .private_upload_destination(&ResourceId::local(3))
        .unwrap();
    (control, destination)
}

fn commands(control: &mut ControlPlane) -> Vec<(u32, Command)> {
    control
        .take_outbound()
        .iter()
        .filter_map(|bytes| match FrameKind::decode(bytes).unwrap().0 {
            FrameKind::Command {
                request_id,
                command,
            } => Some((request_id, command)),
            _ => None,
        })
        .collect()
}

#[test]
fn disconnect_cancels_private_bytes_instead_of_replaying_after_hello() {
    let (mut control, destination) = connected();
    let id = control.put_private_image(&destination, vec![7; 2 * 1024 * 1024]);
    let sent = commands(&mut control);
    assert!(matches!(&sent[0].1, Command::PutFile { offset: 0, .. }));
    control.connection_lost(None);
    let receipts = control.take_file_upload_receipts();
    assert_eq!(receipts[0].transfer_id, id);
    assert_eq!(receipts[0].outcome, FileUploadOutcome::Unknown);
    control.connection_opened();
    control.take_outbound();
    // Even the SAME server returning must not revive a private operation.
    control.handshake_ready = true;
    control.status = Status::Attached;
    seed(&mut control, record(1, 1, 1));
    control.queue_next_upload();
    assert!(commands(&mut control).is_empty());
    assert!(!control.private_upload_destination_is_current(&destination));
}

#[test]
fn private_bytes_never_queue_while_disconnected() {
    let (mut control, destination) = connected();
    control.connection_lost(None);
    let id = control.put_private_image(&destination, vec![7]);
    assert!(commands(&mut control).is_empty());
    let receipts = control.take_file_upload_receipts();
    assert_eq!(receipts[0].transfer_id, id);
    assert_eq!(receipts[0].outcome, FileUploadOutcome::Refused);
}

#[test]
fn recycled_id_session_move_and_new_execution_invalidate_capture_and_delivery() {
    for replacement in [
        record(2, 1, 1),
        record(1, 2, 1),
        record(1, 1, 2),
        record(1, 1, 1).with_activation(Some(ActivationInfo::new(
            ActivationId::new(1),
            ServerInstance::new([2; 16]),
            ExecState::Running,
        ))),
    ] {
        let (mut control, destination) = connected();
        let id = control.put_private_image(&destination, vec![7]);
        let request = commands(&mut control)[0].0;
        seed(&mut control, replacement);
        assert!(!control.private_upload_destination_is_current(&destination));
        control
            .feed(FrameKind::CommandResult {
                request_id: request,
                result: CommandResult::OkWith(CommandValue::FileUpload(FileUploadAck {
                    next_offset: 1,
                    path: Some("/private/image.png".to_owned()),
                })),
            })
            .unwrap();
        let receipts = control.take_file_upload_receipts();
        assert_eq!(receipts[0].transfer_id, id);
        assert_ne!(receipts[0].outcome, FileUploadOutcome::Completed);
        assert!(receipts[0].path.is_none());
        control.put_private_image(&destination, vec![8]);
        assert!(commands(&mut control).is_empty());
    }
}

#[test]
fn no_second_private_chunk_is_sent_to_a_changed_destination() {
    let (mut control, destination) = connected();
    control.put_private_image(&destination, vec![7; 2 * 1024 * 1024]);
    let request = commands(&mut control)[0].0;
    seed(&mut control, record(2, 1, 1));
    control
        .feed(FrameKind::CommandResult {
            request_id: request,
            result: CommandResult::OkWith(CommandValue::FileUpload(FileUploadAck {
                next_offset: 1024 * 1024,
                path: None,
            })),
        })
        .unwrap();
    assert!(commands(&mut control).is_empty());
    assert!(control.take_file_upload_receipts()[0].path.is_none());
}

#[test]
fn unknown_satellite_authority_and_old_server_evidence_fail_closed() {
    let (mut control, destination) = connected();
    assert!(
        control
            .private_upload_destination(&ResourceId::satellite("box", 3))
            .is_none()
    );
    control.organization.begin_seed(OrgFilter::All);
    assert!(!control.private_upload_destination_is_current(&destination));
    seed(&mut control, record(1, 1, 1).with_activation(None));
    assert!(
        control
            .private_upload_destination(&ResourceId::local(3))
            .is_none()
    );
    seed(&mut control, record(1, 1, 1));
    control.server.as_mut().unwrap().id = vec![2; 16];
    assert!(!control.private_upload_destination_is_current(&destination));
}

#[test]
fn authority_changes_and_missing_upload_support_refuse_private_intent() {
    let (mut control, destination) = connected();
    control.organization.begin_seed(OrgFilter::All);
    control.organization.apply_page(
        OrganizationPage::new(AuthorityId::new([2; 16]).unwrap(), Revision::new(1), 0)
            .with_resources(vec![record(1, 1, 1)]),
    );
    assert!(!control.private_upload_destination_is_current(&destination));
    control.server.as_mut().unwrap().features = ServerFeatureSet::default();
    assert!(
        control
            .private_upload_destination(&ResourceId::local(3))
            .is_none()
    );
}

#[test]
fn queued_private_chunks_are_fenced_at_drain_and_release_the_next_upload() {
    let (mut control, destination) = connected();
    let private = control.put_private_image(&destination, b"private".to_vec());
    let ordinary = control.put_file(ResourceId::local(3), "txt".to_owned(), b"ordinary".to_vec());
    seed(&mut control, record(1, 1, 2));
    let sent = commands(&mut control);
    assert_eq!(sent.len(), 1);
    assert!(matches!(&sent[0].1, Command::PutFile { data, .. } if data == b"ordinary"));
    let receipts = control.take_file_upload_receipts();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].transfer_id, private);
    assert_ne!(receipts[0].outcome, FileUploadOutcome::Completed);
    assert!(receipts[0].path.is_none());
    assert!(
        !control.pending.values().any(
            |pending| matches!(pending, crate::control::Pending::PutFile(id) if *id == private)
        )
    );
    assert!(
        control.pending.values().any(
            |pending| matches!(pending, crate::control::Pending::PutFile(id) if *id == ordinary)
        )
    );
    assert!(control.take_outbound().is_empty());
}

#[test]
fn queued_private_chunk_does_not_drain_after_connection_loss() {
    let (mut control, destination) = connected();
    let id = control.put_private_image(&destination, b"private".to_vec());
    control.connection_lost(None);
    assert!(commands(&mut control).is_empty());
    let receipts = control.take_file_upload_receipts();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].transfer_id, id);
    assert_ne!(receipts[0].outcome, FileUploadOutcome::Completed);
    assert!(control.take_file_upload_receipts().is_empty());
}

#[test]
fn settled_private_transfer_does_not_drain_or_publish_a_second_receipt() {
    let (mut control, destination) = connected();
    let id = control.put_private_image(&destination, b"private".to_vec());
    control.strand_extensions("cancelled");
    assert!(control.private_upload_destination_is_current(&destination));
    let receipts = control.take_file_upload_receipts();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].transfer_id, id);
    assert!(commands(&mut control).is_empty());
    assert!(control.take_file_upload_receipts().is_empty());
}

fn ready_for_private_path(control: &mut ControlPlane) {
    use phux_protocol::caps::BootstrapStreamProfile;
    use phux_protocol::ids::{BootstrapId, StreamId};

    let terminal_id = ResourceId::local(3);
    control
        .ensure_engine(
            BootstrapProfile::SynthesizedVtRaw,
            BootstrapLimits::default(),
        )
        .unwrap();
    control.subscriptions.insert(
        &terminal_id,
        phux_protocol::ids::ResourceKind::Terminal,
        crate::control::subscriptions::Origin::SessionAttach,
    );
    let stream_id = StreamId::new(3).unwrap();
    let bootstrap_id = BootstrapId::new(1).unwrap();
    for frame in [
        FrameKind::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: b"inert".to_vec().into(),
        },
        FrameKind::BootstrapReady {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ] {
        control.feed(frame).unwrap();
    }
    control.take_outbound();
    assert!(control.input_ready(&terminal_id));
}

#[test]
fn queued_private_paths_are_fenced_at_drain_without_entering_the_input_journal() {
    let (mut control, destination) = connected();
    ready_for_private_path(&mut control);
    assert!(control.paste_private_image_path(&destination, "/private/image.png "));
    seed(&mut control, record(1, 1, 2));
    assert!(control.take_outbound().is_empty());
    assert!(!control.input_replay.must_order_after(&ResourceId::local(3)));
    control.connection_lost(None);
    control.connection_opened();
    control.take_outbound();
    assert!(commands(&mut control).is_empty());
}

#[test]
fn queued_private_path_does_not_drain_after_connection_loss() {
    let (mut control, destination) = connected();
    ready_for_private_path(&mut control);
    assert!(control.paste_private_image_path(&destination, "/private/image.png "));
    control.connection_lost(None);
    assert!(control.take_outbound().is_empty());
}

#[test]
fn activation_event_fences_already_queued_private_bytes_and_path() {
    let (mut control, destination) = connected();
    ready_for_private_path(&mut control);
    let id = control.put_private_image(&destination, b"private".to_vec());
    assert!(control.paste_private_image_path(&destination, "/private/image.png "));
    control
        .feed(FrameKind::Event {
            terminal: None,
            stamp: None,
            event: phux_protocol::wire::frame::AgentEvent::ActivationChanged {
                resource: ResourceKey::new([1; 16]).unwrap(),
                activation: ActivationId::new(2),
                state: ExecState::Running,
                outcome: None,
            },
        })
        .unwrap();
    assert!(!control.private_upload_destination_is_current(&destination));
    assert!(control.take_outbound().iter().all(|bytes| !matches!(
        FrameKind::decode(bytes).unwrap().0,
        FrameKind::InputPaste { .. }
            | FrameKind::Command {
                command: Command::PutFile { .. },
                ..
            }
    )));
    let receipts = control.take_file_upload_receipts();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].transfer_id, id);
    assert!(receipts[0].path.is_none());
}

#[test]
fn private_filter_preserves_unfenced_frame_order_and_raw_paste_policy() {
    let (mut control, destination) = connected();
    ready_for_private_path(&mut control);
    control.queue_frame(&FrameKind::Ping { nonce: 1 });
    assert!(control.paste_private_image_path(&destination, "/private/image.png "));
    assert!(control.send_paste(
        &ResourceId::local(3),
        b"ordinary".to_vec(),
        PasteTrust::Trusted
    ));
    control.queue_frame(&FrameKind::Ping { nonce: 2 });
    seed(&mut control, record(1, 1, 2));
    let frames: Vec<_> = control
        .take_outbound()
        .iter()
        .map(|bytes| FrameKind::decode(bytes).unwrap().0)
        .collect();
    assert!(matches!(&frames[..], [
        FrameKind::Ping { nonce: 1 },
        FrameKind::InputPaste { event, .. },
        FrameKind::Ping { nonce: 2 },
    ] if event.data == b"ordinary"));
}

#[test]
fn unchanged_private_destination_delivers_chunk_and_path_once() {
    let (mut control, destination) = connected();
    ready_for_private_path(&mut control);
    let id = control.put_private_image(&destination, b"private".to_vec());
    let sent = commands(&mut control);
    assert_eq!(sent.len(), 1);
    control
        .feed(FrameKind::CommandResult {
            request_id: sent[0].0,
            result: CommandResult::OkWith(CommandValue::FileUpload(FileUploadAck {
                next_offset: 7,
                path: Some("/private/image.png".to_owned()),
            })),
        })
        .unwrap();
    let receipts = control.take_file_upload_receipts();
    assert_eq!(receipts[0].transfer_id, id);
    assert_eq!(receipts[0].outcome, FileUploadOutcome::Completed);
    assert!(control.paste_private_image_path(&destination, "/private/image.png "));
    let sent = control.take_outbound();
    assert_eq!(sent.len(), 1);
    assert!(matches!(
        FrameKind::decode(&sent[0]).unwrap().0,
        FrameKind::InputPaste { .. }
    ));
    assert!(control.take_outbound().is_empty());
    assert!(!control.input_replay.must_order_after(&ResourceId::local(3)));
}
