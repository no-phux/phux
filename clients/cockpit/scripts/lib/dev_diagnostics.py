"""Identity-bound, content-free evidence for the SDK's existing native dev loop.

This module observes; it never builds, launches, activates, or sends app input.
Source inputs and on-disk artifacts are evidence, not a linked-build attestation.
"""

from __future__ import annotations

import contextlib
import ctypes
import datetime
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time
import uuid

ROOT = Path(__file__).resolve().parents[2]
MAX_SNAPSHOT = 8 * 1024 * 1024
MAX_LOG = 1024 * 1024
HEADER = re.compile(
    r"ready=true protocol=(0x[0-9a-f]{16}) frame=(\d+) commands=(\d+) "
    r"runtime_uptime_ns=(\d+) dispatch_errors=(\d+) dropped_trace_records=(\d+) "
    r"publisher_pid=(\d+) markup_watch=(armed|off)"
)
HEADER_KEYS = ("protocol", "frame", "commands", "runtime_uptime_ns", "dispatch_errors",
               "dropped_trace_records", "publisher_pid", "markup_watch")
VIEW = r"@w\d+/phux-cockpit-canvas(?:-\d+)?"
ADDRESS = re.compile(rf"{VIEW}(?:#\d+)?")
SCOPES = ("unknown", "terminal", "chrome", "switcher", "settings", "web")


class EvidenceError(RuntimeError):
    """A named refusal safe to retain (never includes raw command output)."""


def require(condition, reason):
    if not condition:
        raise EvidenceError(reason)


def command(argv, cwd=None):
    result = subprocess.run(argv, cwd=cwd, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, timeout=15, check=False,
                            env={**os.environ, "LC_ALL": "C"})
    require(result.returncode == 0, f"{Path(argv[0]).name} inspection failed")
    return result.stdout


def digest(data):
    return hashlib.sha256(data).hexdigest()


def artifact(path):
    path = Path(path).resolve()
    before = path.stat()
    sha = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            sha.update(block)
    after = path.stat()
    require(file_stamp(before) == file_stamp(after), "artifact changed while hashing")
    return {"path": str(path), "sha256": sha.hexdigest(), "size": after.st_size,
            "mtime_ns": after.st_mtime_ns, "device": after.st_dev, "inode": after.st_ino}


def file_stamp(info):
    return info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns


def optional_artifact(path):
    if path is None:
        return {"available": False}
    try:
        return {"available": True, **artifact(path)}
    except OSError:
        return {"available": False, "path": str(path)}


def source_identity(root):
    repo = command(["git", "-C", str(root), "rev-parse", "--show-toplevel"]).decode().strip()
    git = ["git", "-C", repo]
    status = command([*git, "status", "--porcelain=v1", "-z", "--untracked-files=all"])
    return {"root": str(root), "repository_root": repo,
            "revision": command([*git, "rev-parse", "HEAD"]).decode().strip(),
            "dirty": bool(status), "status_sha256": digest(status),
            "tracked_diff_sha256": digest(command([*git, "diff", "HEAD", "--binary"])),
            "untracked_content_included": False}


def sdk_identity(root):
    # Reuse the scoped ZON reader; a local .path must never fall through to
    # Ghostty's URL. No fetching or compiling during capture.
    result = subprocess.run(
        ["bash", "-c", 'source "$1"; zon_dependency_url "$2" native_sdk', "diagnostics",
         str(ROOT / "scripts/lib/zon.sh"), str(root / "build.zig.zon")],
        capture_output=True, timeout=15, check=False,
    )
    url = result.stdout.decode().strip()
    match = re.fullmatch(r"https://github.com/[\w.-]+/[\w.-]+/archive/([0-9a-f]{40})\.tar\.gz", url)
    return {"declared_commit": match.group(1) if match else None,
            "pin_status": "pinned" if match else "unavailable_or_local_override",
            "manifest": optional_artifact(root / "build.zig.zon"),
            "materialized_candidates": [p.name for p in sorted((root / "zig-pkg").glob("native_sdk-*"))],
            "linked_sdk_verified": False}


