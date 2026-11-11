#!/usr/bin/env python3
"""Qualify native Rune rendering/input on an owned, disposable iOS simulator.

Calls the host's real insertText entry point, not physical OS keyboard/IME input.
Never selects a user's simulator or the installed/production phux server.
"""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
import time
import uuid
import urllib.parse

_spec = importlib.util.spec_from_file_location("rune_qualification_reports", Path(__file__).with_name("qualification_reports.py"))
_reports = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_reports)
SESSION = _reports.SESSION
INPUT_CONTRACTS = _reports.INPUT_CONTRACTS
validate_report = _reports.validate_report
validate_input_contracts = _reports.validate_input_contracts
_settle_spec = importlib.util.spec_from_file_location("rune_simulator_settle", Path(__file__).with_name("simulator_settle.py"))
_settle = importlib.util.module_from_spec(_settle_spec)
_settle_spec.loader.exec_module(_settle)

BUNDLE_ID = "sh.phux.rune.ios"
MARKER = "RUNE_IOS_INPUT_OK"
XCRUN = "/usr/bin/xcrun"
# The data-container lookup: tries, and seconds each.
CONTAINER_ATTEMPTS = 3
CONTAINER_TIMEOUT = 60
# Other read-only simulator queries: tries of the default 30s each.
QUERY_ATTEMPTS = 3


def version_tuple(value):
    return tuple((list(map(int, value.split("."))) + [0, 0])[:3])


def available_ios_runtime(runtime):
    return (runtime.get("isAvailable") and ".iOS-" in runtime["identifier"]
            and version_tuple(runtime["version"]) >= (26, 0, 0))


def simulator_pair(runtimes, device_types):
    """Use runtime-supported IDs, not a device name guessed from OS version."""
    phones = {d["identifier"] for d in device_types if d.get("productFamily") == "iPhone"}
    available = filter(available_ios_runtime, runtimes)
    for runtime in sorted(available, key=lambda r: version_tuple(r["version"])):
        compatible = [d for d in runtime.get("supportedDeviceTypes", [])
                      if d["identifier"] in phones]
        if compatible:
            device = min(compatible, key=lambda d: (d["name"] != "iPhone 17", d["name"]))
            return runtime["identifier"], device["identifier"]
    raise ValueError("no available iOS >=26 runtime with an advertised compatible iPhone")


def marker_command(marker=MARKER):
    # Literal marker must not appear in the terminal's command echo.
    escaped = "".join(f"\\{byte:03o}" for byte in (marker + "\n").encode())
    return f"printf '{escaped}'"


def server_environment(scratch):
    home = scratch / "home"
    home.mkdir()
    config = scratch / "config"
    config.mkdir()
    prefs = scratch / "prefs"
    prefs.mkdir()
    return {"HOME": str(home), "XDG_CONFIG_HOME": str(config),
            "XDG_DATA_HOME": str(scratch / "data"), "XDG_STATE_HOME": str(scratch / "state"),
            "PATH": "/usr/bin:/bin", "SHELL": "/bin/sh", "TERM": "xterm-256color",
            "TMPDIR": str(scratch), "PHUX_PROFILE": "rune-ios-qualification",
            "PHUX_WS_SECURE": "1",
            "RUNE_PREFS_DIR": str(prefs)}


