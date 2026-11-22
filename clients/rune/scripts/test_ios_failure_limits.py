"""Forensic-only byte limits: never read a large sparse source unboundedly."""
import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

from test_ios_failure_evidence import FailureEvidenceFixture, failure, qualify

LIMIT = 16 * 1024 * 1024
SIZE_ERROR = "forensic source exceeds 16 MiB limit"


class ReadProbe:
    def __init__(self, handle, reads, before_read):
        self.handle, self.reads, self.before_read = handle, reads, before_read

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.handle.close()

    def fileno(self):
        return self.handle.fileno()

    def read(self, size=-1):
        self.reads.append(size)
        if not 0 < size <= LIMIT + 1:
            raise AssertionError("unbounded forensic read intercepted before allocation")
        self.before_read()
        return self.handle.read(size)


class FailureLimitTests(FailureEvidenceFixture, unittest.TestCase):
    def probe_reads(self, reads, before_read=lambda: None):
        original = os.fdopen

        def opened(descriptor, mode):
            handle = original(descriptor, mode)
            if mode == "rb":
                return ReadProbe(handle, reads, before_read)
            return handle

        return patch.object(os, "fdopen", side_effect=opened)

    def test_oversized_sparse_sources_rejected_by_fstat_without_any_read(self):
        self.assertEqual(failure.MAX_FORENSIC_FILE_BYTES, LIMIT)
        for size in (LIMIT + 1, 64 * 1024**3):
            with self.subTest(size=size), tempfile.TemporaryDirectory() as temporary:
                lane = self.lane(Path(temporary))
                path = lane.documents / "rune-qualification.json"
                with path.open("wb") as handle:
                    handle.truncate(size)  # Sparse; never allocate its contents.
                reads = []
                with self.probe_reads(reads), failure.directory(lane.documents) as source:
                    with self.assertRaisesRegex(OverflowError, SIZE_ERROR):
                        failure.original_bytes(source, path.name)
                self.assertEqual(reads, [])
                receipt = lane.preserve_failure_evidence(quiescent=True)["files"][0]
                self.assertEqual(receipt["status"], "rejected")
                self.assertEqual(receipt["error"], SIZE_ERROR)
                self.assertFalse(self.retained(lane, path.name).exists())
                self.assertNotIn("sha256", receipt)

    def test_small_and_exact_boundary_sources_keep_all_bytes_and_correct_hashes(self):
        for size in (7, LIMIT):
            with self.subTest(size=size), tempfile.TemporaryDirectory() as temporary:
                lane = self.lane(Path(temporary))
                path = lane.documents / "rune-qualification.json"
                with path.open("wb") as handle:
                    handle.write(b"original"[:size])
                    handle.truncate(size)
                reads = []
                with self.probe_reads(reads):
                    receipt = lane.preserve_failure_evidence(quiescent=True)["files"][0]
                self.assertEqual(receipt["status"], "UNVALIDATED")
                self.assertEqual(receipt["bytes"], size)
                retained = self.retained(lane, path.name)
                with path.open("rb") as source, retained.open("rb") as copied:
                    self.assertEqual(hashlib.file_digest(source, "sha256").hexdigest(), receipt["sha256"])
                    self.assertEqual(hashlib.file_digest(copied, "sha256").hexdigest(), receipt["sha256"])
                self.assertEqual(retained.stat().st_size, size)
                self.assertTrue(reads)
                self.assertTrue(all(count == LIMIT + 1 for count in reads))

    def test_growth_after_fstat_hits_bounded_overflow_without_truncation_or_copy(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            path = lane.documents / "rune-qualification.json"
            path.write_bytes(b"small")
            reads = []

            def grow():
                with path.open("r+b") as handle:
                    handle.truncate(LIMIT + 1)

            with self.probe_reads(reads, grow), failure.directory(lane.documents) as source, \
                    failure.directory(lane.out) as destination:
                receipt = failure.capture_file(source, destination, path.name, path.name)
            self.assertEqual(reads, [LIMIT + 1])
            self.assertEqual(receipt["status"], "rejected")
            self.assertEqual(receipt["error"], SIZE_ERROR)
            self.assertNotIn("sha256", receipt)
            self.assertFalse((lane.out / path.name).exists())

    def test_oversized_late_report_keeps_original_failure_other_files_and_owned_delete(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            original = subprocess.TimeoutExpired(["inert-health-query"], 30)

            def late_failure(pid):
                self.seed_native(lane, report=None)
                with (lane.documents / "rune-qualification.json").open("wb") as handle:
                    handle.truncate(64 * 1024**3)
                raise original

            lane.require_alive = Mock(side_effect=late_failure)
            lane.simctl = Mock()
            reads = []
            with self.probe_reads(reads), self.assertRaises(subprocess.TimeoutExpired) as raised:
                self.pipeline(lane)
            self.assertIs(raised.exception, original)
            self.assertEqual(lane.simctl.call_args_list[-1].args, ("delete", lane.udid))
            receipt = lane.failure_evidence_receipt
            self.assertEqual(receipt["files"][0]["status"], "rejected")
            self.assertEqual(receipt["files"][0]["error"], SIZE_ERROR)
            self.assertFalse(self.retained(lane, "rune-qualification.json").exists())
            self.assertEqual(self.retained(lane, "rune-tree.json").read_bytes(), b'{"synthetic":"tree"}')
            self.assertTrue(self.retained(lane, "rune-input-gate.json").exists())
            self.assertFalse((lane.out / "evidence.json").exists())
            self.assert_private_allowlist(lane)
            self.assertTrue(reads)
            self.assertTrue(all(count == LIMIT + 1 for count in reads))


if __name__ == "__main__":
    unittest.main()
