"""Unvalidated, allowlisted forensic bytes; never qualification input."""
from contextlib import contextmanager, ExitStack
import hashlib
import json
import os
from pathlib import Path
import stat

DIRECTORY = "UNVALIDATED-native-failure"
STAGING_DIRECTORY = "UNVALIDATED-before-seal"
# Forensic retention only: never a native qualification/acceptance limit.
MAX_FORENSIC_FILE_BYTES = 16 * 1024 * 1024
# Only files emitted by the qualification probes, never pairing/preferences.
NATIVE_FILES = ("rune-qualification.json", "rune-tree.json", "rune-input-gate.json",
                "rune-glass.json", "rune-background.json", "rune-lifecycle.json")
SEAL_FILES = ("rune-connection-diagnostics.status.json",
              "rune-connection-diagnostics.status.pending")


@contextmanager
def directory(path):
    """Anchor each component without following symlinks, including ancestors."""
    path = Path(path)
    if not path.is_absolute() or ".." in path.parts:
        raise ValueError("forensic directory must be absolute without parent traversal")
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    descriptor = os.open("/", flags)
    try:
        for part in path.parts[1:]:
            child = os.open(part, flags, dir_fd=descriptor)
            os.close(descriptor)
            descriptor = child
        yield descriptor
    finally:
        os.close(descriptor)


def original_bytes(source, name):
    # NONBLOCK prevents an unsafe FIFO from blocking before fstat can reject it.
    descriptor = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=source)
    with os.fdopen(descriptor, "rb") as handle:
        before = os.fstat(handle.fileno())
        if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or before.st_uid != os.geteuid():
            raise ValueError("unsafe forensic source")
        if before.st_size > MAX_FORENSIC_FILE_BYTES:
            raise OverflowError("forensic source exceeds 16 MiB limit")
        data = handle.read(MAX_FORENSIC_FILE_BYTES + 1)
        if len(data) > MAX_FORENSIC_FILE_BYTES:
            raise OverflowError("forensic source exceeds 16 MiB limit")
        after = os.fstat(handle.fileno())
        if (before.st_size, before.st_mtime_ns, before.st_ctime_ns, before.st_nlink) != (
                after.st_size, after.st_mtime_ns, after.st_ctime_ns, after.st_nlink):
            raise ValueError("forensic source changed during capture")
        return data


def write_private(destination, name, data):
    descriptor = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                         0o600, dir_fd=destination)
    with os.fdopen(descriptor, "wb") as handle:
        handle.write(data)


def capture_file(source, destination, name, retained_name):
    receipt = {"source": name, "file": retained_name, "status": "rejected"}
    try:
        data = original_bytes(source, name)
        write_private(destination, retained_name, data)
        receipt.update(status="UNVALIDATED", bytes=len(data),
                       sha256=hashlib.sha256(data).hexdigest())
    except FileNotFoundError:
        receipt["status"] = "missing"
    except OverflowError:
        receipt["error"] = "forensic source exceeds 16 MiB limit"
    except (OSError, ValueError):
        receipt["error"] = "unsafe source, changed bytes, or exclusive copy failed"
    return receipt


def stage_failure_background(lane):
    """Keep the pre-seal inode without reading it while its producer may run."""
    with directory(lane.documents) as source:
        os.mkdir(STAGING_DIRECTORY, 0o700, dir_fd=source)
        staged = os.open(STAGING_DIRECTORY, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=source)
        try:
            os.rename("rune-background.json", "rune-background.json",
                      src_dir_fd=source, dst_dir_fd=staged)
            lane.failure_background_staged = True
        except FileNotFoundError:
            pass  # No earlier acknowledgement; the normal seal handshake still runs.
        finally:
            os.close(staged)


def capture_staged_background(lane, destination, prefix):
    receipt = {"source": STAGING_DIRECTORY + "/rune-background.json",
               "file": prefix + "rune-background.before-seal.json", "status": "rejected"}
    if not lane.failure_background_staged:
        receipt["status"] = "missing"
        return receipt
    try:
        with directory(lane.documents / STAGING_DIRECTORY) as source:
            copied = capture_file(source, destination, "rune-background.json", receipt["file"])
            copied["source"] = receipt["source"]
            return copied
    except FileNotFoundError:
        receipt["status"] = "missing"
    except (OSError, ValueError):
        receipt["error"] = "staged background container unavailable or unsafe"
    return receipt


def capture_sources(lane, destination, prefix, quiescent):
    names = [(name, prefix + name) for name in NATIVE_FILES]
    names.extend((prefix + name, prefix + name) for name in SEAL_FILES)
    rejected = [{"source": name, "file": retained, "status": "rejected"}
                for name, retained in names]
    rejected.append({"source": STAGING_DIRECTORY + "/rune-background.json",
                     "file": prefix + "rune-background.before-seal.json", "status": "rejected"})
    if not quiescent:
        return rejected, "owned simulator shutdown did not establish producer quiescence; no bytes copied"
    if lane.documents is None:
        return rejected, "app data container unavailable; no bytes copied"
    with ExitStack() as stack:
        try:
            source = stack.enter_context(directory(lane.documents))
        except (OSError, ValueError):
            return rejected, "app data container unavailable or unsafe; no bytes copied"
        files = [capture_file(source, destination, name, retained) for name, retained in names]
        files.append(capture_staged_background(lane, destination, prefix))
        return files, None


def preserve_failure_evidence(lane, quiescent):
    """Capture once after shutdown, before delete; malformed bytes stay original."""
    if lane.failure_evidence_receipt is not None:
        return lane.failure_evidence_receipt
    if not lane.diagnostic_prefixes:
        return None  # No owned qualification producer was launched.
    prefix = lane.diagnostic_prefixes[-1]
    if prefix not in ("", "resume-"):
        raise ValueError("unknown forensic capture phase")
    with directory(lane.out) as output, ExitStack() as stack:
        os.mkdir(DIRECTORY, 0o700, dir_fd=output)  # Never reuse/clobber an earlier capture.
        destination = os.open(DIRECTORY, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=output)
        stack.callback(os.close, destination)
        files, limitation = capture_sources(lane, destination, prefix, quiescent)
        receipt = {"schema": 1, "status": "UNVALIDATED", "quiescent": quiescent,
                   "phase": "resumed" if prefix else "initial",
                   "limitation": limitation, "files": files}
        write_private(destination, "receipt.json", (json.dumps(receipt, indent=2) + "\n").encode())
        lane.failure_evidence_receipt = receipt
        return receipt
