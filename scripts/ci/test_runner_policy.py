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
STANDARD = {"ubuntu-latest", "ubuntu-24.04", "ubuntu-24.04-arm", "ubuntu-22.04", "ubuntu-22.04-arm", "macos-26"}


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
        restore from. sccache is the sharp edge: its GHA backend stores one
        entry per compilation object, thousands per lane.
        """
        lane = (ROOT / ".github/actions/setup-rust-lane/action.yml").read_text()
        self.assertIn(
            "SCCACHE_RW_MODE: ${{ github.ref == 'refs/heads/main' "
            "&& 'READ_WRITE' || 'READ_ONLY' }}",
            lane,
        )
        self.assertIn('echo "SCCACHE_GHA_RW_MODE=${SCCACHE_RW_MODE}"', lane)
        self.assertIn("save-if: ${{ github.ref == 'refs/heads/main' }}", lane)

    def test_linux_arm_release_has_a_native_2204_userspace(self):
        self.assertRegex(RELEASE, r"os: ubuntu-22\.04-arm\s+target: aarch64-unknown-linux-gnu")
        self.assertRegex(RELEASE, r"os: ubuntu-22\.04\s+target: x86_64-unknown-linux-gnu")
        self.assertIn("scripts/check-binary-portability.sh target/release/phux target/release/phux-mcp", RELEASE)
        self.assertIn("name: ${{ matrix.target }}", RELEASE)
        self.assertIn("needs.build.result == 'success'", RELEASE)

    def test_release_platform_guard_rejects_wrong_arch_and_newer_glibc(self):
        block = RELEASE.split("- name: Verify native release platform and Linux baseline", 1)[1]
        script = textwrap.dedent(block.split("run: |\n", 1)[1].split("\n      - uses:", 1)[0])
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
