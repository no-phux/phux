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
# A short let-go springs home, then a long one leaves. Stages that end like
# another ("short-release", "release") are listed first, so a stem matches
# its own.
DISMISS_FRAMES = {
    "idle": 0, "mid-drag": 1, "short-release": 2, "short-release-plus-100ms": 8,
    "short-settled": 68, "past-commit": 69, "release": 70, "release-plus-100ms": 76,
    "release-plus-200ms": 82, "settled": 136,
}
DISMISS_GESTURES = (
    "sessions-left", "actions-down", "switcher-down", "inbox-down", "palette-down",
    "settings-back", "rename-down",
)
# Every page coming and going by its own control, after the press.
PAGE_MOTION_FRAMES = {
    "idle": 0, "commit": 1, "plus-50ms": 3, "plus-100ms": 6, "plus-200ms": 12, "settled": 60,
}
PAGE_MOTION_CHANGES = (
    "sessions-open", "sessions-close", "actions-open", "actions-close", "switcher-open",
    "switcher-to-sessions", "inbox-open", "inbox-close", "palette-open", "settings-open",
    "settings-push", "settings-pop", "settings-close", "rename-open", "find-open",
    "machines-open", "news-open",
)
# Size, theme, Reduce Motion and text size of each case.
PAGE_MOTION_CASES = (
    ((393, 852), "dark", "false", "standard"),
    ((320, 568), "light", "false", "standard"),
    ((393, 852), "dark", "true", "standard"),
    ((320, 568), "dark", "true", "ax5"),
)
SWITCHER_SWIPE_FRAMES = {
    "idle": 0, "mid-drag": 1, "past-commit": 2, "release": 3, "release-plus-100ms": 9,
    "settled": 63,
}
SWITCHER_SWIPE_CASES = (((393, 852), "dark"), ((320, 568), "light"))

# The stems whose frames carry a fixed-clock `.motion.json`.
MOTION_STEMS = (
    "pane-motion-", "accessory-resize-", "batched-swipe-", "dismiss-", "motion-",
    "switcher-swipe-",
)


def motion_frames(stem):
    if stem.startswith("batched-swipe-"):
        return BATCHED_SWIPE_FRAMES
    if stem.startswith("dismiss-"):
        return DISMISS_FRAMES
    if stem.startswith("motion-"):
        return PAGE_MOTION_FRAMES
    if stem.startswith("switcher-swipe-"):
        return SWITCHER_SWIPE_FRAMES
    return ACCESSORY_RESIZE_FRAMES if stem.startswith("accessory-resize-") else MOTION_FRAMES


def batched_swipe_inventory(themes=("dark", "light")):
    return {
        f"navigation/batched-swipe-{theme}-{width}x{height}-2x-reduced-{reduced}-{stage}.png":
            (width, height, 2)
        for theme, (width, height), reduced, stage in product(
            themes, ((393, 852), (320, 568), (393, 480), (320, 340)),
            ("false", "true"), BATCHED_SWIPE_FRAMES)
    }


def dismiss_inventory(themes=("dark", "light")):
    return {
        f"navigation/dismiss-{gesture}-{theme}-{width}x{height}-2x-reduced-{reduced}-{stage}.png":
            (width, height, 2)
        for gesture, theme, (width, height), reduced, stage in product(
            DISMISS_GESTURES, themes, ((393, 852), (320, 568)), ("false", "true"),
            DISMISS_FRAMES)
    }


def page_motion_inventory(themes=("dark", "light")):
    return {
        f"navigation/motion-{change}-{theme}-{width}x{height}-2x-{text}-reduced-{reduced}-"
        f"{stage}.png": (width, height, 2)
        for change, ((width, height), theme, reduced, text), stage in product(
            PAGE_MOTION_CHANGES, PAGE_MOTION_CASES, PAGE_MOTION_FRAMES)
        if theme in themes
    }


def switcher_swipe_inventory(themes=("dark", "light")):
    return {
        f"navigation/switcher-swipe-{theme}-{width}x{height}-2x-reduced-{reduced}-{stage}.png":
            (width, height, 2)
        for ((width, height), theme), reduced, stage in product(
            SWITCHER_SWIPE_CASES, ("false", "true"), SWITCHER_SWIPE_FRAMES)
        if theme in themes
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
