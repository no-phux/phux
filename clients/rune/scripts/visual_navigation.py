"""Maintained navigation exporter matrix: reading, focus, scrolling and motion."""
from itertools import product
from struct import pack, unpack

from visual_motion import accessory_resize_inventory, pane_motion_inventory
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
    cases.update({
        f"navigation/connections-keyboard-{theme}-{state}-{width * 2}.png":
            (width, height, 2)
        for theme, state, (width, height) in product(
            themes, ("top", "bottom"), ((320, 340), (393, 480)))
    })
    cases.update(states_inventory(themes))
    cases.update(reconnect_inventory(themes))
    cases.update(pane_motion_inventory(themes))
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
            (width, height, 2)
        for theme, mode, state, (width, height) in product(
            themes, ("normal", "forced", "collapsed"),
            ("idle", "row", "inbox-focus", "inbox-open"),
            ((1100, 720), (640, 360)))
    })
    return cases


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


def reconnect_geometry(width, height, scale, phone, large):
    # Rune's interface zoom changes logical viewport and effective scale, not
    # the requested physical output. Match its f32 math without loosening the
    # maintained geometry check. Phones use native text size instead of zoom.
    if phone == "false" and large == "true":
        zoom = unpack("f", pack("f", 1.4))[0]
        logical = [unpack("f", pack("f", value / zoom))[0] for value in (width, height)]
        return (*logical, zoom)
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
        ("reconnect-", "reconnect_feedback_visual_review_frames"),
        ("attention-", "attention_contrast_visual_review_frames"),
        ("sidebar-focus-", "sidebar_focus_visual_review_frames"),
        ("accessory-resize-", "phone_accessory_resize_visual_review_frames"),
        ("accessory-", "phone_accessory_visual_review_frames"),
        ("connections-keyboard-", "phone_connections_keyboard_visual_review_frames"),
        ("connections-", "phone_connections_visual_review_frames"),
        ("pane-motion-", "phone_pane_motion_visual_review_frames"),
        ("header-", "phone_header_visual_review_frames"),
        ("states-", "phone_states_visual_review_frames"),
        ("retina-", "retina_navigation_renders_in_native_safe_area_and_desktop_phone_viewports"),
    ):
        if stem.startswith(prefix):
            return exporter
    return "session_browser_renders_realistic_dark_light_and_search_states"
