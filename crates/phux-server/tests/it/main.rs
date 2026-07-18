//! Consolidated phux-server integration-test harness (phux-mmxz.2).
//!
//! nextest runs every test in its own process, so folding these files
//! into one binary keeps per-test isolation while collapsing ~50 link
//! steps into one. The `perf_*`, `stress_*`, `key_encode_snapshot`, and
//! `wt_attach` files stay separate binaries: CI gates reference them by name
//! (`binary_id` / `--test`), they own insta snapshots, or they carry a
//! crate-level `#![cfg]`.

mod common;

mod agent_asked;
mod agent_detect;
mod agent_events;
mod attach_create_if_missing;
mod attach_cwd_snapshot;
mod attach_last;
mod attach_lifecycle;
mod attach_viewport_resize;
mod byc_6_1_attach_snapshot;
mod byc_6_3_detach_clean_shutdown;
mod byc_6_5_keystroke_merge_order;
mod byc_6_6_attach_unknown_session_error;
mod command_dispatch;
mod concurrent_attach_l2;
mod concurrent_attach_no_lag;
mod end_to_end;
mod eof_detach;
mod htop_keys;
mod hub_relay_federation;
mod hub_runtime;
mod hunt_grid_extract_edges;
mod hunt_grid_synthesis_roundtrip;
mod hunt_grid_unicode_bounds;
mod input_dispatch;
mod kip_roundtrip;
mod l2_adversarial;
mod metadata_reply;
mod mouse_wheel_e2e;
mod multi_client_scenario;
mod phux_0q8_no_double_emit;
mod phux_3uv_acked_incremental;
mod phux_eb0_in_process_reattach;
mod pty_pump;
mod q0e_1_incremental_synthesis;
mod q0e_3_tick_scheduler;
mod q0e_4_frame_ack;
mod reattach_multipane_input;
mod reconnect_scenario;
mod relay_connector_spike;
mod replay_equivalence;
mod route_input_no_resize;
mod route_paste;
mod screen_harness_demo;
mod server_self_exit;
mod socket_lifecycle;
mod spawn_terminal;
mod statesync_convergence;
mod web_caps_image_gate;
mod ws_attach;
mod ws_transport;