class Qualification:
    def __init__(self, app, phux, out, scratch, transport="quic"):
        self.transport = transport
        self.app, self.phux, self.out, self.scratch = app, phux, out, scratch
        self.udid = None
        self.server = None
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith(("PHUX_", "SIMCTL_CHILD_"))}
        # xcrun must use the real selected Xcode, not a Nix SDK.
        self.env.pop("DEVELOPER_DIR", None)
        self.sequence = 0
        self.configuration = None
        self.documents = None

    def run(self, command, timeout=30, env=None):
        self.sequence += 1
        command = [str(arg) for arg in command]
        prefix = self.out / f"command-{self.sequence:03}"
        prefix.with_suffix(".json").write_text(json.dumps(command) + "\n")
        started = time.monotonic()
        succeeded = False
        try:
            with prefix.with_suffix(".log").open("w") as log, \
                    prefix.with_suffix(".stderr.log").open("w") as diagnostics:
                subprocess.run(command, env=self.env if env is None else env, check=True,
                               stdout=log, stderr=diagnostics, timeout=timeout)
            succeeded = True
        finally:
            prefix.with_suffix(".timing.json").write_text(json.dumps({
                "elapsed_seconds": time.monotonic() - started,
                "deadline_seconds": timeout, "succeeded": succeeded,
            }) + "\n")
        return prefix.with_suffix(".log").read_text()

    def simctl(self, *args, timeout=30, attempts=1):
        """A loaded runner's simulator can stall one call well past its
        deadline; a read-only query (attempts > 1) abandons a stuck call
        for a fresh one. Never retry a call that changes state."""
        for attempt in range(1, attempts + 1):
            try:
                return self.run([XCRUN, "simctl", *args], timeout=timeout)
            except subprocess.TimeoutExpired:
                if attempt == attempts:
                    raise

    def app_container(self):
        """The app's data container. On a just-booted iOS 26 simulator this
        lookup has outlasted 30s right after a successful install."""
        found = self.simctl("get_app_container", self.udid, BUNDLE_ID, "data",
                            timeout=CONTAINER_TIMEOUT, attempts=CONTAINER_ATTEMPTS)
        return Path(found.strip())

    def start_server(self):
        socket = self.scratch / "s"
        env = server_environment(self.scratch)
        policy = Path(env["XDG_CONFIG_HOME"]) / "phux/config.toml"
        policy.parent.mkdir(parents=True, exist_ok=True)
        policy.write_text('[policy]\nmode = "paired"\n')
        # This is an offline authority operation. The CLI rejects --socket
        # because it cannot dial; HOME/config/state are all our private lane.
        self.run([self.phux, "workload", "authority", "--init"], env=env)
        with (self.out / "server.log").open("w") as log:
            self.server = subprocess.Popen(
                [str(self.phux), "--socket", str(socket), "server", "--session", SESSION,
                 "--quic", "127.0.0.1:0", "--listen", "127.0.0.1:0"],
                env=env, cwd=self.scratch, stdout=log, stderr=subprocess.STDOUT,
            )
        (self.out / "server-pid.txt").write_text(str(self.server.pid) + "\n")
        deadline = time.monotonic() + 30
        while not socket.is_socket():
            if self.server.poll() is not None:
                raise ValueError("isolated dev server exited; see server.log")
            if time.monotonic() >= deadline:
                raise TimeoutError("isolated dev server never listened")
            time.sleep(0.1)
        # Never send the pairing document to an evidence log/artifact. The
        # freshly created simulator receives it via its private Documents.
        paired = subprocess.run([str(self.phux), "--socket", str(socket), "pair", "--json", "--enroll"],
                                env=env, capture_output=True, check=True, timeout=30)
        pair = json.loads(paired.stdout)
        # The CLI intentionally does not advertise loopback to phones. Mint
        # the direct QUIC-only shape produced by native SSH setup here. The
        # native contracts also run its actual Swift converter through Rust.
        # Use the real importer/confirmation/enrollment path, not hand-filled settings.
        fields = {"quic": "quic://" + pair["quic_addr"],
                  "name": SESSION, "fp": pair["cert_fingerprint"], "ca": pair["ca_fingerprint"],
                  "token": pair["token"], "enroll": pair["enrollment_ticket"]}
        self.configuration = {"pairing_link": "https://phux.sh/connect?" + urllib.parse.urlencode(
            fields, quote_via=urllib.parse.quote)}
        if self.transport == "wss":
            self.configuration["qualification_endpoint"] = "wss://" + pair["ws_addr"]
        return socket

    def create_simulator(self):
        runtimes = json.loads(self.simctl("list", "runtimes", "-j"))["runtimes"]
        types = json.loads(self.simctl("list", "devicetypes", "-j"))["devicetypes"]
        runtime, device = simulator_pair(runtimes, types)
        created = self.simctl("create", f"Rune qualification {uuid.uuid4().hex[:12]}",
                              device, runtime).strip()
        self.udid = str(uuid.UUID(created))  # Only this newly-created ID may be cleaned up.
        (self.out / "simulator.json").write_text(json.dumps({
            "udid": self.udid, "runtime": runtime, "device_type": device,
        }, indent=2) + "\n")
        self.simctl("boot", self.udid)
        self.simctl("bootstatus", self.udid, "-b", timeout=240)
        self.settle()

    def settle(self):
        return _settle.settle(self.simctl, self.udid, self.out)

    def app_documents(self):
        # Retain the existing bounded container-readiness policy from main,
        # but resolve only before launch, never again during report collection.
        container = self.app_container()
        if not container.is_absolute() or not container.is_dir():
            raise ValueError("simctl returned no existing absolute app data container")
        documents = container / "Documents"
        documents.mkdir(exist_ok=True)
        return documents

    def launch(self, socket):
        self.simctl("install", self.udid, self.app, timeout=120)
        self.documents = self.app_documents()
        secret = self.documents / "qualification-connection.json"
        with open(secret, "x", opener=lambda path, flags: os.open(path, flags, 0o600)) as handle:
            json.dump(self.configuration, handle)
        self.configuration = None
        result = self.simctl(
            # This is a new simulator and freshly installed app: there cannot
            # be an old process to terminate. Asking SpringBoard to terminate
            # it first can stall cold launch registration before any app runs.
            "launch", self.udid, BUNDLE_ID,
            "--qualification-configuration", secret.name, "--qualification-session", SESSION,
            "--qualification-input", marker_command(), "--qualification-marker", MARKER,
            # Cold launch on a loaded runner has outlasted 60s, as install does.
            timeout=120,
        )
        match = re.search(rf"^{re.escape(BUNDLE_ID)}: ([0-9]+)$", result, re.MULTILINE)
        if not match:
            raise ValueError(f"simctl returned no app PID: {result}")
        return int(match[1])

    def require_alive(self, pid):
        listing = self.simctl("spawn", self.udid, "launchctl", "list", attempts=QUERY_ATTEMPTS)
        pattern = rf"^\s*{pid}\s+-?\d+\s+.*{re.escape(BUNDLE_ID)}"
        if not re.search(pattern, listing, re.MULTILINE):
            raise ValueError(f"native app PID {pid} exited")
        if self.server.poll() is not None:
            raise ValueError("isolated server exited during qualification")

    def collect_report(self, pid, resumed=False):
        prefix = "resume-" if resumed else ""
        if self.documents is None:
            raise ValueError("app data container was not resolved before launch")
        documents = self.documents
        source = documents / "rune-qualification.json"
        deadline = time.monotonic() + 90
        while not source.is_file():
            self.require_alive(pid)
            if time.monotonic() >= deadline:
                raise TimeoutError("native qualification report not written within 90s")
            time.sleep(0.5)
        shutil.copy2(source, self.out / (prefix + source.name))
        report = json.loads((self.out / (prefix + source.name)).read_text())
        validate_report(report, self.transport, resumed)
        tree = documents / "rune-tree.json"
        shutil.copy2(tree, self.out / (prefix + tree.name))
        if SESSION not in json.dumps(json.loads((self.out / (prefix + tree.name)).read_text())):
            raise ValueError("native view tree does not show the qualification session")
        self.require_alive(pid)
        self.simctl("io", self.udid, "screenshot", self.out / (prefix + "ios-system-keyboard.png"))
        self.cycle_scene(documents, pid, prefix)
        time.sleep(3)
        self.require_alive(pid)
        self.simctl("io", self.udid, "screenshot", self.out / (prefix + "ios-native.png"))
        if not (self.out / (prefix + "ios-native.png")).stat().st_size:
            raise ValueError("empty simulator screenshot")
        return report

    def await_scene_report(self, source, pid, message):
        deadline = time.monotonic() + 30
        while not source.is_file():
            self.require_alive(pid)
            if time.monotonic() >= deadline:
                raise TimeoutError(message)
            time.sleep(0.5)
        return json.loads(source.read_text())

    def cycle_scene(self, documents, pid, prefix=""):
        # Drive the actual UIKit scene to background/foreground on our own
        # simulator. An in-app state toggle is not OS lifecycle evidence.
        background = documents / "rune-background.json"
        background.unlink(missing_ok=True)
        self.simctl("launch", self.udid, "com.apple.Preferences")
        self.await_scene_report(background, pid, "native scene did not acknowledge OS backgrounding")
        shutil.copy2(background, self.out / (prefix + background.name))
        self.require_alive(pid)
        lifecycle = documents / "rune-lifecycle.json"
        lifecycle.unlink(missing_ok=True)
        self.simctl("launch", self.udid, BUNDLE_ID)
        resumed = self.await_scene_report(lifecycle, pid, "no presented frame after native background/foreground")
        if not resumed.get("marker_seen") or resumed.get("resumed_frames", 0) <= resumed.get("background_frames", 0):
            raise ValueError("native lifecycle did not resume the same live terminal")
        shutil.copy2(lifecycle, self.out / (prefix + lifecycle.name))

    def resume_identity(self, original):
        # New process, no ticket/configuration launch argument: only saved
        # public preferences plus Keychain token and held key may reconnect.
        self.simctl("terminate", self.udid, BUNDLE_ID, timeout=60)
        for name in ("rune-qualification.json", "rune-tree.json"):
            (self.documents / name).unlink(missing_ok=True)
        marker = MARKER + "_RESUMED"
        launched = self.simctl("launch", self.udid, BUNDLE_ID, "--qualification-resume",
            "--qualification-session", SESSION, "--qualification-input", marker_command(marker),
            "--qualification-marker", marker, timeout=120)
        match = re.search(rf"^{re.escape(BUNDLE_ID)}: ([0-9]+)$", launched, re.MULTILINE)
        if not match:
            raise ValueError("simulator did not report the resumed app PID")
        report = self.collect_report(int(match[1]), resumed=True)
        if report["key_id"] != original["key_id"]:
            raise ValueError("relaunch did not use the same saved device identity")
        return report

    def stop_server(self):
        if self.server is not None and self.server.poll() is None:
            self.server.terminate()  # Popen owns this exact PID, never a port/name.
            try:
                self.server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.server.kill()
                self.server.wait(timeout=10)

    def failure_screenshot(self):
        if self.udid is None:
            return
        try:
            self.simctl("io", self.udid, "screenshot", self.out / "ios-native-failure.png")
        except (OSError, subprocess.SubprocessError):
            # Preserve the failing command and its log; a boot failure has no screen.
            pass
        try:
            self.simctl("spawn", self.udid, "log", "show", "--last", "1m", "--style", "compact",
                        "--predicate", 'eventMessage CONTAINS "sh.phux.rune.ios"', timeout=30)
        except (OSError, subprocess.SubprocessError):
            pass

    def cleanup(self):
        errors = []
        try:
            self.stop_server()
        except (OSError, subprocess.SubprocessError) as error:
            errors.append(f"server cleanup: {error}")
        if self.udid is not None:
            for action in ("shutdown", "delete"):
                try:
                    self.simctl(action, self.udid, timeout=60)
                except (OSError, subprocess.SubprocessError) as error:
                    errors.append(f"{action} {self.udid}: {error}")
        (self.out / "cleanup.json").write_text(json.dumps({"errors": errors}) + "\n")
        if errors:
            raise ValueError("owned simulator cleanup failed: " + "; ".join(errors))


