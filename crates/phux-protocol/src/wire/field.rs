//! TLV field ids inside message bodies (`docs/spec/appendix-encoding.md`).
//!
//! Each top-level field is `field_id: varint || wire_type: u8 || value`;
//! decoders skip unknown ids by length. Ids are per message, start at `1`,
//! and are stable within a major version: new fields append, removed ids are
//! retired, never reused. An optional field is simply absent. Nested unions
//! and sub-records stay positional inside a field's value.

mod connection;
mod lifecycle;
mod metadata;
mod organization;

pub use connection::*;
pub use lifecycle::*;
pub use metadata::*;
pub use organization::*;
