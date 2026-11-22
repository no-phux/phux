#!/usr/bin/env python3
"""Package real renderer exports using only the standard library.

PNG validation is header/geometry qualification, NOT decoding, CRC validation,
pixel-diff analysis, a subjective visual pass, or native-device qualification.
"""
import argparse
import hashlib
import html
from itertools import product
import json
import math
from pathlib import Path
import re
import struct
import subprocess
from urllib.parse import quote

from visual_motion import MOTION_FRAMES, motion_frames, pane_motion_inventory
from visual_navigation import navigation_exporter, navigation_inventory
from visual_themes import theme_inventory


FAMILIES = ("inbox", "settings", "navigation", "resources")
THEMES = ("dark", "light")
# Maintained contract: extend this inventory when adding required exporter cases.
INBOX_VIEWPORTS = (
    ("desktop", 1100, 720, 1), ("desktop", 640, 360, 1),
    ("phone", 393, 852, 1), ("phone", 320, 568, 1),
    ("phone", 393, 852, 2), ("phone", 320, 568, 2),
)
SETTINGS_VIEWPORTS = (
    ("desktop", 1100, 720), ("desktop-min", 640, 360),
    ("phone", 393, 852), ("small", 320, 568),
)
PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
LIMITS = (
    "PNG headers and geometry checked; no image decoding or chunk CRC validation. "
    "Ordered elements retain duplicate paths and intentional overlaps. "
    "Offscreen renderer evidence does not prove native window chrome, safe-area "
    "integration, keyboard/IME, screen-reader behavior, touch, or real-device UX. "
    "Source metadata is observed at packaging time, not proof of exact build "
    "inputs; dirty worktrees are not claimed to match sourceRevision."
)
JUDGMENTS = """NOT REVIEWED

# Visual review — one concrete UX improvement per tick

- Selected UX category (see clients/rune/VISUAL_REVIEW.md):
- Next category / rotation override and reason:
- Selected surface (inbox / settings / navigation / resources):
- Observed issue (name exact before case and visible symptom):
- Measurable acceptance (geometry, readability, action reachability, etc.):
- UX improvement implemented this tick:
- Before directory / case / PNG SHA-256:
- After directory / case / PNG SHA-256:
- Before → after verdict (improved / unchanged / regressed; explain evidence):
- Regressions checked (other families, phone/desktop, small viewport, 2×, light/dark):
- Independent fresh-context reviewer / frame references / verdict:
- Review findings and resolution / recapture references:
- Remaining issues / follow-up:
- Reviewer and review time:

## UX category rotation

1. Alignment and spacing
2. Hierarchy and readability
3. Density and responsive layout
4. Navigation and discoverability
5. Input and touch ergonomics
6. Feedback, empty/error/loading
7. Motion and perceived responsiveness
8. Accessibility and contrast

Choose the selected and next categories from this rotation. Record any override
and its evidence; the selected surface is a separate choice.

## Compare actual artifacts

Open fresh before and after index.html files side by side. Match case names,
logical viewport and scale, inspect pixels at readable size, then consult each
ordered geometry/accessibility projection. A matching hash, passing renderer
test, or absence of overlap errors is not a visual judgment. Do not collapse
duplicate element paths or treat every overlap as a defect: lossless-clone
normalization and layered UI can intentionally overlap.

## Native proof and limits

Record separate native-device/window evidence for claims involving safe areas,
keyboard/IME, scrolling/touch, screen readers, window integration or real-device
readability. These headless frames and accessibility projections alone do not
qualify those behaviors. List anything not exercised; do not infer a pass.
"""


def require(condition, message):
    if not condition:
        raise ValueError(message)


def safe_relative(root, path):
    """Never read a symlink escape or expose an absolute artifact URL."""
    path.resolve().relative_to(root.resolve())
    return path.relative_to(root).as_posix()


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(chunk)
    return result.hexdigest()


def png_size(path):
    with path.open("rb") as stream:
        header = stream.read(33)
    require(len(header) == 33, f"{path}: truncated PNG header")
    signature, length, kind, width, height, depth, color, compression, filtering, interlace, _crc = struct.unpack(
        ">8sI4sIIBBBBBI", header
    )
    require((signature, length, kind) == (PNG_SIGNATURE, 13, b"IHDR"),
            f"{path}: invalid PNG signature/IHDR")
    require(0 < width < 2**31 and 0 < height < 2**31, f"{path}: invalid PNG dimensions")
    modes = {0: (1, 2, 4, 8, 16), 2: (8, 16), 3: (1, 2, 4, 8), 4: (8, 16), 6: (8, 16)}
    require(depth in modes.get(color, ()), f"{path}: invalid PNG color/depth")
    require((compression, filtering) == (0, 0), f"{path}: unsupported PNG coding")
    require(interlace in (0, 1), f"{path}: invalid PNG interlace")
    return [width, height]


