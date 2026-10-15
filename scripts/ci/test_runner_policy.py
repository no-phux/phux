#!/usr/bin/env python3
"""Future workflows use free public compute and preserve native release targets."""

import ctypes
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
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

# Darwin <sys/attr.h>. Extended common attributes are requested in forkattr
# and returned only when FSOPT_ATTR_CMN_EXTENDED is set. The buffer is a
# packed (u_int32 length, u_int64 clone id): 64-bit fields stay 4-byte aligned.
_DARWIN_ATTR_BIT_MAP_COUNT = 5
_DARWIN_FSOPT_ATTR_CMN_EXTENDED = 0x00000020
_DARWIN_ATTR_CMNEXT_CLONEID = 0x00000100


class _DarwinAttrList(ctypes.Structure):
    _fields_ = [
        ("bitmapcount", ctypes.c_ushort),
        ("reserved", ctypes.c_uint16),
        ("commonattr", ctypes.c_uint32),
        ("volattr", ctypes.c_uint32),
        ("dirattr", ctypes.c_uint32),
        ("fileattr", ctypes.c_uint32),
        ("forkattr", ctypes.c_uint32),
    ]


class _DarwinCloneId(ctypes.Structure):
    _pack_ = 4
    _fields_ = [
        ("length", ctypes.c_uint32),
        ("clone_id", ctypes.c_uint64),
    ]


def _apfs_clone_id(path: Path) -> int | None:
    """Return the APFS data-stream id, or None when this kernel has none.

    Zero is not a shared stream. ``getattrlist`` is Darwin-only; every other
    platform returns None and the caller has to use inode identity.
    """
    getattrlist = None
    for library in (None, "/usr/lib/libSystem.B.dylib"):
        if library is not None and sys.platform != "darwin":
            continue
        try:
            loaded = ctypes.CDLL(library, use_errno=True)
            getattrlist = loaded.getattrlist
            break
        except (AttributeError, OSError):
            continue
    if getattrlist is None:
        return None
    query = _DarwinAttrList(
        bitmapcount=_DARWIN_ATTR_BIT_MAP_COUNT,
        forkattr=_DARWIN_ATTR_CMNEXT_CLONEID,
    )
    found = _DarwinCloneId()
    getattrlist.argtypes = [
        ctypes.c_char_p,
        ctypes.POINTER(_DarwinAttrList),
        ctypes.POINTER(_DarwinCloneId),
        ctypes.c_size_t,
        ctypes.c_ulong,
    ]
    getattrlist.restype = ctypes.c_int
    if getattrlist(
        os.fsencode(path),
        ctypes.byref(query),
        ctypes.byref(found),
        ctypes.sizeof(found),
        _DARWIN_FSOPT_ATTR_CMN_EXTENDED,
    ) != 0:
        return None
    if found.length < ctypes.sizeof(found) or found.clone_id == 0:
        return None
    return int(found.clone_id)


