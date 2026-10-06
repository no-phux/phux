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
pub(crate) mod agent_state;
pub mod auth;
pub mod autosave;
pub mod connector;
pub mod cwd_query;
pub mod downsample;
pub mod grid;
pub mod health; // phux-zomb.6 (server start history: crash-loop is reportable)
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
pub mod state;
pub mod stream_diagnostics;
pub mod telemetry;
/// Alias for [`resource::terminal`], used by tests, examples, and the CLI.
pub use resource::terminal as terminal_actor;
pub mod transport;
pub mod upgrade;
pub mod workload;

pub use hub::link::{HubLinkStatuses, LinkStatus};
pub use hub::{HubEntry, HubTable, HubTableError, SatelliteSource, SatelliteTarget, TableDiff};
pub use id_bridge::IdBridge;
pub use resource::{
    ResourceCore, ResourceFacetHandle, ResourceHandle, ResourceId, ResourceKind, WrongResourceKind,
};
pub use runtime::{ServerConfig, ServerEnv, ServerError, ServerRuntime, default_socket_path};
pub use state::{
    AttachError, AttachedClient, ClientId, DEFAULT_GROUP_ID, Outbound, ServerState, SharedState,
    TerminalInput,
};
pub use terminal_actor::{
    SnapshotRequest, TerminalActor, TerminalActorBundle, TerminalActorError, TerminalHandle,
};
