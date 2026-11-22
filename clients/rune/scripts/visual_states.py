"""Required frames from the real phone states exporter: the stage's waits
and failures, the reconnect banner, and the phone's secondary lists."""
from itertools import product

STAGE = ("opening", "connecting", "slow", "unreachable", "reconnecting")
LISTS = ("commands", "no-match", "machines")


def states_inventory(themes=("dark", "light")):
    cases = {
        f"navigation/states-{state}-{theme}-native-{native}-{scale}x-{width * scale}.png":
            (width, height, scale)
        for state, theme, (width, height, scale, native) in product(
            STAGE + LISTS, themes, ((393, 852, 1, "false"), (320, 568, 1, "false")))
    }
    # The native host draws its own stage words, not the lists' sheets.
    cases.update({
        f"navigation/states-{state}-{theme}-native-true-2x-786.png": (393, 480, 2)
        for state, theme in product(STAGE, themes)
    })
    return cases
