#!/usr/bin/env python3
"""Future workflows use free public compute and preserve native release targets."""

import ctypes
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".github/workflows"
STANDARD = {"ubuntu-latest", "ubuntu-24.04", "ubuntu-24.04-arm", "xcode-27"}

# Darwin <sys/attr.h>. Extended common attributes live in forkattr and are
# returned only when FSOPT_ATTR_CMN_EXTENDED is set. Asking for
# ATTR_CMN_RETURNED_ATTRS plus FSOPT_PACK_INVAL_ATTRS puts the clone id at a
# fixed offset: u32 length, then the 20-byte returned attribute set, then the
# u64 id. Reading eight bytes at offset 4 instead returns a per-file value on
# Darwin 27, so identical clones look independent.
_DARWIN_ATTR_BIT_MAP_COUNT = 5
_DARWIN_ATTR_CMN_RETURNED_ATTRS = 0x80000000
_DARWIN_FSOPT_NOFOLLOW = 0x00000001
_DARWIN_FSOPT_PACK_INVAL_ATTRS = 0x00000008
_DARWIN_FSOPT_ATTR_CMN_EXTENDED = 0x00000020
_DARWIN_ATTR_CMNEXT_CLONEID = 0x00000100
_DARWIN_CLONE_ID_OFFSET = 24


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


def _clone_id_from_buffer(raw: bytes) -> int | None:
    """Parse one clone id from a returned-attribute ``getattrlist`` buffer.

    ``None`` when the buffer is short, the filesystem did not return
    ``ATTR_CMNEXT_CLONEID``, or the id is zero. An id parked at offset 4,
    which is what a length-plus-u64 reading used to accept, is not this layout.
    """
    if len(raw) < _DARWIN_CLONE_ID_OFFSET + 8:
        return None
    total = int.from_bytes(raw[0:4], sys.byteorder)
    if total < _DARWIN_CLONE_ID_OFFSET + 8:
        return None
    returned_fork = int.from_bytes(raw[20:24], sys.byteorder)
    if returned_fork & _DARWIN_ATTR_CMNEXT_CLONEID == 0:
        return None
    clone_id = int.from_bytes(raw[_DARWIN_CLONE_ID_OFFSET:_DARWIN_CLONE_ID_OFFSET + 8], sys.byteorder)
    if clone_id == 0:
        return None
    return clone_id


