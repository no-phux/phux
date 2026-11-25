#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]
use super::*;
use crate::control::ControlOptions;
use phux_protocol::caps::{ServerFeatureExt, ServerFeatureExtSet};
use phux_protocol::ids::{AuthorityId, ResourceId, Revision, SessionId, SessionKey, WindowId};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, OrganizationPage, SessionRecord,
};
use phux_protocol::wire::info::{ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo};

fn hello(incarnation: u8) -> FrameKind {
    phux_wire_fixtures::HelloOk::new([incarnation; 16], phux_wire_fixtures::l3_caps())
        .features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Organization]))
        .features_ext2(phux_protocol::caps::ServerFeatureExt2Set::with(&[
            phux_protocol::caps::ServerFeatureExt2::SessionEnsure,
        ]))
        .build()
}
fn frames(plane: &mut ControlPlane) -> Vec<FrameKind> {
    plane
        .take_outbound()
        .iter()
        .map(|bytes| FrameKind::decode(bytes).unwrap().0)
        .collect()
}
fn seed_request(frames: &[FrameKind]) -> u32 {
    frames
        .iter()
        .find_map(|frame| match frame {
            FrameKind::Command {
                request_id,
                command: Command::GetOrganization { .. },
            } => Some(*request_id),
            _ => None,
        })
        .expect("organization seed")
}
fn seed(plane: &mut ControlPlane, request: u32, authority: u8, records: Vec<SessionRecord>) {
    plane
        .feed(FrameKind::CommandResult {
            request_id: request,
            result: CommandResult::OkWith(CommandValue::OrganizationPage(Box::new(
                OrganizationPage::new(
                    AuthorityId::new([authority; 16]).unwrap(),
                    Revision::new(1),
                    0,
                )
                .with_sessions(records),
            ))),
        })
        .unwrap();
}
fn record(key: u8, name: &str, live: u32) -> SessionRecord {
    SessionRecord::new(SessionKey::new([key; 16]).unwrap(), name, Revision::new(1))
        .with_live(Some(SessionId::new(live)))
}
fn home() -> ControlPlane {
    let mut plane = ControlPlane::new(
        std::time::Instant::now(),
        ControlOptions {
            attach: Some(AttachTarget::ByName("work".into())),
            viewport: (24, 4),
            ..ControlOptions::default()
        },
    );
    plane.connection_opened();
    let _ = frames(&mut plane);
    plane.feed(hello(1)).unwrap();
    let initial = frames(&mut plane);
    let attach = initial
        .iter()
        .find_map(|frame| match frame {
            FrameKind::Attach { attach_id, .. } => Some(*attach_id),
            _ => None,
        })
        .unwrap();
    seed(
        &mut plane,
        seed_request(&initial),
        1,
        vec![record(7, "work", 1)],
    );
    let session = SessionId::new(1);
    let window = WindowId::new(1);
    let snapshot = SessionSnapshot::new(session, window, ResourceId::local(1))
        .with_sessions(vec![SessionInfo::new(session, "work")])
        .with_windows(vec![WindowInfo::new(window, session, "shell")])
        .with_resources(vec![ResourceInfo::new(ResourceId::local(1), window, 24, 4)]);
    plane
        .feed(phux_wire_fixtures::attached(attach, snapshot, 1))
        .unwrap();
    for frame in
        phux_wire_fixtures::bootstrap_frames(&ResourceId::local(1), [1, 1], [24, 4], b"original")
    {
        plane.feed(frame).unwrap();
    }
    plane
        .feed(FrameKind::AttachReady { attach_id: attach })
        .unwrap();
    let _ = frames(&mut plane);
    assert_eq!(plane.status(), Status::Attached);
    plane
}

#[test]
fn restart_recovery_waits_for_exact_authority_key_and_never_follows_reused_name_or_id() {
    let mut plane = home();
    plane.connection_lost(None);
    plane.connection_opened();
    let _ = frames(&mut plane);
    plane.feed(hello(2)).unwrap();
    let before_seed = frames(&mut plane);
    assert!(
        !before_seed
            .iter()
            .any(|frame| matches!(frame, FrameKind::Attach { .. }))
    );
    seed(
        &mut plane,
        seed_request(&before_seed),
        1,
        vec![record(7, "renamed", 99), record(8, "work", 1)],
    );
    let after_seed = frames(&mut plane);
    assert!(after_seed.iter().any(|frame| matches!(frame,
        FrameKind::Attach { target: AttachTarget::ById(id), .. } if id.get() == 99)));
    assert!(!after_seed.iter().any(|frame| matches!(
        frame,
        FrameKind::Attach {
            target: AttachTarget::ByName(_) | AttachTarget::CreateIfMissing { .. },
            ..
        }
    )));
}

#[test]
fn missing_or_foreign_key_refuses_recovery_without_create_or_late_reinstallation() {
    for authority in [1, 2] {
        let mut plane = home();
        plane.connection_lost(None);
        plane.connection_opened();
        let _ = frames(&mut plane);
        plane.feed(hello(2)).unwrap();
        let initial = frames(&mut plane);
        let records = if authority == 1 {
            vec![record(8, "work", 1)]
        } else {
            vec![record(7, "work", 1)]
        };
        seed(&mut plane, seed_request(&initial), authority, records);
        assert_eq!(plane.status(), Status::Failed);
        assert!(!frames(&mut plane).iter().any(|frame| matches!(
            frame,
            FrameKind::Attach { .. }
                | FrameKind::SpawnResource { .. }
                | FrameKind::Command {
                    command: Command::SessionCreate { .. } | Command::SessionEnsure { .. },
                    ..
                }
        )));
        assert!(plane.org.pending_attach.is_none());
    }
}

#[test]
fn attach_legacy_create_target_is_unavailable_without_any_creation_side_effect() {
    let mut plane = home();
    assert!(!plane.attach_session(AttachTarget::CreateIfMissing {
        name: "new".into(),
        command: None,
        cwd: None
    }));
    assert!(frames(&mut plane).is_empty());
    assert!(plane.pending_organization_operations().next().is_none());
    assert_eq!(plane.selected_session(), Some(1));
    assert!(
        plane
            .last_error()
            .unwrap()
            .contains("attach does not create")
    );
}

#[test]
fn initial_runtime_attach_rejects_creation_before_queuing_any_interest_or_work() {
    let mut plane = ControlPlane::new(
        std::time::Instant::now(),
        ControlOptions {
            attach: Some(AttachTarget::CreateIfMissing {
                name: "new".into(),
                command: None,
                cwd: None,
            }),
            ..ControlOptions::default()
        },
    );
    plane.connection_opened();
    let _ = frames(&mut plane);
    assert!(matches!(
        plane.feed(hello(1)),
        Err(ControlError::Refused(_))
    ));
    assert!(frames(&mut plane).is_empty());
    assert!(plane.pending_organization_operations().next().is_none());
}

#[test]
fn caller_takeover_retires_pending_automatic_selection_before_late_seed_completion() {
    let mut plane = home();
    plane.connection_lost(None);
    plane.connection_opened();
    let _ = frames(&mut plane);
    plane.feed(hello(2)).unwrap();
    let initial = frames(&mut plane);
    plane.set_session_recovery(crate::control::SessionRecovery::CallerValidated);
    seed(
        &mut plane,
        seed_request(&initial),
        1,
        vec![record(7, "renamed", 99)],
    );
    assert!(plane.org.pending_attach.is_none());
    assert!(
        !frames(&mut plane)
            .iter()
            .any(|frame| matches!(frame, FrameKind::Attach { .. }))
    );
    assert_eq!(plane.status(), Status::Negotiated);
}
