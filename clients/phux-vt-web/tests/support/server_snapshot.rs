//! The server-produced GHOSTSNP transcript, split into its transport units.
//!
//! `fixtures/server-snapshot-v1.bin` is captured by phux-server's production
//! prefix and immutable-history paths (`native_state` test
//! `browser_snapshot_fixture_is_the_current_server_capture`). It is
//! `PHUXSNP1`, cols and rows, then `(kind: u8, rows: u32, len: u32, bytes)`
//! units: 0 READY, 1 a history PAGE, 2 FINISH.

const FIXTURE: &[u8] = include_bytes!("../fixtures/server-snapshot-v1.bin");

pub struct ServerSnapshot {
    pub cols: u16,
    pub rows: u16,
    pub ready: Vec<u8>,
    /// History pages with the rows each carries.
    pub pages: Vec<(Vec<u8>, usize)>,
    pub finish: Vec<u8>,
}

pub fn server_snapshot() -> ServerSnapshot {
    assert_eq!(&FIXTURE[..8], b"PHUXSNP1");
    let u16_at = |at: usize| u16::from_le_bytes([FIXTURE[at], FIXTURE[at + 1]]);
    let u32_at =
        |at: usize| u32::from_le_bytes(FIXTURE[at..at + 4].try_into().expect("u32")) as usize;
    let mut fixture = ServerSnapshot {
        cols: u16_at(8),
        rows: u16_at(10),
        ready: Vec::new(),
        pages: Vec::new(),
        finish: Vec::new(),
    };
    let mut offset = 12;
    while offset < FIXTURE.len() {
        let (kind, rows, len) = (FIXTURE[offset], u32_at(offset + 1), u32_at(offset + 5));
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
