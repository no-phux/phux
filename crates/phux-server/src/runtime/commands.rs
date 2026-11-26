//! Control-plane command handlers and pane spawning.

#![expect(
    clippy::redundant_pub_crate,
    reason = "private command modules re-export crate-scoped handlers through this facade; pub would violate unreachable_pub"
)]

mod agent_append;
pub(crate) mod attach_terminal;
mod dispatch;
pub(crate) mod document;
mod kill;
mod kind_commands;
mod lifecycle;
pub(crate) mod native_agent;
mod resource_events;
mod results;
mod routing;
mod satellite;
pub(crate) mod satellite_lease;
mod state_query;
pub(crate) mod terminal;

pub(crate) use attach_terminal::*;
#[cfg(test)]
pub(crate) use kill::handle_kill_terminals;
pub(crate) use lifecycle::*;
pub(crate) use results::*;
pub(crate) use routing::*;
pub(in crate::runtime) use satellite::*;
#[cfg(test)]
use satellite::{reply_satellite_command, resolve_hub_detach_terminal};
#[cfg(test)]
use state_query::{merge_satellite_resources, satellite_host_inventory};

#[cfg(test)]
mod lease_expiry_task_tests;

#[cfg(test)]
mod hub_detach_fence_tests;

#[cfg(test)]
mod relay_satellite_attach_role_tests;

#[cfg(test)]
mod host_inventory_tests;

#[cfg(test)]
mod kill_merge_tests;