def positive_number(value):
    return type(value) in (int, float) and math.isfinite(value) and value > 0


def logical_size(tree, path):
    viewport = tree.get("viewport")
    require(isinstance(viewport, list), f"{path}: viewport must be an array")
    require(len(viewport) == 2, f"{path}: viewport must have two dimensions")
    require(all(positive_number(value) for value in viewport), f"{path}: viewport must be positive")
    require(positive_number(tree.get("scale")), f"{path}: scale must be positive")
    return viewport


def validate_tree(tree, physical, path):
    require(isinstance(tree, dict), f"{path}: tree must be an object")
    require(type(tree.get("schema")) is int and tree["schema"] == 1, f"{path}: unknown projection schema")
    require(isinstance(tree.get("projection"), str), f"{path}: missing projection description")
    require(bool(tree["projection"].strip()), f"{path}: empty projection description")
    require(tree.get("pixels") == physical, f"{path}: PNG/tree pixel dimensions differ")
    viewport = logical_size(tree, path)
    require(all(abs(logical * tree["scale"] - pixels) <= 0.5
                for logical, pixels in zip(viewport, physical)),
            f"{path}: viewport × scale differs from physical pixels")
    elements = tree.get("elements")
    require(isinstance(elements, list), f"{path}: elements must be an ordered array")
    require(bool(elements), f"{path}: even empty inbox UI must have elements")
    require(all(isinstance(element, dict) for element in elements), f"{path}: invalid element record")
    require("accessibility" in tree, f"{path}: missing accessibility projection")
    return viewport


def inbox_inventory():
    cases = {
        f"inbox/{form}-{state}-{theme}-{width}x{height}-{scale}x.png": (width, height, scale)
        for (form, width, height, scale), state, theme in product(
            INBOX_VIEWPORTS, ("question", "reply", "approval", "empty", "scrolled",
                             "sending", "delivered", "refused", "unknown", "new-draft", "other-question"), THEMES)
    }
    cases.update({
        f"inbox/phone-native-reply-{theme}-{width}x{height}-1x.png": (width, height, 1)
        for (width, height), theme in product(((393, 420), (320, 280)), THEMES)
    })
    cases.update({
        f"inbox/phone-{state}-{theme}-{width}x{height}-1x.png": (width, height, 1)
        for state, theme, (width, height) in product(
            ("sending", "delivered", "refused", "unknown", "new-draft", "other-question"),
            THEMES, ((393, 420), (320, 280)))
    })
    cases.update({
        f"inbox/phone-{state}-{theme}-{width}x{height}-1x.png": (width, height, 1)
        for state, theme, (width, height) in product(
            ("inline", "inline-no-choices"), THEMES, ((393, 852), (320, 568)))
    })
    cases.update({
        f"inbox/phone-acp-{state}-{theme}-{width}x{height}-1x.png": (width, height, 1)
        for state, theme, (width, height) in product(
            ("select", "ask", "form", "input"), THEMES, ((393, 852), (320, 568), (320, 280)))
    })
    return cases


def settings_inventory():
    cases = {
        f"settings/{form}{suffix}.png": (width, height, 1)
        for (form, width, height), suffix in product(
            SETTINGS_VIEWPORTS, ("", "-terminal", "-updates", "-dark", "-typed-search", "-no-match"))
    }
    cases.update({
        f"settings/{form}-about-revealed.png": (width, height, 1)
        for form, width, height in SETTINGS_VIEWPORTS[:2]
    })
    cases.update({
        f"settings/{form}-appearance-{state}.png": (width, height, 1)
        for (form, width, height), state in product(
            SETTINGS_VIEWPORTS[2:], ("before", "chooser", "cancel", "accepted"))
    })
    cases.update({
        f"settings/size-{form}-{theme}-{text}-{control}-{scale}x.png": (width, height, scale)
        for (form, width, height, scale), theme, text, control in product(
            (("desktop", 1100, 720, 1), ("desktop-min", 640, 360, 1),
             ("phone", 393, 852, 1), ("small", 320, 568, 1),
             ("native", 393, 480, 2), ("native-small", 320, 340, 2)),
             THEMES, ("standard", "ax5"), ("text-bigger", "interface-bigger"))
    })
    cases.update(theme_inventory(THEMES))
    return cases


