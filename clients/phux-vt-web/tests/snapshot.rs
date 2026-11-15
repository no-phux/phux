//! The official GHOSTSNP decoder against a server-produced transcript and
//! upstream's Kitty animation fixture.

use phux_vt_web::{HistoryProgressKind, NativeCodecError, NativeDecoder, Terminal, Vt};
use wasm_bindgen_test::wasm_bindgen_test;

#[path = "support/server_snapshot.rs"]
mod server_snapshot;
use server_snapshot::{ServerSnapshot, server_snapshot as fixture};

const MAX_BYTES: usize = 64 * 1024 * 1024;

fn feed(decoder: &mut NativeDecoder, bytes: &[u8], cut: usize) {
    for fragment in bytes.chunks(cut) {
        decoder.feed(fragment).expect("stage fragment");
    }
}

fn record(tag: u16, payload: &[u8]) -> Vec<u8> {
    let mut prefix = tag.to_le_bytes().to_vec();
    prefix.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    let mut crc = u32::MAX;
    for byte in prefix.iter().chain(payload) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0x82f6_3b78
            };
        }
    }
    prefix.extend_from_slice(&(!crc).to_le_bytes());
    prefix.extend_from_slice(payload);
    prefix
}

fn last_row(terminal: &Terminal) -> String {
    terminal
        .rows_text()
        .into_iter()
        .rfind(|row| !row.is_empty())
        .unwrap_or_default()
}

async fn ready_decoder(fixture: &ServerSnapshot) -> NativeDecoder {
    let vt = Vt::load().await.expect("load engine");
    let mut decoder = vt.native_decoder(MAX_BYTES).expect("decoder");
    feed(&mut decoder, &fixture.ready, fixture.ready.len());
    decoder.ready().expect("READY");
    decoder
}

#[wasm_bindgen_test]
async fn engine_advertises_the_official_snapshot_decoder() {
    let vt = Vt::load().await.expect("load engine");
    assert!(vt.native_snapshot_available());
}

#[wasm_bindgen_test]
async fn live_clear_discards_deferred_pre_clear_history() {
    let fixture = fixture();
    for clear in [b"\x1b[3J".as_slice(), b"\x1bc".as_slice()] {
        let mut decoder = ready_decoder(&fixture).await;
        decoder.terminal().unwrap().write(clear);
        let cleared_rows = decoder.terminal().unwrap().rows_text().len();
        for (unit, rows) in &fixture.pages {
            feed(&mut decoder, unit, unit.len());
            let progress = decoder
                .next_checked(*rows)
                .expect("authenticate discarded page")
                .expect("discarded page is not FINISH");
            assert_eq!(progress.kind, HistoryProgressKind::Page);
            assert_eq!(progress.authenticated_rows, *rows);
            assert_eq!(
                progress.rows, 0,
                "cleared history must be authenticated, not retained"
            );
        }
        feed(&mut decoder, &fixture.finish, fixture.finish.len());
        assert_eq!(decoder.next_checked(0), Ok(None), "authenticated FINISH");
        assert_eq!(
            decoder.terminal().unwrap().rows_text().len(),
            cleared_rows,
            "pre-clear history must not reappear after {clear:?}"
        );
    }
}

#[wasm_bindgen_test]
async fn server_snapshot_decodes_across_arbitrary_fragment_cuts() {
    let fixture = fixture();
    let vt = Vt::load().await.expect("load engine");
    for cut in [1, 2, 7, 10, 11, 37, 509, 4096, usize::MAX] {
        let mut decoder = vt.native_decoder(MAX_BYTES).expect("decoder");
        assert!(decoder.terminal().is_none(), "nothing renders before READY");
        feed(&mut decoder, &fixture.ready, cut);
        let terminal = decoder.ready().expect("authenticated READY");
        let rows = terminal.rows_text();
        assert_eq!(rows.len(), usize::from(fixture.rows), "cut {cut}");
        assert_eq!(terminal.grid().cols, fixture.cols, "cut {cut}");
        assert_eq!(last_row(terminal), "live-tail", "cut {cut}: {rows:?}");
        assert_eq!(terminal.title(), "phux fixture", "cut {cut}");

        let mut total = 0;
        for (index, (unit, rows)) in fixture.pages.iter().enumerate() {
            feed(&mut decoder, unit, cut);
            let progress = decoder
                .next_checked(*rows)
                .expect("authenticated page")
                .expect("a page, not FINISH");
            assert_eq!(progress.authenticated_rows, *rows);
            assert_eq!(progress.rows, *rows, "page {index} kept");
            assert_eq!(progress.kind, HistoryProgressKind::Page);
            total += rows;
            assert_eq!(decoder.staged_capacity(), 0, "page storage released");
        }
        feed(&mut decoder, &fixture.finish, cut);
        assert_eq!(decoder.next_checked(0), Ok(None), "authenticated FINISH");

        let terminal = decoder.terminal().expect("terminal after FINISH");
        let screen = terminal.screen_rows();
        assert_eq!(screen.first().map(String::as_str), Some("fixture-0000"));
        assert!(screen.len() > total, "history restored: {}", screen.len());
        assert_eq!(decoder.feed(b"x"), Err(NativeCodecError::InvalidState));
    }
}

