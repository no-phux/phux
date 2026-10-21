//! Client connections: accept, HELLO, dispatch, metadata, and the writer.
//!
//! Pane event drains, the pane exit lifecycle, and the socket's filesystem
//! guard live in the submodules.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use bytes::BytesMut;
use phux_dial::window::SendWindow;
use phux_protocol::PROTOCOL_VERSION;
#[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
use phux_protocol::caps::BootstrapCapabilities;
use phux_protocol::caps::{
    BootstrapLimits, BootstrapProfile, ClientCapabilities, Compression, LayerSet, QUIC_STREAMS,
    ServerCapabilities, ServerFeature, ServerFeatureExt, ServerFeatureExtSet, ServerFeatureSet,
    select_bootstrap_profile,
};
use phux_protocol::ids::{ResourceId as WireResourceId, StreamId};
use phux_protocol::policy::TransportType;
use phux_protocol::wire::frame::Scope;
use phux_protocol::wire::frame::{
    AgentEvent, Command, CommandResult, DetachReason, ErrorCode, FrameKind, RESOURCE_AGENT_KEY,
};
use phux_protocol::wire::framing::FramingError;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace, warn};

use super::input_lane::{InputCredits, InputLaneHandle, InputStalled, RoutedInput};
use super::{
    ServerError, SpawnRequest, bootstrap_attach_terminal, handle_attach, handle_detach_terminal,
    handle_frame_ack, handle_move_terminal, handle_terminal_input, handle_terminal_reply,
    handle_terminal_resize, handle_viewport_resize, subscribe_attach_terminal,
};
use crate::auth::Standing;
use crate::hooks::HookEvent;
use crate::policy::{Goodbye, Revocation};
use crate::state::{
    ClientId, DEFAULT_CLIENT_MAILBOX, Outbound, ServerInterceptedKey, ServerState, SharedState,
    TerminalInput,
};
use crate::terminal_actor::ConsumerDetachRequest;
use crate::transport::quic::{
    QuicStreamEvent, QuicStreamFailure, QuicWriter, pump_terminal_stream, refuse_terminal_stream,
};
use crate::transport::{
    AcceptErrorDisposition, FrameOrigin, FrameReader, FrameWriter, Incoming,
    WS_REJECTION_WARN_INTERVAL,
};

pub mod pane_events;
pub mod pane_exit;
pub mod socket;

use pane_events::invalidate_agent_detector;
pub(crate) use pane_events::{
    PaneEvents, ensure_event_pump, reap_pane_journaling_close, spawn_agent_state_drain,
};
pub(super) use pane_exit::journal_pane_closed;
pub(crate) use pane_exit::{broadcast_terminal_closed, spawn_terminal_exit_watcher};
pub(crate) use socket::{handle_existing_socket, prepare_socket_dir, secure_socket_file};

include!("client/command_dispatch.rs");
include!("client/connection_negotiation.rs");
include!("client/connection_lifecycle.rs");
include!("client/connection_plumbing.rs");
include!("client/hello.rs");
include!("client/history.rs");
include!("client/connection_dispatch.rs");
include!("client/frame_dispatch.rs");
include!("client/terminal_streams.rs");
include!("client/metadata_read.rs");
include!("client/session_create.rs");
include!("client/metadata_mutation.rs");
include!("client/metadata_subscriptions.rs");
include!("client/outbound_generation.rs");
include!("client/writer.rs");
include!("client/writer_close_tests.rs");
include!("client/fatal_preflight_close_tests.rs");
include!("client/metadata_tests.rs");
