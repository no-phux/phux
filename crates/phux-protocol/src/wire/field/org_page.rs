/// `AuthorityId` (16 bytes).
pub const AUTHORITY: u32 = 1;
/// The designated Intake `SessionKey` (16 bytes); legacy name, unchanged tag.
pub const INBOX: u32 = 2;
/// One session record: nested [`org`](super::org) fields. Repeated.
pub const SESSION: u32 = 3;
/// One resource record: nested [`org`](super::org) fields. Repeated.
pub const RESOURCE: u32 = 4;
/// Sessions matching the filter, all pages (`u32`).
pub const TOTAL_SESSIONS: u32 = 5;
/// Resources matching the filter, all pages (`u32`).
pub const TOTAL_RESOURCES: u32 = 6;
/// Sessions awaiting organization among those matching (`u32`).
pub const TOTAL_INTAKE: u32 = 7;
/// Organization revision the page was cut at (`u64`).
pub const SNAPSHOT_REVISION: u32 = 8;
/// Event-journal `seq` at the cut (`u64`): subscribe after it.
pub const JOURNAL_SEQ: u32 = 9;
/// Opaque cursor of the next page (bytes, at most 4 KiB); absent on the
/// last page.
pub const NEXT_CURSOR: u32 = 10;
