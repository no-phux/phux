//! Native launch transaction and exclusive authenticated controller broker.

mod closure;
mod controller;
mod integrations;
mod launch;
mod publication;
mod relaunch;
mod source_dispatch;

pub(crate) use closure::retry_pending_closures;
pub(crate) use controller::handle;
pub(crate) use relaunch::claim_relaunch;
pub(crate) use source_dispatch::source_exited;

#[cfg(test)]
mod tests;
