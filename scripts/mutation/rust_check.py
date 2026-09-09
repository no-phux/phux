#!/usr/bin/env python3
"""Real-tool acceptance checks using disposable Rust fixtures, not production edits.

Run: CARGO_MUTANTS_BIN=/path/to/cargo-mutants python3 scripts/mutation/rust_check.py
Requires the exact runner pin, Git, and the repository Rust toolchain.
Reports remain under target/mutation/rust-check-*/; fixtures are removed.
"""

import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / "scripts/mutation/rust.sh"
SOURCE = """pub fn increment(n: u32) -> u32 { n + 1 }
pub fn even(n: u32) -> bool { n % 2 == 0 }
pub struct Token;
pub fn token() -> Token { Token }
pub fn ready() -> bool { true }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incomplete_increment_contract() {
        // Deliberately incomplete: constant 1 satisfies this one input.
        assert_eq!(increment(0), 1);
    }
    #[test]
    fn parity_contract() {
        assert!(even(4));
        assert!(!even(3));
    }
    #[test]
    fn readiness_wait() {
        if !ready() { std::thread::sleep(std::time::Duration::from_secs(5)); }
    }
}
"""
TOPOLOGY = '''#!/usr/bin/env python3
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

directory = Path(__file__).parent
role = sys.argv[-1]
signal.signal(signal.SIGTERM, signal.SIG_IGN)
signal.signal(signal.SIGINT, signal.SIG_IGN)
if role == "--version" and directory.name == "external-sigterm":
    # Exit early during graceful cleanup so descendants are reparented.
    signal.signal(signal.SIGINT, lambda *_: sys.exit(0))
(directory / (role + ".pid")).write_text(str(os.getpid()))
print("partial diagnostic: " + role, flush=True)
if role == "--version":
    subprocess.Popen([sys.executable, __file__, "middle"], start_new_session=True)
elif role == "middle":
    subprocess.Popen([sys.executable, __file__, "leaf"], start_new_session=True)
else:
    (directory / "ready").touch()
while True:
    time.sleep(1)
'''


def command(argv, root):
    return subprocess.check_output(argv, cwd=root, text=True, stderr=subprocess.STDOUT)


def fixture(root):
    (root / "src").mkdir()
    (root / "src/lib.rs").write_text(SOURCE)
    (root / "Cargo.toml").write_text(
        '[package]\nname = "rust-mutation-fixture"\nversion = "0.0.0"\nedition = "2024"\n'
    )
    (root / ".gitignore").write_text("/target/\n")
    shutil.copyfile(ROOT / "rust-toolchain.toml", root / "rust-toolchain.toml")
    command(["cargo", "generate-lockfile"], root)
    command(["git", "init", "-q"], root)
    command(["git", "add", "."], root)
    commit(root, "base fixture")


def commit(root, message):
    command(["git", "-c", "user.name=Mutation fixture", "-c", "user.email=fixture@example.invalid",
             "-c", "core.hooksPath=/dev/null", "commit", "-qm", message], root)


def run(root, reports, name, options, expected_code=0):
    output = reports / name
    before = command(["git", "status", "--porcelain"], root)
    source = (root / "src/lib.rs").read_bytes()
    process = subprocess.run(["bash", str(RUNNER), "--root", str(root), "--output", str(output),
                              "--package", "rust-mutation-fixture", *options], check=False)
    assert process.returncode == expected_code, (name, process.returncode)
    assert command(["git", "status", "--porcelain"], root) == before, name
    assert (root / "src/lib.rs").read_bytes() == source, name
    return json.loads((output / "summary.json").read_text())


def classifications(root, reports):
    report = run(root, reports, "classifications", [
        "--re", ("replace increment -> u32 with 1$|replace even -> bool with true$|"
                 "replace token -> Token with Default::default|replace ready -> bool with false$"),
        "--test-timeout", "1",
    ])
    assert report["status"] == "completed"
    assert report["counts"] == {"caught": 1, "missed": 1, "unviable": 1, "timeout": 1}, report
    raw = json.loads((reports / "classifications/mutants.out/outcomes.json").read_text())
    by_summary = {row["summary"]: row for row in raw["outcomes"]}
    assert by_summary["CaughtMutant"]["phase_results"][-1]["phase"] == "Test"
    assert by_summary["Unviable"]["phase_results"][-1]["phase"] == "Build"
    assert by_summary["Success"]["scenario"] == "Baseline"


def diff_and_sample(root, reports):
    base = command(["git", "rev-parse", "HEAD"], root).strip()
    source = root / "src/lib.rs"
    source.write_text(SOURCE + "\npub fn added(n: u32) -> u32 { n * 2 }\n")
    command(["git", "add", "src/lib.rs"], root)
    commit(root, "add function")
    options = ["--in-diff", base, "--list", "--limit", "2"]
    report = run(root, reports, "diff-list", options)
    assert report["merge_base"] == base
    assert report["status"] == "listed"
    assert 0 < report["selected"] <= 2 < report["candidates"]
    selected = json.loads((reports / "diff-list/selected.json").read_text())
    assert all(row["function"]["function_name"] == "added" for row in selected)
    run(root, reports, "diff-repeat", options)
    assert (reports / "diff-list/selected.json").read_bytes() == (
        reports / "diff-repeat/selected.json").read_bytes()
    report = run(root, reports, "empty-diff", ["--in-diff", "HEAD"])
    assert report["status"] == "empty_scope"
    assert report["selected"] == 0
    # Tracked, uncommitted edits must be compared against the real new-side text.
    source.write_text(source.read_text().replace("n * 2", "n * 3"))
    report = run(root, reports, "dirty-diff", ["--in-diff", "HEAD", "--list", "--limit", "1"])
    assert report["selected"] == 1


