#!/usr/bin/env python3
"""Exercise this checkout's native FFI against disposable enrolled QUIC/WSS servers.

Builds same-checkout artifacts and the provider for the selected profile. No app is launched.
All credentials, registry entries, sockets, and PTYs belong to the scratch home.
"""
import argparse
from contextlib import contextmanager
import hashlib
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
TERMINATION_SIGNALS = (signal.SIGTERM, signal.SIGHUP, signal.SIGINT)


def terminate_request(signum, _frame):
    # Repeated termination requests must not interrupt the cleanup already in
    # progress. SystemExit unwinds every fixture/owned-child context below.
    for number in TERMINATION_SIGNALS:
        signal.signal(number, signal.SIG_IGN)
    raise SystemExit(128 + signum)


@contextmanager
def termination_scope():
    previous = {number: signal.signal(number, terminate_request) for number in TERMINATION_SIGNALS}
    try:
        yield
    finally:
        for number, handler in previous.items():
            signal.signal(number, handler)


@contextmanager
def owned_child(argv, **kwargs):
    # A pending signal cannot land between Popen returning and establishing
    # ownership. Unblocking inside the try also safely handles that window.
    previous = signal.pthread_sigmask(signal.SIG_BLOCK, TERMINATION_SIGNALS)
    child = None
    try:
        # This runner is single-threaded. Restore the child's inherited mask
        # before exec so SIGTERM can still stop the owned server/probe normally.
        child = subprocess.Popen(list(map(str, argv)),
                                 preexec_fn=lambda: signal.pthread_sigmask(signal.SIG_SETMASK, previous),
                                 **kwargs)
        signal.pthread_sigmask(signal.SIG_SETMASK, previous)
        yield child
    finally:
        try:
            if child is not None:
                stop(child)
        finally:
            signal.pthread_sigmask(signal.SIG_SETMASK, previous)


def run(argv, directory, env, timeout=100, **kwargs):
    try:
        return subprocess.run(list(map(str, argv)), cwd=directory, env=env,
                              check=True, timeout=timeout, text=True, **kwargs)
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


def reap(server):
    try:
        server.wait(timeout=10)
    except subprocess.TimeoutExpired:
        server.kill()
        server.wait()


def stop(server):
    if server.poll() is not None:
        return
    try:
        server.send_signal(signal.SIGCONT)
    finally:
        try:
            server.terminate()
        finally:
            reap(server)


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
        try:
            with owned_child(command, cwd=directory, env=env,
                             stdin=subprocess.DEVNULL, stdout=log, stderr=log) as server:
                wait_path(directory / "s", server)
                yield server
        finally:
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
    with owned_child([probe, config, "loop", "stall"], cwd=directory, env=env) as child:
        try:
            wait_path(directory / "stall-ready", child)
            server.send_signal(signal.SIGSTOP)
            wait_path(directory / "loss-detected", child, seconds=45)
            server.send_signal(signal.SIGCONT)
            (directory / "server-resumed").touch()
            assert child.wait(timeout=30) == 0, "stalled-peer probe failed"
        finally:
            server.send_signal(signal.SIGCONT)


def assert_execution_records(directory, expected):
    assert (directory / "executed").read_text().splitlines() == expected
    assert (directory / "second-executed").read_text().splitlines() == [
        "second-terminal-only"], "secondary terminal received unexpected input"


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
        assert_execution_records(directory, expected)
        assert config.read_bytes() == original, "resolution/dial mutated the saved registry"
        # Stop the actual coordinator before cleanup; its own shutdown ends
        # the PTYs. Reopening must not present a cold server as retained work.
        stop(server)
    with fixture_server(phux, directory, env, port, transport):
        run([probe, config, "loop", "cold"], directory, env)
        inventory = json.loads(run([phux, "ls", "--remote", "loop", "--json"],
                                   directory, env, capture_output=True).stdout)
        assert inventory["sessions"] == [], "cold fixture unexpectedly recreated lost work"


def compile_probe(artifact_dir, destination):
    archive = artifact_dir / "libphux_client_ffi.a"
    if not archive.is_file():
        raise SystemExit(f"Expected rebuilt archive is missing: {archive}")
    run(["cc", "-std=c11", "-Wall", "-Wextra", "-Werror", "-g",
         "-I", ROOT / "crates/phux-client-ffi/include", TESTS / "probe.c", archive,
         "-framework", "CoreFoundation", "-framework", "Security", "-liconv",
         "-o", destination], ROOT, build_environment())


def build_environment():
    env = dict(os.environ)
    for name in ("PHUX_CLIENT_FFI_INCLUDE_DIR", "PHUX_CLIENT_FFI_LIB_DIR",
                 "CARGO_TARGET_DIR", "CARGO_BUILD_TARGET"):
        env.pop(name, None)
    env["CARGO_BUILD_JOBS"] = "2"
    return env


