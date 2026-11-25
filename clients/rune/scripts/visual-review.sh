#!/usr/bin/env bash
# Immutable local renderer evidence, not an app/server launch or a visual pass.
# Usage: bash clients/rune/scripts/visual-review.sh /absolute/fresh/before
set -euo pipefail
scripts="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$scripts/../../.." && pwd)"
if [[ $# != 1 || "$1" != /* ]]; then
    echo "usage: bash clients/rune/scripts/visual-review.sh ABS_FRESH_OUT" >&2
    exit 2
fi
out="$1"
if [[ -e "$out" || -L "$out" ]]; then
    echo "refusing existing output (use separate fresh before/after directories): $out" >&2
    exit 2
fi
# mkdir, without -p, also rejects concurrent claims of this exact output.
mkdir "$out"
mkdir "$out/logs" "$out/inbox" "$out/settings" "$out/navigation" "$out/resources" "$out/prefs"
cd "$repo/clients/rune"

# Do not inherit a production server address or another run's export paths.
for name in ${!PHUX_@} ${!SIMCTL_CHILD_PHUX_@}; do
    unset "$name"
done
export RUNE_INBOX_SNAP="$out/inbox"
export RUNE_SETTINGS_SNAP="$out/settings"
export RUNE_NAVIGATION_SNAP="$out/navigation"
export RUNE_RESOURCE_SNAP="$out/resources/mixed.png"
export RUNE_PREFS_DIR="$out/prefs"

render() {
    local filter="$1"
    local log="$out/logs/$filter.log"
    echo "rendering $filter"
    if ! cargo test --release --locked --lib "$filter" -- --nocapture --test-threads=1 >"$log" 2>&1; then
        cat "$log" >&2
        return 1
    fi
    if grep -q 'running 0 tests' "$log"; then
        echo "test filter matched nothing: $filter (see $log)" >&2
        return 1
    fi
}

render phone_inbox_visual_review_frames
render phone_acp_visual_review_frames
render acp_form_visual_review_frames
render acp_ask_owner_visual_review_frames
render settings_native_render_frames
render settings_size_visual_review_frames
render settings_theme_visual_review_frames
render mobile_appearance_and_real_theme_palette_render_accept_and_cancel
render session_browser_renders_realistic_dark_light_and_search_states
render phone_session_canvas_visual_review_frames
render phone_session_search_visual_review_frames
render retina_navigation_renders_in_native_safe_area_and_desktop_phone_viewports
render phone_header_visual_review_frames
render phone_states_visual_review_frames
render reconnect_feedback_visual_review_frames
render phone_connections_visual_review_frames
render phone_connections_keyboard_visual_review_frames
render phone_accessory_visual_review_frames
render phone_accessory_resize_visual_review_frames
render phone_pane_motion_visual_review_frames
render phone_pane_picker_visual_review_frames
render attention_contrast_visual_review_frames
render sidebar_focus_visual_review_frames
render sidebar_hierarchy_visual_review_frames
render native_png_headless_renderer_smoke
python3 -B "$scripts/visual_review.py" "$out" --repo "$repo" >"$out/logs/manifest.log" 2>&1 || {
    cat "$out/logs/manifest.log" >&2
    exit 1
}
echo "Evidence: $out/index.html"
echo "NOT REVIEWED: complete $out/judgments.md after inspecting before and after."
