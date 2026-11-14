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
    let mut control = ControlPlane::new(ControlOptions::default());
    control.connection_opened();
    control.take_outbound();
    control.server = Some(ServerInfo {
        id: vec![1; 16],
        features: ServerFeatureSet::with(&[ServerFeature::FileUpload]),
        features_ext: ServerFeatureExtSet::default(),
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
fn ordinary_file_uploads_keep_their_existing_reconnect_policy() {
    let (mut control, _) = connected();
    control.put_file(ResourceId::local(3), "txt".to_owned(), b"ordinary".to_vec());
    let first = commands(&mut control);
    control.connection_lost(None);
    control.connection_opened();
    control.take_outbound();
    control.handshake_ready = true;
    control.queue_next_upload();
    let replay = commands(&mut control);
    assert_eq!(first[0].1, replay[0].1);
    assert!(control.take_file_upload_receipts().is_empty());
}
