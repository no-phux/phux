#!/usr/bin/env python3
"""Offline cache/profile wiring checks; never invoke a Rust or Zig compiler."""

import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
REPO_ROOT = ROOT.parent.parent


def copy_build_scripts(scripts):
    scripts.mkdir(parents=True, exist_ok=True)
    for name in ("build-phux-cli.sh", "build-phux-artifacts.sh", "stage-phux-cli.sh", "native-cargo-target.sh"):
        source = ROOT / "scripts" / name
        (scripts / name).write_text(source.read_text())


def mock_rustc(tools):
    rustc = tools / "rustc"
    rustc.write_text("#!/bin/sh\ntest \"$1\" = -vV || exit 91\nprintf 'rustc fixture\\nhost: aarch64-apple-darwin\\n'\n")
    rustc.chmod(0o755)


class BuildContracts(unittest.TestCase):
    def test_product_panes_refuse_framework_terminal_store(self):
        completed = subprocess.run(
            [sys.executable, str(ROOT / "scripts" / "check-shell-engine.py")],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
        self.assertEqual(completed.returncode, 0, completed.stdout + completed.stderr)

    def test_product_phux_keeps_vt_off_the_4096_channel(self):
        completed = subprocess.run(
            [sys.executable, str(ROOT / "scripts" / "check-vt-channel.py")],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
        self.assertEqual(completed.returncode, 0, completed.stdout + completed.stderr)

    def cache_steps(self):
        workflow = (REPO_ROOT / ".github/workflows/cockpit-ci.yml").read_text()
        steps = re.findall(
            r"(?ms)^      - uses: actions/cache/(?:restore|save)@.*?(?=^      - |\Z)",
            workflow,
        )
        self.assertEqual(len(steps), 2)
        return steps

    def test_ci_uses_shared_zig_cache_without_isolated_duplicate(self):
        env = dict(os.environ, GITHUB_ACTIONS="true")
        env.pop("PHUX_ZIG_CACHE_MODE", None)
        env.pop("PHUX_ZIG_BUILD_TIMEOUT", None)
        config = subprocess.check_output(
            ["bash", str(ROOT / "scripts/zig-build.sh"), "--print-config"],
            env=env, text=True,
        )
        self.assertRegex(config, r"(?m)^isolation:     shared$")
        self.assertRegex(config, r"(?m)^timeout:       none$")
        self.assertNotIn(".zig-global-cache", config)
        workflow = (REPO_ROOT / ".github/workflows/cockpit-ci.yml").read_text()
        self.assertIn("PHUX_ZIG_CACHE_MODE: shared", workflow)
        self.assertIn('PHUX_ZIG_BUILD_TIMEOUT: "0"', workflow)
        for step in self.cache_steps():
            self.assertIn("            ~/.cache/zig\n", step)
            self.assertIn("            clients/cockpit/.zig-cache\n", step)
            self.assertNotIn(".zig-global-cache", step)

    def test_local_default_stays_isolated(self):
        env = dict(os.environ)
        env.pop("GITHUB_ACTIONS", None)
        env.pop("PHUX_ZIG_CACHE_MODE", None)
        env.pop("PHUX_ZIG_BUILD_TIMEOUT", None)
        config = subprocess.check_output(
            ["bash", str(ROOT / "scripts/zig-build.sh"), "--print-config"],
            env=env, text=True,
        )
        self.assertRegex(config, r"(?m)^isolation:     isolated$")
        self.assertRegex(config, r"(?m)^timeout:       600s$")
        cache = re.search(r"(?m)^global cache:  (.+)$", config).group(1)
        self.assertEqual(Path(cache).relative_to(REPO_ROOT).as_posix(),
                         "clients/cockpit/.zig-global-cache")

    def test_cache_is_reusable_with_stable_fallback(self):
        # A commit-SHA suffix makes every main push a unique immutable entry
        # that never exact-hits (phux-6khi). Manifest hash rotates the key;
        # the prefix restores the latest compatible base.
        restore, save = self.cache_steps()
        key = re.search(r"(?m)^          key: (.+)$", restore).group(1)
        self.assertNotIn("${{ github.sha }}", key)
        self.assertIn("${{ runner.os }}", key)
        self.assertIn("${{ runner.arch }}", key)
        self.assertTrue(key.startswith("mini-v2-cockpit-zig-"))
        self.assertIn("hashFiles(", key)
        self.assertIn(f"          key: {key}", save)
        self.assertRegex(restore, r"restore-keys: \|\n            mini-v2-cockpit-zig-0\.16\.0-\$\{\{ runner.os \}\}-\$\{\{ runner.arch \}\}-\n")
        self.assertIn("github.ref == 'refs/heads/main'", save)
        self.assertIn("steps.zig-cache.outputs.cache-hit != 'true'", save)

    def test_profile_selects_monorepo_archive_directory(self):
        build = (ROOT / "build.zig").read_text()
        self.assertRegex(build, r'"phux-client-ffi-profile",\s*"[^"\n]+",\s*\) orelse "ffi-release"')
        self.assertIn('"target", ffi_profile', build)
        self.assertIn("resolvePhuxFfi(b, opt_include, opt_lib, ffi_profile)", build)
        self.assertNotIn('"target/ffi-release"', build)

    def test_cache_consumers_share_paths_and_wrapper(self):
        # Actions includes the path list in the cache version, independently of
        # the visible key. A matching restore prefix cannot bridge different lists.
        restore = self.cache_steps()[0]
        paths = re.search(r"(?ms)^          path: \|\n(.*?)^          key:", restore).group(1)
        for name in ("cockpit-release.yml", "cockpit-sdk-head.yml"):
            workflow = (REPO_ROOT / ".github/workflows" / name).read_text()
            actual = re.search(r"(?ms)^          path: \|\n(.*?)^          key:", workflow).group(1)
            self.assertEqual(actual, paths, name)
            self.assertIn("./scripts/zig-build.sh", workflow)
            self.assertIn("PHUX_ZIG_CACHE_MODE: shared", workflow)
            self.assertIn('PHUX_ZIG_BUILD_TIMEOUT: "0"', workflow)
            self.assertIn("./scripts/zig-build.sh --fetch=all", workflow)
            self.assertNotRegex(workflow, r"(?m)^\s*(?:run:\s*)?zig\s+build\b")
        package = (ROOT / "scripts/package-macos.sh").read_text()
        self.assertIn("bash ./scripts/build-shipping-app.sh package", package)

    def test_package_retries_hdiutil_without_quiet(self):
        # Main-push cockpit-ci failed after a verified ZIP with a silent
        # hdiutil exit 1 (Resource busy; -quiet closed stderr). The PR-only
        # production build step is not the missing dependency: this script
        # owns the shipping compile on main.
        package = (ROOT / "scripts/package-macos.sh").read_text()
        self.assertIn("hdiutil_retry() {", package)
        for verb in ("create", "verify", "attach", "detach"):
            self.assertIn(f"hdiutil_retry {verb}", package)
        self.assertNotRegex(package, r"(?m)^\s*/usr/bin/hdiutil create\b")
        self.assertNotRegex(package, r"(?m)^\s*/usr/bin/hdiutil verify\b")
        self.assertNotRegex(package, r"(?m)^\s*/usr/bin/hdiutil attach\b")
        create = re.search(r"hdiutil_retry create \\\n(?:.*\n)*?    \"\$\{DMG\}\"", package)
        self.assertIsNotNone(create)
        self.assertNotIn("-quiet", create.group(0))
        self.assertIn("${HDIUTIL:-/usr/bin/hdiutil}", package)
        # GitHub macOS runners have /bin/sync; invoking /usr/bin/sync aborted
        # packaging before hdiutil_retry could run (#708 follow-up).
        self.assertNotRegex(package, r"(?m)^\s*/usr/bin/sync\b")
        self.assertIn("command -v sync", package)

        helper = re.search(r"^hdiutil_retry\(\) \{.*?\n\}\n", package, re.M | re.S)
        self.assertIsNotNone(helper)
        with tempfile.TemporaryDirectory(prefix="cockpit-hdiutil-retry-") as directory:
            root = Path(directory)
            state = root / "calls"
            mock = root / "hdiutil"
            mock.write_text(
                "#!/bin/sh\n"
                'n=$(cat "$STATE" 2>/dev/null || echo 0)\n'
                'n=$((n + 1))\n'
                'echo "$n" > "$STATE"\n'
                'printf "%s\\n" "$@" >> "$ARGS"\n'
                'if [ "$n" -lt 3 ]; then\n'
                '  echo "hdiutil: create failed - Resource busy" >&2\n'
                "  exit 1\n"
                "fi\n"
                'echo "created: fake.dmg"\n'
                )
            mock.chmod(0o755)
            script = root / "retry.sh"
            script.write_text(
                "#!/usr/bin/env bash\n"
                "set -euo pipefail\n"
                f'DMG="{root / "out.dmg"}"\n'
                f'DMG_MOUNT="{root / "mnt"}"\n'
                f'HDIUTIL="{mock}"\n'
                "HDIUTIL_RETRY_ATTEMPTS=5\n"
                "HDIUTIL_RETRY_DELAY=0\n"
                f'STATE="{state}"\n'
                f'ARGS="{root / "args"}"\n'
                "export STATE ARGS\n"
                f"{helper.group(0)}"
                "hdiutil_retry create -ov \"$DMG\"\n"
            )
            script.chmod(0o755)
            subprocess.run(["bash", str(script)], check=True, capture_output=True)
            self.assertEqual(state.read_text().strip(), "3")

            always_fail = root / "hdiutil-fail"
            always_fail.write_text(
                "#!/bin/sh\n"
                'echo "hdiutil: create failed - Resource busy" >&2\n'
                "exit 1\n"
            )
            always_fail.chmod(0o755)
            fail_script = root / "retry-fail.sh"
            fail_script.write_text(
                "#!/usr/bin/env bash\n"
                "set -euo pipefail\n"
                f'DMG="{root / "out.dmg"}"\n'
                f'DMG_MOUNT="{root / "mnt"}"\n'
                f'HDIUTIL="{always_fail}"\n'
                "HDIUTIL_RETRY_ATTEMPTS=3\n"
                "HDIUTIL_RETRY_DELAY=0\n"
                f"{helper.group(0)}"
                "hdiutil_retry create -ov \"$DMG\"\n"
            )
            fail_script.chmod(0o755)
            failed = subprocess.run(["bash", str(fail_script)], capture_output=True, text=True)
            self.assertNotEqual(failed.returncode, 0)
            self.assertIn("hdiutil failed after 3 attempts", failed.stderr)

    def test_shipping_compile_and_package_share_inputs(self):
        with tempfile.TemporaryDirectory(prefix="cockpit-shipping-contract-") as directory:
            root = Path(directory)
            scripts = root / "scripts"
            scripts.mkdir()
            helper = scripts / "build-shipping-app.sh"
            helper.write_text((ROOT / "scripts/build-shipping-app.sh").read_text())
            wrapper = scripts / "zig-build.sh"
            wrapper.write_text('#!/bin/sh\nprintf "%s\\n" "$@"\n')
            wrapper.chmod(0o755)
            for goal in ([], ["package"]):
                result = subprocess.check_output(["bash", str(helper), *goal, "--summary", "all"], text=True)
                self.assertEqual(result.splitlines(), [*goal, "--summary", "all",
                    "-Dtarget=aarch64-macos", "-Dcpu=baseline",
                    "-Doptimize=ReleaseSafe", "-Dphux-enabled=true", "-Dtrace=off"])

    def test_main_has_one_shipping_compile_owner_and_debug_tests(self):
        workflow = (REPO_ROOT / ".github/workflows/cockpit-ci.yml").read_text()
        app = re.search(r"(?ms)^      - name: Build the production macOS app\n(.*?)(?=^      - name:)", workflow).group(1)
        self.assertIn("if: github.event_name == 'pull_request'", app)
        self.assertIn("build-shipping-app.sh", app)
        self.assertIn("check-ring-p256-helpers.py zig-out/bin/phux-cockpit", workflow)
        checker = (ROOT / "scripts" / "check-ring-p256-helpers.py").read_text()
        self.assertIn("_ring_core_0_17_14__p256_mul_mont", checker)
        self.assertIn("_ring_core_0_17_14__p256_sqr_mont", checker)
        self.assertIn("zig-build.sh test -Dplatform=null -Dphux-enabled=true --summary all", workflow)
        self.assertEqual(len(re.findall(r"zig-build\.sh test", workflow)), 1)
        self.assertNotIn("Test the app graph without the Phux provider", workflow)
        self.assertNotIn("cockpit-rust-artifacts", workflow)
        self.assertIn("bash clients/cockpit/scripts/build-phux-artifacts.sh", workflow)
        build = (ROOT / "build.zig").read_text()
        self.assertIn("addDisabledProviderCompileCheck", build)
        self.assertIn("keepRingP256Helpers(artifacts.exe)", build)
        self.assertIn("compile.link_gc_sections = false", build)
        self.assertIn('.name = "disabled-phux-provider"', build)
        self.assertIn("if (phux_enabled) addDisabledProviderCompileCheck", build)
        self.assertIn("cancel-in-progress: true", workflow)
        self.assertIn("uses: ./.github/actions/classify-changes", workflow)
        self.assertNotRegex(workflow, r"(?m)^    paths:")

    def test_node_model_suite_gates_local_and_ci_once(self):
        justfile = (REPO_ROOT / "justfile").read_text()
        workflow = (REPO_ROOT / ".github/workflows/cockpit-ci.yml").read_text()
        helper = (ROOT / "scripts/cockpit-node-test.sh").read_text()
        self.assertRegex(justfile, r"(?m)^cockpit-test:.*\bcockpit-node-test\b")
        self.assertRegex(justfile, r"(?m)^cockpit-node-test:\n    \./scripts/cockpit-node-test\.sh$")
        self.assertEqual(workflow.count("run: ./scripts/cockpit-node-test.sh"), 1)
        self.assertIn("npm ci --ignore-scripts --no-audit --no-fund", helper)
        self.assertIn("node --import ./src/tests/navigation-loader.mjs --test", helper)
        self.assertIn("./src/tests/*.test.mjs ./src/keybindings.test.ts", helper)

    def test_detached_launcher_regressions_gate_local_and_ci(self):
        justfile = (REPO_ROOT / "justfile").read_text()
        workflow = (REPO_ROOT / ".github/workflows/cockpit-ci.yml").read_text()
        self.assertEqual(justfile.count("./scripts/dev-run_test.sh"), 1)
        self.assertEqual(workflow.count("./scripts/dev-run_test.sh"), 1)

    def test_live_journey_cleanup_is_signal_safe_and_stops_only_owned_pid_once(self):
        with tempfile.TemporaryDirectory(prefix="cockpit-live-cleanup-") as directory:
            root = Path(directory)
            scripts = root / "scripts"
            (scripts / "lib").mkdir(parents=True)
            journey = scripts / "cockpit-live-journey.sh"
            journey.write_text((ROOT / "scripts/cockpit-live-journey.sh").read_text())
            (scripts / "lib/app-instance.sh").write_text('''
app_instance_require_free() { :; }
app_instance_pids() { printf '%s\\n' "${MOCK_LIVE_PIDS:-}"; }
app_instance_stop() { printf '%s\\n' "$1" >> "$STOP_LOG"; }
''')
            dev_run = scripts / "dev-run.sh"
            dev_run.write_text("#!/bin/sh\nprintf 'pid 4242, log fixture\\n'\nexit 23\n")
            dev_run.chmod(0o755)
            source = journey.read_text()
            self.assertIn("trap 'exit 130' INT", source)
            self.assertIn("trap 'exit 143' TERM", source)
            self.assertIn('pid="$(reported_pid)"', source)
            for live_pids, expected_stops in (("4242", ["4242"]), ("7777", [])):
                with self.subTest(live_pids=live_pids):
                    stop_log = root / "stops"
                    env = dict(os.environ, MOCK_LIVE_PIDS=live_pids, STOP_LOG=str(stop_log), TMPDIR=directory)
                    result = subprocess.run(["bash", str(journey), "--no-build"], env=env,
                                            capture_output=True, text=True)
                    self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                    self.assertIn("dev-run pipeline exited 23/0", result.stderr)
                    actual = stop_log.read_text().splitlines() if stop_log.exists() else []
                    self.assertEqual(actual, expected_stops)
                    stop_log.unlink(missing_ok=True)

            dev_run.write_text('''#!/usr/bin/env bash
printf 'pid 4242, log fixture\\n'
trap 'exit 130' INT
trap 'exit 143' TERM
while :; do sleep 1; done
''')
            for sent_signal, expected_status in ((signal.SIGINT, 130), (signal.SIGTERM, 143)):
                with self.subTest(sent_signal=sent_signal):
                    stop_log = root / "stops"
                    env = dict(os.environ, MOCK_LIVE_PIDS="4242", STOP_LOG=str(stop_log), TMPDIR=directory)
                    process = subprocess.Popen(["bash", str(journey), "--no-build"], env=env,
                                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                               text=True, start_new_session=True)
                    first_line = process.stdout.readline()
                    self.assertEqual(first_line, "pid 4242, log fixture\n")
                    os.killpg(process.pid, sent_signal)
                    stdout, stderr = process.communicate(timeout=10)
                    self.assertEqual(process.returncode, expected_status, first_line + stdout + stderr)
                    self.assertEqual(stop_log.read_text().splitlines(), ["4242"])
                    stop_log.unlink()

    def check_dev_invocation(self, options, profile):
        # Stop at the build boundary: no compiler, staging, or app launch.
        with tempfile.TemporaryDirectory(prefix="cockpit-build-contract-") as directory:
            root = Path(directory)
            scripts = root / "scripts"
            (scripts / "lib").mkdir(parents=True)
            (scripts / "dev-run.sh").write_text((ROOT / "scripts/dev-run.sh").read_text())
            (scripts / "lib/dev-app.sh").write_text("dev_app_home_init() { :; }\n")
            (scripts / "lib/app-instance.sh").write_text("")
            wrapper = scripts / "zig-build.sh"
            wrapper.write_text('#!/usr/bin/env bash\nprintf "%s\\n" "$@" > "$CAPTURE"\nexit 23\n')
            wrapper.chmod(0o755)
            # The forwarding contract is host-independent despite the app's guard.
            (root / "uname").write_text("#!/usr/bin/env bash\necho Darwin\n")
            (root / "uname").chmod(0o755)
            (root / "zig").write_text("#!/usr/bin/env bash\necho 'raw zig bypassed the wrapper' >&2\nexit 97\n")
            (root / "zig").chmod(0o755)
            capture = root / "args"
            env = dict(os.environ, CAPTURE=str(capture), PATH=f"{root}:{os.environ['PATH']}")
            result = subprocess.run(
                ["bash", str(scripts / "dev-run.sh"), "--phux", *options],
                env=env, capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 23, result.stdout + result.stderr)
            self.assertEqual(capture.read_text().splitlines(), [
                "package", "-Doptimize=ReleaseSafe", "-Dphux-enabled=true",
                f"-Dphux-client-ffi-profile={profile}",
            ])

    def test_dev_forwards_iteration_profile(self):
        self.check_dev_invocation(["--ffi-profile", "ffi-dev"], "ffi-dev")

    def test_dev_preserves_production_default(self):
        self.check_dev_invocation([], "ffi-release")

    def test_matching_cli_build_uses_checkout_target_and_copies_executable(self):
        with tempfile.TemporaryDirectory(prefix="cockpit cli contract ") as directory:
            repo = Path(directory)
            scripts = repo / "clients/cockpit/scripts"
            copy_build_scripts(scripts)
            script = scripts / "build-phux-cli.sh"
            tools = repo / "tools"
            tools.mkdir()
            mock_rustc(tools)
            cargo = tools / "cargo"
            cargo.write_text('''#!/usr/bin/env bash
set -eu
printf '%s\\n' "$CARGO_TARGET_DIR" "$@" > "$CAPTURE"
mkdir -p "$CARGO_TARGET_DIR/aarch64-apple-darwin/ffi-dev"
printf '#!/bin/sh\\nexit 0\\n' > "$CARGO_TARGET_DIR/aarch64-apple-darwin/ffi-dev/phux"
''')
            cargo.chmod(0o755)
            # A global installed phux is deliberately poisonous; staging should
            # neither execute it nor copy it.
            installed = tools / "phux"
            installed.write_text("#!/bin/sh\nexit 99\n")
            installed.chmod(0o755)
            capture = repo / "capture"
            destination = repo / "app with spaces/Contents/MacOS/phux"
            destination.parent.mkdir(parents=True)
            destination.write_text("old CLI")
            # A retained descriptor represents a running coordinator's inode:
            # staging must replace the name without truncating that old image.
            previous = destination.open("rb")
            self.addCleanup(previous.close)
            env = dict(os.environ, CAPTURE=str(capture),
                       RUSTC=str(tools / "rustc"),
                       CARGO_TARGET_DIR="/unrelated/target",
                       PATH=f"{tools}:{os.environ['PATH']}")
            subprocess.run(["bash", str(script), "ffi-dev", str(destination)],
                           env=env, check=True, capture_output=True)
            self.assertEqual(capture.read_text().splitlines(), [
                str(repo / "target"), "build", "--locked", "--manifest-path",
                str(repo / "Cargo.toml"), "--profile", "ffi-dev", "--target", "aarch64-apple-darwin", "-p", "phux",
            ])
            self.assertTrue(os.access(destination, os.X_OK))
            self.assertEqual(destination.read_bytes(), (repo / "target/aarch64-apple-darwin/ffi-dev/phux").read_bytes())
            self.assertEqual(previous.read(), b"old CLI")

            # A producer failure must not stage an old executable already in target.
            cargo.write_text("#!/bin/sh\nexit 23\n")
            destination.write_text("keep the staged coordinator")
            failed = subprocess.run(["bash", str(script), "ffi-dev", str(destination)], env=env, capture_output=True)
            self.assertEqual(failed.returncode, 23)
            self.assertEqual(destination.read_text(), "keep the staged coordinator")

    def test_artifact_producer_rejects_abort_profile_before_cargo(self):
        result = subprocess.run(["bash", str(ROOT / "scripts/build-phux-artifacts.sh"), "release"],
                                capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unwind FFI profile", result.stderr)

    def test_producer_preserves_measured_staticlib_and_cli_sequence(self):
        with tempfile.TemporaryDirectory(prefix="cockpit-producer-contract-") as directory:
            repo = Path(directory)
            scripts = repo / "clients/cockpit/scripts"
            copy_build_scripts(scripts)
            helper = scripts / "build-phux-artifacts.sh"
            tools = repo / "tools"
            tools.mkdir()
            mock_rustc(tools)
            cargo = tools / "cargo"
            cargo.write_text('''#!/usr/bin/env python3
import json, os, pathlib, sys
with open(os.environ["CAPTURE"], "a") as log:
    log.write(json.dumps({"args": [os.environ["CARGO_TARGET_DIR"], *sys.argv[1:]],
                          "rustflags": os.environ.get("RUSTFLAGS"),
                          "libghostty_cpu": os.environ.get("LIBGHOSTTY_VT_SYS_CPU")}) + "\\n")
profile = sys.argv[sys.argv.index("--profile") + 1]
output = pathlib.Path(os.environ["CARGO_TARGET_DIR"]) / "aarch64-apple-darwin" / profile
output.mkdir(parents=True, exist_ok=True)
(output / "libphux_client_ffi.a").write_text("archive")
cli = output / "phux"
cli.write_text("#!/bin/sh\\nexit 0\\n")
cli.chmod(0o755)
''')
            cargo.chmod(0o755)
            capture = repo / "calls.jsonl"
            env = dict(os.environ, PATH=f"{tools}:{os.environ['PATH']}", CAPTURE=str(capture),
                       RUSTC=str(tools / "rustc"),
                       CARGO_TARGET_DIR="/unrelated/target")
            env.pop("RUSTFLAGS", None)
            env.pop("LIBGHOSTTY_VT_SYS_CPU", None)
            subprocess.run(["bash", str(helper)], env=env, check=True, capture_output=True)
            actual = [json.loads(line) for line in capture.read_text().splitlines()]
            common = ["--locked", "--manifest-path", str(repo / "Cargo.toml"), "--profile", "ffi-release", "--target", "aarch64-apple-darwin"]
            self.assertEqual([call["args"] for call in actual], [
                [str(repo / "target"), "rustc", *common, "-p", "phux-client-ffi", "--lib", "--crate-type", "staticlib"],
                [str(repo / "target"), "build", *common, "-p", "phux"],
            ])
            self.assertTrue(all(call["rustflags"] == "-C target-cpu=apple-m1" for call in actual))
            self.assertTrue(all(call["libghostty_cpu"] == "baseline" for call in actual))

            capture.unlink()
            subprocess.run(["bash", str(helper), "ffi-dev"], env=env, check=True, capture_output=True)
            development = [json.loads(line) for line in capture.read_text().splitlines()]
            self.assertTrue(all(call["rustflags"] is None for call in development))
            self.assertTrue(all(call["libghostty_cpu"] is None for call in development))

    def test_candidate_cli_stage_requires_fresh_verification(self):
        with tempfile.TemporaryDirectory(prefix="cockpit-candidate-contract-") as directory:
            repo = Path(directory)
            scripts = repo / "clients/cockpit/scripts"
            scripts.mkdir(parents=True)
            for name in ("build-phux-cli.sh", "stage-phux-cli.sh"):
                (scripts / name).write_text((ROOT / "scripts" / name).read_text())
            tools = repo / "tools"
            tools.mkdir()
            cargo = tools / "cargo"
            cargo.write_text('#!/bin/sh\nprintf "fresh-cli" > "$CLI"\nprintf "built" > "$BUILD_CAPTURE"\n')
            cargo.chmod(0o755)
            verifier = repo / "scripts/ci/cockpit_artifacts.py"
            verifier.parent.mkdir(parents=True)
            verifier.write_text('import os, sys\nfrom pathlib import Path\nassert sys.argv[1:] == ["verify"]\nassert Path.cwd() == Path(__file__).resolve().parents[2]\nsys.exit(int(os.environ["VERIFY_STATUS"]))\n')
            cli = repo / "target/ffi-release/phux"
            cli.parent.mkdir(parents=True)
            destination = repo / "app/phux"
            destination.parent.mkdir()
            capture = repo / "build-capture"
            env = dict(os.environ, PHUX_CI_ARTIFACTS_VERIFIED="true", CLI=str(cli), BUILD_CAPTURE=str(capture),
                       PATH=f"{tools}:{os.environ['PATH']}")
            for status, expected_status, expected_cli in (("0", 0, "verified-cli"), ("1", 1, "keep staged CLI")):
                cli.write_text("verified-cli")
                destination.write_text("keep staged CLI")
                result = subprocess.run(["bash", str(scripts / "build-phux-cli.sh"), "ffi-release", str(destination)],
                                        cwd=scripts.parent, env=dict(env, VERIFY_STATUS=status), capture_output=True, text=True)
                self.assertEqual(result.returncode, expected_status, result.stderr)
                self.assertEqual(destination.read_text(), expected_cli)
                self.assertFalse(capture.exists())

    def test_artifact_profiles_preserve_ffi_unwind_boundary(self):
        manifest = (REPO_ROOT / "Cargo.toml").read_text()
        for profile in ("ffi-release", "ffi-dev"):
            section = re.search(rf"(?ms)^\[profile\.{profile}\]\n(.*?)(?=^\[|\Z)", manifest).group(1)
            self.assertRegex(section, r'(?m)^panic = "unwind"$')

    def test_native_artifacts_override_environment_and_cargo_config_targets(self):
        # Model Cargo's --target > env > build.target precedence. A successful
        # wrong-target build leaves poisonous old host-layout artifacts behind.
        for setting in ("environment", "config"):
            for command in ("build-phux-cli.sh", "build-phux-artifacts.sh"):
                with self.subTest(setting=setting, command=command):
                    self.check_native_artifacts(setting, command)

    def check_native_artifacts(self, setting, command):
        with tempfile.TemporaryDirectory(prefix="cockpit target contract ") as directory:
            repo = Path(directory)
            scripts = repo / "clients/cockpit/scripts"
            copy_build_scripts(scripts)
            tools = repo / "tools"
            tools.mkdir()
            mock_rustc(tools)
            config = repo / ".cargo/config.toml"
            config.parent.mkdir()
            config.write_text('[build]\ntarget = "x86_64-unknown-linux-gnu"\n' if setting == "config" else "")
            cargo = tools / "cargo"
            cargo.write_text('''#!/usr/bin/env python3
import json, os, pathlib, re, sys
args = sys.argv[1:]
with open(os.environ["CAPTURE"], "a") as log:
    log.write(json.dumps(args) + "\\n")
config = pathlib.Path(os.environ["CONFIG"]).read_text()
configured = re.search(r'target = "([^"]+)"', config)
target = os.environ.get("CARGO_BUILD_TARGET") or (configured[1] if configured else "")
if "--target" in args:
    target = args[args.index("--target") + 1]
profile = args[args.index("--profile") + 1]
output = pathlib.Path(os.environ["CARGO_TARGET_DIR"]) / target / profile
output.mkdir(parents=True, exist_ok=True)
name = "libphux_client_ffi.a" if args[0] == "rustc" else "phux"
artifact = output / name
artifact.write_text("fresh " + target + " " + name)
artifact.chmod(0o755)
''')
            cargo.chmod(0o755)
            legacy = repo / "target/ffi-release"
            legacy.mkdir(parents=True)
            for name in ("phux", "libphux_client_ffi.a"):
                (legacy / name).write_text("stale " + name)
                (legacy / name).chmod(0o755)
            destination = repo / "staged/phux"
            env = dict(os.environ, PATH=f"{tools}:{os.environ['PATH']}",
                       RUSTC=str(tools / "rustc"), CAPTURE=str(repo / "calls"), CONFIG=str(config),
                       CARGO_TARGET_DIR=str(repo / "unrelated-target"), PHUX_CI_ARTIFACTS_VERIFIED="false")
            env.pop("CARGO_BUILD_TARGET", None)
            if setting == "environment":
                env["CARGO_BUILD_TARGET"] = "x86_64-unknown-linux-gnu"
            subprocess.run(["bash", str(scripts / command), "ffi-release", str(destination)],
                           env=env, check=True, capture_output=True)
            staged = destination if command == "build-phux-cli.sh" else legacy / "phux"
            self.assertEqual(staged.read_text(), "fresh aarch64-apple-darwin phux")
            if command == "build-phux-artifacts.sh":
                self.assertEqual((legacy / "libphux_client_ffi.a").read_text(),
                                 "fresh aarch64-apple-darwin libphux_client_ffi.a")
            for call in (json.loads(line) for line in (repo / "calls").read_text().splitlines()):
                self.assertIn("--target", call)
                self.assertEqual(call[call.index("--target") + 1], "aarch64-apple-darwin")


if __name__ == "__main__":
    unittest.main()
