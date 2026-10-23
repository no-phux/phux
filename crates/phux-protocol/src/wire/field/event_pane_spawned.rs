/// `ResourceKind` tag (`u8`); absent = `Terminal`.
pub const KIND: u32 = 1;
/// Parent resource (positional tagged `ResourceId`). Absent = a root.
pub const PARENT: u32 = 2;
