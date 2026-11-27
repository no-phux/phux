"""Required fixed-clock frames from real phone gesture and control exporters."""
from itertools import product


MOTION_FRAMES = {
    "idle": 0, "start": 1, "first-axis": 2, "mid-drag": 3,
    "release": 4, "release-plus-100ms": 10, "settled": 100,
}

ACCESSORY_RESIZE_FRAMES = {"first": 1, "middle": 6, "settled": 60}
BATCHED_SWIPE_FRAMES = {
    "idle": 0, "start": 1, "release": 2, "release-plus-100ms": 8, "settled": 128,
}


def motion_frames(stem):
    if stem.startswith("batched-swipe-"):
        return BATCHED_SWIPE_FRAMES
    return ACCESSORY_RESIZE_FRAMES if stem.startswith("accessory-resize-") else MOTION_FRAMES


def batched_swipe_inventory(themes=("dark", "light")):
    return {
        f"navigation/batched-swipe-{theme}-{width}x{height}-2x-reduced-{reduced}-{stage}.png":
            (width, height, 2)
        for theme, (width, height), reduced, stage in product(
            themes, ((393, 852), (320, 568), (393, 480), (320, 340)),
            ("false", "true"), BATCHED_SWIPE_FRAMES)
    }


def accessory_resize_inventory(themes=("dark", "light")):
    return {
        f"navigation/accessory-resize-{flow}{direction}-{theme}-{width}x{height}-2x-{stage}.png":
            (width, height, 2)
        for flow, direction, theme, (width, height), stage in product(
            ("", "settings-"), ("grow", "shrink"), themes,
            ((320, 340), (393, 480)), ACCESSORY_RESIZE_FRAMES)
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
