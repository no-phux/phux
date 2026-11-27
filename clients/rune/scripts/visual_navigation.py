"""Maintained navigation exporter matrix: reading, focus, scrolling and motion."""
from itertools import product
from struct import pack, unpack

from visual_motion import accessory_resize_inventory, batched_swipe_inventory, pane_motion_inventory
from visual_states import states_inventory


def navigation_inventory(themes=("dark", "light")):
    cases = {
        f"navigation/{state}-{theme}-{width}.png": (width, height, 1)
        for state, theme, (width, height) in product(
            ("inventory", "scrolled", "matching", "search"), themes,
            ((393, 852), (320, 568), (768, 1024)))
    }
    cases.update({
        f"navigation/retina-{scale}x-native-{native}-{width * scale}.png": (width, height, scale)
        for width, height, scale, native in (
            (320, 568, 2, "false"), (393, 852, 3, "false"),
            (320, 340, 2, "true"), (393, 480, 3, "true"))
    })
    cases.update({
        f"navigation/header-{state}-{theme}-native-{native}-{scale}x-{width * scale}.png":
            (width, height, scale)
        for state, theme, (width, height, scale, native) in product(
            ("quiet", "waiting", "split", "keyboard", "large-text", "empty"), themes,
            ((393, 852, 1, "false"), (320, 568, 1, "false"),
             (393, 480, 2, "true"), (320, 340, 2, "true")))
    })
    cases.update(connections_inventory(themes))
    cases.update(session_search_inventory(themes))
    cases.update({
        f"navigation/canvas-{state}-{theme}-{width * 2}.png":
            (width, keyboard_height if state.startswith("query-") else height, 2)
        for state, theme, (width, height, keyboard_height) in product(
            ("connections", "query-top", "query-bottom", "empty"), themes,
            ((320, 478, 340), (393, 759, 480)))
    })
    cases.update({
        f"navigation/connections-keyboard-{theme}-{state}-{width * 2}.png":
            (width, height, 2)
        for theme, state, (width, height) in product(
            themes, ("top", "bottom"), ((320, 340), (393, 480)))
    })
    cases.update(states_inventory(themes))
    cases.update(reconnect_inventory(themes))
    cases.update(pane_motion_inventory(themes))
    cases.update(batched_swipe_inventory(themes))
    cases.update({
        f"navigation/pane-picker-{theme}-{text}-{width}x{height}-{scale}x-"
        f"native-{native}-{state}.png": (width, height, scale)
        for theme, text, (width, height, scale, native), state in product(
            themes, ("standard", "ax5"),
            ((393, 852, 1, "false"), (320, 568, 1, "false"),
             (393, 480, 2, "true"), (320, 340, 2, "true")),
            ("top", "focused", "bottom"))
    })
    cases.update(accessory_inventory(themes))
    cases.update(accessory_resize_inventory(themes))
    cases.update({
        f"navigation/attention-{theme}-{mode}-{state}-{width}x{height}-2x.png":
            (width, height, 2)
        for theme, mode, state, (width, height) in product(
            themes, ("normal", "forced"), ("closed", "focused", "inbox"),
            ((1100, 720), (640, 360)))
    })
    cases.update({
        f"navigation/sidebar-focus-{theme}-{mode}-{state}-{width}x{height}-2x.png":
            zoomed_geometry(width, height, 2) if mode == "large" else (width, height, 2)
        for theme, mode, state, (width, height) in product(
            themes, ("normal", "forced", "large"),
            ("idle", "row", "inbox-focus", "inbox-open"),
            ((1100, 720), (640, 360)))
    })
    cases.update(sidebar_hierarchy_inventory(themes))
    return cases


def sidebar_hierarchy_inventory(themes):
    return {
        f"navigation/sidebar-hierarchy-{theme}-{mode}-{state}-{width}x{height}-2x.png":
            zoomed_geometry(width, height, 2) if mode == "large" else (width, height, 2)
        for theme, mode, state, (width, height) in product(
            themes, ("normal", "large", "forced"),
            ("expanded", "single", "split", "hosts", "focused", "empty"),
            ((1100, 720), (640, 360)))
    }


