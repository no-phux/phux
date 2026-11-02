//! The Terminal app's host side: session attach and the input lane, over the
//! engine in `phux-terminal`.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private module shared across the runtime; pub would violate unreachable_pub"
)]

pub(crate) mod agent_state;
pub(crate) mod attach;
pub(crate) mod checkout;
pub(crate) mod commands;
mod frames;
#[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
pub(crate) mod history;
pub(crate) mod input_lane;
pub(crate) mod launch;
pub(crate) mod program_status;
pub(crate) mod terminal_streams;
pub(crate) mod upgrade;
pub(crate) mod upload;
pub(crate) mod voice;

pub(crate) use commands::TerminalApp;

/// The native bootstrap profiles this build can offer.
pub(crate) fn bootstrap_capabilities() -> phux_protocol::caps::BootstrapCapabilities {
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    {
        phux_terminal::native_state::native_bootstrap_capabilities()
    }
    #[cfg(not(all(feature = "native-engine", not(target_arch = "wasm32"))))]
    {
        phux_protocol::caps::BootstrapCapabilities::new()
    }
}