#[wasm_bindgen_test]
async fn resized_history_pages_are_authenticated_but_not_mistaken_for_finish() {
    let fixture = fixture();
    let mut decoder = ready_decoder(&fixture).await;
    let terminal = decoder.terminal().expect("READY terminal");
    terminal.resize(fixture.cols + 1, fixture.rows, 8, 16);
    terminal.write(b"1mnew output");
    for (unit, rows) in &fixture.pages {
        feed(&mut decoder, unit, 7);
        let progress = decoder
            .next_checked(*rows)
            .expect("page remains authenticated after resize")
            .expect("a dropped page is not FINISH");
        assert_eq!(progress.kind, HistoryProgressKind::Page);
        assert_eq!(progress.authenticated_rows, *rows);
        assert_eq!(progress.rows, 0, "old-width page is discarded");
    }
    feed(&mut decoder, &fixture.finish, 1);
    assert_eq!(decoder.next_checked(0), Ok(None));
    assert_eq!(
        decoder.terminal().expect("terminal survives").grid().cols,
        fixture.cols + 1
    );
}

#[wasm_bindgen_test]
async fn unknown_records_between_page_and_placements_are_authenticated() {
    let fixture = fixture();
    let (page, rows) = &fixture.pages[0];
    for corrupt in [1, 2, 0] {
        let mut decoder = ready_decoder(&fixture).await;
        let before = decoder.terminal().expect("READY").screen_rows();
        let mut unknown = record(4242, b"optional extension");
        if corrupt == 1 {
            *unknown.last_mut().expect("extension payload") ^= 0x40;
        }
        let mut unit = page.clone();
        unit.extend_from_slice(&unknown);
        // Empty placements for the fixture's primary screen.
        let mut placements = record(10, &[0; 6]);
        if corrupt == 2 {
            *placements.last_mut().expect("placements payload") ^= 0x40;
        }
        unit.extend_from_slice(&placements);
        feed(&mut decoder, &unit, 1);
        let progress = decoder.next_checked(*rows);
        if corrupt != 0 {
            assert!(
                matches!(progress, Err(NativeCodecError::InvalidUnit)),
                "suffix CRC case {corrupt}"
            );
            assert_eq!(
                last_row(decoder.terminal().expect("terminal survives")),
                "live-tail"
            );
            assert_eq!(
                decoder.terminal().expect("terminal survives").screen_rows(),
                before,
                "corrupt suffix must not apply its otherwise-valid PAGE"
            );
            continue;
        }
        let progress = progress.expect("extension + placements").expect("PAGE");
        assert_eq!(progress.kind, HistoryProgressKind::Page);
        assert_eq!(progress.rows, *rows);
        for (page, rows) in &fixture.pages[1..] {
            feed(&mut decoder, page, 7);
            assert!(decoder.next_checked(*rows).expect("next page").is_some());
        }
        feed(&mut decoder, &fixture.finish, 1);
        assert_eq!(decoder.next_checked(0), Ok(None));
    }
}

#[wasm_bindgen_test]
async fn bare_primary_pages_leave_the_live_alternate_screen_unchanged() {
    let fixture = fixture();
    let mut decoder = ready_decoder(&fixture).await;
    decoder.terminal().unwrap().write(b"\x1b[?1049halt-live");
    let alternate_before = decoder.terminal().unwrap().screen_rows();
    assert!(last_row(decoder.terminal().unwrap()).ends_with("alt-live"));
    for (page, rows) in &fixture.pages {
        feed(&mut decoder, page, 7);
        let progress = decoder
            .next_checked(*rows)
            .expect("primary history with alternate active")
            .expect("PAGE, not FINISH");
        assert_eq!(progress.authenticated_rows, *rows);
        assert_eq!(progress.kind, HistoryProgressKind::Page);
    }
    feed(&mut decoder, &fixture.finish, 1);
    assert_eq!(decoder.next_checked(0), Ok(None));
    assert_eq!(
        decoder.terminal().unwrap().screen_rows(),
        alternate_before,
        "primary history must not target or change the live alternate screen"
    );
    decoder.terminal().unwrap().write(b"\x1b[?1049l");
    assert_eq!(last_row(decoder.terminal().unwrap()), "live-tail");
}

