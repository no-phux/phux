use super::*;
use libghostty_vt::snapshot::FeedDecoder;

fn limits() -> SnapshotCaptureLimits {
    SnapshotCaptureLimits {
        max_bytes: 64 * 1024 * 1024,
        max_record_bytes: 1024 * 1024,
        max_records: 10_000,
        max_rows: 10_000,
    }
}

fn record(tag: u16, payload: &[u8]) -> Vec<u8> {
    let mut bytes = tag.to_le_bytes().to_vec();
    bytes.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(&[0; 4]); // Indexing trusts the encoder; native validates CRCs.
    bytes.extend_from_slice(payload);
    bytes
}

fn framed(records: &[(u16, &[u8])]) -> Bytes {
    let mut bytes = ENVELOPE.to_vec();
    for &(tag, payload) in records {
        bytes.extend(record(tag, payload));
    }
    bytes.into()
}

fn assert_exact_units(snapshot: &EncodedSnapshot, stock: &[u8]) {
    let mut concatenated = Vec::new();
    let mut end = 0;
    for range in snapshot
        .prefix_records
        .iter()
        .chain(snapshot.history.iter().map(|unit| &unit.range))
    {
        assert_eq!(range.start, end);
        let slice = snapshot.bytes.slice(range.clone());
        assert_eq!(slice.as_ptr(), snapshot.bytes[range.clone()].as_ptr());
        concatenated.extend_from_slice(&slice);
        end = range.end;
    }
    assert_eq!(end, stock.len());
    assert_eq!(concatenated, stock);
    assert_eq!(
        snapshot.history.iter().filter(|unit| unit.finish).count(),
        1
    );
    assert!(snapshot.history.last().unwrap().finish);
}

#[test]
fn stock_bytes_are_unchanged_with_images_both_screens_and_continuation() {
    let mut terminal = Terminal::new(200, 3).unwrap();
    terminal.set_scrollback_max_bytes(Some(usize::MAX)).unwrap();
    terminal.set_scrollback_max_lines(Some(5000)).unwrap();
    terminal
        .set_continuation_max_bytes(64 * 1024 * 1024)
        .unwrap();
    phux_protocol::kitty_replay::configure_terminal_for_kitty_graphics(&mut terminal).unwrap();
    terminal.resize(200, 3, 8, 16).unwrap();
    terminal.vt_write(b"\x1b_Ga=T,f=32,s=1,v=1,i=7,q=2;/wAA/w==\x1b\\");
    terminal.vt_write(&b"history\r\n".repeat(3000));
    terminal.vt_write(b"\x1b[?1049halternate\r\n\x1b_Ga=T,f=32,s=1,v=1,i=8,q=2;/wAA/w==\x1b\\");
    terminal.vt_write(b"\x1b]2;unfinished");
    let mut stock = Vec::new();
    terminal.encode_snapshot(&mut stock).unwrap();
    let snapshot = EncodedSnapshot::capture(&terminal, limits()).unwrap();
    assert_exact_units(&snapshot, &stock);
    // Capturing retains neither a terminal borrow nor live history references.
    terminal.vt_write(b" title\x07\x1b[?1049l\x1b[3Jnew output");
    terminal.resize(80, 24, 8, 16).unwrap();
    assert_eq!(snapshot.bytes.as_ref(), stock);
    assert!(snapshot.history.iter().filter(|unit| unit.rows > 0).count() > 1);
    assert!(
        snapshot
            .history
            .iter()
            .any(|unit| unit.rows == 0 && !unit.finish)
    );

    let mut decoder = FeedDecoder::new(limits().max_bytes).unwrap();
    for range in &snapshot.prefix_records {
        decoder.feed(&snapshot.bytes[range.clone()]).unwrap();
    }
    let mut decoder = decoder.ready().unwrap();
    for unit in &snapshot.history {
        decoder.feed(&snapshot.bytes[unit.range.clone()]).unwrap();
        let progress = decoder.next_checked(unit.rows).unwrap();
        assert_eq!(progress.is_none(), unit.finish);
    }
}

#[test]
fn unknown_records_and_page_placements_follow_staged_grammar() {
    let bytes = framed(&[
        (1, &[]),
        (READY, &[]),
        (HISTORY, &[]),
        (11, &[]),
        (PAGE, &[0, 0, 2, 0]),
        (12, &[]),
        (PLACEMENTS, &[]),
        (13, &[]),
        (IMAGE, &[1]),
        (HISTORY, &[]),
        (HISTORY, &[]),
        (0, &[]),
        (FINISH, &[]),
    ]);
    let snapshot = EncodedSnapshot::index(bytes.clone(), limits()).unwrap();
    assert_exact_units(&snapshot, &bytes);
    assert_eq!(snapshot.history.len(), 3);
    assert_eq!(snapshot.history[0].range.len(), 54);
    assert_eq!(snapshot.history[0].rows, 2);
    assert_eq!(snapshot.history[1].range.len(), 21);
    assert!(!snapshot.history[1].finish);
    assert_eq!(snapshot.history[2].range.len(), 40);
}