def required_inventory():
    """989 required cases: 192 Inbox, 226 Settings, 570 navigation, 1 resource.

    Values are (logical width, logical height, scale); additional cases are
    welcome, but cannot replace any maintained case.
    """
    return {
        **inbox_inventory(), **settings_inventory(), **navigation_inventory(THEMES),
        "resources/mixed.png": (800, 600, 1),
    }


def required_label(tree, key, path):
    value = tree.get(key)
    require(isinstance(value, str), f"{path}: missing exporter {key}")
    require(bool(value.strip()), f"{path}: empty exporter {key}")
    return value


def inbox_exporter_for(stem):
    if stem.startswith("phone-acp-"):
        return "phone_acp_visual_review_frames"
    return "phone_inbox_visual_review_frames"


def exporter_for(family, stem):
    if family == "settings":
        if stem.startswith("size-"):
            return "settings_size_visual_review_frames"
        if stem.startswith("theme-"):
            return "settings_theme_visual_review_frames"
        if "-appearance-" in stem:
            return "mobile_appearance_and_real_theme_palette_render_accept_and_cancel"
        return "settings_native_render_frames"
    if family == "navigation":
        return navigation_exporter(stem)
    return {
        "inbox": inbox_exporter_for(stem),
        "resources": "native_png_headless_renderer_smoke",
    }[family]


def case_record(root, png):
    relative = safe_relative(root, png)
    family = Path(relative).parts[0]
    require(family in FAMILIES, f"{relative}: PNG outside a known family")
    tree_path = png.with_suffix(".json")
    tree_relative = safe_relative(root, tree_path)
    require(tree_path.is_file(), f"{relative}: missing matching tree")
    physical = png_size(png)
    tree = json.loads(tree_path.read_text(encoding="utf-8"))
    logical = validate_tree(tree, physical, tree_path)
    case = {
        "family": family, "png": relative, "tree": tree_relative,
        "exporter": exporter_for(family, png.stem),
        "scenario": required_label(tree, "scenario", tree_path),
        "theme": required_label(tree, "theme", tree_path),
        "pngSha256": digest(png), "treeSha256": digest(tree_path),
        "physical": physical, "logical": logical, "scale": tree["scale"],
        "elementCount": len(tree["elements"]),
    }
    if png.stem.startswith(("pane-motion-", "accessory-resize-")):
        case.update(motion_record(root, png, tree))
    return case


def motion_record(root, png, tree):
    frames = motion_frames(png.stem)
    path = png.with_suffix(".motion.json")
    relative = safe_relative(root, path)
    require(path.is_file(), f"{png}: missing motion timing")
    motion = json.loads(path.read_text(encoding="utf-8"))
    require(isinstance(motion, dict), f"{path}: motion must be an object")
    stage = motion.get("stage")
    require(type(stage) is str and stage in frames, f"{path}: unknown motion stage")
    require(type(motion.get("schema")) is int and motion["schema"] == 1
            and motion.get("scenario") == tree["scenario"]
            and png.stem == f'{motion["scenario"]}-{stage}', f"{path}: wrong motion case")
    frame, elapsed = motion.get("frame"), motion.get("elapsed_ms")
    require(type(frame) is int and frame == frames[stage], f"{path}: wrong motion frame")
    require(type(elapsed) in (int, float) and math.isfinite(elapsed)
            and abs(elapsed - frame * 1000.0 / 60.0) < 1e-8, f"{path}: wrong motion clock")
    return {"motion": relative, "motionSha256": digest(path)}


def validate_required_geometry(case, expected):
    if expected is None:
        return
    width, height, scale = expected
    require(case["logical"] == [width, height] and case["scale"] == scale,
            f'{case["png"]}: geometry differs from maintained required case')


def collect_cases(root):
    inventory = required_inventory()
    paths = sorted(root.rglob("*.png"))
    missing = set(inventory) - {path.relative_to(root).as_posix() for path in paths}
    require(not missing, f"missing required cases: {', '.join(sorted(missing))}")
    cases = [case_record(root, path) for path in paths]
    for case in cases:
        validate_required_geometry(case, inventory.get(case["png"]))
    return cases


def git(repo, *args):
    return subprocess.check_output(["git", "-C", str(repo), *args])


