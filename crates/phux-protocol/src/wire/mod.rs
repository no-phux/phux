//! Wire codec: length-prefixed, field-tagged TLV frames, big-endian
//! (`docs/spec/appendix-encoding.md`). Terminal content is opaque VT bytes and
//! bootstrap payloads (ADR-0013, ADR-0070).

pub mod compress;
pub mod decode;
pub mod encode;
pub mod error;
pub mod field;
pub mod frame;
pub mod framing;
pub mod info;
pub mod listeners;
pub mod ssh_origin;
pub mod stream_bind;

pub use error::DecodeError;
pub use framing::{FramingError, LENGTH_PREFIX_LEN};
pub use listeners::{
    ListenerDisabledReason, REMOTE_LISTENERS_SCHEMA_VERSION, RemoteListenerSlot,
    RemoteListenerTransport, RemoteListenersReport,
};