#[wasm_bindgen_test]
async fn ready_restores_the_parser_continuation() {
    let fixture = fixture();
    let decoder = ready_decoder(&fixture).await;
    let terminal = decoder.terminal().expect("READY terminal");
    // The fixture ends mid-CSI (`ESC [ 3`); finishing it selects red.
    terminal.write(b"1mX");
    let grid = terminal.grid();
    let row = grid
        .cells
        .chunks(usize::from(grid.cols))
        .find(|row| {
            row.iter()
                .map(|cell| cell.ch)
                .collect::<String>()
                .starts_with("live-tail X")
        })
        .expect("continued row");
    assert!(
        row[10].fg.is_some(),
        "the restored SGR applied: {:?}",
        row[10]
    );
    assert!(row[0].fg.is_none());
}

#[wasm_bindgen_test]
async fn malformed_ready_prefixes_fail_closed() {
    let fixture = fixture();
    let vt = Vt::load().await.expect("load engine");
    let mut bad_magic = fixture.ready.clone();
    bad_magic[0] ^= 0xff;
    let mut corrupt = fixture.ready.clone();
    let middle = corrupt.len() / 2;
    corrupt[middle] ^= 0x40;
    let mut trailing = fixture.ready.clone();
    trailing.extend_from_slice(&fixture.pages[0].0);
    let cases: [(&str, &[u8]); 5] = [
        ("empty", &[]),
        ("truncated", &fixture.ready[..fixture.ready.len() - 1]),
        ("bad magic", &bad_magic),
        ("corrupt record", &corrupt),
        ("bytes after READY", &trailing),
    ];
    for (name, bytes) in cases {
        let mut decoder = vt.native_decoder(MAX_BYTES).expect("decoder");
        feed(&mut decoder, bytes, 113);
        assert!(decoder.ready().is_err(), "{name} must not authenticate");
        assert!(decoder.terminal().is_none(), "{name} renders nothing");
        assert_eq!(
            decoder.ready().err(),
            Some(NativeCodecError::InvalidState),
            "{name} poisons the decoder"
        );
    }

    let mut bounded = vt.native_decoder(16).expect("decoder");
    assert_eq!(
        bounded.feed(&fixture.ready),
        Err(NativeCodecError::LimitExceeded)
    );
    let mut early = vt.native_decoder(MAX_BYTES).expect("decoder");
    assert_eq!(
        early.next_checked(0),
        Err(NativeCodecError::InvalidState),
        "history before READY"
    );
}

#[wasm_bindgen_test]
async fn malformed_history_units_fail_closed_and_keep_the_terminal() {
    let fixture = fixture();
    let (page, rows) = &fixture.pages[0];
    let mut corrupt = page.clone();
    let last = corrupt.len() - 1;
    corrupt[last] ^= 0x40;
    let mut two_pages = page.clone();
    two_pages.extend_from_slice(&fixture.pages[1].0);
    let cases: [(&str, &[u8], usize, NativeCodecError); 5] = [
        (
            "truncated",
            &page[..page.len() - 1],
            *rows,
            NativeCodecError::InvalidUnit,
        ),
        (
            "two units",
            &two_pages,
            *rows,
            NativeCodecError::InvalidUnit,
        ),
        (
            "row mismatch",
            page,
            rows + 1,
            NativeCodecError::InvalidUnit,
        ),
        (
            "FINISH with rows",
            &fixture.finish,
            1,
            NativeCodecError::InvalidUnit,
        ),
        ("empty", &[], 0, NativeCodecError::InvalidUnit),
    ];
    for (name, bytes, expected_rows, error) in cases {
        let mut decoder = ready_decoder(&fixture).await;
        let before = decoder.terminal().expect("READY").screen_rows();
        feed(&mut decoder, bytes, 61);
        assert_eq!(decoder.next_checked(expected_rows), Err(error), "{name}");
        let terminal = decoder.terminal().expect("terminal survives");
        assert_eq!(
            terminal.screen_rows(),
            before,
            "{name} left history untouched"
        );
        assert_eq!(last_row(terminal), "live-tail");
        assert_eq!(decoder.staged_capacity(), 0, "{name} released staging");
        assert_eq!(
            decoder.feed(page),
            Err(NativeCodecError::InvalidState),
            "{name} poisons the decoder"
        );
    }

    let mut decoder = ready_decoder(&fixture).await;
    feed(&mut decoder, &corrupt, 61);
    assert!(
        matches!(
            decoder.next_checked(*rows),
            Err(NativeCodecError::InvalidUnit)
        ),
        "a corrupt page fails its checksum"
    );
    assert_eq!(
        last_row(decoder.terminal().expect("terminal survives")),
        "live-tail"
    );
}

