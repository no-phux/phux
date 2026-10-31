/// `ResourceLifecycle` (`u8`); absent = `RUNNING`.
pub const LIFECYCLE: u32 = 1;
/// `ExitFacet` (positional); present iff exited and retained (ADR-0124).
pub const EXIT: u32 = 2;
/// `ClientId` (`u32`) of the input-lease holder; absent = open.
pub const INPUT_HOLDER: u32 = 3;
/// `ClientId` (`u32`) of one `VIEWER` (ADR-0127); repeated.
pub const VIEWER: u32 = 4;
