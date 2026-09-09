//! Core domain types for phux.
//!
//! Defines sessions, windows, resources (Terminals and agent sessions), and
//! the layout tree as pure data — no I/O, no terminal emulation, no PTY
//! handling. The server crate composes these with libghostty-vt and PTY
//! plumbing.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]

pub mod ids;
pub mod registry;
pub mod resource;
pub mod screen;
pub mod session;
pub mod session_list;
pub mod terminal;
pub mod window;

pub use ids::{ResourceId, SessionId, WindowId};
pub use registry::{Registry, RegistryError};
pub use resource::{AgentFacet, ResourceDescriptor, ResourceKind};
pub use screen::{CursorState, SCHEMA_VERSION, ScreenState};
pub use session::Session;
pub use terminal::TerminalFacet;
pub use window::{Direction, LayoutError, LayoutNode, SplitDir, Window};