def baseline_failure(root, reports):
    source = root / "src/lib.rs"
    source.write_text(SOURCE.replace("assert_eq!(increment(0), 1)", "assert_eq!(increment(0), 9)"))
    report = run(root, reports, "baseline-failed", ["--limit", "1"], expected_code=4)
    assert report["status"] == "baseline_failed"
    assert sum(report["counts"].values()) == 0
    assert report["baseline"][0]["summary"] == "Failure"


def invalid_ref(root, reports):
    report = run(root, reports, "invalid-ref", ["--in-diff", "refs/heads/absent-base"],
                 expected_code=1)
    assert report["status"] == "tool_error"
    assert "merge-base exited" in report["error"]
    assert report["baseline"] == []


def invocation_timeout(root, reports):
    source = root / "src/lib.rs"
    source.write_text(SOURCE.replace("if !ready()", "if ready()"))
    report = run(root, reports, "invocation-timeout", ["--run-timeout", "1", "--limit", "1"],
                 expected_code=1)
    assert report["status"] == "tool_error"
    assert "timed out" in report["error"]
    # cargo-mutants starts Cargo/tests in separate process groups: interrupting
    # just its group must still let its handler reap the fixture test process.
    assert root.name not in command(["ps", "-axo", "command="], root)


def topology_pids(directory):
    return [int(path.read_text()) for path in directory.glob("*.pid")]


def topology_alive(pid, helper):
    process = subprocess.run(["ps", "-p", str(pid), "-o", "command="],
                             text=True, capture_output=True, check=False)
    return str(helper) in process.stdout


def check_topology(root, reports, mode):
    directory = root / "target" / mode
    directory.mkdir(parents=True)
    helper = directory / "topology.py"
    helper.write_text(TOPOLOGY)
    helper.chmod(0o755)
    output = reports / mode
    env = dict(os.environ, CARGO_MUTANTS_BIN=str(helper))
    source = (root / "src/lib.rs").read_bytes()
    before = command(["git", "status", "--porcelain"], root)
    started = time.monotonic()
    with subprocess.Popen([sys.executable, "-c", "import time; time.sleep(30)"],
                          start_new_session=True) as sentinel, subprocess.Popen(
                          ["bash", str(RUNNER), "--root", str(root), "--output", str(output),
                           "--run-timeout", "2"], env=env, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, text=True) as runner:
        try:
            wait_ready(directory)
            if mode == "external-sigterm":
                runner.send_signal(signal.SIGTERM)
            stdout, stderr = collect_runner(runner)
            evidence = topology_evidence(output, helper, runner.returncode)
            evidence["elapsed"] = time.monotonic() - started
            evidence["stdout"] = stdout
            evidence["stderr"] = stderr
            evidence["unrelated_survived"] = sentinel.poll() is None
            (reports / (mode + "-evidence.json")).write_text(json.dumps(evidence, indent=2))
        finally:
            sentinel.terminate()
            if runner.poll() is None:
                runner.kill()
            # Red tests must clean up the exact helper processes they created.
            for pid in topology_pids(directory):
                if topology_alive(pid, helper):
                    os.kill(pid, signal.SIGKILL)
    assert (root / "src/lib.rs").read_bytes() == source
    assert command(["git", "status", "--porcelain"], root) == before
    return evidence


def collect_runner(runner):
    try:
        return runner.communicate(timeout=10)
    except subprocess.TimeoutExpired:
        return "", "runner did not terminate within 10 seconds"


def wait_ready(directory):
    deadline = time.monotonic() + 5
    while not (directory / "ready").exists():
        if time.monotonic() > deadline:
            raise AssertionError("topology did not start")
        time.sleep(0.02)


def topology_evidence(output, helper, code):
    summary = output / "summary.json"
    log = output / "version.stdout.log"
    return {
        "exit": code,
        "summary": json.loads(summary.read_text()) if summary.exists() else None,
        "partial_stdout": log.read_text() if log.exists() else "",
        "survivors": [pid for pid in topology_pids(helper.parent) if topology_alive(pid, helper)],
    }


def process_checks(root, reports):
    results = [check_topology(root, reports, mode)
               for mode in ("external-sigterm", "detached-timeout")]
    print("Process topology evidence:", reports)
    for result in results:
        assert_topology_evidence(result)
    assert results[0]["exit"] == 143, results[0]
    assert results[0]["summary"]["status"] == "interrupted", results[0]
    assert results[1]["exit"] == 1, results[1]


def assert_topology_evidence(result):
    assert not result["survivors"], result
    assert result["exit"] > 0, result
    assert result["summary"]["status"] in ("interrupted", "tool_error"), result
    assert "partial diagnostic:" in result["partial_stdout"], result
    assert result["elapsed"] < 10, result
    assert result["unrelated_survived"], result


def main():
    parent = ROOT / "target/mutation"
    parent.mkdir(parents=True, exist_ok=True)
    reports = Path(tempfile.mkdtemp(prefix="rust-check-", dir=parent))
    # Respect the harness's approved scratch root on macOS; portable elsewhere.
    scratch = os.environ.get("TMPDIR", tempfile.gettempdir())
    if Path("/private/tmp/opencode").is_dir():
        scratch = "/private/tmp/opencode"
    with tempfile.TemporaryDirectory(prefix="rust-fixture-", dir=scratch) as directory:
        root = Path(directory)
        fixture(root)
        process_checks(root, reports)
        if "--process-only" in sys.argv:
            return
        classifications(root, reports)
        diff_and_sample(root, reports)
        invalid_ref(root, reports)
        invocation_timeout(root, reports)
        baseline_failure(root, reports)
    print(f"Rust runner acceptance checks passed; reports: {reports}")


if __name__ == "__main__":
    main()
