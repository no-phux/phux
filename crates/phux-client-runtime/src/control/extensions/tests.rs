use super::*;
use crate::control::ControlOptions;

#[test]
fn an_offline_upload_expires_with_a_lossless_receipt() {
    let mut control = ControlPlane::new(ControlOptions::default());
    let transfer_id = control.put_file(ResourceId::local(7), "png".to_owned(), vec![1, 2]);
    let updated = Instant::now()
        .checked_sub(INPUT_RETRY_HORIZON)
        .and_then(|created_at| {
            control
                .extensions
                .uploads
                .get_mut(&transfer_id)
                .map(|upload| {
                    upload.created_at = created_at;
                })
        });
    assert_eq!(updated, Some(()));

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
