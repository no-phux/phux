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


def _sibling(name, module):
    """A helper module beside this script, loaded without a package."""
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(module + ".py"))
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


_reports = _sibling("rune_qualification_reports", "qualification_reports")
SESSION = _reports.SESSION
INPUT_CONTRACTS = _reports.INPUT_CONTRACTS
validate_report = _reports.validate_report
validate_input_contracts = _reports.validate_input_contracts
validate_tree = _reports.validate_tree
_settle = _sibling("rune_simulator_settle", "simulator_settle")
_trace = _sibling("rune_connection_diagnostics", "connection_diagnostics")
_failure = _sibling("rune_failure_evidence", "failure_evidence")
_glass = _sibling("rune_liquid_glass", "liquid_glass")
_input = _sibling("rune_qualify_input", "qualify_input")
INPUT_GATE = _input.INPUT_GATE
input_gate_reason = _input.input_gate_reason
print_input_order_lines = _input.print_input_order_lines
_host = _sibling("rune_qualify_host", "qualify_host")
simulator_pair = _host.simulator_pair
server_environment = _host.server_environment

BUNDLE_ID = "sh.phux.rune.ios"
MARKER = "RUNE_IOS_INPUT_OK"
XCRUN = "/usr/bin/xcrun"
# The data-container lookup: tries, and seconds each.
CONTAINER_ATTEMPTS = 3
CONTAINER_TIMEOUT = 60
# Other read-only simulator queries: tries of the default 30s each.
QUERY_ATTEMPTS = 3


def marker_command(marker=MARKER):
    # The complete closed witness must not appear in the command echo.
    escaped = "".join(f"\\{byte:03o}" for byte in ("[[" + marker + "]]\n").encode())
    return f"printf '{escaped}'"


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
        self.diagnostic_prefixes = []
        self.diagnostic_reports = set()
        self.diagnostic_receipts = {}
        self.diagnostic_pid = None
        self.diagnostic_sealed = set()
        self.failure_evidence_requested = False
        self.failure_evidence_receipt = None
        self.failure_background_staged = False

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
        # Extra server phases are opt-in for this isolated fixture only.
        env["RUST_LOG"] = ("phux=info,warn,phux_server::publication_diagnostics=debug,"
                           "phux_server::enrollment_diagnostics=debug")
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
        # -b boots and waits under the existing 240s readiness deadline,
        # without a separate boot RPC's premature generic 30s cutoff.
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
        self.diagnostic_prefixes.append("")
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
        self.diagnostic_pid = int(match[1])
        return self.diagnostic_pid

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
                raise TimeoutError("native qualification report not written within 90s" + input_gate_reason(documents))
            time.sleep(0.5)
        shutil.copy2(source, self.out / (prefix + source.name))
        report = json.loads((self.out / (prefix + source.name)).read_text())
        validate_report(report, self.transport, resumed)
        tree = documents / "rune-tree.json"
        shutil.copy2(tree, self.out / (prefix + tree.name))
        marker = MARKER + "_RESUMED" if resumed else MARKER
        validate_tree(json.loads((self.out / (prefix + tree.name)).read_text()), report, marker)
        self.require_alive(pid)
        self.simctl("io", self.udid, "screenshot", self.out / (prefix + "ios-system-keyboard.png"))
        self.cycle_scene(documents, pid, prefix)
        time.sleep(3)
        self.require_alive(pid)
        self.simctl("io", self.udid, "screenshot", self.out / (prefix + "ios-native.png"))
        if not (self.out / (prefix + "ios-native.png")).stat().st_size:
            raise ValueError("empty simulator screenshot")
        self.diagnostic_reports.add(prefix)
        return report

    def qualify_glass(self, pid):
        return _glass.qualify(self, pid)

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
        # Home is already running; cold Settings startup can stall its launch RPC.
        self.simctl("launch", self.udid, "com.apple.springboard")
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
        self.finish_connection_diagnostics()
        self.simctl("terminate", self.udid, BUNDLE_ID, timeout=60)
        self.preserve_connection_diagnostics(prefixes=("",))
        for name in ("rune-qualification.json", "rune-tree.json", INPUT_GATE):
            (self.documents / name).unlink(missing_ok=True)
        marker = MARKER + "_RESUMED"
        self.diagnostic_prefixes.append("resume-")
        launched = self.simctl("launch", self.udid, BUNDLE_ID, "--qualification-resume",
            "--qualification-session", SESSION, "--qualification-input", marker_command(marker),
            "--qualification-marker", marker, timeout=120)
        match = re.search(rf"^{re.escape(BUNDLE_ID)}: ([0-9]+)$", launched, re.MULTILINE)
        if not match:
            raise ValueError("simulator did not report the resumed app PID")
        self.diagnostic_pid = int(match[1])
        report = self.collect_report(self.diagnostic_pid, resumed=True)
        if report["key_id"] != original["key_id"]:
            raise ValueError("relaunch did not use the same saved device identity")
        return report

    def emit_input_order(self):
        """Print input_ready:true, then the first InputKey send, per capture."""
        for prefix, label in (("", self.transport), ("resume-", f"{self.transport} resume")):
            path = self.out / (prefix + "rune-connection-diagnostics.jsonl")
            try:
                text = path.read_text()
            except OSError:
                continue
            print_input_order_lines(label, text)

    preserve_connection_diagnostics = _trace.preserve_connection_diagnostics
    require_connection_diagnostics = _trace.require_connection_diagnostics
    finish_connection_diagnostics = _trace.finish_connection_diagnostics
    stop_server = _trace.stop_server
    preserve_failure_evidence = _failure.preserve_failure_evidence
    stage_failure_background = _failure.stage_failure_background

    failure_screenshot = _trace.failure_screenshot
    cleanup = _trace.cleanup
    cleanup_simulator = _trace.cleanup_simulator


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
            lane.qualify_glass(pid)
            resumed = lane.resume_identity(report)
            (out / "evidence.json").write_text(json.dumps({
                "schema": 1, "report": report, "resumed_report": resumed, "app_pid": pid,
                "phux": str(phux), "phux_sha256": hashlib.sha256(phux.read_bytes()).hexdigest(),
                "input_proof": f"native insertText + Enter over paired {transport.upper()} with platform-held enrolled identity; not OS keyboard/IME proof",
            }, indent=2) + "\n")
        except Exception:
            lane.failure_evidence_requested = True
            lane.failure_screenshot()
            raise
        finally:
            failed = sys.exc_info()[0] is not None
            try:
                # Shutdown quiesces the writer before capture and simulator
                # deletion. Keep first-failure evidence and any later recovery.
                lane.cleanup()
                if not failed:
                    lane.require_connection_diagnostics()
                    lane.emit_input_order()
            except (ValueError, OSError, subprocess.SubprocessError) as error:
                if not failed:
                    raise
                # cleanup.json retains the exact owned-resource failure;
                # never replace the app/assertion failure with teardown noise.
                print(f"Additional qualification teardown failure: {error}", file=sys.stderr)


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
