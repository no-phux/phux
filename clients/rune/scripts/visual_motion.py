"""Required fixed-clock frames from the real phone pane-gesture exporter."""
from itertools import product


MOTION_FRAMES = {
    "idle": 0, "start": 1, "first-axis": 2, "mid-drag": 3,
    "release": 4, "release-plus-100ms": 10, "settled": 100,
}


def pane_motion_inventory(themes=("dark", "light")):
    small = (320, 568, 1, "false", "false", "false")
    scenarios = [("horizontal", *view) for view in (
        small, (393, 852, 1, "false", "false", "false"),
        (393, 480, 2, "true", "false", "false"),
        (320, 340, 2, "true", "false", "false"))]
    scenarios += [(gesture, *small) for gesture in ("vertical", "cancel", "boundary", "reversal")]
    scenarios += [
        ("horizontal", 320, 568, 1, "false", "true", "false"),
        ("horizontal", 320, 568, 1, "false", "false", "true"),
    ]
    return {
        f"navigation/pane-motion-{gesture}-{theme}-{width}x{height}-{scale}x-"
        f"native-{native}-keyboard-{keyboard}-reduced-{reduced}-{stage}.png":
            (width, height, scale)
        for (gesture, width, height, scale, native, keyboard, reduced), theme, stage
        in product(scenarios, themes, MOTION_FRAMES)
    }