def process_identity(pid):
    require(pid > 0, "invalid publisher PID")
    # proc_pidpath reports the executable, without collecting argv/environment
    # (both routinely contain terminal commands or credentials).
    libproc = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    buf = ctypes.create_string_buffer(4096)
    require(libproc.proc_pidpath(pid, buf, len(buf)) > 0, "publisher process unavailable")
    started = command(["ps", "-p", str(pid), "-o", "lstart="]).decode().strip()
    require(bool(started), "publisher start time unavailable")
    cwd_record = command(["/usr/sbin/lsof", "-a", "-p", str(pid), "-d", "cwd", "-Fn"])
    cwd = re.search(rb"(?m)^n([^\n]+)$", cwd_record)
    require(cwd is not None, "process working directory unavailable")
    return {"pid": pid, "executable": str(Path(os.fsdecode(buf.value)).resolve()),
            "cwd": str(Path(os.fsdecode(cwd.group(1))).resolve()),
            "started": started, "start_time_precision": "seconds",
            "started_unix": datetime.datetime.strptime(started, "%a %b %d %H:%M:%S %Y").timestamp()}


def live_publishers():
    pids = set()
    for name in ("phux-cockpit", "phux-cockpit-dev"):
        result = subprocess.run(["pgrep", "-x", name], capture_output=True, timeout=5, check=False)
        require(result.returncode in (0, 1), "cannot enumerate Cockpit publishers")
        pids.update(int(pid) for pid in result.stdout.split())
    return sorted(pids)


def check_publisher(expected, actual, pids):
    require(pids == [expected["pid"]], "single-publisher check failed")
    require(actual == expected, "publisher executable or start time changed")


def sanitize_snapshot(raw, pid):
    # Pinned SDK snapshot.zig:339 emits a typed header before any user data.
    # At :367/:383/:581 it interpolates window.title, view.text and widget.name
    # as raw {s}. A label can close quotes and forge perfectly balanced records.
    # Neither line splitting nor quote tracking can recover their provenance.
    # Retain only the first physical header; no byte of the body is interpreted.
    require(len(raw) <= MAX_SNAPSHOT, "snapshot exceeds capture limit")
    first_line, newline, _ = raw.partition(b"\n")
    require(bool(newline), "snapshot header is unterminated")
    header = HEADER.fullmatch(first_line.decode("ascii", errors="replace"))
    require(header is not None, "snapshot header is missing or unsupported")
    fields = dict(zip(HEADER_KEYS, header.groups()))
    require(int(fields["publisher_pid"]) == pid, "snapshot publisher does not match run")
    return {"header": fields, "records": [],
            "structure_status": "unsupported_unescaped_sdk_text",
            "ui_health_observed": "unavailable",
            "input_scope_observed": "unavailable",
            "resource_identity_observed": "unavailable"}


def log_summary(path):
    """Retain only diagnostic counts from a bounded tail, never log text."""
    if path is None:
        return {"available": False}
    try:
        with Path(path).open("rb") as source:
            size = source.seek(0, os.SEEK_END)
            source.seek(max(0, size - MAX_LOG))
            data = source.read(MAX_LOG)
    except OSError:
        return {"available": False}
    categories = ("zero_canvas_layout", "zero_canvas_ui", "markup", "error", "warning")
    return {"available": True, "file_size": size, "tail_bytes": len(data),
            "truncated": size > MAX_LOG,
            "category_counts": {name: data.lower().count(name.encode()) for name in categories},
            "launch_events_status": "unsupported_unframed_log_text",
            "attribution": "operator-supplied log; counts are not publisher-verified"}


def coordinator_identity(options):
    result = {"endpoint_declared": options.get("socket"),
              "incarnation_observed": None, "pid_observed": None}
    if not options.get("phux_cli") or not options.get("socket"):
        return result
    try:
        raw = command([options["phux_cli"], "--socket", options["socket"], "status", "--json"])
        report = json.loads(raw)
        require(isinstance(report, dict), "server status is not an object")
        pid = report.get("pid")
        require(type(pid) is int and pid > 0, "server PID unavailable")
        result["pid_observed"] = pid
        result["process"] = process_identity(pid)
    except (EvidenceError, OSError, ValueError, KeyError, TypeError, subprocess.TimeoutExpired):
        result["probe"] = "unavailable"
    return result


@contextlib.contextmanager
def exclusive(path):
    with Path(path).open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise EvidenceError("another diagnostics capture owns this dropbox") from error
        yield


