/// Stable question id (`str`).
pub const ID: u32 = 1;
/// Question text (`str`).
pub const QUESTION: u32 = 2;
/// One suggested answer (`str`), repeated in order.
pub const SUGGESTION: u32 = 3;
/// Optional seconds waiting (`u64`); absent = unknown.
pub const ELAPSED_SECONDS: u32 = 4;
