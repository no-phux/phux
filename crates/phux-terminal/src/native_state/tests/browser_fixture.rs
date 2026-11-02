use super::*;

/// The browser decoder's committed server-produced GHOSTSNP transcript,
/// resolved at run time so no checkout path is compiled into the test.
fn browser_fixture_path() -> std::path::PathBuf {
    std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"),
    )
    .join("../../clients/phux-vt-web/tests/fixtures/server-snapshot-v1.bin")
}

/// Capture the browser fixture through the production prefix and leased
/// history paths. Layout: `PHUXSNP1`, cols u16, rows u16, then units of
/// kind u8 (0 READY prefix, 1 history page, 2 FINISH), rows u32, len
/// u32, bytes; integers little-endian.
fn browser_fixture_transcript() -> Vec<u8> {
    const COLS: u16 = 120;
    const ROWS: u16 = 6;
    let mut terminal = GhosttyTerminal::new(COLS, ROWS).expect("fixture terminal");
    terminal
        .set_scrollback_max_lines(Some(100_000))
        .expect("history rows");
    terminal
        .set_scrollback_max_bytes(None)
        .expect("unlimited history bytes");
    let limits = BootstrapLimits::default();
    let mut manager = NativeTerminalManager::new(terminal, 1).expect("native manager");
    manager.vt_write(b"\x1b]2;phux fixture\x07");
    for row in 0..1_000 {
        manager.vt_write(format!("\x1b[3{}mfixture-{row:04}\x1b[0m\r\n", 1 + row % 6).as_bytes());
    }
    // End mid-CSI: READY must carry the parser continuation.
    manager.vt_write(b"live-tail \x1b[3");

    let mut out = b"PHUXSNP1".to_vec();
    out.extend_from_slice(&COLS.to_le_bytes());
    out.extend_from_slice(&ROWS.to_le_bytes());
    let mut unit = |kind: u8, rows: usize, bytes: &[u8]| {
        out.push(kind);
        out.extend_from_slice(&u32::try_from(rows).expect("rows").to_le_bytes());
        out.extend_from_slice(&u32::try_from(bytes.len()).expect("len").to_le_bytes());
        out.extend_from_slice(bytes);
    };

    let mut capture = manager
        .begin_generation_capture(limits, MAX_NATIVE_PREFIX_BYTES, MAX_NATIVE_PREFIX_CHUNKS)
        .expect("generation capture");
    let mut prefix = Vec::new();
    loop {
        let required = match capture.step(&mut []) {
            Err(NativeStateError::OutOfSpace { required_bytes, .. }) => required_bytes,
            other => panic!("probe: {other:?}"),
        };
        let mut exact = vec![0; required];
        let chunk = capture.step(&mut exact).expect("record");
        prefix.extend_from_slice(chunk.bytes);
        if matches!(chunk.kind, NativeCheckpointChunkKind::Ready) {
            break;
        }
    }
    unit(0, 0, &prefix);
    let (cursor, seed) = manager
        .finish_generation_capture(capture)
        .expect("detach READY");
    let bounds = seed.bounds();
    manager
        .install_generation(
            cursor,
            seed,
            bounds,
            bounds.required_reserved_bytes().expect("reservation"),
        )
        .expect("install");
    let mut index = 0;
    loop {
        let record = next_record(&mut manager, &cursor, index, limits);
        if record.finish {
            unit(2, record.rows, &record.bytes);
            break;
        }
        unit(1, record.rows, &record.bytes);
        index += 1;
    }
    out
}

/// What a browser fixture transcript means, independent of the host that
/// encoded it: geometry, the unit kinds, and the terminal the official
/// decoder rebuilds from it. How rows split between READY and history
/// pages follows the encoding host's page capacity, so it is not part of
/// the meaning.
#[derive(Debug, PartialEq, Eq)]
struct DecodedFixture {
    cols: u16,
    rows: u16,
    units: Vec<u8>,
    title: String,
    scrollback_rows: usize,
    /// Every row, scrollback included, as styled VT, after a probe
    /// completes the parser continuation READY carried.
    vt: String,
}

