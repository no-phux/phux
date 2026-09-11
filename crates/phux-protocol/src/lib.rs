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
//! - **`server`** (off by default): adds conversions to `libghostty-vt` atoms
//!   and engine-dependent SGR, Kitty replay, and render-pool helpers. Native
//!   terminal consumers enable it; decoding wire messages does not require it.
//!
//! [`docs/spec/`]: https://github.com/no-phux/phux/tree/main/docs/spec
//! [ADR-0013]: https://github.com/no-phux/phux/blob/main/ADR/0013-libghostty-bytes-on-wire.md

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]
#![cfg_attr(docsrs, feature(doc_cfg))]

// The wire codec and its input atoms are libghostty-free (ADR-0024) and so
// build for any target, including wasm browser consumers. libghostty
// conversions for the atoms live behind the `server` feature. The policy
// vocabulary (ALPN constants, peer identity, capabilities) is likewise on the
// ungated shell: it is pure std/serde, and the wire crate must own the ALPN
// bytes for consumers like `phux-dial` that deliberately stay off the
// `server` feature.
pub mod input;
pub mod wire;

pub mod caps;
pub mod ids;
pub mod policy;

#[cfg(feature = "server")]
pub mod sgr;

#[cfg(feature = "server")]
pub mod kitty_replay;

#[cfg(feature = "server")]
pub mod render_pool;

pub use caps::{
    ACKNOWLEDGED_INPUT, BootstrapCapabilities, BootstrapCodec, BootstrapLimits, BootstrapProfile,
    BootstrapProfileKind, BootstrapProfileSet, BootstrapStreamProfile, ClientCapabilities,
    CodecUnavailable, ColorSupport, DEFAULT_BOOTSTRAP_CHUNK_BYTES, DEFAULT_HISTORY_PAGE_BYTES,
    EngineCodec, EngineCodecSet, EngineFeature, EngineFeatureSet, FILE_UPLOAD, ImageProtocol,
    ImageProtocolSet, KeyboardProtocol, KeyboardProtocolSet, Layer, LayerSet,
    MAX_BOOTSTRAP_CHUNK_BYTES, MAX_HISTORY_PAGE_BYTES, MOVE_RESOURCE, OutputMode, RESOURCE_KINDS,
    ServerFeature, ServerFeatureSet, TERMINAL_REPLY, TerminalColor, TerminalDefaultColors,
    select_bootstrap_profile,
};
pub use ids::{
    BootstrapId, ClientId, FileUploadId, FrameId, GroupId, InputOperationId, ResourceId,
    ResourceKind, SatelliteHost, ServerInstance, SessionId, StreamId, WindowId,
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
/// Bumped from `0.1.0` to `0.2.0` in phux-vp0.4: [`ResourceId`] becomes a
/// tagged union (`Local` / `Satellite`) per ADR-0016, which prepends a
/// 1-byte tag to every `ResourceId` field on the wire.
///
/// Bumped from `0.2.0` to `0.3.0` by the "Option B" wire re-tier
/// (ADR-0019 / ADR-0027): the L2 collection lifecycle verbs
/// `CREATE_SESSION` / `KILL_COLLECTION` / `RENAME_SESSION` (command tags
/// `0x09`..=`0x0b`) are removed and replaced by a single atomic
/// multi-terminal op, `KILL_RESOURCES` (reusing tag `0x09`); grouping
/// (membership + names) moves to L3 metadata + client logic. Removing wire
/// verbs is wire-breaking, so pre-1.0 this bumps the minor.
///
/// Bumped from `0.3.0` to `0.4.0` by the field-tagged TLV wire migration:
/// every message body changes from positional, fixed-order fields to
/// field-tagged TLV (`field_id: varint || wire_type: u8 || length-delimited
/// value`) per `docs/spec/appendix-encoding.md`. Decoders now match top-level
/// fields by stable id (start at `1`, contiguous per message) and skip any id
/// they do not recognise by its declared length; optional / trailing fields
/// become simply-absent tagged fields. Nested tagged unions and sub-records
/// (`ResourceId`, `ViewportInfo`, `Command`, `SessionSnapshot`, ...) stay
/// positional inside a field's value. Every body's bytes change, so this is
/// wire-breaking; pre-1.0 it bumps the minor.
///
/// Bumped from `0.4.0` to `0.5.0` by phux-q1ni (ADR-0030): the `INPUT_SELECTION`
/// frame (type `0x15`), its `Selection` input-event tag (`0x04`), and the
/// `SelectionEvent` / `SelectionMode` wire types are removed. Selection is a
/// client-side projection over the consumer's own engine, never a wire tier —
/// the client extracts the selected text from its own libghostty `Terminal` and
/// copies it locally (OSC 52). Removing a wire frame is wire-breaking, so pre-1.0
/// this bumps the minor.
///
/// Bumped from `0.5.0` to `0.6.0` by ADR-0059: `PUT_FILE` adds a sandboxed,
/// chunked, acknowledged upload command and `FILE_UPLOAD` negotiation bit.
/// Older clients and servers remain valid but MUST negotiate the capability
/// before using the new command.
///
/// Bumped from `0.6.0` to `0.7.0` by ADR-0070: `TERMINAL_SNAPSHOT` (`0x91`)
/// is permanently retired. Explicit native/compatibility profile negotiation,
/// generation-bound bootstrap/history streams, and READY-fenced attach replace
/// synthesized snapshot ordering. Protocol 0.6 and 0.7 peers reject each other.
///
/// Bumped from `0.7.0` to `0.8.0` by ADR-0085: `REPORT_AGENT_STATE` adds
/// capability-gated hook evidence to the server-side detector.
///
/// Bumped from `0.8.0` to `0.9.0` by ADR-0102: the substrate is renamed once,
/// from Terminal to resource. `TerminalId` becomes [`ResourceId`],
/// `TERMINAL_OUTPUT` becomes `RESOURCE_OUTPUT`, `SPAWN_TERMINAL` becomes
/// `SPAWN_RESOURCE`, `TERMINAL_SPAWNED` / `TERMINAL_CLOSED` become
/// `RESOURCE_SPAWNED` / `RESOURCE_CLOSED`, `TERMINAL_RESIZE` becomes
/// `RESIZE_TERMINAL`, and `MOVE_`, `ATTACH_`, `DETACH_`, `KILL_`, and
/// `SUBSCRIBE_*_EVENTS` take the resource spelling. Frame types, command
/// tags, and every field number are byte-identical: no encoder or decoder
/// changes behavior, so a 0.8 peer and a 0.9 peer put the same bytes on the
/// wire. The minor still bumps because the names the spec and every
/// generated binding expose are part of the published surface (ADR-0061),
/// and because [`ServerFeature::RESOURCE_KINDS`](crate::caps::ServerFeature)
/// lands with them. Facet frames keep Terminal in their names.
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