def write_json(path, value):
    # Publish complete evidence only, without replacing an existing capture.
    path = Path(path)
    temporary = path.with_name(f".{path.name}-{uuid.uuid4().hex}.partial")
    try:
        with temporary.open("x", encoding="utf-8") as output:
            json.dump(value, output, sort_keys=True, indent=2)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        os.link(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def new_run(root, options):
    home = root / ".dev-run/diagnostics"
    home.mkdir(parents=True, exist_ok=True, mode=0o700)
    name = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    run = home / f"{name}-{uuid.uuid4().hex[:12]}"
    process = process_identity(options["pid"])
    binary = artifact(options["binary"])
    require(process["executable"] == binary["path"], "PID does not execute the selected artifact")
    require(process["cwd"] == str(root), "publisher CWD is not the native dev source root")
    check_publisher(process, process, live_publishers())
    manifest = {"schema": 2, "run_id": run.name, "created_ns": time.time_ns(),
                "source": source_identity(root), "process": process,
                "binary_on_disk_at_bind": binary, "native_cli": artifact(options["native"]),
                "sdk_inputs": sdk_identity(root),
                "ffi_archive_input": optional_artifact(options.get("ffi_lib")),
                "ffi_header_input": optional_artifact(root.parent.parent / "crates/phux-client-ffi/include/phux/client.h"),
                "loaded_build_configuration": "unknown; source flags and notices are not artifact attestation",
                "linked_ffi_verified": False, "options": options}
    run.mkdir(mode=0o700)
    write_json(run / "run.json", manifest)
    return run


def check_artifact_stamp(expected):
    # Hash once when binding. Rehashing a large Debug binary every two seconds
    # would contaminate the very dev-run timing diagnostics we are collecting.
    stamp = file_stamp(Path(expected["path"]).stat())
    want = tuple(expected[key] for key in ("device", "inode", "size", "mtime_ns"))
    require(stamp == want, "on-disk artifact changed since binding; begin a new run after relaunch")


def publication_identity(root, process):
    info = (Path(root) / ".zig-cache/native-sdk-automation/snapshot.txt").stat()
    # lstart is second-granular. Requiring the next whole second is deliberately
    # conservative: a cached snapshot in the PID's launch second is ambiguous.
    require(info.st_mtime_ns >= (process["started_unix"] + 1) * 1_000_000_000,
            "snapshot publication predates publisher start or is launch-second ambiguous; retry after publication")
    return {"mtime_ns": info.st_mtime_ns, "observed_age_ns": time.time_ns() - info.st_mtime_ns}


def bound_snapshot(manifest):
    options = manifest["options"]
    expected = manifest["process"]
    check_publisher(expected, process_identity(expected["pid"]), live_publishers())
    check_artifact_stamp(manifest["binary_on_disk_at_bind"])
    check_artifact_stamp(manifest["native_cli"])
    root = manifest["source"]["root"]
    published_before = publication_identity(root, expected)
    raw = command([options["native"], "automate", "snapshot"], cwd=root)
    snapshot = sanitize_snapshot(raw, expected["pid"])
    check_publisher(expected, process_identity(expected["pid"]), live_publishers())
    check_artifact_stamp(manifest["binary_on_disk_at_bind"])
    check_artifact_stamp(manifest["native_cli"])
    if options["require_markup_watch"]:
        require(snapshot["header"]["markup_watch"] == "armed", "runtime markup watcher is not armed")
    snapshot["publication_before_read"] = published_before
    snapshot["publication_after_read"] = publication_identity(root, expected)
    return snapshot


def capture(run, kind, target=None, scope="unknown"):
    manifest = json.loads((run / "run.json").read_text())
    root = Path(manifest["source"]["root"])
    options = manifest["options"]
    with exclusive(root / ".dev-run/diagnostics/capture.lock"):
        evidence = {"schema": 2, "run_id": manifest["run_id"], "kind": kind,
                    "captured_ns": time.time_ns(), "status": "invalid",
                    "source_now": None,
                    "target_declared": target, "input_scope_declared": scope,
                    "coordinator": None, "log_diagnostics": None}
        phase = "source"
        try:
            evidence["source_now"] = source_identity(root)
            phase = "runtime"
            evidence["coordinator"] = coordinator_identity(options)
            evidence["log_diagnostics"] = log_summary(options.get("log"))
            snapshot = bound_snapshot(manifest)
            require(target is None, "target verification unsupported: SDK snapshot body is unescaped text")
            evidence["snapshot"] = snapshot
            evidence["status"] = "valid"
        except EvidenceError as error:
            evidence["refusal"] = str(error) if phase == "runtime" else "source inspection failed"
        except (OSError, ValueError, subprocess.TimeoutExpired):
            evidence["refusal"] = f"{phase} inspection unavailable"
        name = f"{evidence['captured_ns']}-{uuid.uuid4().hex[:8]}-{kind}.json"
        write_json(run / name, evidence)
    return run / name, evidence["status"] == "valid"
