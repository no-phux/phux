//! phux server: the daemon side.
//!
//! Owns the canonical state of every session and resource for one user.
//! Hosts an IPC endpoint for clients (see `phux-protocol`) and forwards each
//! resource's output to attached clients as `RESOURCE_OUTPUT` frames per
//! ADR-0013. What a resource is lives in the app that serves its kind: the
//! Terminal kind is `phux-terminal`, over the kind-neutral `phux-resource`
//! substrate.

#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]

pub mod auth;
pub mod autosave;
pub mod connector;
pub mod daemon;
pub mod delegation;
pub mod downsample;
pub mod health; // phux-zomb.6 (server start history: crash-loop is reportable)
pub mod hooks;
pub mod hub;
pub mod id_bridge;
pub mod org_scale;
pub mod org_store;
pub mod org_wire;
/// Supervised `[[startup]]` sidecars declared by plugins.
pub mod plugin_startup;
pub mod policy;
pub(crate) mod push;
pub mod reconcile;
pub mod runtime;
pub mod state;
pub mod stream_diagnostics;
pub mod telemetry;
pub(crate) use phux_resource::{agent_asked, agent_state, proc_query};
pub use phux_resource::{cwd_query, perf};
pub use phux_terminal::actor as terminal_actor;
pub(crate) use phux_terminal::agent_detect;
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub use phux_terminal::native_state;
pub use phux_terminal::{grid, input};

/// The substrate plus the apps this host serves.
pub mod resource {
    pub use phux_resource::*;
    pub use phux_terminal::actor as terminal;
}

/// Mailbox payloads toward a client and toward a pane.
pub mod mailbox {
    pub use phux_resource::mailbox::*;
    pub use phux_terminal::TerminalInput;
}
pub mod transport;
pub mod upgrade;
pub mod uplink;
pub mod workload;

pub use hub::link::{HubLinkStatuses, LinkStatus};
pub use hub::{HubEntry, HubTable, HubTableError, SatelliteSource, SatelliteTarget, TableDiff};
pub use id_bridge::IdBridge;
pub use org_store::OrgStoreLocation;
pub use resource::{
    ResourceCore, ResourceFacet, ResourceHandle, ResourceId, ResourceKind, WrongResourceKind,
};
pub use runtime::{ServerConfig, ServerEnv, ServerError, ServerRuntime, default_socket_path};
pub use state::{
    AttachError, AttachedClient, ClientId, DEFAULT_GROUP_ID, Outbound, ServerState, SharedState,
    TerminalInput,
};
pub use terminal_actor::{
    SnapshotRequest, TerminalActor, TerminalActorBundle, TerminalActorError, TerminalHandle,
};