/// Decode a transcript the way the browser does: READY through the feed
/// decoder, then each history page and FINISH checked against its rows.
fn decode_browser_fixture(transcript: &[u8]) -> DecodedFixture {
    use libghostty_vt::fmt::{Format, Formatter, FormatterOptions};
    use libghostty_vt::snapshot::{FeedDecoder, FeedIncrementalDecoder};

    assert_eq!(&transcript[..8], b"PHUXSNP1", "fixture magic");
    let u16_at = |at: usize| u16::from_le_bytes([transcript[at], transcript[at + 1]]);
    let u32_at =
        |at: usize| u32::from_le_bytes(transcript[at..at + 4].try_into().expect("u32 field"));
    let (cols, rows) = (u16_at(8), u16_at(10));
    let mut units = Vec::new();
    let mut decoder = None;
    let mut terminal = None;
    let mut offset = 12;
    while offset < transcript.len() {
        let (kind, unit_rows) = (transcript[offset], u32_at(offset + 1));
        let len = usize::try_from(u32_at(offset + 5)).expect("unit length");
        let bytes = &transcript[offset + 9..offset + 9 + len];
        offset += 9 + len;
        units.push(kind);
        if kind == 0 {
            let mut feed = FeedDecoder::new(MAX_NATIVE_PREFIX_BYTES).expect("feed decoder");
            feed.set_max_continuation_bytes(CONTINUATION_LIMIT)
                .expect("continuation limit");
            feed.feed(bytes).expect("stage READY");
            let mut ready = feed.ready().expect("decode READY");
            ready
                .terminal_mut()
                .set_scrollback_max_lines(Some(100_000))
                .expect("history rows");
            decoder = Some(ready);
            continue;
        }
        let ready = decoder.as_mut().expect("READY precedes history");
        ready.feed(bytes).expect("stage history unit");
        let progress = ready
            .next_checked(usize::try_from(unit_rows).expect("rows"))
            .expect("decode history unit");
        assert_eq!(progress.is_none(), kind == 2, "FINISH ends the stream");
        if kind == 2 {
            terminal = decoder.take().map(FeedIncrementalDecoder::into_terminal);
        }
    }
    let mut terminal = terminal.expect("transcript ends with FINISH");
    terminal.vt_write(b"1mprobe\x1b[0m");
    let title = terminal.title().expect("title").to_owned();
    let scrollback_rows = terminal.scrollback_rows().expect("scrollback rows");
    let mut formatter = Formatter::new(
        &terminal,
        FormatterOptions::new()
            .with_format(Format::Vt)
            .with_trim(true),
    )
    .expect("formatter");
    let vt = formatter.format_alloc(None).expect("format terminal");
    let vt = String::from_utf8(vt.to_vec()).expect("utf-8 terminal text");
    DecodedFixture {
        cols,
        rows,
        units,
        title,
        scrollback_rows,
        vt,
    }
}

/// GHOSTSNP pages carry the encoding host's page layout, so the same
/// terminal encodes to different bytes and page splits on macOS and
/// Linux. The committed fixture is current when it decodes to the same
/// terminal as this host's capture, whichever host blessed it.
#[test]
fn browser_snapshot_fixture_is_the_current_server_capture() {
    let captured = browser_fixture_transcript();
    assert_eq!(
        captured,
        browser_fixture_transcript(),
        "the fixture capture must be deterministic"
    );
    let fixture = browser_fixture_path();
    if std::env::var_os("PHUX_BLESS_WEB_FIXTURE").is_some() {
        std::fs::write(&fixture, &captured).expect("write browser fixture");
    }
    let expected = decode_browser_fixture(&captured);
    assert!(
        expected.vt.contains("fixture-0000") && expected.vt.contains("probe"),
        "the decoded fixture must carry the oldest history row and the \
             continuation probe: {}",
        expected.vt
    );
    let committed = std::fs::read(&fixture).expect("read browser fixture");
    assert!(
        decode_browser_fixture(&committed) == expected,
        "{} is stale; regenerate it with PHUX_BLESS_WEB_FIXTURE=1 \
             cargo nextest run -p phux-server browser_snapshot_fixture",
        fixture.display()
    );
}