def native_target(env):
    output = run(["rustc", "-vV"], ROOT, env, capture_output=True).stdout
    for line in output.splitlines():
        if line.startswith("host: "):
            target = line.removeprefix("host: ").strip()
            if target:
                return target
    raise RuntimeError("rustc -vV did not identify its native host target")


def build_provider(profile, artifact_dir, env, ffi_only):
    if ffi_only:
        return None
    cockpit = ROOT / "clients/cockpit"
    run([cockpit / "scripts/zig-build.sh", "everyday-remote-provider", "-j2",
         f"-Dphux-client-ffi-profile={profile}",
         f"-Dphux-client-ffi-include-dir={ROOT}/crates/phux-client-ffi/include",
         f"-Dphux-client-ffi-lib-dir={artifact_dir}"], cockpit, env, timeout=1800)
    return cockpit / "zig-out/bin/everyday-remote-provider"


def build_artifacts(profile, ffi_only):
    env = build_environment()
    target = native_target(env)
    common = ["--locked", "--manifest-path", ROOT / "Cargo.toml", "--profile", profile,
              "--target", target, "--target-dir", ROOT / "target"]
    # Same library/CLI sequence as build-phux-artifacts.sh, with an explicit
    # target so build.target cannot redirect output to an unchecked old path.
    run(["cargo", "rustc", *common, "-p", "phux-client-ffi", "--lib", "--crate-type", "staticlib"],
        ROOT, env, timeout=1800)
    run(["cargo", "build", *common, "-p", "phux"], ROOT, env, timeout=1800)
    artifact_dir = ROOT / "target" / target / profile
    return {"target": target, "directory": artifact_dir,
            "provider": build_provider(profile, artifact_dir, env, ffi_only)}


def file_digest(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def git_bytes(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT)


def source_identity():
    paths = git_bytes("ls-files", "-z", "--cached", "--others", "--exclude-standard").split(b"\0")
    tree = hashlib.sha256()
    for relative in sorted(set(filter(None, paths))):
        path = ROOT / os.fsdecode(relative)
        if path.is_file():
            tree.update(relative + b"\0" + file_digest(path).encode() + b"\0")
    return {"root": str(ROOT), "revision": git_bytes("rev-parse", "HEAD").decode().strip(),
            "diff_sha256": hashlib.sha256(git_bytes("diff", "--no-ext-diff", "--no-textconv", "--binary", "HEAD")).hexdigest(),
            "source_tree_sha256": tree.hexdigest()}


def artifact_identity(artifact_dir, probe, provider_probe):
    paths = {"header": ROOT / "crates/phux-client-ffi/include/phux/client.h",
             "archive": artifact_dir / "libphux_client_ffi.a",
             "phux": artifact_dir / "phux", "c_probe": probe}
    if provider_probe:
        paths["provider_probe"] = provider_probe
    return {name: {"path": str(path), "sha256": file_digest(path)} for name, path in paths.items()}


def publish_evidence(evidence, scratch_root):
    with tempfile.NamedTemporaryFile(mode="w", prefix="everyday-remote-provenance-",
                                     suffix=".json", dir=scratch_root, delete=False) as output:
        json.dump(evidence, output, indent=2)
        output.write("\n")
        print(f"PROVENANCE: {output.name}")


def arguments():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", choices=("ffi-dev", "ffi-release"), default="ffi-dev")
    parser.add_argument("--scratch-root", type=Path, default=Path("/private/tmp/opencode"))
    parser.add_argument("--transport", choices=("quic", "wss", "both"), default="both")
    parser.add_argument("--ffi-only", action="store_true", help="Scoped inner loop; skip the Zig provider probe")
    return parser.parse_args()


def verify(args):
    source = source_identity()
    built = build_artifacts(args.profile, args.ffi_only)
    provider_probe = built["provider"]
    artifact_dir = built["directory"]
    phux = artifact_dir / "phux"
    with tempfile.TemporaryDirectory(prefix="everyday-remote-", dir=args.scratch_root) as scratch:
        directory = Path(scratch)
        probe = directory / "probe"
        compile_probe(artifact_dir, probe)
        artifacts = artifact_identity(artifact_dir, probe, provider_probe)
        transports = ("quic", "wss") if args.transport == "both" else (args.transport,)
        for transport in transports:
            home = directory / transport
            home.mkdir()
            exercise(phux, probe, provider_probe, home, transport)
        assert source_identity() == source, "checkout source changed during verification"
        assert artifact_identity(artifact_dir, probe, provider_probe) == artifacts, "artifacts changed during verification"
        publish_evidence({"source": source, "profile": args.profile, "artifacts": artifacts,
                          "native_target": built["target"], "transports": transports,
                          "result": "passed"}, args.scratch_root)
    lane = "FFI-only" if args.ffi_only else "FFI + production provider"
    print(f"PASS: isolated {lane} transport proof; genuine remote Cockpit acceptance remains separate")


def main():
    with termination_scope():
        verify(arguments())


if __name__ == "__main__":
    main()
