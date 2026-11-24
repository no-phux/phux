"""Header-only fake Cargo for runner unit tests, NEVER visual review evidence."""
import json
import os
from pathlib import Path
import struct
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path.cwd() / "scripts"))
import visual_review as review

assert sys.argv[1:5] == ["test", "--release", "--locked", "--lib"]
assert sys.argv[6:] == ["--", "--nocapture", "--test-threads=1"]
assert not any(key.startswith(("PHUX_", "SIMCTL_CHILD_PHUX_")) for key in os.environ)
assert Path.cwd().name == "rune"
preferences = Path(os.environ["RUNE_PREFS_DIR"])
assert preferences == Path(os.environ["RUNE_INBOX_SNAP"]).parent / "prefs"
assert preferences.is_dir()
# Simulate a preference write only to qualify runner isolation, never rendering.
(preferences / "runner-fixture.json").write_text('{"fixture": true}\n')
filters = {
    "phone_inbox_visual_review_frames": "RUNE_INBOX_SNAP",
    "phone_acp_visual_review_frames": "RUNE_INBOX_SNAP",
    "acp_form_visual_review_frames": "RUNE_INBOX_SNAP",
    "acp_ask_owner_visual_review_frames": "RUNE_INBOX_SNAP",
    "settings_native_render_frames": "RUNE_SETTINGS_SNAP",
    "settings_size_visual_review_frames": "RUNE_SETTINGS_SNAP",
    "settings_theme_visual_review_frames": "RUNE_SETTINGS_SNAP",
    "mobile_appearance_and_real_theme_palette_render_accept_and_cancel": "RUNE_SETTINGS_SNAP",
    "session_browser_renders_realistic_dark_light_and_search_states": "RUNE_NAVIGATION_SNAP",
    "retina_navigation_renders_in_native_safe_area_and_desktop_phone_viewports": "RUNE_NAVIGATION_SNAP",
    "phone_header_visual_review_frames": "RUNE_NAVIGATION_SNAP",
    "phone_states_visual_review_frames": "RUNE_NAVIGATION_SNAP",
    "reconnect_feedback_visual_review_frames": "RUNE_NAVIGATION_SNAP",
    "phone_connections_visual_review_frames": "RUNE_NAVIGATION_SNAP",
    "phone_connections_keyboard_visual_review_frames": "RUNE_NAVIGATION_SNAP",
    "phone_accessory_visual_review_frames": "RUNE_NAVIGATION_SNAP",
    "phone_accessory_resize_visual_review_frames": "RUNE_NAVIGATION_SNAP",
    "phone_pane_motion_visual_review_frames": "RUNE_NAVIGATION_SNAP",
    "attention_contrast_visual_review_frames": "RUNE_NAVIGATION_SNAP",
    "sidebar_focus_visual_review_frames": "RUNE_NAVIGATION_SNAP",
    "native_png_headless_renderer_smoke": "RUNE_RESOURCE_SNAP",
}
filter = sys.argv[5]
variable = filters[filter]
for relative, (width, height, scale) in review.required_inventory().items():
    case = Path(relative)
    if review.exporter_for(case.parts[0], case.stem) != filter:
        continue
    path = Path(os.environ[variable])
    if variable != "RUNE_RESOURCE_SNAP":
        path = path / case.name
    pixels = [round(width * scale), round(height * scale)]
    path.write_bytes(struct.pack(">8sI4sIIBBBBBI", b"\x89PNG\r\n\x1a\n",
                                13, b"IHDR", *pixels, 8, 6, 0, 0, 0, 0))
    tree = {
        "schema": 1, "projection": "test fixture", "viewport": [width, height],
        "scale": scale, "pixels": pixels,
        "scenario": "exported fixture", "theme": "dark",
        "elements": [{"path": "root"}], "accessibility": {},
    }
    if path.stem.startswith(("pane-motion-", "accessory-resize-")):
        frames = review.motion_frames(path.stem)
        stage = next(stage for stage in frames if path.stem.endswith(f"-{stage}"))
        scenario = path.stem[:-(len(stage) + 1)]
        tree["scenario"] = scenario
        frame = frames[stage]
        path.with_suffix(".motion.json").write_text(json.dumps({
            "schema": 1, "scenario": scenario, "stage": stage,
            "frame": frame, "elapsed_ms": frame * 1000.0 / 60.0,
        }))
    path.with_suffix(".json").write_text(json.dumps(tree))
print("running 1 test")
print("test result: ok. 1 passed")
