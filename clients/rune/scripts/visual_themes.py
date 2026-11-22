"""Selected Theme flow: integrated text scales and real palette round trips."""
from itertools import product


def theme_inventory(themes):
    return {
        f"settings/theme-{form}-{theme}-{text}-{state}-{scale}x.png": (width, height, scale)
        for (form, width, height, scale), theme, text, state in product(
            (("desktop", 1100, 720, 1), ("desktop-min", 640, 360, 1),
             ("phone", 393, 852, 1), ("small", 320, 568, 1),
             ("native", 393, 480, 2), ("native-small", 320, 340, 2)),
            themes, ("standard", "ax5"),
            ("revealed", "heading", "search", "chooser", "cancelled", "accepted"))
    }
