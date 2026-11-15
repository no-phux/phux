// The browser binaries to run, and proof each required regression executed.
export const BROWSER_SUITES =
  "--lib --test canvas --test render --test e2e_browser --test e2e_input".split(" ");

export const REQUIRED_TESTS = [
  "renders_engine_grid_to_canvas",
  "a_wide_character_paints_across_its_spacer_cell",
  "exact_wasm_codec_selects_native_and_renders_live_server",
  "agents_started_after_browser_attach_update_badges_without_reloading",
  "synthesized_only_browser_remains_compatible_with_native_server",
  "keys_ime_commits_and_paste_reach_the_terminal_and_nothing_else_is_captured",
  "resize_reflows_the_live_terminal_and_canvas",
  "scrollback_pages_by_wheel_and_shift_page_up_and_a_drag_copies_from_it",
  "find_highlights_matches_in_history_and_steps_between_them",
  "mouse_reports_reach_a_tracking_program_and_shift_drag_still_selects",
  "modifier_click_opens_http_links_in_a_new_tab_and_nothing_else",
  "the_bell_announces_itself_and_flashes_the_canvas",
  "a_double_width_character_copies_without_a_trailing_space",
  "focus_changes_reach_a_program_that_asks_for_them",
  "the_wheel_on_the_alternate_screen_is_arrow_keys",
  "mouse_positions_land_on_their_cells_after_another_client_reports_other_cells",
  "a_finger_scrolls_scrollback_a_hold_selects_and_a_tap_opens_a_link",
  "a_hold_still_selects_scrollback_when_another_cell_size_detaches",
  "on_screen_keys_and_the_ctrl_latch_reach_the_terminal",
  // Unit tests in src/ that need a DOM and the live server.
  "exported_start_retains_live_client_until_transport_failure",
  "closed_ws_send_and_repeated_reconnect_teardown_release_every_app",
  "a_version_refusal_marks_the_canvas_and_never_redials",
  "dropping_polled_establishment_disposes_app_reader_and_transport",
  "dpr_backing_store_keeps_css_extent_and_measured_dataset",
  "repeated_prepare_and_one_axis_resize_do_not_compound_transforms",
  "css_hit_testing_and_wire_pixels_do_not_depend_on_dpr",
  "cursor_row_and_translated_pane_render_in_logical_coordinates",
  "pane_borders_are_one_logical_pixel_and_neutral",
  "invalid_dpr_falls_back_to_logical_resolution",
] as const;
