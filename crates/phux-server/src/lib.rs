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
pub mod perf;
/// Supervised `[[startup]]` sidecars declared by plugins.
pub mod plugin_startup;
pub mod policy;
pub(crate) mod push;
pub mod reconcile;
pub mod runtime;
pub mod state;
pub mod stream_diagnostics;
pub mod telemetry;
pub use phux_resource::mailbox;

/// The substrate every kind shares.
pub use phux_resource as resource;
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
};
