#!/usr/bin/env python3
"""Exercise this checkout's native FFI against disposable enrolled QUIC/WSS servers.

Build prerequisites: scripts/build-phux-artifacts.sh ffi-dev. No app is launched.
All credentials, registry entries, sockets, and PTYs belong to the scratch home.
"""
import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import shlex
import signal
import socket
import subprocess
import tempfile
import time


ROOT = Path(__file__).resolve().parents[3]
TESTS = ROOT / "clients/cockpit/tests/everyday-remote"


def run(argv, directory, env, **kwargs):
    try:
        return subprocess.run(list(map(str, argv)), cwd=directory, env=env,
                              check=True, timeout=100, text=True, **kwargs)
    except subprocess.CalledProcessError as error:
        print(error.stderr or "child failed; see inherited stderr")
        raise


def isolated_env(directory):
    env = {key: value for key, value in os.environ.items() if not key.startswith("PHUX_")}
    env.update(HOME=str(directory), XDG_CONFIG_HOME=str(directory / "config"),
               XDG_STATE_HOME=str(directory / "state"), XDG_RUNTIME_DIR=str(directory / "run"),
               XDG_DATA_HOME=str(directory / "data"), XDG_CACHE_HOME=str(directory / "cache"),
               SHELL="/bin/sh", PHUX_PROFILE="default", PHUX_WS_SECURE="1",
               PHUX_SSH=str(directory / "no-ssh"), PHUX_TAILSCALE=str(directory / "no-tailscale"))
    for name in ("config/phux", "state", "run", "data", "cache"):
        (directory / name).mkdir(parents=True)
    return env


def free_port(kind):
    # The server cannot inherit these descriptors. A bind collision fails the
    # readiness assertion rather than accidentally passing against another server.
    with socket.socket(socket.AF_INET, kind) as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def wait_path(path, server, seconds=15):
    deadline = time.monotonic() + seconds
    while not path.exists():
        if server.poll() is not None or time.monotonic() >= deadline:
            raise RuntimeError(f"fixture did not publish {path.name}")
        time.sleep(0.02)


def stop(server):
    server.terminate()
    try:
        server.wait(timeout=10)
    except subprocess.TimeoutExpired:
        server.kill()
        server.wait()


