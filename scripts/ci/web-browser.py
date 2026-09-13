#!/usr/bin/env python3
"""Run real Chrome tests with an isolated, readiness-checked demo server.

Run inside `nix develop .#browser` (or with docs/SETUP.md's browser/native tools).
CHROMEDRIVER and CHROME select a matching local browser/driver pair. The
standalone client's cwd, Cargo.lock, and .cargo/config.toml remain authoritative.
"""

import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import ssl
import subprocess
import tempfile
import threading
import time


ROOT = Path(__file__).resolve().parents[2]
REQUIRED_TESTS = (
    "renders_engine_grid_to_canvas",
    "exact_wasm_codec_selects_native_and_renders_live_server",
    "synthesized_only_browser_remains_compatible_with_native_server",
)
AUTH_REQUIRED_TESTS = ("blackholed_wt_falls_back_to_authenticated_wss",)
TEST_TOKEN = "11" * 32


class UdpBlackhole:
    """Own a loopback UDP port that silently discards every QUIC datagram."""

    def __init__(self):
        self.socket = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.socket.bind(("127.0.0.1", 0))
        self.socket.settimeout(0.1)
        self.port = self.socket.getsockname()[1]
        self.packets = 0
        self.running = True
        self.thread = threading.Thread(target=self._discard, daemon=True)

    def _discard(self):
        while self.running:
            try:
                self.socket.recvfrom(65535)
                self.packets += 1
            except TimeoutError:
                pass
            except OSError:
                return

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, _kind, _value, _traceback):
        self.running = False
        self.socket.close()
        self.thread.join(timeout=2)


def signal_group(group, signum):
    """Signal only the session/process group that this runner created."""
    try:
        os.killpg(group, signum)
    except ProcessLookupError:
        return False
    return True


def stop(process):
    """Drain the owned group, even when its leader exited before its children."""
    signal_group(process.pid, signal.SIGINT)
    # Reap the leader while checking the whole group, rather than taking the
    # leader's exit as proof that Chrome/driver/compiler descendants exited.
    for _ in range(100):
        process.poll()
        if not signal_group(process.pid, 0):
            process.wait(timeout=5)
            return
        time.sleep(0.1)
    # A descendant can ignore SIGINT. Escalate the same owned group after the
    # bounded grace, including when process.wait() would already have returned.
    signal_group(process.pid, signal.SIGKILL)
    process.wait(timeout=5)


def run(command, *, cwd, env, timeout, log):
    """Give commands their own process group so cancellation reaps their tools."""
    with subprocess.Popen(
        command, cwd=cwd, env=env, stdout=log, stderr=subprocess.STDOUT,
        start_new_session=True,
    ) as process:
        try:
            code = process.wait(timeout=timeout)
            if code:
                raise subprocess.CalledProcessError(code, command)
        finally:
            stop(process)


def unused_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def websocket_upgrade(sock, token=None):
    protocol = b""
    if token:
        protocol = f"Sec-WebSocket-Protocol: phux.v1, phux.bearer.{token}\r\n".encode()
    sock.sendall(
        b"GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n"
        b"Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\n"
        b"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n" + protocol + b"\r\n"
    )
    return sock.recv(4096).startswith(b"HTTP/1.1 101 ")


def websocket_ready(port, *, tls=False, token=None):
    with socket.create_connection(("127.0.0.1", port), timeout=0.5) as raw:
        if not tls:
            return websocket_upgrade(raw, token)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        context.check_hostname = False
        context.verify_mode = ssl.CERT_NONE
        with context.wrap_socket(raw, server_hostname="localhost") as secure:
            return websocket_upgrade(secure, token)


def wait_ready(server, port, *, tls=False, token=None):
    for _ in range(120):
        if server.poll() is not None:
            raise RuntimeError("demo server exited before WebSocket readiness")
        try:
            if websocket_ready(port, tls=tls, token=token):
                return
        except OSError:
            pass
        time.sleep(0.1)
    raise TimeoutError("demo server WebSocket upgrade failed after 120 bounded probes")


def require_browser_tests(output, required=REQUIRED_TESTS):
    for name in required:
        if not re.search(rf"test (?:\S*::)?{name} \.\.\. ok", output):
            raise RuntimeError(f"Chrome did not report a passing browser test: {name}")
    print(f"Chrome execution verified: {len(required)} required browser tests passed")


def webdriver_environment(env, directory, cwd):
    """Merge the chosen binary into capabilities understood by wasm-bindgen."""
    key = "WASM_BINDGEN_TEST_WEBDRIVER_JSON"
    source = cwd / env.get(key, "webdriver.json")
    capabilities = {}
    if source.exists() or key in env:
        capabilities = json.loads(source.read_text())
    if env.get("CHROME"):
        capabilities.setdefault("goog:chromeOptions", {})["binary"] = env["CHROME"]
    if env.get("PHUX_TEST_ACCEPT_INSECURE_CERTS") == "1":
        capabilities["acceptInsecureCerts"] = True
    destination = directory / "webdriver.json"
    destination.write_text(json.dumps(capabilities))
    return dict(env, **{key: str(destination)})


def run_chrome(command, env, logs):
    cwd = ROOT / "clients/phux-web"
    # Preserve user capabilities without modifying their file, and keep this
    # copy alive until wasm-bindgen has finished both browser test binaries.
    with tempfile.TemporaryDirectory(prefix="phux-webdriver-", dir=env.get("TMPDIR")) as scratch:
        web_env = webdriver_environment(env, Path(scratch), cwd)
        web_env["CARGO_TARGET_DIR"] = env.get("PHUX_WEB_CARGO_TARGET_DIR", str(cwd / "target"))
        with (logs / "chrome.log").open("w") as log:
            run(command, cwd=cwd, env=web_env, timeout=600, log=log)


