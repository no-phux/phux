//! Wire codec — length-prefixed TLV framing per `docs/spec/appendix-encoding.md`.
//!
//! All multi-byte integers are big-endian. Frames are length-prefixed.
//! Field IDs and message types match SPEC §7's catalog.
//!
//! Under ADR-0013 and ADR-0070, live terminal content is VT bytes in
//! `RESOURCE_OUTPUT`; initial native state and history are opaque,
//! profile-negotiated bootstrap payloads. No cell-level diff or engine record
//! parser exists in this crate.

pub mod compress;
pub mod decode;
pub mod encode;
pub mod error;
pub mod field;
pub mod frame;
pub mod framing;
pub mod info;
pub mod ssh_origin;

pub use error::DecodeError;
pub use framing::{FramingError, LENGTH_PREFIX_LEN};
