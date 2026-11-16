//! Framing rejects ambiguous units before the decoder can mutate a terminal.

use super::*;

fn record(tag: u16, payload: &[u8]) -> Vec<u8> {
    let mut bytes = tag.to_le_bytes().to_vec();
    bytes.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(&crc32c(&bytes, payload).to_le_bytes());
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
    let mut grouped = record(SNAPSHOT_RECORD_HISTORY, &[0; 6]);
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
    let manifest = record(SNAPSHOT_RECORD_HISTORY, &[0; 6]);
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
    page.extend_from_slice(&record(SNAPSHOT_RECORD_HISTORY, &[0; 6]));
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

#[test]
fn castagnoli_canonical_vectors() {
    for (bytes, expected) in [
        (b"".as_slice(), 0),
        (b"123456789".as_slice(), 0xe306_9283),
        (&[0; 32], 0x8a91_36aa),
        (&[0xff; 32], 0x62a8_ab43),
    ] {
        assert_eq!(crc32c(bytes, &[]), expected);
        for split in 0..=bytes.len() {
            assert_eq!(crc32c(&bytes[..split], &bytes[split..]), expected);
        }
    }
    let increasing: Vec<u8> = (0..32).collect();
    assert_eq!(crc32c(&increasing, &[]), 0x46dd_794e);
}

#[test]
fn every_record_byte_is_covered_before_page_application() {
    let mut unit = record(SNAPSHOT_RECORD_HISTORY, &[0; 6]);
    unit.extend_from_slice(&record(4242, b"before PAGE"));
    unit.extend_from_slice(&record(SNAPSHOT_RECORD_PAGE, &[0, 0, 7, 0]));
    unit.extend_from_slice(&record(0, b"after PAGE"));
    unit.extend_from_slice(&record(SNAPSHOT_RECORD_PAGE_PLACEMENTS, &[0; 6]));
    assert!(history_unit(&unit).is_some());
    for at in 0..unit.len() {
        let mut corrupt = unit.clone();
        corrupt[at] ^= 0x40;
        assert_eq!(history_unit(&corrupt), None, "corruption at {at}");
    }
    for end in 0..unit.len() {
        // Complete PAGE and unknown record boundaries are valid bare units.
        let mut offset = 0;
        let mut valid_bare_boundary = false;
        while let Some((tag, _, next)) = snapshot_record(&unit[..end], offset) {
            offset = next;
            if tag == SNAPSHOT_RECORD_PAGE || unknown_snapshot_record(tag) {
                valid_bare_boundary = history_unit(&unit[..end]).is_some();
            }
        }
        if !valid_bare_boundary {
            assert_eq!(history_unit(&unit[..end]), None, "truncated at {end}");
        }
    }
}

fn manifest(screen_key: u16) -> Vec<u8> {
    let mut payload = [0; 6];
    payload[..2].copy_from_slice(&screen_key.to_le_bytes());
    payload[2..].copy_from_slice(&1_u32.to_le_bytes());
    record(SNAPSHOT_RECORD_HISTORY, &payload)
}

#[test]
fn local_suffix_tracks_the_history_sequence_not_the_active_screen() {
    let mut framing = HistoryFraming::default();
    let page = record(SNAPSHOT_RECORD_PAGE, &[0, 0, 7, 0]);
    assert_eq!(framing.prepare(&page), None, "no HISTORY key yet");
    let mut unit = manifest(0);
    unit.extend_from_slice(&page);
    let (kind, suffix) = framing.prepare(&unit).expect("first PAGE");
    assert_eq!(kind.rows(), 7);
    let suffix = suffix.expect("bare PAGE needs local lookahead");
    assert_eq!(suffix, record(10, &[0; 6]).as_slice());
    assert_eq!(snapshot_record(&suffix, 0), Some((10, 10, 16)));
    assert_eq!(framing.prepare(&page), Some((kind, Some(suffix))));

    let mut switch = manifest(0);
    switch.extend_from_slice(&manifest(1));
    switch.extend_from_slice(&page);
    let (_, alternate_suffix) = framing.prepare(&switch).expect("alternate history");
    let alternate_suffix = alternate_suffix.expect("alternate no-op");
    assert_eq!(&alternate_suffix[10..12], &1_u16.to_le_bytes());
    assert_eq!(
        framing.prepare(&page),
        Some((kind, Some(alternate_suffix))),
        "later pages keep the last manifest key"
    );

    let mut with_placements = page;
    with_placements.extend_from_slice(&alternate_suffix);
    assert_eq!(
        framing.prepare(&with_placements),
        Some((
            HistoryUnit::Page {
                rows: 7,
                placements: true
            },
            None
        ))
    );
    assert_eq!(
        framing.prepare(&record(SNAPSHOT_RECORD_IMAGE, b"pixels")),
        Some((HistoryUnit::Image, None))
    );
    assert_eq!(
        framing.prepare(&record(SNAPSHOT_RECORD_FINISH, &[])),
        Some((HistoryUnit::Finish, None))
    );
}

#[test]
fn rejected_units_do_not_commit_manifest_keys() {
    let mut framing = HistoryFraming::default();
    let mut seed = manifest(0);
    let page = record(SNAPSHOT_RECORD_PAGE, &[0, 0, 7, 0]);
    seed.extend_from_slice(&page);
    framing.prepare(&seed).unwrap();
    let before = framing;

    let mut switched = manifest(1);
    switched.extend_from_slice(&page);
    switched.extend_from_slice(&record(4242, b"optional"));
    for at in 0..switched.len() {
        let mut corrupt = switched.clone();
        corrupt[at] ^= 1;
        assert_eq!(framing.prepare(&corrupt), None, "bad CRC at {at}");
        assert_eq!(framing, before, "invalid units cannot change key");
    }
    let mut short_manifest = record(SNAPSHOT_RECORD_HISTORY, &[1, 0]);
    short_manifest.extend_from_slice(&page);
    assert_eq!(framing.prepare(&short_manifest), None);
    assert_eq!(framing, before);
}

#[test]
fn real_ghostty_records_and_all_truncations() {
    // Keep the published protocol crate's fixtures self-contained. These are
    // real upstream records, not a synthetic decoder-ready terminal snapshot.
    let bytes: Vec<u8> = include_str!("fixtures/kitty-records-v1.hex")
        .lines()
        .flat_map(|line| line.split('#').next().unwrap().split_whitespace())
        .map(|hex| u8::from_str_radix(hex, 16).unwrap())
        .collect();
    let mut offset = snapshot_ready_end(&bytes).expect("real Ghostty READY");
    for end in 0..offset {
        assert_eq!(snapshot_ready_end(&bytes[..end]), None, "READY cut {end}");
    }
    for at in 0..offset {
        let mut corrupt = bytes.clone();
        corrupt[at] ^= 0x40;
        assert_eq!(snapshot_ready_end(&corrupt), None, "READY corruption {at}");
    }
    // Upstream's PAGE is a viewport page; seed a history key to exercise
    // local suffix generation over its original, independently encoded CRC.
    let mut framing = HistoryFraming {
        screen_key: Some(0),
    };
    let mut images = 0;
    while offset < bytes.len() {
        let (tag, _, end) = snapshot_record(&bytes, offset).expect("real record CRC");
        let unit = &bytes[offset..end];
        if tag != SNAPSHOT_RECORD_HISTORY {
            let (kind, _) = framing.prepare(unit).expect("real progress record");
            assert_eq!(kind == HistoryUnit::Finish, tag == SNAPSHOT_RECORD_FINISH);
            for cut in 0..unit.len() {
                assert_eq!(history_unit(&unit[..cut]), None, "history cut {cut}");
            }
        }
        if tag == SNAPSHOT_RECORD_IMAGE {
            assert_eq!(framing.prepare(unit), Some((HistoryUnit::Image, None)));
            let mut corrupt = unit.to_vec();
            *corrupt.last_mut().unwrap() ^= 1;
            assert_eq!(framing.prepare(&corrupt), None);
            images += 1;
        }
        offset = end;
    }
    assert_eq!(images, 3);
}
