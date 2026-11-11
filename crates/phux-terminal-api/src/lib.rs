//! The engine-independent Terminal contract. Engines implement the factory and
//! input ports; the host owns authorization, organization and publication.

pub mod actor;
pub mod agent_detect;
pub mod factory;
pub mod grid;
pub mod handoff;
pub mod input;
pub mod launch;
pub mod mailbox;
pub mod native_state;
pub use factory::*;
pub use mailbox::{ControlRequest, TerminalInput};
pub use phux_resource::mailbox::Outbound;
