//! Core domain types for phux.
//!
//! Defines the organization records (sessions, resources, arrangements,
//! activations), their persistent keys, and the pure [`apply()`] function
//! that validates a command against them, as pure data: no I/O, no clock,
//! no randomness, no terminal emulation, no PTY handling. The server crate
//! persists transitions and runs their effects.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]

pub mod activation;
pub mod actor;
pub mod apply;
pub mod arrangement;
pub mod command;
pub mod host_list;
pub mod ids;
pub mod keys;
pub mod org;
pub mod process;
pub mod registry;
pub mod resource;
pub mod screen;
pub mod session;
pub mod session_list;
pub mod terminal;

pub use activation::{
    Activation, ExecState, ExitPolicy, Outcome, ProcIdentity, SessionLifetime, Stamp,
};
pub use actor::{Actor, PrincipalId, Verb};
pub use apply::apply;
pub use arrangement::{
    Arrangement, ArrangementError, ArrangementOwner, EntryRef, EntryTarget, Group, LayoutError,
    LayoutNode, SplitDir, first_group,
};
pub use command::{
    Applied, Command, Created, Effect, LaunchMode, NewResource, Op, Refusal, Reply, SourcePin,
    TerminateMode,
};
pub use ids::{ResourceId, SessionId};
pub use keys::{
    ActivationId, ArrangementKey, AuthorityId, FreshKeys, InstanceId, Key16, LeaseEpoch,
    OperationId, ResourceKey, Revision, SequentialKeys, SessionKey, WorkspaceKey,
};
pub use org::{
    Authority, Delta, Event, InvariantViolation, InverseEdit, Org, Receipt, RecordRef, RevisionPin,
    Transition, Undo,
};
pub use process::{ExitOutcome, TerminalProcessState};
pub use registry::{Committed, Registry};
pub use resource::{
    AgentFacet, AgentLaunch, LaunchSpec, Provenance, Resource, ResourceDescriptor, ResourceKind,
    TerminalLaunch,
};
pub use screen::{CursorState, SCHEMA_VERSION, ScreenState};
pub use session::{Attrs, Session};
pub use terminal::TerminalFacet;