def _shares_stored_bytes(
    left: os.stat_result,
    right: os.stat_result,
    left_clone: int | None,
    right_clone: int | None,
) -> bool:
    """True when the two files are one allocated copy.

    ``hardlink(1)`` on ext4 keeps a single inode. APFS ``clonefile`` keeps
    distinct inodes on one data stream, and each clone's ``st_blocks`` still
    reports the full size, so a shared inode is not a portable proof that the
    duplicate stopped costing disk. A missing or zero clone id is not
    evidence.
    """
    if left.st_dev != right.st_dev:
        return False
    if left.st_ino == right.st_ino:
        return True
    return bool(left_clone) and left_clone == right_clone


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
        for name in ("stress.yml", "mutation.yml"):
            self.assertNotIn("  schedule:", (WORKFLOWS / name).read_text())

    def test_mutation_workflow_keeps_rust_without_archived_zig_pilot(self):
        mutation = (WORKFLOWS / "mutation.yml").read_text()
        self.assertIn("run: bash scripts/mutation/rust.sh", mutation)
        self.assertNotIn("scripts/mutation/zig.sh", mutation)
        self.assertNotIn("test_zig.py", mutation)
        self.assertNotIn("inputs.language", mutation)

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
        # The dependency-only cache of the test lanes follows the same rule.
        self.assertIn("uses: Swatinem/rust-cache@", lane)
        self.assertIn(
            "save-if: ${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}",
            lane,
        )

    def test_main_prunes_superseded_build_cache_entries(self):
        """Keep the newest large archive. Delete sccache's per-object shards.

        Restores take only the newest entry for a prefix. sccache's GitHub
        backend writes one small object per compilation; those stay LRU-hot
        and evict the test-lane dependency archives.
        The xcframework dep archive did the same at ~1 GB a copy.
        """
        ci = (WORKFLOWS / "ci.yml").read_text()
        prune = ci.split("\n  prune-build-cache:\n", 1)[1]
        self.assertIn("github.event_name == 'push' && github.ref == 'refs/heads/main'", prune)
        self.assertIn("needs: [check, test, test-integration, e2e, uniffi, perf-icount]", prune)
        self.assertIn("actions: write", prune)
        self.assertIn("keep_newest check linux-arm64-mbx-phux-check-", prune)
        self.assertIn("drop_all test-mbx linux-arm64-mbx-phux-test-", prune)
        for lane in ("test", "test-integration", "e2e", "uniffi"):
            self.assertIn(f"v0-rust-phux-{lane}-Linux-", prune)
        self.assertIn("keep_newest icount linux-x64-mbx-phux-icount-", prune)
        self.assertIn("drop_all sccache sccache", prune)
        self.assertIn("drop_all xcframework-deps v0-rust-mini-v1-ffi-xcframework-", prune)
        self.assertIn("keep_newest desktop-release v0-rust-mini-v1-desktop-release-", prune)
        self.assertIn("keep_newest xcframework-zig mini-v1-ffi-xcframework-zig-", prune)
        self.assertIn("tail -n +2", prune)
        ffi = (WORKFLOWS / "ffi-xcframework.yml").read_text()
        self.assertNotIn("shared-key: mini-v1-ffi-xcframework", ffi)
        self.assertIn("mini-v1-ffi-xcframework-zig-", ffi)

    def test_rust_lane_cache_keeps_action_results_and_skips_repeat_exports(self):
        """The test lane's 8 GiB collection deleted the store it had just restored.

        Run 38442503688 restored ~19 GiB logical, evicted every action result,
        compiled with 0 hits, then spent ~11 minutes exporting that cold
        rebuild because the generated key included the commit. 48 GiB kept
        those hits and then outgrew the export copy (run 38505694112).
        """
        lane = (ROOT / ".github/actions/setup-rust-lane/action.yml").read_text()
        self.assertNotIn("mbx gc --max-size 8GiB", lane)
        self.assertNotIn("mbx gc --max-size 48GiB", lane)
        self.assertIn("mbx gc --max-size 40GiB", lane)
        self.assertIn("cache-key: ${{ steps.mbx-env.outputs.cache_key }}", lane)
        self.assertIn("restore-keys: ${{ steps.mbx-env.outputs.restore_key }}", lane)
        self.assertNotIn("github.sha", lane)
        self.assertIn("date -u +%Y%m%d", lane)
        self.assertIn("linux-arm64", lane)
        self.assertIn("linux-x64", lane)

    def test_test_lanes_cache_dependencies_and_split_the_build(self):
        """The test profile's restore and compile were the required check's long pole.

        Run 38503810705 spent 6m 28s unpacking a 6.1 GB mbx archive (27.8 GiB
        on disk) and then 7m 51s compiling the workspace crates anyway. The
        test lanes restore only third-party artifacts and split the workspace
        compile across four runners, each keeping --workspace.
        """
        lane = (ROOT / ".github/actions/setup-rust-lane/action.yml").read_text()
        self.assertIn("default: mbx", lane)
        self.assertIn("CARGO_INCREMENTAL=0", lane)
        self.assertIn("CARGO_PROFILE_DEV_DEBUG=0", lane)
        self.assertIn("shared-key: phux-${{ inputs.cache-lane }}", lane)
        self.assertIn('cache-on-failure: "true"', lane)
        ci = (WORKFLOWS / "ci.yml").read_text()
        self.assertEqual(ci.count("          build-cache: deps\n"), 4)
        self.assertIn("PHUX_UNIT_PART: lib", ci)
        self.assertIn("PHUX_UNIT_PART: integration", ci)
        self.assertIn(
            "needs: [changes, check, test, test-integration, e2e, uniffi, "
            "workflow-gate, integrations, perf-icount]",
            ci,
        )
        recipe = (ROOT / "just/test.just").read_text()
        self.assertIn("lib) targets=(--lib --bins) ;;", recipe)
        self.assertIn("integration) targets=(--test '*' --examples) ;;", recipe)
        self.assertIn("--workspace ${targets[@]+", recipe)

    def test_failed_main_lane_saves_the_objects_archive(self):
        """A red assertion must not throw away a finished compile.

        Run 38457963019 spent 10m 37s compiling and saved nothing because
        the action exports only from its success post step. The save stays
        on pushes to main, and only when this restore missed.
        """
        ci = (WORKFLOWS / "ci.yml").read_text()
        save_if = (
            "if: failure() && github.event_name == 'push' && "
            "github.ref == 'refs/heads/main' && steps.setup.outputs.cache-hit != 'true' "
            "&& steps.mbx-fail-export.outputs.exported == 'true'"
        )
        # check and icount; the test lanes' rust-cache saves on failure itself.
        self.assertEqual(ci.count(save_if), 2)
        self.assertEqual(ci.count("bash scripts/ci/mbx-export-failed-lane.sh"), 2)
        self.assertNotIn("save-on-pull-request", ci)
        lane = (ROOT / ".github/actions/setup-rust-lane/action.yml").read_text()
        self.assertIn("value: ${{ steps.mbx-env.outputs.cache_key }}", lane)

        script = ROOT / "scripts/ci/mbx-export-failed-lane.sh"
        with tempfile.TemporaryDirectory() as scratch:
            output = Path(scratch) / "github-output"
            refused = subprocess.run(
                ["bash", str(script), str(Path(scratch) / "elsewhere")],
                check=False, capture_output=True, text=True,
            )
            self.assertEqual(refused.returncode, 2)
            unset = subprocess.run(
                ["bash", str(script), str(Path(scratch) / "phux-mbx-objects")],
                check=False, capture_output=True, text=True,
                env={"PATH": "/usr/bin:/bin", "GITHUB_OUTPUT": str(output)},
            )
            self.assertEqual(unset.returncode, 0)
            self.assertNotIn("exported=true", output.read_text() if output.exists() else "")

            bindir = Path(scratch) / "bin"
            bindir.mkdir()
            fake = bindir / "mbx"
            # mbx cache export --group GROUP --format directory DEST
            fake.write_text("#!/bin/sh\nmkdir -p \"$7\"\nexit 0\n")
            fake.chmod(0o755)
            exported = subprocess.run(
                ["bash", str(script), str(Path(scratch) / "phux-mbx-objects")],
                check=False, capture_output=True, text=True,
                env={
                    "PATH": f"{bindir}:/usr/bin:/bin",
                    "MBX_CACHE_EXPORT_GROUP": "group-1",
                    "GITHUB_OUTPUT": str(output),
                },
            )
            self.assertEqual(exported.returncode, 0, exported.stderr)
            self.assertIn("exported=true", output.read_text())

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

    def test_shared_storage_signal_accepts_hardlinks_and_clone_ids(self):
        """Inode identity is the ext4 proof; a shared clone id is the APFS proof."""
        same = os.stat_result((0o644, 1, 7, 2, 0, 0, 11, 0, 0, 0))
        other = os.stat_result((0o644, 2, 7, 1, 0, 0, 11, 0, 0, 0))
        other_device = os.stat_result((0o644, 1, 8, 1, 0, 0, 11, 0, 0, 0))
        self.assertEqual(ctypes.sizeof(_DarwinCloneId), 12)
        self.assertEqual(_DarwinCloneId.clone_id.offset, 4)
        self.assertTrue(_shares_stored_bytes(same, same, None, None))
        self.assertFalse(_shares_stored_bytes(same, other, None, None))
        self.assertFalse(_shares_stored_bytes(same, other, 0, 0))
        self.assertFalse(_shares_stored_bytes(same, other, None, 9))
        self.assertFalse(_shares_stored_bytes(same, other, 4, 9))
        self.assertFalse(_shares_stored_bytes(same, other_device, 9, 9))
        self.assertTrue(_shares_stored_bytes(same, other, 9, 9))

    def test_export_headroom_collapses_target_copies_without_calling_mbx(self):
        """A duplicate target must stop occupying its own copy of the store.

        ext4 hardlink(1) does that by sharing an inode with the object-store
        blob. APFS clonefile keeps a distinct inode for the same data stream,
        so st_blocks still looks like a full copy and inode equality is not
        the portable signal. Two target files whose mode or mtime disagree
        still cannot share an inode: the collapse puts that metadata back.
        """
        script = ROOT / "scripts/ci/mbx-export-headroom.sh"
        payload = b"same-object" * 1000
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            home = root / "home"
            target = home / ".cache" / "mbx" / "targets" / "view"
            cas = home / ".cache" / "mbx" / "actions" / "cas" / "v1" / "blake3" / "ab"
            target.mkdir(parents=True)
            cas.mkdir(parents=True)
            first = target / "libfirst.rlib"
            second = target / "libsecond.rlib"
            blob = cas / "blob"
            unique = target / "only-here"
            for path in (first, second, blob):
                path.write_bytes(payload)
            unique.write_bytes(b"not-in-the-store")
            os.chmod(first, 0o755)
            os.chmod(second, 0o640)
            os.chmod(blob, 0o444)
            os.utime(first, (1_000, 1_000))
            os.utime(second, (2_000, 2_000))
            os.utime(blob, (3_000, 3_000))
            bindir = root / "bin"
            bindir.mkdir()
            mbx = bindir / "mbx"
            mbx.write_text("#!/bin/sh\necho mbx-was-called >&2\nexit 1\n")
            mbx.chmod(0o755)
            completed = subprocess.run(
                ["bash", str(script)],
                check=False,
                capture_output=True,
                text=True,
                env={
                    "PATH": f"{bindir}{os.pathsep}{os.environ.get('PATH', '')}",
                    "HOME": str(home),
                    "PHUX_MBX_EXPORT_HEADROOM": "1",
                },
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertNotIn("mbx-was-called", completed.stderr)
            first_stat, second_stat = first.stat(), second.stat()
            blob_stat, unique_stat = blob.stat(), unique.stat()
            first_clone, second_clone = _apfs_clone_id(first), _apfs_clone_id(second)
            blob_clone, unique_clone = _apfs_clone_id(blob), _apfs_clone_id(unique)
            self.assertNotEqual(first_stat.st_ino, second_stat.st_ino)
            collapsed = (
                _shares_stored_bytes(first_stat, blob_stat, first_clone, blob_clone)
                or _shares_stored_bytes(second_stat, blob_stat, second_clone, blob_clone)
            )
            self.assertTrue(
                collapsed,
                "target copies still occupy their own storage "
                f"(inodes {first_stat.st_ino}, {second_stat.st_ino}, "
                f"blob {blob_stat.st_ino}; "
                f"clone ids {first_clone}, {second_clone}, {blob_clone})",
            )
            self.assertFalse(
                _shares_stored_bytes(unique_stat, blob_stat, unique_clone, blob_clone)
            )
            self.assertEqual(first.read_bytes(), payload)
            self.assertEqual(second.read_bytes(), payload)
            self.assertEqual(blob.read_bytes(), payload)
            self.assertEqual(stat.S_IMODE(first.stat().st_mode), 0o755)
            self.assertEqual(int(first.stat().st_mtime), 1_000)
            self.assertEqual(stat.S_IMODE(second.stat().st_mode), 0o640)
            self.assertEqual(int(second.stat().st_mtime), 2_000)
            self.assertEqual(unique.read_bytes(), b"not-in-the-store")
            self.assertIn("collapsed duplicate target files", completed.stdout)

    def test_export_headroom_skips_the_save_when_the_copy_cannot_fit(self):
        """A store larger than the free disk must not fail the green job.

        Retiring the pending receipts makes the post step report an empty
        export. The object bytes stay, and so does a file outside the group.
        """
        script = ROOT / "scripts/ci/mbx-export-headroom.sh"
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            home = root / "home"
            store = home / ".cache" / "mbx" / "actions"
            receipt = store / "build-receipts" / "v1" / "groups" / "abc" / "1.json"
            kept = store / "cas" / "v1" / "blake3" / "ab" / "blob"
            receipt.parent.mkdir(parents=True)
            kept.parent.mkdir(parents=True)
            receipt.write_text('{"group":"keep-until-the-disk-says-no"}')
            kept.write_text("object")
            bindir = root / "bin"
            bindir.mkdir()
            (bindir / "df").write_text(
                "#!/bin/sh\n"
                "if [ \"$1\" = \"-h\" ]; then\n"
                "  echo 'Filesystem Size Used Avail Use% Mounted on'\n"
                "  echo '/dev/root 145G 145G 1M 100% /'\n"
                "  exit 0\n"
                "fi\n"
                "echo 'Filesystem 1024-blocks Used Available Capacity Mounted on'\n"
                "echo '/dev/root 100000000 99999000 1000 100% /'\n"
            )
            (bindir / "du").write_text("#!/bin/sh\necho '8000000\t'$2\n")
            for name in ("df", "du"):
                (bindir / name).chmod(0o755)
            completed = subprocess.run(
                ["bash", str(script)],
                check=False,
                capture_output=True,
                text=True,
                env={
                    "PATH": f"{bindir}{os.pathsep}/usr/bin:/bin",
                    "HOME": str(home),
                    "PHUX_MBX_EXPORT_HEADROOM": "1",
                },
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertIn("not saving this store", completed.stderr)
            self.assertFalse(receipt.exists())
            self.assertEqual(kept.read_text(), "object")

            receipt.parent.mkdir(parents=True, exist_ok=True)
            receipt.write_text("still-pending")
            kept_run = subprocess.run(
                ["bash", str(script)],
                check=False,
                capture_output=True,
                text=True,
                env={
                    "PATH": "/usr/bin:/bin",
                    "HOME": str(home),
                    "PHUX_MBX_EXPORT_HEADROOM": "1",
                },
            )
            self.assertEqual(kept_run.returncode, 0, kept_run.stderr)
            self.assertNotIn("not saving this store", kept_run.stderr)
            self.assertEqual(receipt.read_text(), "still-pending")

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
