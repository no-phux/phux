//! Checkout lowering tests.

use super::*;
use phux_protocol::ResourceId;
use phux_protocol::checkout::CheckoutRefusal;

fn lower(outcome: CheckoutOutcome) -> WireCheckoutAnswer {
    CheckoutResponse {
        request_id: 9,
        terminal_id: ResourceId::local(2),
        outcome,
    }
    .into()
}

#[test]
fn answers_lower_to_typed_swift_records() {
    let status = lower(CheckoutOutcome::Answered(Box::new(CheckoutAnswer::Status(
        CheckoutStatus {
            branch: Some("main".to_owned()),
            ahead: 2,
            entries: vec![CheckoutEntry {
                path: "src/a.rs".to_owned(),
                from: None,
                area: CheckoutArea::Staged,
                code: "M".to_owned(),
                added: Some(3),
                deleted: Some(1),
                binary: false,
            }],
            ..CheckoutStatus::default()
        },
    ))));
    assert_eq!(status.request_id, 9);
    assert_eq!(status.terminal_id, "local:2");
    let WireCheckoutResult::Status { status } = status.result else {
        panic!("status");
    };
    assert_eq!(status.branch.as_deref(), Some("main"));
    assert_eq!(status.ahead, 2);
    assert_eq!(status.entries[0].area, WireCheckoutArea::Staged);
    assert_eq!(status.entries[0].added, Some(3));

    let refused = lower(CheckoutOutcome::Answered(Box::new(
        CheckoutAnswer::Refused(CheckoutRefusal {
            code: phux_protocol::checkout::CheckoutRefusalCode::GhUnavailable,
            message: "no gh".to_owned(),
        }),
    )));
    assert_eq!(
        refused.result,
        WireCheckoutResult::Failed {
            error: WireCheckoutError::GhUnavailable,
            message: "no gh".to_owned()
        }
    );
    let unanswered = lower(CheckoutOutcome::Failed {
        failure: CheckoutFailure::Unanswered,
        message: "gone".to_owned(),
    });
    assert_eq!(
        unanswered.result,
        WireCheckoutResult::Failed {
            error: WireCheckoutError::Unanswered,
            message: "gone".to_owned()
        }
    );
}
