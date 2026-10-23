/// Free-form client identifier string.
pub const CLIENT_NAME: u32 = 1;
/// Protocol major version (`u16`).
pub const PROTOCOL_MAJOR: u32 = 2;
/// Protocol minor version (`u16`).
pub const PROTOCOL_MINOR: u32 = 3;
/// Protocol patch version (`u16`).
pub const PROTOCOL_PATCH: u32 = 4;
/// `ClientCapabilities` blob (positional sub-record).
pub const CLIENT_CAPS: u32 = 5;
/// Frame compressions the client accepts (`u8` bitset); absent = none.
/// Top-level because the `CLIENT_CAPS` byte order is fixed (§6.2).
pub const COMPRESSION: u32 = 6;
// Ids 7 and 8 are retired-unshipped (ADR-0116) and stay reserved.
/// Positional `SshOrigin` stamped by `phux stdio-bridge`
/// (`docs/spec/L3.md` §3.9); honored only from a same-uid Unix peer.
pub const SSH_ORIGIN: u32 = 9;
/// Client can demultiplex QUIC stream-per-Terminal; absent = false.
pub const QUIC_STREAMS: u32 = 10;