def session_search_inventory(themes):
    return {
        f"navigation/session-search-{state}-{theme}-{text}-native-{native}-"
        f"{width}x{height}-{width * 2}.png": (width, height, 2)
        for state, theme, text, (width, height, native) in product(
            ("matching", "no-match", "long", "blurred", "bottom"), themes, ("standard", "ax5"),
            ((320, 568, "false"), (393, 852, "false"),
             (320, 478, "true"), (393, 759, "true"),
             (320, 340, "true"), (393, 480, "true")))
    }


def reconnect_inventory(themes):
    return {
        f"navigation/reconnect-{state}-{theme}-large-{large}-phone-{phone}-"
        f"native-{native}-{scale}x-{phase}-{width * scale}.png":
            reconnect_geometry(width, height, scale, phone, large)
        for state, theme, large, (width, height, scale, phone, native), phase in product(
            ("reconnecting", "long", "pair", "update"), themes, ("false", "true"),
            ((393, 852, 1, "true", "false"), (320, 568, 1, "true", "false"),
             (393, 480, 2, "true", "true"), (320, 340, 2, "true", "true"),
             (1100, 720, 1, "false", "false"), (640, 360, 1, "false", "false")),
            ("top", "bottom", "focused"))
        if phone == "true" or phase == "top"
    }


def zoomed_geometry(width, height, scale):
    zoom = unpack("f", pack("f", 1.4))[0]
    logical = [unpack("f", pack("f", value / zoom))[0] for value in (width, height)]
    effective_scale = unpack("f", pack("f", scale * zoom))[0]
    return (*logical, effective_scale)


def reconnect_geometry(width, height, scale, phone, large):
    # Rune's interface zoom changes logical viewport and effective scale, not
    # the requested physical output. Match its f32 math without loosening the
    # maintained geometry check. Phones use native text size instead of zoom.
    if phone == "false" and large == "true":
        return zoomed_geometry(width, height, scale)
    return (width, height, scale)


def accessory_inventory(themes):
    return {
        f"navigation/accessory-{flow}-{text}-{theme}-native-{native}-"
        f"{width}x{height}-{scale}x-{edge}-{width * scale}.png": (width, height, scale)
        for flow, text, theme, (width, height, scale, native), edge in product(
            ("sessions", "settings"), ("standard", "ax2", "ax3", "ax5"), themes,
            ((393, 852, 1, "false"), (320, 568, 1, "false"),
             (393, 480, 2, "true"), (320, 340, 2, "true")),
            ("leading", "trailing", "tab-revealed"))
    }


def connections_inventory(themes):
    return {
        f"navigation/connections-{case}-{theme}-{scale}x-native-{native}{state}-{width * scale}.png":
            (width, height, scale)
        for case, theme, (width, height, scale, native), state in product(
            ("ordinary", "long", "large"), themes,
            ((393, 852, 1, "false"), (320, 568, 1, "false"),
             (393, 759, 2, "true"), (320, 478, 2, "true")),
            ("", "-bottom", "-focused"))
        if case != "large" or state != "-focused"
    }


def navigation_exporter(stem):
    for prefix, exporter in (
        ("batched-swipe-", "batched_swipe_visual_review_frames"),
        ("session-search-", "phone_session_search_visual_review_frames"),
        ("canvas-", "phone_session_canvas_visual_review_frames"),
        ("reconnect-", "reconnect_feedback_visual_review_frames"),
        ("attention-", "attention_contrast_visual_review_frames"),
        ("sidebar-focus-", "sidebar_focus_visual_review_frames"),
        ("sidebar-hierarchy-", "sidebar_hierarchy_visual_review_frames"),
        ("accessory-resize-", "phone_accessory_resize_visual_review_frames"),
        ("accessory-", "phone_accessory_visual_review_frames"),
        ("connections-keyboard-", "phone_connections_keyboard_visual_review_frames"),
        ("connections-", "phone_connections_visual_review_frames"),
        ("pane-motion-", "phone_pane_motion_visual_review_frames"),
        ("pane-picker-", "phone_pane_picker_visual_review_frames"),
        ("header-", "phone_header_visual_review_frames"),
        ("states-", "phone_states_visual_review_frames"),
        ("retina-", "retina_navigation_renders_in_native_safe_area_and_desktop_phone_viewports"),
    ):
        if stem.startswith(prefix):
            return exporter
    return "session_browser_renders_realistic_dark_light_and_search_states"
