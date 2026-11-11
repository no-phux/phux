/// Selected protocol major version (`u16`).
pub const PROTOCOL_MAJOR: u32 = 1;
/// Selected protocol minor version (`u16`).
pub const PROTOCOL_MINOR: u32 = 2;
/// Selected protocol patch version (`u16`).
pub const PROTOCOL_PATCH: u32 = 3;
/// `ServerCapabilities` blob (positional sub-record).
pub const SERVER_CAPS: u32 = 4;
/// Opaque server identity bytes.
pub const SERVER_ID: u32 = 5;
/// Selected explicit `BootstrapProfile` sub-record.
pub const SELECTED_PROFILE: u32 = 6;
/// Negotiated maximum `BOOTSTRAP_CHUNK.payload` bytes (`u32`).
pub const MAX_CHUNK_BYTES: u32 = 7;
/// Negotiated maximum `HISTORY_PAGE.payload` bytes (`u32`).
pub const MAX_HISTORY_PAGE_BYTES: u32 = 8;
/// Selected frame compression (`u8` tag); absent or `0` = none.
pub const COMPRESSION: u32 = 9;
