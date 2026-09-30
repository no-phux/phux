//! Wire protocol for phux.
//!
//! This crate defines the protocol described in [`docs/spec/`] at the workspace
//! root: framing, message catalog, version negotiation, and the VT-bytes-on-
//! wire terminal content shape (per [ADR-0013]).
//!
//! The protocol is the source of truth. Code in this crate is normative;
//! implementations elsewhere defer to it.
//!
//! # Crate features
//!
//! - The default features expose the pure-Rust [`input`] and [`wire`] codec,
//!   [`ids`], [`caps`], [`policy`], and [`Version`], including for browser clients.
//! - **`render-pool`** (off by default): `render_pool`, the libghostty
//!   render-trio pool. Enables `libghostty-vt` only — no png, no kitty
//!   graphics, no input-atom conversions. See [ADR-0086].
//! - **`server`** (off by default): enables `render-pool`, then adds
//!   conversions to `libghostty-vt` atoms and engine-dependent SGR and Kitty
//!   replay helpers. Native terminal consumers enable it; decoding wire
//!   messages does not require it.
//!
//! [`docs/spec/`]: https://github.com/no-phux/phux/tree/main/docs/spec
//! [ADR-0013]: https://github.com/no-phux/phux/blob/main/docs/adr/0013-libghostty-bytes-on-wire.md
//! [ADR-0086]: https://github.com/no-phux/phux/blob/main/docs/adr/0086-shared-render-pool.md

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg))]

// The codec, input atoms, and policy vocabulary are pure Rust (ADR-0024), so
// they build for wasm; libghostty conversions sit behind `server`.
pub mod input;
pub mod wire;

pub mod caps;
pub mod ids;
pub mod kinds;
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

/// Protocol version this crate implements. Bump history and wire-breaking
/// rationale live in `docs/spec/CHANGELOG.md`.
pub const PROTOCOL_VERSION: Version = Version {
    major: 0,
    minor: 9,
    patch: 0,
};

/// A semantic protocol version: `major.minor.patch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Version {
    /// Wire-breaking changes bump this.
    pub major: u16,
    /// Additive changes bump this.
    pub minor: u16,
    /// Editorial; behavior unchanged.
    pub patch: u16,
}