/// Upstream's Kitty animation: image records stay distinct from history
/// rows and FINISH even though the freestanding engine renders no graphics.
fn kitty_fixture() -> (Vec<u8>, Vec<Vec<u8>>, Vec<u8>) {
    let bytes: Vec<u8> = include_str!("fixtures/upstream-kitty-animation-v1.hex")
        .lines()
        .flat_map(|line| line.split('#').next().expect("line").split_whitespace())
        .map(|hex| u8::from_str_radix(hex, 16).expect("fixture hex"))
        .collect();
    assert_eq!(&bytes[..10], b"GHOSTSNP\x01\0");
    let mut offset = 10;
    let mut ready_end = None;
    let mut images = Vec::new();
    let mut finish_start = None;
    while offset < bytes.len() {
        let tag = u16::from_le_bytes(bytes[offset..offset + 2].try_into().expect("tag"));
        let len =
            u32::from_le_bytes(bytes[offset + 2..offset + 6].try_into().expect("len")) as usize;
        let end = offset + 10 + len;
        match tag {
            5 => ready_end = Some(end),
            9 => images.push(bytes[offset..end].to_vec()),
            4 => finish_start = Some(offset),
            _ => {}
        }
        offset = end;
    }
    assert_eq!(images.len(), 3, "three image/animation frames");
    (
        bytes[..ready_end.expect("READY")].to_vec(),
        images,
        bytes[finish_start.expect("HISTORY + FINISH")..].to_vec(),
    )
}

#[wasm_bindgen_test]
async fn kitty_image_units_report_progress_not_finish() {
    let (ready, images, finish) = kitty_fixture();
    let vt = Vt::load().await.expect("load engine");
    for cut in [1, 7, usize::MAX] {
        let mut decoder = vt.native_decoder(1024 * 1024).expect("decoder");
        feed(&mut decoder, &ready, cut);
        let terminal = decoder.ready().expect("Kitty-bearing READY");
        assert_eq!((terminal.grid().cols, terminal.grid().rows), (4, 2));
        terminal.write(b"live");
        for image in &images {
            feed(&mut decoder, image, cut);
            let progress = decoder
                .next_checked(0)
                .expect("authenticate image pixels")
                .expect("image is not FINISH");
            assert_eq!(progress.kind, HistoryProgressKind::Image);
            assert_eq!(progress.authenticated_rows, 0);
            assert_eq!(progress.rows, 0);
            assert_eq!(decoder.staged_capacity(), 0);
            assert_eq!(
                decoder.terminal().expect("live terminal").rows_text()[0],
                "live"
            );
        }
        feed(&mut decoder, &finish, cut);
        assert_eq!(decoder.next_checked(0), Ok(None), "authenticated FINISH");
    }
}

#[wasm_bindgen_test]
async fn history_manifests_before_skipped_images_are_delivered_with_finish() {
    let (ready, images, finish) = kitty_fixture();
    let (manifest, finish) = finish.split_at(16);
    let vt = Vt::load().await.expect("load engine");
    let mut decoder = vt.native_decoder(1024 * 1024).expect("decoder");
    feed(&mut decoder, &ready, 7);
    decoder.ready().expect("READY");
    for (index, image) in images.iter().enumerate() {
        if index == 0 {
            feed(&mut decoder, manifest, 1);
        }
        feed(&mut decoder, image, 7);
        let progress = decoder.next_checked(0).expect("skip pixels").expect("IMAGE");
        assert_eq!(progress.kind, HistoryProgressKind::Image);
        assert_eq!(progress.authenticated_rows, 0);
        assert_eq!(progress.rows, 0);
    }
    feed(&mut decoder, finish, 1);
    assert_eq!(decoder.next_checked(0), Ok(None));
}

#[wasm_bindgen_test]
async fn corrupt_or_row_bearing_images_fail_closed() {
    let (ready, images, _) = kitty_fixture();
    let vt = Vt::load().await.expect("load engine");
    for declared_rows in [0, 1] {
        let mut decoder = vt.native_decoder(1024 * 1024).expect("decoder");
        feed(&mut decoder, &ready, 7);
        decoder.ready().expect("READY");
        let before = decoder.terminal().expect("terminal").screen_rows();
        let mut image = images[0].clone();
        if declared_rows == 0 {
            *image.last_mut().expect("pixels") ^= 0x40;
        }
        feed(&mut decoder, &image, 1);
        let error = decoder
            .next_checked(declared_rows)
            .expect_err("reject image");
        if declared_rows == 0 {
            assert!(
                matches!(error, NativeCodecError::InvalidUnit),
                "bad CRC: {error}"
            );
        } else {
            assert_eq!(error, NativeCodecError::InvalidUnit, "images have no rows");
        }
        assert_eq!(
            decoder.terminal().expect("terminal survives").screen_rows(),
            before
        );
        assert_eq!(
            decoder.feed(&images[1]),
            Err(NativeCodecError::InvalidState)
        );
    }
}
