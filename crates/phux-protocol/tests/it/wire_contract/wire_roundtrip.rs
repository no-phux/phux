//! Wire-codec round-trip and malformed-input tests.
//!
//! `roundtrip_frame_kind` plus `arb_frame_kind` cover the thin-wrapper
//! catalog. Hand-rolled cases cover known-bad inputs and confirm the
//! decoder returns `DecodeError` rather than panicking. Focused properties
//! remain only where a constraint the catalog strategy cannot express.
//!
//! Protocol 0.7 binds `ResourceOutput` to non-zero stream/bootstrap ids; native
//! checkpoint and history frames have focused semantic tests in
//! `bootstrap_wire.rs`.
//!
//! The TLV byte-building helpers (`docs/protocol.md#framing`) live in
//! `tests/common/mod.rs`, shared with the other wire test files.

#![allow(clippy::unwrap_used)]

use bytes::BytesMut;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ClientCapabilities, ColorSupport,
    EngineCodec, ImageProtocol, ImageProtocolSet, KeyboardProtocol, KeyboardProtocolSet, Layer,
    LayerSet, OutputMode, ServerCapabilities, ServerFeature, ServerFeatureSet, TerminalColor,
    TerminalDefaultColors,
};
use phux_protocol::ids::{
    BootstrapId, ClientId, FileUploadId, GroupId, InputOperationId, ResourceId, ResourceKind,
    SessionId, StreamId, WindowId,
};
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{
    AgentEvent, AttachTarget, CloseReason, Command, CommandResult, CommandValue, ControlAction,
    DetachReason, DirectoryEntry, DirectoryErrorCode, DirectoryListing, DirectoryListingError,
    DirectoryListingResult, ErrorCode, FileChunk, FileUploadAck, InputMode, ListenerTransport,
    MAX_APPEND_BYTES, MAX_APPLY_INPUT_COMMAND_BODY, MAX_APPLY_INPUT_EVENTS, MAX_FILE_UPLOAD_CHUNK,
    MAX_FILE_UPLOAD_SIZE, MAX_RESOURCE_NATIVE_ID_BYTES, MAX_RESOURCE_PROVIDER_BYTES, MoveError,
    MoveResult, PerfOp, ReportedAgentState, ResourceLifecycle, Scope, SpawnError, SpawnResource,
    SpawnResult, StateScope, TerminalSignal, ViewportInfo,
};
use phux_protocol::wire::info::{
    AgentFacet, ResourceInfo, SessionInfo, SessionSnapshot, WindowInfo,
};
use phux_protocol::wire::{DecodeError, decode::Decoder, frame::FrameKind};

use crate::common;
use common::{assert_round_trip, framed_tlv, local_id_bytes, record, tlv_field};

mod attach_and_checkout;
mod codec_props;
mod command_payloads;
mod commands;
mod events;
mod frame_strategies;
mod hello_growth;
mod host_inventory;
mod layout_and_l3;
mod resource_kinds;
mod service_http;
mod snapshots;
mod strategies;

use events::{arb_actor, arb_agent_event, arb_event_stamp};
use frame_strategies::{
    arb_detach_reason, arb_error_code, arb_frame_kind_input, arb_frame_kind_lifecycle,
    arb_frame_kind_metadata, arb_terminal_id, arb_vt_bytes,
};
use layout_and_l3::{
    arb_close_reason, arb_directory_listing_result, arb_env_pair, arb_layer_set,
    arb_metadata_value, arb_move_result, arb_scope, arb_spawn_result,
};
use strategies::{arb_frame_kind, arb_resource_info, arb_resource_kind};

// -----------------------------------------------------------------------------
// Strategies
// -----------------------------------------------------------------------------

// -----------------------------------------------------------------------------
// ATTACH / ATTACHED generation correlation.
// -----------------------------------------------------------------------------

// -----------------------------------------------------------------------------
// The retired `WindowInfo` layout slot (docs/protocol.md, ADR-0030): a present
// tree is skipped, whatever its ratio, and the fields after it still decode.
// -----------------------------------------------------------------------------

// -----------------------------------------------------------------------------
// L3 metadata frames — SPEC §7.4 / §11.L3 (phux-4li.2).
// -----------------------------------------------------------------------------

// -----------------------------------------------------------------------------
// L1 Terminal lifecycle frames — SPEC §7.2 / §10.1 (phux-4li.10).
//
// Wire substrate for split-pane / kill-pane (phux-4li.5) and post-SIGWINCH
// per-pane `ioctl(TIOCSWINSZ)` (phux-4li.9). Server-side handler + client-
// side emission land in follow-up tickets; the codec lands here.
// -----------------------------------------------------------------------------

// -----------------------------------------------------------------------------
// Agent-event frames — SPEC §7.5 (phux-y2t).
// -----------------------------------------------------------------------------

// -----------------------------------------------------------------------------
// Resource kinds: the additive spawn / close / snapshot / command shapes.
// -----------------------------------------------------------------------------