#[test]
fn rejects_incomplete_invalid_and_trailing_framing() {
    let cases = [
        Bytes::from_static(b"GHOSTSNP\x02\x00"),
        framed(&[(FINISH, &[])]),
        framed(&[(READY, &[1]), (FINISH, &[])]),
        framed(&[(READY, &[])]),
        framed(&[(READY, &[]), (PAGE, &[0, 0, 1])]),
        framed(&[(READY, &[]), (PAGE, &[0, 0, 0, 0]), (FINISH, &[])]),
        framed(&[(READY, &[]), (FINISH, &[1])]),
        framed(&[(READY, &[]), (READY, &[]), (FINISH, &[])]),
        framed(&[(READY, &[]), (FINISH, &[]), (11, &[])]),
        framed(&[(READY, &[]), (PLACEMENTS, &[]), (FINISH, &[])]),
        framed(&[
            (READY, &[]),
            (HISTORY, &[]),
            (HISTORY, &[]),
            (HISTORY, &[]),
            (FINISH, &[]),
        ]),
    ];
    for bytes in cases {
        assert!(matches!(
            EncodedSnapshot::index(bytes, limits()),
            Err(SnapshotCaptureError::InvalidSnapshot)
        ));
    }
    let complete = framed(&[(READY, &[]), (FINISH, &[])]);
    for end in 0..complete.len() {
        assert!(matches!(
            EncodedSnapshot::index(complete.slice(..end), limits()),
            Err(SnapshotCaptureError::InvalidSnapshot)
        ));
    }
}

#[test]
fn bounds_cover_aggregate_records_groups_and_page_rows() {
    let bytes = framed(&[
        (READY, &[]),
        (HISTORY, &[]),
        (PAGE, &[0, 0, 2, 0]),
        (FINISH, &[]),
    ]);
    for bounded in [
        SnapshotCaptureLimits {
            max_bytes: bytes.len() - 1,
            ..limits()
        },
        SnapshotCaptureLimits {
            max_record_bytes: 9,
            ..limits()
        },
        SnapshotCaptureLimits {
            max_record_bytes: 13,
            ..limits()
        },
        SnapshotCaptureLimits {
            max_record_bytes: 23,
            ..limits()
        },
        SnapshotCaptureLimits {
            max_records: 3,
            ..limits()
        },
        SnapshotCaptureLimits {
            max_rows: 1,
            ..limits()
        },
    ] {
        assert!(matches!(
            EncodedSnapshot::index(bytes.clone(), bounded),
            Err(SnapshotCaptureError::LimitExceeded)
        ));
    }
    EncodedSnapshot::index(
        bytes.clone(),
        SnapshotCaptureLimits {
            max_bytes: bytes.len(),
            max_record_bytes: 24,
            max_records: 4,
            max_rows: 2,
        },
    )
    .unwrap();
}

#[test]
fn sink_rejects_before_growing_and_capture_preserves_error_kind() {
    let mut sink = BoundedWriter {
        bytes: Vec::new(),
        max_bytes: 4,
        error: None,
    };
    sink.write_all(b"1234").unwrap();
    let capacity = sink.bytes.capacity();
    assert!(sink.write_all(b"5").is_err());
    assert_eq!(sink.bytes, b"1234");
    assert_eq!(sink.bytes.capacity(), capacity);
    assert!(sink.write(b"").is_err());
    assert!(matches!(
        sink.error,
        Some(SnapshotCaptureError::LimitExceeded)
    ));
    let terminal = Terminal::new(2, 2).unwrap();
    assert!(matches!(
        EncodedSnapshot::capture(
            &terminal,
            SnapshotCaptureLimits {
                max_bytes: 4,
                ..limits()
            }
        ),
        Err(SnapshotCaptureError::LimitExceeded)
    ));
    let mut terminal = Terminal::new(2, 2).unwrap();
    terminal.vt_write(b"\x1b[3");
    assert!(matches!(
        EncodedSnapshot::capture(&terminal, limits()),
        Err(SnapshotCaptureError::Engine(_))
    ));
}

#[test]
fn many_small_writes_grow_geometrically_without_exceeding_the_budget() {
    for budget in [0, 1, 7, 4095, 4096, 4097, 32_769] {
        let mut sink = BoundedWriter {
            bytes: Vec::new(),
            max_bytes: budget,
            error: None,
        };
        let mut growths = 0;
        for _ in 0..budget {
            let capacity = sink.bytes.capacity();
            sink.write_all(b"x").unwrap();
            growths += usize::from(sink.bytes.capacity() != capacity);
            assert!(sink.bytes.len() <= budget);
            assert!(sink.bytes.capacity() <= budget);
        }
        assert!(growths <= 5, "budget {budget} required {growths} growths");
        assert_eq!(sink.bytes.len(), budget);
        assert!(sink.write_all(b"x").is_err());
        assert!(matches!(
            sink.error,
            Some(SnapshotCaptureError::LimitExceeded)
        ));
        assert!(sink.bytes.capacity() <= budget);
    }
}

#[test]
fn a_large_write_reserves_required_bytes_relative_to_length() {
    let mut sink = BoundedWriter {
        bytes: Vec::new(),
        max_bytes: 20_003,
        error: None,
    };
    sink.write_all(b"x").unwrap();
    assert_eq!(sink.bytes.capacity(), INITIAL_CAPACITY);
    sink.write_all(&vec![1; 10_000]).unwrap();
    assert_eq!(sink.bytes.len(), 10_001);
    assert_eq!(sink.bytes.capacity(), 10_001);
    sink.write_all(b"x").unwrap();
    assert_eq!(sink.bytes.capacity(), 20_002);
    sink.write_all(&vec![2; 10_001]).unwrap();
    assert_eq!(sink.bytes.len(), 20_003);
    assert_eq!(sink.bytes.capacity(), 20_003);
}

#[test]
fn index_metadata_growth_is_amortized() {
    let mut records = Vec::new();
    let mut growths = 0;
    for index in 0..10_000 {
        let capacity = records.capacity();
        push(&mut records, index).unwrap();
        growths += usize::from(records.capacity() != capacity);
    }
    assert!(growths <= 16, "metadata required {growths} growths");
    assert_eq!(records, (0..10_000).collect::<Vec<_>>());
}