def _apfs_clone_id(path: Path) -> int | None:
    """Return the APFS data-stream id, or None when this kernel has none.

    Zero is not a shared stream. ``getattrlist`` is Darwin-only; every other
    platform returns None and the caller has to use inode identity.
    """
    if sys.platform != "darwin":
        return None
    try:
        library = ctypes.CDLL("/usr/lib/libSystem.B.dylib", use_errno=True)
        getattrlist = library.getattrlist
    except (AttributeError, OSError):
        return None
    query = _DarwinAttrList(
        bitmapcount=_DARWIN_ATTR_BIT_MAP_COUNT,
        commonattr=_DARWIN_ATTR_CMN_RETURNED_ATTRS,
        forkattr=_DARWIN_ATTR_CMNEXT_CLONEID,
    )
    found = ctypes.create_string_buffer(64)
    getattrlist.argtypes = [
        ctypes.c_char_p,
        ctypes.POINTER(_DarwinAttrList),
        ctypes.c_void_p,
        ctypes.c_size_t,
        ctypes.c_ulong,
    ]
    getattrlist.restype = ctypes.c_int
    options = (
        _DARWIN_FSOPT_NOFOLLOW
        | _DARWIN_FSOPT_PACK_INVAL_ATTRS
        | _DARWIN_FSOPT_ATTR_CMN_EXTENDED
    )
    if getattrlist(
        os.fsencode(path),
        ctypes.byref(query),
        ctypes.cast(found, ctypes.c_void_p),
        ctypes.sizeof(found),
        options,
    ) != 0:
        return None
    return _clone_id_from_buffer(found.raw)


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
        self.assertIn("needs: [check, test-build, uniffi, perf-icount]", prune)
        self.assertIn("actions: write", prune)
        self.assertIn("keep_newest check linux-arm64-mbx-phux-check-", prune)
        self.assertIn("drop_all test-mbx linux-arm64-mbx-phux-test-", prune)
        for lane in ("test", "uniffi"):
            self.assertIn(f"keep_newest {lane}-deps v0-rust-phux-{lane}-Linux-", prune)
        # The run jobs restore no cache; their former entries go.
        for lane in ("test-integration", "e2e"):
            self.assertIn(f"drop_all {lane}-deps v0-rust-phux-{lane}-Linux-", prune)
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

    def test_test_lanes_compile_once_and_run_from_the_archive(self):
        """The test profile compiles once; the run jobs only execute.

        Run 38503810705 spent 6m 28s unpacking a 6.1 GB mbx archive (27.8 GiB
        on disk), so the test build restores only third-party artifacts. Run
        38982580303 then compiled the workspace crates three times in
        parallel lanes (4m 52s, 3m 38s, 2m 21s); one archive build serves the
        unit halves and e2e, which restore no build cache.
        """
        lane = (ROOT / ".github/actions/setup-rust-lane/action.yml").read_text()
        self.assertIn("default: mbx", lane)
        self.assertIn("CARGO_INCREMENTAL=0", lane)
        self.assertIn("CARGO_PROFILE_DEV_DEBUG=0", lane)
        self.assertIn("shared-key: phux-${{ inputs.cache-lane }}", lane)
        self.assertIn('cache-on-failure: "true"', lane)
        ci = (WORKFLOWS / "ci.yml").read_text()
        # The test build and uniffi compile; the three run jobs do not.
        self.assertEqual(ci.count("          build-cache: deps\n"), 2)
        self.assertEqual(ci.count("          build-cache: none\n"), 3)
        build = ci.split("\n  test-build:\n", 1)[1].split("\n  test:\n", 1)[0]
        self.assertIn("cache-lane: test\n", build)
        self.assertIn("just test-archive", build)
        self.assertIn("uses: actions/upload-artifact@", build)
        self.assertIn("compression-level: 0", build)
        self.assertEqual(ci.count("needs: [changes, test-build]"), 3)
        self.assertEqual(ci.count("uses: actions/download-artifact@"), 3)
        self.assertEqual(ci.count("PHUX_TEST_ARCHIVE: ${{ runner.temp }}/phux-tests.tar.zst"), 4)
        self.assertIn("PHUX_UNIT_PART: lib", ci)
        self.assertIn("PHUX_UNIT_PART: integration", ci)
        self.assertIn(
            "needs: [changes, check, test-build, test, test-integration, e2e, uniffi, "
            "workflow-gate, integrations, perf-icount]",
            ci,
        )
        recipe = (ROOT / "just/test.just").read_text()
        self.assertIn('lib) targets=(--lib --bins); kind="kind(lib) + kind(bin) + kind(proc-macro)" ;;', recipe)
        self.assertIn('select(((((."required-features" // []) - $default) == []) | tostring) == $want)', recipe)
        self.assertIn("source=(--workspace {{TEST_FEATURES}} ${targets[@]+", recipe)
        self.assertIn("targets=(--lib --bins --tests)", recipe)
        self.assertIn("cargo nextest archive --workspace {{TEST_FEATURES}}", recipe)
        # One feature set: the unit pool, e2e, and stress share a build.
        self.assertIn("source=(--workspace {{TEST_FEATURES}} {{E2E_TARGETS}})", recipe)
        self.assertIn("{{TEST_FEATURES}} --features phux-server/stress", recipe)
        self.assertIn('TEST_FEATURES := "--features phux-server/measure"', recipe)
        self.assertNotIn("E2E_FEATURES", recipe)

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
        shared = bytearray(64)
        shared[0:4] = (32).to_bytes(4, sys.byteorder)
        shared[20:24] = _DARWIN_ATTR_CMNEXT_CLONEID.to_bytes(4, sys.byteorder)
        shared[_DARWIN_CLONE_ID_OFFSET:_DARWIN_CLONE_ID_OFFSET + 8] = (9).to_bytes(8, sys.byteorder)
        self.assertEqual(_clone_id_from_buffer(shared), 9)
        # Issue #1339 read a packed (length, u64) at offset 4. That window is
        # the returned attribute set, not the stream id, so a short buffer of
        # that shape must not count as a clone id.
        misplaced = (12).to_bytes(4, sys.byteorder) + (9).to_bytes(8, sys.byteorder)
        self.assertIsNone(_clone_id_from_buffer(misplaced))
        shared[20:24] = (0).to_bytes(4, sys.byteorder)
        self.assertIsNone(_clone_id_from_buffer(shared))
        shared[20:24] = _DARWIN_ATTR_CMNEXT_CLONEID.to_bytes(4, sys.byteorder)
        shared[_DARWIN_CLONE_ID_OFFSET:_DARWIN_CLONE_ID_OFFSET + 8] = (0).to_bytes(8, sys.byteorder)
        self.assertIsNone(_clone_id_from_buffer(shared))
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
        self._assert_target_copies_collapsed(hide_hardlink=False)

    def test_export_headroom_collapses_target_copies_without_hardlink_binary(self):
        """Darwin's util-linux hardlink is keg-only and often not on PATH.

        The copies still have to share storage. ``os.link`` is what does that
        when the binary is missing; skipping the assertion is not.
        """
        self._assert_target_copies_collapsed(hide_hardlink=True)

    def _assert_target_copies_collapsed(self, *, hide_hardlink: bool) -> None:
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
            if hide_hardlink:
                for tool in ("bash", "python3", "df", "du", "rm", "awk", "dirname"):
                    source = shutil.which(tool)
                    self.assertIsNotNone(source, tool)
                    os.symlink(source, bindir / tool)
                path = str(bindir)
            else:
                path = f"{bindir}{os.pathsep}{os.environ.get('PATH', '')}"
            completed = subprocess.run(
                ["bash", str(script)],
                check=False,
                capture_output=True,
                text=True,
                env={
                    "PATH": path,
                    "HOME": str(home),
                    "PHUX_MBX_EXPORT_HEADROOM": "1",
                },
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertNotIn("mbx-was-called", completed.stderr)
            if hide_hardlink:
                self.assertIn("hardlink is not installed", completed.stderr)
                self.assertNotIn("leaving target copies", completed.stderr)
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
                f"nlink {first_stat.st_nlink}, {second_stat.st_nlink}, "
                f"{blob_stat.st_nlink}; "
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


if __name__ == "__main__":
    unittest.main()
