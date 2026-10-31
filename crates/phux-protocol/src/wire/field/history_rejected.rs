/// Target `ResourceId`.
pub const TERMINAL_ID: u32 = 1;
/// Logical `StreamId`.
pub const STREAM_ID: u32 = 2;
/// Replica `BootstrapId`.
pub const BOOTSTRAP_ID: u32 = 3;
/// Opaque history cursor that was not advanced.
pub const CURSOR: u32 = 4;
/// `HistoryRejectionReason` tag (`u8`).
pub const REASON: u32 = 5;
/// Non-zero required retry byte limit (`u32`).
pub const REQUIRED_BYTES: u32 = 6;
/// Non-zero required retry row limit (`u32`).
pub const REQUIRED_ROWS: u32 = 7;
