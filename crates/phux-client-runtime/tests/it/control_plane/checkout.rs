use super::*;
use phux_client_runtime::control::{CheckoutFailure, CheckoutOutcome, MAX_CHECKOUT_ANSWERS};
use phux_protocol::checkout::{CheckoutAnswer, CheckoutArea, CheckoutRequest, CheckoutStatus};

fn plane_with_checkout() -> ControlPlane {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    let _ = plane.take_outbound();
    let mut hello = hello_ok(PROTOCOL_VERSION.patch);
    if let FrameKind::HelloOk { server_caps, .. } = &mut hello {
        *server_caps =
            server_caps.with_features_ext(ServerFeatureExtSet::with(&[ServerFeatureExt::Checkout]));
    }
    plane.feed(hello).unwrap();
    let _ = plane.take_outbound();
    plane
}

#[test]
fn checkout_requires_the_capability_and_correlates_typed_answers() {
    let mut plane = ControlPlane::new(ControlOptions::default());
    plane.connection_opened();
    let _ = plane.take_outbound();
    plane.feed(hello_ok(PROTOCOL_VERSION.patch)).unwrap();
    let _ = plane.take_outbound();
    assert_eq!(plane.checkout(terminal(), CheckoutRequest::Status), 0);
    assert!(
        plane.take_outbound().is_empty(),
        "an older server is never asked"
    );

    let mut plane = plane_with_checkout();
    let target = ResourceId::satellite(SatelliteHost::new("peer"), 4);
    let id = plane.checkout(target.clone(), CheckoutRequest::Branches);
    assert_ne!(id, 0);
    assert!(matches!(
        &decode(&plane.take_outbound()[0]),
        FrameKind::Command {
            request_id,
            command: Command::Checkout { terminal_id, request: CheckoutRequest::Branches },
        } if *request_id == id && *terminal_id == target
    ));
    let answer = CheckoutAnswer::Status(CheckoutStatus {
        branch: Some("main".to_owned()),
        ..CheckoutStatus::default()
    });
    plane
        .feed(FrameKind::CommandResult {
            request_id: id,
            result: CommandResult::OkWith(CommandValue::Checkout(Box::new(answer.clone()))),
        })
        .unwrap();
    let answers = plane.take_checkout_answers();
    assert_eq!(answers.len(), 1);
    assert_eq!(answers[0].request_id, id);
    assert_eq!(answers[0].terminal_id, target);
    assert_eq!(
        answers[0].outcome,
        CheckoutOutcome::Answered(Box::new(answer))
    );

    let denied = plane.checkout(terminal(), CheckoutRequest::Status);
    let _ = plane.take_outbound();
    plane
        .feed(FrameKind::CommandResult {
            request_id: denied,
            result: CommandResult::Error {
                code: ErrorCode::UnsupportedSatelliteRoute,
                message: "satellite lacks CHECKOUT".to_owned(),
            },
        })
        .unwrap();
    assert!(matches!(
        &plane.take_checkout_answers()[..],
        [answer] if answer.request_id == denied
            && matches!(answer.outcome, CheckoutOutcome::Failed { failure: CheckoutFailure::Unsupported, .. })
    ));
}

#[test]
fn malformed_or_oversized_requests_send_nothing() {
    let mut plane = plane_with_checkout();
    let unknown_area = CheckoutRequest::Diff {
        path: "a".to_owned(),
        area: CheckoutArea::Unknown,
    };
    assert_eq!(plane.checkout(terminal(), unknown_area), 0);
    let huge = CheckoutRequest::Stage {
        paths: vec!["x".repeat(70 * 1024)],
    };
    assert_eq!(plane.checkout(terminal(), huge), 0);
    assert!(plane.take_outbound().is_empty());
}

#[test]
fn checkouts_are_bounded_and_answered_unanswered_on_disconnect() {
    let mut plane = plane_with_checkout();
    let ids: Vec<_> = (0..MAX_CHECKOUT_ANSWERS)
        .map(|_| plane.checkout(terminal(), CheckoutRequest::Status))
        .collect();
    assert!(ids.iter().all(|id| *id != 0));
    assert_eq!(plane.checkout(terminal(), CheckoutRequest::Status), 0);
    plane.connection_lost(None);
    let answers = plane.take_checkout_answers();
    assert_eq!(answers.len(), MAX_CHECKOUT_ANSWERS);
    assert!(answers.iter().all(|answer| matches!(
        answer.outcome,
        CheckoutOutcome::Failed {
            failure: CheckoutFailure::Unanswered,
            ..
        }
    )));
    let mut answered: Vec<_> = answers.iter().map(|answer| answer.request_id).collect();
    answered.sort_unstable();
    assert_eq!(answered, ids);
    assert_eq!(
        plane.checkout(terminal(), CheckoutRequest::Status),
        0,
        "nothing is sent while disconnected"
    );
}
