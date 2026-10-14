"""Human diagnostics only; machine documents and child output bypass this module."""
import os
import sys

_STYLES = {"error": 31, "warn": 33, "ok": 32, "info": 36}


def color_enabled(stream):
    if os.environ.get("NO_COLOR") or os.environ.get("TERM") == "dumb":
        return False
    if os.environ.get("CLICOLOR_FORCE", "") not in ("", "0"):
        return True
    return os.environ.get("CLICOLOR") != "0" and stream.isatty()


def status(level, message):
    label = f"phux: {level}:"
    if color_enabled(sys.stderr):
        label = f"\033[1mphux\033[0m: \033[{_STYLES[level]}m{level}\033[0m:"
    print(f"{label} {message}", file=sys.stderr)


def hint(message):
    for line in str(message).splitlines():
        print(f"  hint: {line.lstrip()}", file=sys.stderr)