def untracked_sources(repo):
    paths = git(repo, "ls-files", "--others", "--exclude-standard", "-z", "--",
                "clients/rune/src", "clients/rune/scripts").decode("utf-8").split("\0")
    records = []
    for name in filter(None, paths):
        path = repo / name
        if path.suffix in (".rs", ".py", ".sh"):
            records.append({"path": safe_relative(repo, path), "sha256": digest(path)})
    return records


def source_metadata(repo):
    cargo = (repo / "clients/rune/Cargo.toml").read_text(encoding="utf-8")
    pin = re.search(r'^rune\s*=\s*\{[^\n]*\brev\s*=\s*"([^"]+)"', cargo, re.MULTILINE)
    require(pin is not None, "cannot find current Rune pin")
    return {
        "sourceRevision": git(repo, "rev-parse", "HEAD").decode().strip(),
        "currentRunePin": pin.group(1),
        "dirty": bool(git(repo, "status", "--porcelain", "--untracked-files=all")),
        "gitDiffHeadSha256": hashlib.sha256(
            git(repo, "diff", "--binary", "--no-ext-diff", "--no-textconv", "HEAD", "--")
        ).hexdigest(),
        "untrackedSourceFiles": untracked_sources(repo),
        "sourceQualification": "Observed at packaging time; not an exact build-input attestation.",
    }


def artifact_link(path, label):
    return f'<a href="{html.escape(quote(path, safe="/"), quote=True)}">{html.escape(label)}</a>'


def case_html(case):
    caption = (f'{case["scenario"]} · {case["theme"]} · {case["png"]} '
               f'· {case["logical"][0]:g}×{case["logical"][1]:g} logical '
               f'· {case["physical"][0]}×{case["physical"][1]} pixels · {case["scale"]:g}×')
    image = html.escape(quote(case["png"], safe="/"), quote=True)
    return (
        '<figure>'
        f'<a href="{image}"><img loading="lazy" src="{image}" alt="{html.escape(caption, quote=True)}"></a>'
        f'<figcaption>{html.escape(caption)}<br>{artifact_link(case["tree"], "Ordered tree")}'
        f'<br><small>PNG SHA-256: {case["pngSha256"]}</small></figcaption></figure>'
    )


def gallery(cases):
    sections = []
    for family in FAMILIES:
        cards = "".join(case_html(case) for case in cases if case["family"] == family)
        sections.append(f'<section id="{family}"><h2>{family.title()}</h2><div class="grid">{cards}</div></section>')
    navigation = " · ".join(f'<a href="#{family}">{family.title()}</a>' for family in FAMILIES)
    return f"""<!doctype html>
<html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Rune visual review — NOT REVIEWED</title>
<style>
body{{font:16px/1.5 system-ui,sans-serif;margin:1.5rem;background:#eef0f3;color:#18202b}}
a{{color:#124d9e}} .grid{{display:grid;grid-template-columns:repeat(auto-fit,minmax(min(100%,360px),1fr));gap:1rem}}
figure{{margin:0;padding:1rem;background:white;border:1px solid #bac2cf;border-radius:8px;min-width:0}}
img{{display:block;max-width:100%;height:auto;margin:auto}}
figcaption{{margin-top:.75rem;overflow-wrap:anywhere}}small{{font-size:.75rem}}
</style>
<h1>Rune visual review — NOT REVIEWED</h1>
<p>Open before and after galleries side by side; click a frame for its full-size image.
Complete {artifact_link("judgments.md", "judgments.md")} with one concrete UX improvement.</p>
<p>{html.escape(LIMITS)}</p><nav>{navigation}</nav>
{"".join(sections)}</html>
"""


def produce(root, repo):
    require(root.is_absolute(), "output path must be absolute")
    require(root.is_dir(), "output directory does not exist")
    outputs = [root / name for name in ("manifest.json", "index.html", "judgments.md")]
    require(not any(path.exists() or path.is_symlink() for path in outputs),
            "refusing to overwrite an existing review bundle")
    cases = collect_cases(root)
    manifest = {
        "schema": 1, "reviewStatus": "NOT REVIEWED", "limits": LIMITS,
        **source_metadata(repo), "cases": cases,
    }
    contents = (json.dumps(manifest, indent=2) + "\n", gallery(cases), JUDGMENTS)
    for path, content in zip(outputs, contents):
        with path.open("x", encoding="utf-8") as stream:
            stream.write(content)
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parents[3])
    args = parser.parse_args()
    try:
        manifest = produce(args.output, args.repo)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"visual review: {error}\n")
    print(f'{len(manifest["cases"])} cases packaged; NOT REVIEWED')


if __name__ == "__main__":
    main()
