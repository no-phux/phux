/// `AttachTarget` tagged union (positional).
pub const TARGET: u32 = 1;
/// `ViewportInfo` (positional sub-record).
pub const VIEWPORT: u32 = 2;
/// `request_scrollback: bool`.
pub const REQUEST_SCROLLBACK: u32 = 3;
/// `scrollback_limit_lines: u32`.
pub const SCROLLBACK_LIMIT_LINES: u32 = 4;
/// Client-chosen attach correlation id (`u32`).
pub const ATTACH_ID: u32 = 5;
/// `role_policy: u8` (ADR-0127); absent = `{ PRIMARY, NEVER }`.
pub const ROLE_POLICY: u32 = 6;
