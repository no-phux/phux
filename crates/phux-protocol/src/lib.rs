//! Wire protocol for phux: framing, message catalog, version negotiation,
//! capabilities, and the VT-bytes-out, structured-input-in shape of terminal
//! traffic.
//!
//! This crate is the specification. `docs/spec/wire-schema.json` in the
//! repository is generated from it and test-pinned; `docs/protocol.md`
//! describes framing and the handshake in prose.
//!
//! # Crate features
//!
//! - The default features expose the pure-Rust [`input`] and [`wire`] codec,
//!   [`ids`], [`caps`], [`policy`], and [`Version`], including for browser clients.
//! - **`render-pool`** (off by default): `render_pool`, the libghostty
//!   render-trio pool. Enables `libghostty-vt` only, with no png, no kitty
//!   graphics, and no input-atom conversions.
//! - **`server`** (off by default): enables `render-pool`, then adds
//!   conversions to `libghostty-vt` atoms and engine-dependent SGR and Kitty
//!   replay helpers. Native terminal consumers enable it; decoding wire
//!   messages does not require it.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg))]

// The codec, input atoms, and policy vocabulary are pure Rust (ADR-0024), so
// they build for wasm; libghostty conversions sit behind `server`.
pub mod input;
pub mod wire;

pub mod caps;
pub mod checkout;
pub mod enroll;
pub mod ids;
pub mod kinds;
pub mod native_agent;
pub mod policy;
pub mod scope;

#[cfg(feature = "server")]
pub mod sgr;

#[cfg(feature = "server")]
pub mod kitty_replay;

#[cfg(feature = "render-pool")]
pub mod render_pool;

pub use caps::{
    ACKNOWLEDGED_INPUT, BootstrapCapabilities, BootstrapCodec, BootstrapLimits, BootstrapProfile,
    BootstrapProfileKind, BootstrapProfileSet, BootstrapStreamProfile, ClientCapabilities,
    CodecUnavailable, ColorSupport, DEFAULT_BOOTSTRAP_CHUNK_BYTES, DEFAULT_HISTORY_PAGE_BYTES,
    EngineCodec, EngineCodecSet, EngineFeature, EngineFeatureSet, FILE_UPLOAD, ImageProtocol,
    ImageProtocolSet, KeyboardProtocol, KeyboardProtocolSet, Layer, LayerSet,
    MAX_BOOTSTRAP_CHUNK_BYTES, MAX_HISTORY_PAGE_BYTES, MOVE_RESOURCE, OutputMode, RESOURCE_KINDS,
    ServerFeature, ServerFeatureExt, ServerFeatureExtSet, ServerFeatureSet, TERMINAL_REPLY,
    TerminalColor, TerminalDefaultColors, select_bootstrap_profile,
};
pub use ids::{
    ApprovalId, BootstrapId, ClientId, FileUploadId, FrameId, GroupId, IdempotencyKey,
    InputOperationId, ResourceId, ResourceKind, SatelliteHost, ServerInstance, SessionId, StreamId,
    WindowId,
};
pub use wire::frame::{
    CloseReason, MAX_APPEND_BYTES, MAX_APPLY_INPUT_COMMAND_BODY, MAX_APPLY_INPUT_EVENTS,
    MAX_FILE_UPLOAD_CHUNK, MAX_FILE_UPLOAD_SIZE, MAX_HISTORY_CURSOR_BYTES, MAX_HISTORY_PAGE_ROWS,
    MAX_INPUT_TERMINAL_REPLY_BYTES, MAX_RESOURCE_NATIVE_ID_BYTES, MAX_RESOURCE_PROVIDER_BYTES,
    SpawnResource,
};
pub use wire::info::AgentFacet;
pub use wire::info::{HostInventory, HostSessionInfo};

/// Protocol version this crate implements.
///
/// The major is the compatibility line: any client and server with the same
/// major interoperate, whatever their minor and patch. Additive changes ship
/// as feature bits, new frame types or tags, new TLV fields, or fields
/// appended to a record's end, and a peer that does not know one ignores or
/// refuses it with `ERROR`/`Unsupported` without closing. Minor and patch
/// are informational; only a change no additive shape can express bumps
/// the major.
pub const PROTOCOL_VERSION: Version = Version {
    major: 1,
    minor: 0,
    patch: 0,
};

/// A semantic protocol version: `major.minor.patch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Version {
    /// The compatibility line: peers interoperate iff this matches; only a
    /// wire-breaking change bumps it.
    pub major: u16,
    /// Informational: additive changes may bump it, and peers never refuse
    /// each other over it.
    pub minor: u16,
    /// Informational: editorial or behavior-preserving changes.
    pub patch: u16,
}

impl Version {
    /// Whether a peer at `self` interoperates with one at `other`: the same
    /// major. Minor and patch never refuse a connection.
    #[must_use]
    pub const fn is_compatible_with(self, other: Self) -> bool {
        self.major == other.major
    }
}
