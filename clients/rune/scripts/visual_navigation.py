"""Maintained navigation exporter matrix: reading, focus, scrolling and motion."""
from itertools import product

from visual_motion import pane_motion_inventory


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
    cases.update(pane_motion_inventory(themes))
    return cases


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
