//! Server-to-client frame handling: dispatches `FrameKind` variants to
//! the right state mutations and rendering.
//!
//! Returns a `FrameOutcome` describing the follow-up the async driver
//! should take (e.g. exit on `DETACHED`, send `GET_METADATA` after
//! `ATTACHED`, repaint after a layout-replacing frame).

mod engine_route;
mod handler;
mod index;
mod outcome;
#[cfg(test)]
mod tests;

pub(super) use handler::handle_server_frame;
pub(super) use index::AgentMetaIndex;
pub(super) use outcome::FrameOutcome;

#[cfg(test)]
use crate::layout_ops::DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID;
#[cfg(test)]
use engine_route::{attach_participants, route_engine_frame};
#[cfg(test)]
use handler::{handle_window_spawned, reconcile_loaded_workspace};
