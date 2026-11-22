//! Terminal-facet commands: screen reads, resize, signals, input, and the
//! input lease. Each refuses other kinds with `WRONG_RESOURCE_KIND`.

#![expect(
    clippy::redundant_pub_crate,
    reason = "private command modules re-export crate-scoped handlers through this facade; pub would violate unreachable_pub"
)]

mod agent_reports;
mod app;
pub(crate) mod attach_resource;
mod attach_resource_pump;
pub(crate) mod input_lease;
pub(crate) mod input_route;
pub(crate) mod resize;
pub(crate) mod screen;
pub(crate) mod seed;
pub(crate) mod signal;
pub(crate) mod spawn;
pub(crate) mod terminal_input;

pub(crate) use input_route::*;
pub(crate) use resize::*;
pub(crate) use screen::*;
pub(crate) use spawn::*;
pub(crate) use terminal_input::*;

pub(crate) use app::TerminalApp;
