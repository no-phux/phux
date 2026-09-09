#!/usr/bin/env python3
"""Offline cache/profile wiring checks; never invoke a Rust or Zig compiler."""

import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
REPO_ROOT = ROOT.parent.parent


class BuildContracts(unittest.TestCase):
    def cache_steps(self):
        workflow = (REPO_ROOT / ".github/workflows/cockpit-ci.yml").read_text()
        steps = re.findall(
            r"(?ms)^      - uses: actions/cache/(?:restore|save)@.*?(?=^      - |\Z)",
            workflow,
        )
        self.assertEqual(len(steps), 2)
        return steps

    def test_cache_covers_wrapper_default(self):
        env = dict(os.environ, PHUX_ZIG_CACHE_MODE="isolated")
        config = subprocess.check_output(
            ["bash", str(ROOT / "scripts/zig-build.sh"), "--print-config"],
            env=env, text=True,
        )
        cache = re.search(r"(?m)^global cache:  (.+)$", config).group(1)
        relative = Path(cache).relative_to(REPO_ROOT).as_posix()
        for step in self.cache_steps():
            self.assertIn(f"            {relative}\n", step)
            self.assertIn("            ~/.cache/zig\n", step)
            self.assertIn("            clients/cockpit/.zig-cache\n", step)

    def test_cache_rotates_with_stable_fallback(self):
        restore, save = self.cache_steps()
        key = re.search(r"(?m)^          key: (.+)$", restore).group(1)
        self.assertIn("${{ github.sha }}", key)
        self.assertIn("${{ runner.os }}", key)
        self.assertIn("hashFiles(", key)
        self.assertIn(f"          key: {key}", save)
        self.assertRegex(restore, r"restore-keys: \|\n            cockpit-zig-0\.16\.0-\$\{\{ runner.os \}\}-\n")
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
            self.assertNotRegex(workflow, r"(?m)^\s*(?:run:\s*)?zig\s+build\b")
        package = (ROOT / "scripts/package-macos.sh").read_text()
        self.assertIn("bash ./scripts/build-shipping-app.sh package", package)

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
                    "-Dtarget=aarch64-macos", "-Doptimize=ReleaseSafe", "-Dphux-enabled=true"])

    def test_main_has_one_shipping_compile_owner_and_debug_tests(self):
        workflow = (REPO_ROOT / ".github/workflows/cockpit-ci.yml").read_text()
        app = re.search(r"(?ms)^      - name: Build the production macOS app\n(.*?)(?=^      - name:)", workflow).group(1)
        self.assertIn("if: github.event_name == 'pull_request'", app)
        self.assertIn("build-shipping-app.sh", app)
        self.assertIn("zig-build.sh test -Dplatform=null -Dphux-enabled=true --summary all", workflow)
        self.assertIn("cancel-in-progress: true", workflow)
        self.assertIn("uses: ./.github/actions/classify-changes", workflow)
        self.assertNotRegex(workflow, r"(?m)^    paths:")

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
            scripts.mkdir(parents=True)
            script = scripts / "build-phux-cli.sh"
            script.write_text((ROOT / "scripts/build-phux-cli.sh").read_text())
            (scripts / "stage-phux-cli.sh").write_text((ROOT / "scripts/stage-phux-cli.sh").read_text())
            tools = repo / "tools"
            tools.mkdir()
            cargo = tools / "cargo"
            cargo.write_text('''#!/usr/bin/env bash
set -eu
printf '%s\\n' "$CARGO_TARGET_DIR" "$@" > "$CAPTURE"
mkdir -p "$CARGO_TARGET_DIR/ffi-dev"
printf '#!/bin/sh\\nexit 0\\n' > "$CARGO_TARGET_DIR/ffi-dev/phux"
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
                       CARGO_TARGET_DIR="/unrelated/target",
                       PATH=f"{tools}:{os.environ['PATH']}")
            subprocess.run(["bash", str(script), "ffi-dev", str(destination)],
                           env=env, check=True, capture_output=True)
            self.assertEqual(capture.read_text().splitlines(), [
                str(repo / "target"), "build", "--locked", "--manifest-path",
                str(repo / "Cargo.toml"), "--profile", "ffi-dev", "-p", "phux",
            ])
            self.assertTrue(os.access(destination, os.X_OK))
            self.assertEqual(destination.read_bytes(), (repo / "target/ffi-dev/phux").read_bytes())
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
            scripts.mkdir(parents=True)
            helper = scripts / "build-phux-artifacts.sh"
            helper.write_text((ROOT / "scripts/build-phux-artifacts.sh").read_text())
            tools = repo / "tools"
            tools.mkdir()
            cargo = tools / "cargo"
            cargo.write_text('''#!/usr/bin/env python3
import json, os, pathlib, sys
with open(os.environ["CAPTURE"], "a") as log:
    log.write(json.dumps([os.environ["CARGO_TARGET_DIR"], *sys.argv[1:]]) + "\\n")
output = pathlib.Path(os.environ["CARGO_TARGET_DIR"]) / "ffi-release"
output.mkdir(parents=True, exist_ok=True)
(output / "libphux_client_ffi.a").write_text("archive")
cli = output / "phux"
cli.write_text("#!/bin/sh\\nexit 0\\n")
cli.chmod(0o755)
''')
            cargo.chmod(0o755)
            capture = repo / "calls.jsonl"
            env = dict(os.environ, PATH=f"{tools}:{os.environ['PATH']}", CAPTURE=str(capture),
                       CARGO_TARGET_DIR="/unrelated/target")
            subprocess.run(["bash", str(helper)], env=env, check=True, capture_output=True)
            actual = [json.loads(line) for line in capture.read_text().splitlines()]
            common = ["--locked", "--manifest-path", str(repo / "Cargo.toml"), "--profile", "ffi-release"]
            self.assertEqual(actual, [
                [str(repo / "target"), "rustc", *common, "-p", "phux-client-ffi", "--lib", "--crate-type", "staticlib"],
                [str(repo / "target"), "build", *common, "-p", "phux"],
            ])

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


if __name__ == "__main__":
    unittest.main()
