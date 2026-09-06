//! Terminal actors, input encoding, synthesis, and replay integration tests.

mod history_cursor_race;
mod htop_keys;
mod hunt_grid_extract_edges;
mod hunt_grid_synthesis_roundtrip;
mod hunt_grid_unicode_bounds;
mod input_dispatch;
mod kip_roundtrip;
mod lagged_consumer_resync;
mod phux_0q8_no_double_emit;
mod phux_3uv_acked_incremental;
mod pty_pump;
mod q0e_1_incremental_synthesis;
mod q0e_actor;
mod replay_equivalence;
mod route_input_no_resize;
mod route_paste;
mod screen_harness_demo;
