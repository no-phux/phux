//! Attach, bootstrap, reconnect, and consumer convergence integration tests.
//! Submodules retain their original suite names in nextest test paths.

mod attach_create_if_missing;
mod attach_cwd_snapshot;
mod attach_last;
mod attach_terminal_closed;
mod attach_viewport_resize;
mod bootstrap_compression;
mod byc_6_1_attach_snapshot;
mod byc_6_6_attach_unknown_session_error;
mod concurrent_attach_l2;
mod detach_terminal;
mod eof_detach;
mod hello_survives_detach;
mod lagged_attach_terminal_resync;
mod multi_client_scenario;
mod phux_eb0_in_process_reattach;
mod reattach_multipane_input;
mod reconnect_scenario;
mod release_bootstrap_milestones;
mod retry_generation;
mod statesync_convergence;
