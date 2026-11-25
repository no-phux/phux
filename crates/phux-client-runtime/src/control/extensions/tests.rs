use super::*;
use crate::control::ControlOptions;

#[test]
fn an_offline_upload_expires_with_a_lossless_receipt() {
    let mut control = ControlPlane::new(std::time::Instant::now(), ControlOptions::default());
    let transfer_id = control.put_file(ResourceId::local(7), "png".to_owned(), vec![1, 2]);
    let created = control.observed_time();
    assert!(!control.expire_uploads());
    assert!(control.take_file_upload_receipts().is_empty());
    control.observe_time(
        (created + INPUT_RETRY_HORIZON)
            .checked_sub(std::time::Duration::from_millis(1))
            .unwrap(),
    );
    assert!(!control.expire_uploads());
    control.observe_time(created + INPUT_RETRY_HORIZON);

    assert!(control.expire_uploads());
    assert_eq!(
        control.take_file_upload_receipts(),
        vec![FileUploadReceipt {
            transfer_id,
            outcome: FileUploadOutcome::Refused,
            path: None,
            code: None,
            message: "the file-upload retry horizon expired".to_owned(),
        }]
    );
}

#[test]
fn a_regressing_observation_cannot_extend_a_queued_upload_horizon() {
    let origin = Instant::now();
    let mut control = ControlPlane::new(origin, ControlOptions::default());
    control.observe_time(origin + std::time::Duration::from_secs(5));
    let transfer = control.put_file(ResourceId::local(7), "png".to_owned(), vec![1]);
    control.observe_time(origin);
    control.observe_time(origin + INPUT_RETRY_HORIZON);
    assert!(
        !control.expire_uploads(),
        "submission used the latest observed instant"
    );
    control.observe_time(origin + std::time::Duration::from_secs(5) + INPUT_RETRY_HORIZON);
    assert!(control.expire_uploads());
    let receipts = control.take_file_upload_receipts();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].transfer_id, transfer);
    assert_eq!(receipts[0].outcome, FileUploadOutcome::Refused);
    assert!(
        !control.expire_uploads(),
        "expiry publishes exactly one receipt"
    );
}
