//! The Terminal app's host side: session attach and the input lane, over the
//! engine in `phux-terminal`.

#![allow(
    clippy::redundant_pub_crate,
    reason = "private module shared across the runtime; pub would violate unreachable_pub"
)]

pub(crate) mod agent_state;
mod app;
pub(crate) mod attach;
pub(crate) mod capture;
pub(crate) mod checkout;
mod engine;
pub(crate) mod history;
pub(crate) mod input_lane;
pub(crate) mod launch;
pub(crate) mod notify_mode;
pub(crate) mod parent;
pub(crate) mod program_note;
pub(crate) mod program_status;
pub(crate) mod resume;
pub(crate) mod scrollback;
pub(crate) mod standing;
#[cfg(test)]
pub(crate) mod test_support;
pub(crate) mod upload;
pub(crate) mod voice;

pub(crate) use app::TerminalApp;

/// Start the input lane and schedule cleanup of stale uploads off the runtime thread.
pub(crate) fn start(state: &crate::state::SharedState) -> std::io::Result<input_lane::InputLane> {
    let lane = input_lane::spawn_input_lane(state.clone())?;
    let upload_env = state.with(|s| s.config().server_env.clone());
    drop(tokio::task::spawn_blocking(move || {
        upload::sweep_stale_partials_at_startup(&upload_env);
    }));
    Ok(lane)
}

/// The injected engine's metric table, if there is an engine.
pub(crate) fn metric_table(
    state: &crate::state::SharedState,
) -> Option<&'static [phux_perf::Metric]> {
    state.terminal_engine().map(|engine| engine.metrics())
}

/// The native bootstrap profiles this build can offer.
pub(crate) fn bootstrap_capabilities(
    state: &crate::state::SharedState,
) -> phux_protocol::caps::BootstrapCapabilities {
    state
        .terminal_engine()
        .map_or_else(phux_protocol::caps::BootstrapCapabilities::new, |engine| {
            engine.bootstrap_capabilities()
        })
}
