/// One argv element (`str`). Repeated, in order; absent = the default
/// shell.
pub const COMMAND: u32 = 1;
/// Working directory (`str`).
pub const CWD: u32 = 2;
/// One environment pair, positional `str name || str value`. Repeated.
pub const ENV: u32 = 3;
/// `TERM` override (`str`).
pub const TERM: u32 = 4;
/// Kind-specific restore record (bytes), e.g. the agent conversation a
/// relaunched Terminal resumes.
pub const RESTORE: u32 = 5;
