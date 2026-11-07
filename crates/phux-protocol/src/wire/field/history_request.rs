/// Target `ResourceId`.
pub const TERMINAL_ID: u32 = 1;
/// Logical `StreamId`.
pub const STREAM_ID: u32 = 2;
/// Replica `BootstrapId`.
pub const BOOTSTRAP_ID: u32 = 3;
/// Opaque current cursor.
pub const CURSOR: u32 = 4;
/// Requested maximum page bytes (`u32`); zero is retryably rejected.
pub const MAX_BYTES: u32 = 5;
/// Requested maximum page rows (`u32`); zero is retryably rejected.
pub const MAX_ROWS: u32 = 6;