def browser_tests(env, logs):
    port = unused_port()
    env["PHUX_WS_ADDR"] = f"127.0.0.1:{port}"
    env["PHUX_TEST_WS_URL"] = f"ws://127.0.0.1:{port}/"
    command = ["wasm-pack", "test", "--headless", "--chrome", "--locked",
               "--test", "render", "--test", "e2e_browser"]
    # wasm-pack accepts a driver; wasm-bindgen needs capabilities for the binary.
    if env.get("CHROMEDRIVER"):
        command[4:4] = ["--chromedriver", env["CHROMEDRIVER"]]
    server_bin = Path(env["CARGO_TARGET_DIR"]) / "debug/examples/ws_demo_server"
    with (logs / "server.log").open("w") as server_log:
        server = subprocess.Popen([str(server_bin)], cwd=ROOT, env=env,
                                  stdout=server_log, stderr=subprocess.STDOUT,
                                  start_new_session=True)
        try:
            wait_ready(server, port)
            # Do not reuse the native target or override the standalone rustflags.
            run_chrome(command, env, logs)
            require_browser_tests((logs / "chrome.log").read_text())
        finally:
            stop(server)


def write_auth_fixture(directory):
    cert = directory / "cert.pem"
    key = directory / "key.pem"
    tokens = directory / "tokens.json"
    subprocess.run([
        "openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
        "-keyout", str(key), "-out", str(cert), "-days", "1",
        "-subj", "/CN=localhost",
        "-addext", "subjectAltName=DNS:localhost,IP:127.0.0.1",
    ], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)
    verifier = hashlib.sha256(bytes.fromhex(TEST_TOKEN)).hexdigest()
    tokens.write_text(json.dumps({
        "version": 1,
        "credentials": [{
            "id": "browser-e2e",
            "verifier": f"sha256:{verifier}",
            "principal": "device:browser-e2e",
            "scopes": ["terminal.control"],
            "issued_at": "2026-09-13T00:00:00Z",
            "expires_at": None,
            "revoked_at": None,
            "generation": 1,
        }],
    }))
    key.chmod(0o600)
    tokens.chmod(0o600)
    return cert, key, tokens


def authenticated_fallback_tests(env, logs, directory):
    port = unused_port()
    cert, key, tokens = write_auth_fixture(directory)
    auth_env = dict(env)
    auth_env.update({
        "PHUX_WS_ADDR": f"127.0.0.1:{port}",
        "PHUX_WS_SECURE": "1",
        "PHUX_WS_TLS_CERT": str(cert),
        "PHUX_WS_TLS_KEY": str(key),
        "PHUX_WS_TOKENS": str(tokens),
        "PHUX_TEST_WSS_URL": f"wss://127.0.0.1:{port}/",
        "PHUX_TEST_TOKEN": TEST_TOKEN,
        "PHUX_TEST_ACCEPT_INSECURE_CERTS": "1",
    })
    with UdpBlackhole() as blackhole:
        auth_env["PHUX_TEST_WT_URL"] = (
            f"https://127.0.0.1:{blackhole.port}/session?token={TEST_TOKEN}"
        )
        command = ["wasm-pack", "test", "--headless", "--chrome", "--locked",
                   "--test", "e2e_fallback_auth"]
        if auth_env.get("CHROMEDRIVER"):
            command[4:4] = ["--chromedriver", auth_env["CHROMEDRIVER"]]
        server_bin = Path(auth_env["CARGO_TARGET_DIR"]) / "debug/examples/ws_demo_server"
        with (logs / "auth-server.log").open("w") as server_log:
            server = subprocess.Popen([str(server_bin)], cwd=ROOT, env=auth_env,
                                      stdout=server_log, stderr=subprocess.STDOUT,
                                      start_new_session=True)
            try:
                wait_ready(server, port, tls=True, token=TEST_TOKEN)
                run_chrome(command, auth_env, logs)
                require_browser_tests((logs / "chrome.log").read_text(), AUTH_REQUIRED_TESTS)
                if blackhole.packets == 0:
                    raise RuntimeError("Chrome sent no datagrams to the WebTransport blackhole")
                print(f"WebTransport blackhole verified: {blackhole.packets} datagrams discarded")
            finally:
                stop(server)


def interrupted(signum, _frame):
    raise SystemExit(128 + signum)


def main():
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    logs = ROOT / "target/web-browser-logs"
    logs.mkdir(parents=True, exist_ok=True)
    for name in ("build.log", "chrome.log", "server.log", "auth-server.log"):
        (logs / name).unlink(missing_ok=True)
    env = dict(os.environ)
    for variable, binary in (("CHROME", "chromium"), ("CHROMEDRIVER", "chromedriver")):
        executable = shutil.which(binary)
        if executable:
            env.setdefault(variable, executable)
    env.setdefault("CARGO_BUILD_JOBS", "2")
    env["CARGO_TARGET_DIR"] = str(Path(env.get("CARGO_TARGET_DIR", ROOT / "target/web-native")).resolve())
    try:
        with (logs / "build.log").open("w") as log:
            run(["cargo", "build", "--locked", "-p", "phux-server", "--example", "ws_demo_server"],
                cwd=ROOT, env=env, timeout=1200, log=log)
        with tempfile.TemporaryDirectory(prefix="phux-browser-") as scratch:
            env["TMPDIR"] = scratch
            if env.get("PHUX_BROWSER_AUTH_ONLY") != "1":
                browser_tests(env, logs)
            authenticated_fallback_tests(env, logs, Path(scratch))
    finally:
        for path in sorted(logs.glob("*.log")):
            print(f"\n{path}:\n{path.read_text()}", flush=True)


if __name__ == "__main__":
    main()
