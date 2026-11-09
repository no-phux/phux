//! What the audit log records as a refusal, a hold, and its decision cross
//! the wire, and that no payload byte reaches it.

use super::*;

const SECRET: &[u8] = b"hunter2-never-logged";

#[tokio::test(flavor = "current_thread")]
async fn refusals_holds_and_decisions_are_audited_without_payload_bytes() {
    use crate::audit::{Kind, Outcome, Transport};
    let dir = tempfile::tempdir().unwrap();
    crate::audit::capture_for_test(dir.path(), crate::audit::ROTATE_BYTES);
    LocalSet::new()
        .run_until(async {
            let world = world(|_, _| {
                vec![
                    (REQUESTER, vec!["observe,bind,?signal@global".to_owned()]),
                    (APPROVER, vec!["observe,bind,signal@global".to_owned()]),
                    (UNSIGNALED, vec!["observe,bind@global".to_owned()]),
                ]
            });
            let requester = connect(&world.state, REQUESTER, TransportType::Quic).await;
            let approver = connect(&world.state, APPROVER, TransportType::Quic).await;
            let unsignaled = connect(&world.state, UNSIGNALED, TransportType::Quic).await;

            unsignaled.send(&kill(9, &world.alpha));
            let result = unsignaled.result(9).await;
            assert!(is_denied(&result, "permission denied"), "{result:?}");
            // Input and file bytes it may not send are refused, and never
            // written down.
            unsignaled.send(&command(
                10,
                Command::PutFile {
                    upload_id: phux_protocol::ids::FileUploadId::new([1; 16]).unwrap(),
                    terminal_id: world.alpha.clone(),
                    extension: "txt".to_owned(),
                    offset: 0,
                    data: SECRET.to_vec(),
                    final_chunk: true,
                    sha256: None,
                },
            ));
            let result = unsignaled.result(10).await;
            assert!(is_denied(&result, "permission denied"), "{result:?}");
            unsignaled.send(&FrameKind::InputPaste {
                terminal_id: world.alpha.clone(),
                event: phux_protocol::input::paste::PasteEvent {
                    trust: phux_protocol::input::paste::PasteTrust::Untrusted,
                    data: SECRET.to_vec(),
                },
            });

            requester.send(&kill(HELD, &world.alpha));
            let id = the_pending_approval(&world.state).await;
            approver.send(&decision(11, id, APPROVAL_DENY));
            let result = requester.result(HELD).await;
            assert!(is_denied(&result, "approval denied"), "{result:?}");
            for peer in [requester, approver, unsignaled] {
                peer.close().await;
            }
        })
        .await;
    crate::audit::release_test_capture();

    let raw = std::fs::read(dir.path().join(crate::audit::LOG_FILE)).unwrap();
    assert!(
        !raw.windows(SECRET.len()).any(|window| window == SECRET),
        "a payload reached the audit log"
    );
    let records: Vec<crate::audit::AuditRecord> = crate::audit::read_records(dir.path())
        .unwrap()
        .into_iter()
        .map(Result::unwrap)
        .collect();
    let find = |kind, action: &str| {
        records
            .iter()
            .find(|record| record.kind == kind && record.action == action)
            .unwrap_or_else(|| panic!("no {kind:?} {action} in {records:#?}"))
    };
    let refused = find(Kind::CommandRefused, "KILL_RESOURCE");
    assert_eq!(refused.outcome, Outcome::Denied);
    assert_eq!(
        refused.actor.credential.as_deref(),
        Some(credential_id(UNSIGNALED).as_str())
    );
    assert_eq!(refused.actor.transport, Transport::Quic);

    let put = find(Kind::CommandRefused, "PUT_FILE");
    assert_eq!(put.detail["bytes"], SECRET.len());
    find(Kind::CommandRefused, "INPUT_PASTE");

    let held = find(Kind::ApprovalHeld, "KILL_RESOURCE");
    assert_eq!(
        held.actor.credential.as_deref(),
        Some(credential_id(REQUESTER).as_str())
    );
    let decided = find(Kind::ApprovalDecided, "KILL_RESOURCE");
    assert_eq!(decided.outcome, Outcome::Rejected);
    assert_eq!(decided.detail["ended"], "denied");
    assert_eq!(
        decided.actor.credential.as_deref(),
        Some(credential_id(APPROVER).as_str())
    );
    assert_eq!(held.detail["id"], decided.detail["id"]);
    assert!(
        held.target
            .as_deref()
            .is_some_and(|t| t.starts_with("terminal:"))
    );
    assert_eq!(refused.target, held.target);

    let report = crate::audit::verify_dir(dir.path()).unwrap();
    assert_eq!(report.failure, None);
}
