//! The official GHOSTSNP decoder against a server-produced transcript.
//!
//! `fixtures/server-snapshot-v1.bin` is captured by phux-server's production
//! prefix and leased-history paths (`native_state` test
//! `browser_snapshot_fixture_is_the_current_server_capture`, which fails when
//! the committed bytes drift from the server's output).

use phux_vt_web::{NativeCodecError, NativeDecoder, Terminal, Vt};
use wasm_bindgen_test::wasm_bindgen_test;

const FIXTURE: &[u8] = include_bytes!("fixtures/server-snapshot-v1.bin");
const MAX_BYTES: usize = 64 * 1024 * 1024;

struct Fixture {
    cols: u16,
    rows: u16,
    ready: Vec<u8>,
    pages: Vec<(Vec<u8>, usize)>,
    finish: Vec<u8>,
}

fn fixture() -> Fixture {
    assert_eq!(&FIXTURE[..8], b"PHUXSNP1");
    let u16_at = |at: usize| u16::from_le_bytes([FIXTURE[at], FIXTURE[at + 1]]);
    let u32_at = |at: usize| {
        u32::from_le_bytes([
            FIXTURE[at],
            FIXTURE[at + 1],
            FIXTURE[at + 2],
            FIXTURE[at + 3],
        ]) as usize
    };
    let mut fixture = Fixture {
        cols: u16_at(8),
        rows: u16_at(10),
        ready: Vec::new(),
        pages: Vec::new(),
        finish: Vec::new(),
    };
    let mut offset = 12;
    while offset < FIXTURE.len() {
        let kind = FIXTURE[offset];
        let rows = u32_at(offset + 1);
        let len = u32_at(offset + 5);
        let bytes = FIXTURE[offset + 9..offset + 9 + len].to_vec();
        offset += 9 + len;
        match kind {
            0 => fixture.ready = bytes,
            1 => fixture.pages.push((bytes, rows)),
            2 => fixture.finish = bytes,
            _ => panic!("unknown fixture unit {kind}"),
        }
    }
    assert!(!fixture.ready.is_empty() && !fixture.finish.is_empty());
    assert!(
        fixture.pages.len() >= 2,
        "fixture needs several history pages"
    );
    fixture
}

fn feed(decoder: &mut NativeDecoder, bytes: &[u8], cut: usize) {
    for fragment in bytes.chunks(cut) {
        decoder.feed(fragment).expect("stage fragment");
    }
}

fn last_row(terminal: &Terminal) -> String {
    terminal
        .rows_text()
        .into_iter()
        .rfind(|row| !row.is_empty())
        .unwrap_or_default()
}

async fn ready_decoder(fixture: &Fixture) -> NativeDecoder {
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
            Err(NativeCodecError::Status { .. })
        ),
        "a corrupt page fails its checksum"
    );
    assert_eq!(
        last_row(decoder.terminal().expect("terminal survives")),
        "live-tail"
    );
}
