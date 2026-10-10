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

pub(super) use engine_route::attach_participants;
pub(super) use handler::pane_is_referenced;
pub(super) use handler::{FrameEnv, handle_server_frame};
// The composited output frame, shared with the driver's frame
// pacer so a paced settle paints through exactly the same path a live
// `RESOURCE_OUTPUT` does.
pub(super) use handler::{OutputFrame, paint_output_frame};
pub(super) use index::AgentMetaIndex;
pub(in crate::attach) use outcome::pane_label;
pub(super) use outcome::{FrameOutcome, ProjectTagFrame};

#[cfg(test)]
use engine_route::{attach_agent_sessions, route_engine_frame};
#[cfg(test)]
use handler::{handle_window_spawned, reconcile_loaded_workspace};
#[cfg(test)]
use phux_client::layout_ops::DEFAULT_LAYOUT_GROUP_ID as DEFAULT_GROUP_ID;