def qualify(app, phux, out, transport="quic"):
    with (app / "Info.plist").open("rb") as handle:
        info = plistlib.load(handle)
    if info.get("CFBundleIdentifier") != BUNDLE_ID:
        raise ValueError("unexpected app bundle identifier")
    if info.get("CFBundleSupportedPlatforms") != ["iPhoneSimulator"]:
        raise ValueError("qualification requires an iOS simulator bundle")
    out.mkdir(parents=True, exist_ok=True)
    if any(out.iterdir()):
        raise ValueError("qualification output must be empty (no stale evidence)")
    # Explicitly short UDS: simulator clients share the host's Unix namespace.
    with tempfile.TemporaryDirectory(prefix="rune-ios-", dir="/tmp") as temporary:
        lane = Qualification(app, phux, out, Path(temporary), transport)
        try:
            socket = lane.start_server()
            lane.create_simulator()
            pid = lane.launch(socket)
            report = lane.collect_report(pid)
            resumed = lane.resume_identity(report)
            (out / "evidence.json").write_text(json.dumps({
                "schema": 1, "report": report, "resumed_report": resumed, "app_pid": pid,
                "phux": str(phux), "phux_sha256": hashlib.sha256(phux.read_bytes()).hexdigest(),
                "input_proof": f"native insertText + Enter over paired {transport.upper()} with platform-held enrolled identity; not OS keyboard/IME proof",
            }, indent=2) + "\n")
        except Exception:
            lane.failure_screenshot()
            raise
        finally:
            failed = sys.exc_info()[0] is not None
            try:
                lane.cleanup()
            except ValueError as error:
                if not failed:
                    raise
                # cleanup.json retains the exact owned-resource failure;
                # never replace the app/assertion failure with teardown noise.
                print(f"Additional simulator cleanup failure: {error}", file=sys.stderr)


def absolute_path(value):
    path = Path(value)
    if not path.is_absolute():
        raise argparse.ArgumentTypeError("use an absolute path")
    return path.resolve()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", type=absolute_path, required=True)
    parser.add_argument("--phux", type=absolute_path, required=True)
    parser.add_argument("--out", type=absolute_path, required=True)
    parser.add_argument("--transport", choices=("quic", "wss"), default="quic")
    args = parser.parse_args()
    qualify(args.app, args.phux, args.out, args.transport)
    print(f"native iOS qualification passed: {args.out}")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        sys.exit(f"iOS qualification failed: {error}")
