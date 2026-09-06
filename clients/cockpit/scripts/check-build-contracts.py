#!/usr/bin/env python3
"""Offline cache/profile wiring checks; never invoke a Rust or Zig compiler."""

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
        self.assertIn("./scripts/zig-build.sh package", package)

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


if __name__ == "__main__":
    unittest.main()