@contextmanager
def fixture_server(phux, directory, env, port, transport):
    quic = port if transport == "quic" else free_port(socket.SOCK_DGRAM)
    wss = port if transport == "wss" else free_port(socket.SOCK_STREAM)
    # Both listeners MUST be explicit. Omitting either permits auto-binding
    # the detected overlay address, even when PHUX_TAILSCALE names no binary.
    command = [phux, "--socket", "s", "server", "--no-seed",
               "--quic", f"127.0.0.1:{quic}", "--listen", f"127.0.0.1:{wss}",
               "--exit-after-idle", "120"]
    with (directory / "server.log").open("w+") as log:
        server = subprocess.Popen(list(map(str, command)), cwd=directory, env=env,
                                  stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        try:
            wait_path(directory / "s", server)
            yield server
        finally:
            try:
                if server.poll() is None:
                    subprocess.run([str(phux), "--socket", "s", "kill", "everyday"],
                                   cwd=directory, env=env, capture_output=True, timeout=10, check=False)
            finally:
                stop(server)
                log.seek(0)
                print(log.read())


def registry(directory, endpoint, token, stale, fingerprint):
    token_file = directory / "token"
    token_file.write_text(token + "\n")
    token_file.chmod(0o600)
    bad_token = directory / "stale-token"
    bad_token.write_text(stale + "\n")
    bad_token.chmod(0o600)
    entries = []
    for name, secret, pin in (("loop", token_file, fingerprint),
                              ("stale-token", bad_token, fingerprint),
                              ("stale-pin", token_file, "cd" * 32)):
        entries.append(f'[[remote]]\nname = "{name}"\nendpoint = "{endpoint}"\n'
                       f'token-file = {json.dumps(str(secret))}\ncert-fingerprint = "{pin}"\n')
    config = directory / "config/phux/config.toml"
    config.write_text("\n".join(entries))
    return config


def start_workload(phux, directory, env, server):
    # A real PTY child is the execution oracle, independent of rendered echo.
    # Each accepted line appends once; stty disables terminal-driver echo.
    workload = (f"cd {shlex.quote(str(directory))}; stty -echo; i=0; "
                "while [ $i -lt 1600 ]; do printf 'HISTORY-%04d retained line\\n' $i; "
                "i=$((i+1)); done; printf 'READY\\n'; touch ready; "
                "while IFS= read -r line; do printf '%s\\n' \"$line\" >> executed; "
                "printf 'EXEC:%s\\n' \"$line\"; done")
    run([phux, "--socket", "s", "new", "--json", "-s", "everyday", "--",
         "/bin/sh", "-c", workload], directory, env, capture_output=True)
    wait_path(directory / "ready", server)


def exercise_stall(probe, config, directory, env, server):
    child = subprocess.Popen([str(probe), str(config), "loop", "stall"], cwd=directory, env=env)
    try:
        wait_path(directory / "stall-ready", child)
        server.send_signal(signal.SIGSTOP)
        wait_path(directory / "loss-detected", child, seconds=45)
        server.send_signal(signal.SIGCONT)
        (directory / "server-resumed").touch()
        assert child.wait(timeout=30) == 0, "stalled-peer probe failed"
    finally:
        server.send_signal(signal.SIGCONT)
        if child.poll() is None:
            stop(child)


def exercise(phux, probe, provider_probe, directory, transport):
    env = isolated_env(directory)
    paired = json.loads(run([phux, "pair", "--json"], directory, env,
                            capture_output=True).stdout)
    stale = json.loads(run([phux, "pair", "--json"], directory, env,
                           capture_output=True).stdout)
    port = free_port({"quic": socket.SOCK_DGRAM, "wss": socket.SOCK_STREAM}[transport])
    endpoint = f"{transport}://127.0.0.1:{port}"
    config = registry(directory, endpoint, paired["token"], stale["token"], paired["cert_fingerprint"])
    original = config.read_bytes()
    with fixture_server(phux, directory, env, port, transport) as server:
        who = json.loads(run([phux, "whoami", "--remote", "loop", "--json"],
                             directory, env, capture_output=True).stdout)
        assert who["credential_id"] == paired["credential_id"], who
        assert who["auth_route"].startswith("bearer-"), who
        print(f"{transport}: auth_route={who['auth_route']}; issued credential confirmed")
        formerly_valid = json.loads(run([phux, "whoami", "--remote", "stale-token", "--json"],
                                        directory, env, capture_output=True).stdout)
        assert formerly_valid["credential_id"] == stale["credential_id"]
        run([phux, "pair", "revoke", stale["credential_id"], "--json"],
            directory, env, capture_output=True)
        start_workload(phux, directory, env, server)
        run([probe, config, "loop", "lifecycle"], directory, env)
        exercise_stall(probe, config, directory, env, server)
        run([probe, config, "loop", "lease"], directory, env)
        expected = ["first-input", "after-reconnect", "after-link-stall", "after-expired-history"]
        if provider_probe:
            run([provider_probe], directory, dict(env, EVERYDAY_REMOTE_CONFIG=str(config)))
            expected.extend(["provider-before-reconnect", "provider-after-reconnect"])
        for target in ("stale-token", "stale-pin"):
            run([probe, config, target, "refused"], directory, env)
        assert (directory / "executed").read_text().splitlines() == expected
        assert config.read_bytes() == original, "resolution/dial mutated the saved registry"
        # Stop the actual coordinator before cleanup; its own shutdown ends
        # the PTYs. Reopening must not present a cold server as retained work.
        stop(server)
    with fixture_server(phux, directory, env, port, transport):
        run([probe, config, "loop", "cold"], directory, env)
        inventory = json.loads(run([phux, "ls", "--remote", "loop", "--json"],
                                   directory, env, capture_output=True).stdout)
        assert inventory["sessions"] == [], "cold fixture unexpectedly recreated lost work"


def compile_probe(profile, destination):
    archive = ROOT / "target" / profile / "libphux_client_ffi.a"
    if not archive.is_file():
        raise SystemExit(f"Build same-checkout artifacts first: bash clients/cockpit/scripts/build-phux-artifacts.sh {profile}")
    run(["cc", "-std=c11", "-Wall", "-Wextra", "-Werror", "-g",
         "-I", ROOT / "crates/phux-client-ffi/include", TESTS / "probe.c", archive,
         "-framework", "CoreFoundation", "-framework", "Security", "-liconv",
         "-o", destination], ROOT, os.environ)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("ffi-dev", "ffi-release"), default="ffi-dev")
    parser.add_argument("--scratch-root", type=Path, default=Path("/private/tmp/opencode"))
    parser.add_argument("--transport", choices=("quic", "wss", "both"), default="both")
    parser.add_argument("--ffi-only", action="store_true", help="Scoped inner loop; skip the Zig provider probe")
    args = parser.parse_args()
    phux = ROOT / "target" / args.profile / "phux"
    assert phux.is_file(), "same-checkout phux binary required"
    provider_probe = None if args.ffi_only else ROOT / "clients/cockpit/zig-out/bin/everyday-remote-provider"
    if provider_probe and not provider_probe.is_file():
        raise SystemExit("From clients/cockpit, build the headless provider: ./scripts/zig-build.sh everyday-remote-provider -Dphux-client-ffi-profile=ffi-dev -j2")
    with tempfile.TemporaryDirectory(prefix="everyday-remote-", dir=args.scratch_root) as scratch:
        directory = Path(scratch)
        probe = directory / "probe"
        compile_probe(args.profile, probe)
        transports = ("quic", "wss") if args.transport == "both" else (args.transport,)
        for transport in transports:
            home = directory / transport
            home.mkdir()
            exercise(phux, probe, provider_probe, home, transport)
    lane = "FFI-only" if args.ffi_only else "FFI + production provider"
    print(f"PASS: isolated {lane} transport proof; genuine remote Cockpit acceptance remains separate")


if __name__ == "__main__":
    main()
