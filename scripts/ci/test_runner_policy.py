#!/usr/bin/env python3
"""Future workflows use free public compute and preserve native release targets."""

import os
from pathlib import Path
import re
import subprocess
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".github/workflows"
RELEASE = (WORKFLOWS / "release.yml").read_text()
NEXT_RELEASE = (WORKFLOWS / "next-release.yml").read_text()
LINUX_RELEASE_SETUP = (ROOT / "scripts/ci/setup-linux-release-userspace.sh").read_text()
STANDARD = {"ubuntu-latest", "ubuntu-24.04", "ubuntu-24.04-arm", "xcode-27"}
RETIRED_UBUNTU_2204 = re.compile(
    r"(?m)^[ \t]*(?:runs-on:|- os:) ubuntu-22\.04(?:-arm)?[ \t]*$"
)


class RunnerPolicyTests(unittest.TestCase):
    def test_all_runner_selectors_are_standard_public_capacity(self):
        for path in WORKFLOWS.glob("*.yml"):
            with self.subTest(workflow=path.name):
                body = path.read_text()
                self.assertNotIn("blacksmith-", body)
                self.assertNotIn("self-hosted,", body)
                selectors = re.findall(r"^\s*(?:runs-on:|- os:) (.+)$", body, re.M)
                self.assertTrue(set(selectors) <= STANDARD | {"${{ matrix.os }}"})
                for group in re.findall(r"^\s*group: (.+)$", body, re.M):
                    self.assertTrue(group.startswith("mini-v1-"), group)

    def test_optional_scans_are_not_scheduled(self):
        for name in ("stress.yml", "mutation.yml", "cockpit-sdk-head.yml"):
            self.assertNotIn("  schedule:", (WORKFLOWS / name).read_text())
        self.assertNotIn("  repository_dispatch:", (WORKFLOWS / "cockpit-sdk-head.yml").read_text())

    def test_pr_janitor_reclaims_concurrency_and_cache_budget(self):
        """A closed PR holds queue slots and cache bytes until something frees them.

        The free-runner cutover retired cancellation because runner minutes had
        stopped costing money. Concurrency and the 10 GB cache cap are still
        finite and still shared, so the janitor reclaims both on `closed`.
        """
        janitor = (WORKFLOWS / "pr-janitor.yml").read_text()
        self.assertIn("  pull_request:\n    types: [closed]", janitor)
        self.assertIn("actions: write", janitor)
        self.assertIn("/actions/runs/${id}/cancel", janitor)
        self.assertIn("/actions/caches/${id}", janitor)
        # Caches go after the cancel, or a still-live run repopulates the scope.
        self.assertIn("needs: cancel", janitor)

    def test_pull_request_lanes_never_write_to_the_shared_cache_budget(self):
        """Actions cache is 10 GB per repository, LRU across every ref.

        A `refs/pull/N/merge` entry is restorable only from that PR, so any PR
        write is unreachable bytes that evict the warm `main` entries all lanes
        restore from. The mbx action saves only from pushes to the default
        branch unless an input opts other events in, so the lane must name
        none of those inputs.
        """
        lane = (ROOT / ".github/actions/setup-rust-lane/action.yml").read_text()
        self.assertIn("uses: jdx/mr-boxington-action@", lane)
        for opt_in in ("save-on-pull-request", "save-on-workflow-dispatch",
                       "save-on-protected-branch"):
            self.assertNotIn(opt_in, lane)
        for workflow in ("ci.yml", "stress.yml"):
            body = (WORKFLOWS / workflow).read_text()
            self.assertNotIn("mr-boxington-action", body)

    def test_main_prunes_superseded_build_cache_entries(self):
        """Every main push saves a fresh multi-GB mbx entry per lane.

        Restores take only the newest, so ci.yml deletes the older ones after
        a main run instead of letting them evict other workflows' caches.
        """
        ci = (WORKFLOWS / "ci.yml").read_text()
        prune = ci.split("\n  prune-build-cache:\n", 1)[1]
        self.assertIn("github.event_name == 'push' && github.ref == 'refs/heads/main'", prune)
        self.assertIn("needs: [check, test]", prune)
        self.assertIn("actions: write", prune)
        self.assertIn("key=linux-arm64-mbx-phux-${lane}-", prune)
        self.assertIn("tail -n +2", prune)

    def test_export_cleanup_preserves_pending_cache_and_cargo_tools(self):
        """Download cleanup must not evict the current build's export closure."""
        script = ROOT / "scripts/ci/mbx-export-headroom.sh"
        self._assert_headroom(script)
        refused = subprocess.run(
            ["bash", str(script)],
            env={"PATH": "/usr/bin:/bin", "PHUX_MBX_EXPORT_HEADROOM": "1", "HOME": ""},
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("refusing", refused.stderr)

    def _assert_headroom(self, script: Path) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            home = root / "home"
            cargo_registry = home / ".cargo" / "registry" / "src"
            cargo_git = home / ".cargo" / "git" / "db"
            cargo_bin = home / ".cargo" / "bin"
            keep = home / "keep-store"
            for path in (cargo_registry, cargo_git, cargo_bin, keep):
                path.mkdir(parents=True)
                (path / "sentinel").write_text("keep")
            bindir = root / "bin"
            bindir.mkdir()
            # GC after a build invalidates its pending export group. Refuse any
            # mbx call so the filesystem cleanup cannot silently reintroduce it.
            (bindir / "mbx").write_text("#!/bin/sh\nexit 1\n")
            os.chmod(bindir / "mbx", 0o755)
            env = os.environ.copy()
            env["PATH"] = f"{bindir}{os.pathsep}{env.get('PATH', '')}"
            env["HOME"] = str(home)
            env["PHUX_MBX_EXPORT_HEADROOM"] = "1"
            env.pop("GITHUB_ACTIONS", None)
            env.pop("CARGO_HOME", None)
            completed = subprocess.run(
                ["bash", str(script)],
                env=env,
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertFalse((cargo_registry / "sentinel").exists())
            self.assertFalse((cargo_git / "sentinel").exists())
            self.assertTrue((cargo_bin / "sentinel").exists())
            self.assertEqual((keep / "sentinel").read_text(), "keep")

            skipped = subprocess.run(
                ["bash", str(script)],
                env={**env, "PHUX_MBX_EXPORT_HEADROOM": "0", "PATH": "/usr/bin:/bin"},
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(skipped.returncode, 0, skipped.stderr)
            self.assertIn("skipped", skipped.stdout)

    def test_linux_release_keeps_glibc_2204_userspace_without_retired_runners(self):
        for name, body in (("release.yml", RELEASE), ("next-release.yml", NEXT_RELEASE)):
            with self.subTest(workflow=name):
                self.assertNotRegex(body, RETIRED_UBUNTU_2204)
                self.assertRegex(
                    body,
                    r"os: ubuntu-24\.04\s+"
                    r"""container: '\{"image":"ubuntu:22\.04"\}'\s+"""
                    r"target: x86_64-unknown-linux-gnu",
                )
                self.assertRegex(
                    body,
                    r"os: ubuntu-24\.04-arm\s+"
                    r"""container: '\{"image":"ubuntu:22\.04"\}'\s+"""
                    r"target: aarch64-unknown-linux-gnu",
                )
                self.assertIn("container: ${{ fromJSON(matrix.container) }}", body)
                self.assertIn("scripts/ci/setup-linux-release-userspace.sh", body)
                self.assertIn(
                    "scripts/check-binary-portability.sh target/release/phux target/release/phux-mcp",
                    body,
                )
                self.assertIn("name: ${{ matrix.target }}", body)
                self.assertIn("test \"$(getconf GNU_LIBC_VERSION)\" = 'glibc 2.35'", body)
        self.assertIn("needs.build.result == 'success'", RELEASE)

    def test_linux_release_container_trusts_the_host_mounted_workspace(self):
        command = 'git config --global --add safe.directory "$GITHUB_WORKSPACE"'
        self.assertRegex(LINUX_RELEASE_SETUP, rf"(?m)^\s+{re.escape(command)}$")
        self.assertLess(
            LINUX_RELEASE_SETUP.index("apt-get install"),
            LINUX_RELEASE_SETUP.index(command),
        )

        build_job = RELEASE.split("\n  build:\n", 1)[1].split("\n  release:\n", 1)[0]
        self.assertLess(
            build_job.index("scripts/ci/setup-linux-release-userspace.sh"),
            build_job.index('git fetch --force --tags origin'),
        )

    def test_release_platform_guard_rejects_wrong_arch_and_newer_glibc(self):
        block = RELEASE.split("- name: Verify native release platform and Linux baseline", 1)[1]
        script = textwrap.dedent(block.split("run: |\n", 1)[1].split("\n      - ", 1)[0])
        cases = [
            ("aarch64-apple-darwin", "Darwin", "arm64", "22.04", "2.35", True),
            ("aarch64-unknown-linux-gnu", "Linux", "aarch64", "22.04", "2.35", True),
            ("x86_64-unknown-linux-gnu", "Linux", "x86_64", "22.04", "2.35", True),
            ("x86_64-unknown-linux-gnu", "Linux", "aarch64", "22.04", "2.35", False),
            ("aarch64-unknown-linux-gnu", "Darwin", "arm64", "22.04", "2.35", False),
            ("aarch64-unknown-linux-gnu", "Linux", "aarch64", "24.04", "2.39", False),
            ("aarch64-unknown-linux-gnu", "Linux", "aarch64", "22.04", "2.39", False),
        ]
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "uname").write_text('#!/bin/sh\ncase "$1" in -s) echo "$TEST_OS";; -m) echo "$TEST_ARCH";; esac\n')
            (root / "getconf").write_text('#!/bin/sh\necho "glibc $TEST_GLIBC"\n')
            (root / "uname").chmod(0o755)
            (root / "getconf").chmod(0o755)
            script = script.replace("/etc/os-release", f"{tmp}/os-release")
            for target, system, arch, version, glibc, success in cases:
                with self.subTest(target=target, system=system, arch=arch, version=version, glibc=glibc):
                    (root / "os-release").write_text(f'ID=ubuntu\nVERSION_ID="{version}"\n')
                    env = {**os.environ, "PATH": f"{tmp}:{os.environ['PATH']}", "TARGET": target,
                           "TEST_OS": system, "TEST_ARCH": arch, "TEST_GLIBC": glibc}
                    result = subprocess.run(["bash", "-euo", "pipefail", "-c", script], env=env,
                                            capture_output=True, text=True, check=False)
                    self.assertEqual(result.returncode == 0, success, result.stderr)


if __name__ == "__main__":
    unittest.main()
