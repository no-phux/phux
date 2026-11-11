//! Framing rejects ambiguous units before the decoder can mutate a terminal.

use super::*;

fn record(tag: u16, payload: &[u8]) -> Vec<u8> {
    let mut bytes = tag.to_le_bytes().to_vec();
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&[0; 4]); // CRC authentication belongs to the engine.
    bytes.extend_from_slice(payload);
    bytes
}

#[test]
fn page_and_placements_form_one_unit() {
    let page = record(SNAPSHOT_RECORD_PAGE, &[0, 0, 7, 0]);
    assert_eq!(
        history_unit(&page),
        Some(HistoryUnit::Page {
            rows: 7,
            placements: false
        })
    );
    let mut grouped = record(SNAPSHOT_RECORD_HISTORY, &[]);
    grouped.extend_from_slice(&page);
    grouped.extend_from_slice(&record(SNAPSHOT_RECORD_PAGE_PLACEMENTS, &[]));
    assert_eq!(
        history_unit(&grouped),
        Some(HistoryUnit::Page {
            rows: 7,
            placements: true
        })
    );

    for extra in [
        record(SNAPSHOT_RECORD_PAGE, &[0, 0, 1, 0]),
        record(SNAPSHOT_RECORD_IMAGE, &[]),
        record(SNAPSHOT_RECORD_FINISH, &[]),
        vec![0],
    ] {
        let mut invalid = grouped.clone();
        invalid.extend_from_slice(&extra);
        assert_eq!(history_unit(&invalid), None, "two progress units");
    }
    grouped.pop();
    assert_eq!(history_unit(&grouped), None, "truncated placements");
    let mut short_page = record(SNAPSHOT_RECORD_PAGE, &[0, 0]);
    short_page.extend_from_slice(&record(SNAPSHOT_RECORD_PAGE_PLACEMENTS, &[]));
    assert_eq!(
        history_unit(&short_page),
        None,
        "PAGE header cannot borrow placement bytes"
    );
}

#[test]
fn image_progress_is_not_finish_or_a_page() {
    let image = record(SNAPSHOT_RECORD_IMAGE, &[1, 2, 3]);
    assert_eq!(history_unit(&image), Some(HistoryUnit::Image));
    assert_eq!(HistoryUnit::Image.rows(), 0);
    let finish = record(SNAPSHOT_RECORD_FINISH, &[]);
    assert_eq!(history_unit(&finish), Some(HistoryUnit::Finish));

    let mut two = image;
    two.extend_from_slice(&finish);
    assert_eq!(history_unit(&two), None, "IMAGE + FINISH is two units");
    assert_eq!(history_unit(&record(SNAPSHOT_RECORD_PAGE, &[0, 0])), None);
    assert_eq!(
        history_unit(&record(SNAPSHOT_RECORD_PAGE_PLACEMENTS, &[])),
        None
    );
}

#[test]
fn history_manifests_and_lengths_are_bounded() {
    let manifest = record(SNAPSHOT_RECORD_HISTORY, &[]);
    let finish = record(SNAPSHOT_RECORD_FINISH, &[]);
    let mut bytes = manifest.repeat(2);
    bytes.extend_from_slice(&finish);
    assert_eq!(history_unit(&bytes), Some(HistoryUnit::Finish));
    let mut invalid = manifest;
    invalid.extend_from_slice(&bytes);
    assert_eq!(history_unit(&invalid), None, "at most two screens");
    assert_eq!(history_unit(&[]), None);

    let mut oversized = finish;
    oversized[2..6].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(history_unit(&oversized), None, "declared payload must fit");
}

#[test]
fn unknown_records_preserve_upstream_extension_skipping_without_merging_units() {
    let unknown = record(4242, &[1, 2, 3]);
    let mut page = unknown.clone();
    page.extend_from_slice(&record(SNAPSHOT_RECORD_HISTORY, &[]));
    page.extend_from_slice(&record(SNAPSHOT_RECORD_PAGE, &[0, 0, 7, 0]));
    page.extend_from_slice(&unknown);
    page.extend_from_slice(&record(SNAPSHOT_RECORD_PAGE_PLACEMENTS, &[]));
    assert_eq!(
        history_unit(&page),
        Some(HistoryUnit::Page {
            rows: 7,
            placements: true
        })
    );
    page.extend_from_slice(&unknown);
    assert_eq!(
        history_unit(&page),
        None,
        "no trailing bytes after placements"
    );

    let mut image = unknown;
    image.extend_from_slice(&record(SNAPSHOT_RECORD_IMAGE, &[]));
    assert_eq!(history_unit(&image), Some(HistoryUnit::Image));
}
