/// Target `ResourceId`.
pub const TERMINAL_ID: u32 = 1;
/// Logical `StreamId`.
pub const STREAM_ID: u32 = 2;
/// Replica `BootstrapId`.
pub const BOOTSTRAP_ID: u32 = 3;
/// Concrete bootstrap codec.
pub const CODEC: u32 = 4;
/// Authoritative columns (`u16`).
pub const COLS: u32 = 5;
/// Authoritative rows (`u16`).
pub const ROWS: u32 = 6;
/// Live emitter (`OutputMode`).
pub const OUTPUT_MODE: u32 = 7;
/// Actor cut sequence (`u64`).
pub const BASE_SEQ: u32 = 8;
/// Last agent record committed by the consumer (`u64`); agent/raw only.
pub const RESUME_AFTER_SEQ: u32 = 9;
