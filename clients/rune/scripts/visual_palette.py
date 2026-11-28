"""The command palette's maintained visual review cases."""
from itertools import product

EXPORTER = "palette_visual_review_frames"
THEMES = ("dark", "light")
PALETTE_WINDOWS = (
    ("desktop", 1100, 720), ("desktop", 640, 360),
    ("phone", 393, 852), ("phone", 320, 568),
)
PALETTE_STATES = ("empty", "typed", "no-match", "themes", "sessions")


def palette_inventory():
    """The command palette's frames (`palette_visual_review_frames`), all 2x."""
    cases = {}
    for (form, width, height), state, theme in product(PALETTE_WINDOWS, PALETTE_STATES, THEMES):
        cases[f"palette/{form}-{width}x{height}-{state}-{theme}.png"] = (width, height, 2)
    for (form, width, height), state, variant, theme in product(
            PALETTE_WINDOWS[2:], ("empty", "typed"), ("keyboard", "largest"), THEMES):
        cases[f"palette/{form}-{width}x{height}-{state}-{variant}-{theme}.png"] = (width, height, 2)
    return cases
