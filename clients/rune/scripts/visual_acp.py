"""Selected ACP form flow, matched to phone_tests/form/render.rs (152 frames)."""
from itertools import product


PHONES = (
    ("phone", 393, 852, 852, 1), ("small", 320, 568, 568, 1),
    ("native", 393, 759, 480, 2), ("native-small", 320, 478, 340, 2),
)
CONTENTS = ("ordinary", "long")
TEXT_SIZES = ("standard", "ax5")
EXPORTER = "acp_form_visual_review_frames"
ASK_OWNER_EXPORTER = "acp_ask_owner_visual_review_frames"


def ask_owner_inventory(themes):
    """Supported choice-only Cursor asks; reduced 2× is not UIKit IME proof."""
    return {
        f"inbox/acp-ask-owner-{form}-{theme}-{stage}-{width}x{height}-{scale}x.png":
            (width, height, scale)
        for (form, width, height, scale), theme, stage in product(
            (("desktop", 1100, 720, 1), ("desktop-min", 640, 360, 1),
             ("phone", 393, 852, 1), ("small", 320, 568, 1),
             ("native", 393, 480, 2), ("native-small", 320, 340, 2)),
            themes, ("a-open", "a-selected", "b-open", "b-selected", "a-return"))
    }


def case(form, content, text, theme, stage, width, height, scale):
    name = f"acp-form-{form}-{content}-{text}-{theme}-{stage}-{width}x{height}-{scale}x"
    return f"inbox/{name}.png", (width, height, scale)


def phone_inventory(themes):
    cases = {}
    for (form, width, height, editing, scale), content, text, theme in product(
            PHONES, CONTENTS, TEXT_SIZES, themes):
        stages = [("top", height), ("field-focus", editing), ("final-action", editing)]
        if scale == 2:
            stages.append(("submit-revealed", editing))
        if scale == 2 and content == "long":
            stages.append(("field-heading", editing))
        if scale == 2 and content == "ordinary":
            stages.append(("selected", height))
        for stage, current_height in stages:
            key, geometry = case(form, content, text, theme, stage, width, current_height, scale)
            cases[key] = geometry
    return cases


def desktop_inventory(themes):
    return dict(
        case(form, content, "standard", theme, stage, width, height, 1)
        for (form, width, height), content, theme, stage in product(
            (("desktop", 1100, 720), ("desktop-min", 640, 360)),
            CONTENTS, themes, ("top", "final-action"))
    )


def status_inventory(themes):
    return dict(
        case(form, "ordinary", "ax5", theme, stage, width, editing, scale)
        for (form, width, _height, editing, scale), theme, stage in product(
            PHONES[2:], themes, ("held", "error"))
    )


def form_inventory(themes):
    return {**phone_inventory(themes), **desktop_inventory(themes), **status_inventory(themes)}
