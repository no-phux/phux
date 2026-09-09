#!/usr/bin/env python3
"""Retain content-free diagnostics from an existing pinned Native SDK dev run."""

import argparse
import os
from pathlib import Path
import subprocess
import sys
import time

from lib import dev_diagnostics as evidence


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    begin = commands.add_parser("begin", help="bind the sole live publisher; print run directory")
    begin.add_argument("--pid", type=int, required=True)
    begin.add_argument("--native", type=Path, required=True, help="matching pinned automation CLI")
    begin.add_argument("--binary", type=Path, default=evidence.ROOT / "zig-out/bin/phux-cockpit")
    begin.add_argument("--ffi-lib", type=Path, help="candidate archive input, not a linked attestation")
    begin.add_argument("--log", type=Path, help="this native dev invocation's stdout/stderr log")
    begin.add_argument("--socket", help="explicit coordinator endpoint; never inferred from this shell")
    begin.add_argument("--phux-cli", type=Path, help="optional CLI for read-only status at --socket")
    begin.add_argument("--require-markup-watch", action="store_true")
    mark = commands.add_parser("mark-problem", help="retain an identity-checked incident capture")
    mark.add_argument("--run", type=Path, required=True)
    mark.add_argument("--target", help="request widget verification (refused at the current unescaped SDK pin)")
    mark.add_argument("--input-scope", choices=evidence.SCOPES, default="unknown")
    watch = commands.add_parser("watch", help="retain periodic diagnostics until refusal or Ctrl-C")
    watch.add_argument("--run", type=Path, required=True)
    watch.add_argument("--interval", type=float, default=2)
    return parser.parse_args()


def begin(args):
    options = vars(args).copy()
    del options["command"]
    for key, value in options.items():
        if isinstance(value, Path):
            options[key] = str(value.resolve())
    run = evidence.new_run(evidence.ROOT, options)
    print(run, flush=True)
    _, valid = evidence.capture(run, "begin")
    return 0 if valid else 1


def mark(args):
    if args.target:
        evidence.require(evidence.ADDRESS.fullmatch(args.target) is not None,
                         "target must be a Cockpit canvas/widget address from the SDK snapshot")
    path, valid = evidence.capture(args.run.resolve(), "mark-problem", args.target, args.input_scope)
    print(path)
    return 0 if valid else 1


def watch(args):
    evidence.require(0.5 <= args.interval <= 3600, "interval must be between 0.5 and 3600 seconds")
    while True:
        path, valid = evidence.capture(args.run.resolve(), "sample")
        print(path, flush=True)
        if not valid:
            return 1
        time.sleep(args.interval)


def main():
    os.umask(0o077)
    args = parse_args()
    try:
        return {"begin": begin, "mark-problem": mark, "watch": watch}[args.command](args)
    except evidence.EvidenceError as error:
        print(f"diagnostics: {error}", file=sys.stderr)
        return 1
    except (OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired):
        print("diagnostics: cannot read or write evidence inputs", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
