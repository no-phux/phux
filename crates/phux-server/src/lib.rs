//! phux server: the daemon side.
//!
//! Owns the canonical state of every session, window, and terminal for one
//! user. Hosts an IPC endpoint for clients (see `phux-protocol`), feeds
//! PTY output into per-terminal `libghostty_vt::Terminal` instances, and
//! forwards bytes to attached clients as `RESOURCE_OUTPUT` frames per
//! ADR-0013.

#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]

pub(crate) mod agent_asked;
pub(crate) mod agent_detect;
pub mod agent_explain;
pub(crate) mod agent_state;
pub mod auth;
pub mod connector;
pub mod cwd_query;
pub mod downsample;
pub mod extract;
pub mod grid;
pub mod health; // phux-zomb.6 (server start history: crash-loop is reportable)
// Pure viewport-alignment core for the ADR-0078 alternate-screen history
// harvest. Deliberately not wired to the terminal actor: that ADR is still
// Proposed and nothing may scroll a live pane before it is accepted.
pub(crate) mod history_merge;
pub mod hooks;
pub mod hub;
pub mod id_bridge;
pub mod input;
pub mod mailbox;
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub mod native_state;
pub mod perf;
pub mod policy;
pub(crate) mod proc_query;
pub mod resource;
pub mod runtime;
pub mod search;
pub mod state;
pub mod telemetry;
/// The Terminal engine, at the path it has always been reachable from.
/// [`resource::terminal`] is the module; this alias keeps every
/// `terminal_actor::` path in tests, examples, and the CLI valid.
pub use resource::terminal as terminal_actor;
pub mod transport;
pub mod upgrade;
pub mod workload;

pub use hub::link::{HubLinkStatuses, LinkStatus};
pub use hub::{HubEntry, HubTable, HubTableError, SatelliteTarget};
pub use id_bridge::IdBridge;
pub use resource::{
    ResourceCore, ResourceFacetHandle, ResourceHandle, ResourceId, ResourceKind, WrongResourceKind,
};
pub use runtime::{ServerConfig, ServerError, ServerRuntime, default_socket_path};
pub use state::{
    AttachError, AttachedClient, ClientId, DEFAULT_GROUP_ID, Outbound, ServerState, SharedState,
    TerminalInput,
};
pub use terminal_actor::{
    SnapshotRequest, TerminalActor, TerminalActorBundle, TerminalActorError, TerminalHandle,
};
