/// Target `ResourceId`.
pub const TERMINAL_ID: u32 = 1;
/// Logical `StreamId`.
pub const STREAM_ID: u32 = 2;
/// Replica `BootstrapId`.
pub const BOOTSTRAP_ID: u32 = 3;
/// Opaque cursor consumed by this page.
pub const CURSOR: u32 = 4;
/// Optional cursor for the next older page.
pub const NEXT_CURSOR: u32 = 5;
/// Opaque selected-codec page bytes.
pub const PAYLOAD: u32 = 6;
/// Non-zero page sequence within one generation-bound cursor lineage.
pub const PAGE_SEQ: u32 = 7;
/// Number of native history rows encoded by the payload (`u32`).
pub const ROWS: u32 = 8;
