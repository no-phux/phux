use super::*;
use phux_protocol::snapshot_framing::history_unit;

fn record(tag: u16, payload: &[u8]) -> Vec<u8> {
    let mut bytes = tag.to_le_bytes().to_vec();
    bytes.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
    let mut crc = u32::MAX;
    for byte in bytes.iter().chain(payload) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0x82f6_3b78
            };
        }
    }
    bytes.extend_from_slice(&(!crc).to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

fn manifest(key: u16) -> Vec<u8> {
    let mut payload = [0; 6];
    payload[..2].copy_from_slice(&key.to_le_bytes());
    record(4, &payload)
}

fn stage(
    history: &mut BrowserHistory,
    bytes: Vec<u8>,
) -> Result<Option<Vec<u8>>, NativeCodecError> {
    let kind = history_unit(&bytes).expect("full framing and CRC preflight");
    history.stage(bytes, kind)
}

#[test]
fn page_keeps_original_prefix_but_drops_all_unsupported_suffix_records() {
    let mut history = BrowserHistory::default();
    let prefix = [
        manifest(0),
        record(4242, b"before PAGE"),
        record(3, &[0, 0, 7, 0]),
    ]
    .concat();
    assert_eq!(
        stage(&mut history, prefix.clone()),
        Ok(Some(prefix.clone()))
    );
    let with_suffix = [
        prefix.clone(),
        record(4242, b"after PAGE"),
        record(10, &[0; 6]),
    ]
    .concat();
    assert_eq!(stage(&mut history, with_suffix), Ok(Some(prefix)));
    assert_eq!(history.len, 0);
}

#[test]
fn images_are_skipped_without_buffering_optional_records_or_pixels() {
    let mut history = BrowserHistory::default();
    let image = [record(4242, b"optional"), record(9, &[0xff; 4096])].concat();
    for _ in 0..10 {
        assert_eq!(stage(&mut history, image.clone()), Ok(None));
        assert_eq!(history.len, 0, "no image/extension accumulation");
    }
}

#[test]
fn manifests_before_images_reach_the_next_page_once() {
    let mut history = BrowserHistory::default();
    let first = manifest(0);
    let image = [first.clone(), record(9, b"pixels")].concat();
    assert_eq!(stage(&mut history, image), Ok(None));
    assert_eq!(history.len, MANIFEST_BYTES);
    let page = record(3, &[0, 0, 7, 0]);
    let mut expected = first;
    expected.extend_from_slice(&page);
    assert_eq!(stage(&mut history, page.clone()), Ok(Some(expected)));
    assert_eq!(history.len, 0);
    assert_eq!(stage(&mut history, page.clone()), Ok(Some(page)));
}

#[test]
fn two_manifests_can_span_skipped_images_and_finish() {
    let mut history = BrowserHistory::default();
    for key in 0..2 {
        let image = [
            manifest(key),
            record(4242, b"ignored"),
            record(9, b"pixels"),
        ]
        .concat();
        assert_eq!(stage(&mut history, image), Ok(None));
    }
    assert_eq!(history.len, 32);
    let before = history.manifests;
    for progress in [
        record(9, b"pixels"),
        record(3, &[0, 0, 7, 0]),
        record(6, &[]),
    ] {
        let third = [manifest(1), progress].concat();
        assert_eq!(
            stage(&mut history, third),
            Err(NativeCodecError::InvalidUnit),
            "pending plus current manifests are bounded before engine mutation"
        );
        assert_eq!(history.manifests, before);
        assert_eq!(history.len, 32);
    }
    let finish = record(6, &[]);
    let expected = [manifest(0), manifest(1), finish.clone()].concat();
    assert_eq!(stage(&mut history, finish), Ok(Some(expected)));
    assert_eq!(history.len, 0);
}

#[test]
fn current_manifests_are_not_duplicated_after_deferred_manifests() {
    let mut history = BrowserHistory::default();
    assert_eq!(
        stage(&mut history, [manifest(0), record(9, b"pixels")].concat()),
        Ok(None)
    );
    let unit = [manifest(1), record(6, &[])].concat();
    let expected = [manifest(0), unit.clone()].concat();
    assert_eq!(stage(&mut history, unit), Ok(Some(expected)));
}

#[test]
fn defensive_record_walk_rejects_truncation_and_oversized_manifests() {
    let mut history = BrowserHistory::default();
    for invalid in [vec![], vec![0; 9], record(4, &[0; 7])] {
        assert_eq!(
            history.stage(invalid, HistoryUnit::Image),
            Err(NativeCodecError::InvalidUnit)
        );
        assert_eq!(history.len, 0);
    }
}
