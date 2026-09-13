#!/usr/bin/env python3
"""Extract one Keep-a-Changelog version section for Linear release notes."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

HEADING = re.compile(r"^## \[([^\]]+)\]")


def version_from_tag(tag: str) -> str:
    if tag.startswith("v") and len(tag) > 1 and tag[1].isdigit():
        return tag[1:]
    marker = tag.rfind("-v")
    if marker >= 0 and marker + 2 < len(tag) and tag[marker + 2].isdigit():
        return tag[marker + 2 :]
    raise ValueError(f"unsupported release tag {tag!r}; expected vX.Y.Z or <component>-vX.Y.Z")


def extract_section(text: str, version: str) -> str:
    lines = text.splitlines()
    start = None
    for index, line in enumerate(lines):
        match = HEADING.match(line)
        if match and match.group(1) == version:
            start = index
            break
    if start is None:
        raise ValueError(f"no changelog heading for {version}")
    end = len(lines)
    for index in range(start + 1, len(lines)):
        if HEADING.match(lines[index]):
            end = index
            break
    section = "\n".join(lines[start:end]).strip()
    return section + "\n"


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True, help="Release tag, e.g. v0.36.0 or cockpit-v0.23.3")
    parser.add_argument("--changelog", required=True, help="Path to the Keep-a-Changelog file")
    parser.add_argument("--output", help="Write the section here instead of stdout")
    args = parser.parse_args(argv)

    version = version_from_tag(args.tag)
    section = extract_section(Path(args.changelog).read_text(encoding="utf-8"), version)
    if args.output:
        Path(args.output).write_text(section, encoding="utf-8")
    else:
        sys.stdout.write(section)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1) from error
