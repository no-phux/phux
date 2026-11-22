"""Failure capture's filesystem safety, staging and quiescence contracts."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

from test_ios_failure_evidence import FailureEvidenceFixture, FIRST_FAILURE, failure, qualify


class FailureCaptureTests(FailureEvidenceFixture, unittest.TestCase):
    def test_failed_shutdown_records_rejects_without_reading_mutable_native_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            self.seed_native(lane)
            lane.failure_evidence_requested = True

            def simctl(action, *args, **kwargs):
                if action == "shutdown":
                    raise subprocess.TimeoutExpired(["inert-shutdown"], 60)

            lane.simctl = Mock(side_effect=simctl)
            with patch.object(qualify._failure, "original_bytes", side_effect=AssertionError("no reads")):
                with self.assertRaisesRegex(ValueError, "shutdown"):
                    lane.cleanup()
            receipt = lane.failure_evidence_receipt
            self.assertFalse(receipt["quiescent"])
            self.assertIn("no bytes copied", receipt["limitation"])
            self.assertTrue(all(row["status"] == "rejected" for row in receipt["files"]))
            self.assertEqual([p.name for p in (lane.out / failure.DIRECTORY).iterdir()], ["receipt.json"])
            self.assertEqual(lane.simctl.call_args_list[-1].args, ("delete", lane.udid))

    def test_symlink_hardlink_directory_fifo_sources_are_rejected_without_credentials(self):
        for kind in ("symlink", "hardlink", "directory", "fifo"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as temporary:
                lane = self.lane(Path(temporary))
                secret = lane.documents / "qualification-connection.json"
                secret.write_bytes(b"CREDENTIAL-SENTINEL")
                path = lane.documents / "rune-qualification.json"
                if kind == "symlink":
                    path.symlink_to(secret)
                elif kind == "hardlink":
                    os.link(secret, path)
                elif kind == "directory":
                    path.mkdir()
                else:
                    os.mkfifo(path)
                receipt = lane.preserve_failure_evidence(quiescent=True)
                self.assertEqual(receipt["files"][0]["status"], "rejected")
                self.assertFalse(self.retained(lane, "rune-qualification.json").exists())
                self.assert_private_allowlist(lane)

    def test_symlinked_container_or_ancestor_is_rejected(self):
        for ancestor in (False, True):
            with self.subTest(ancestor=ancestor), tempfile.TemporaryDirectory() as temporary:
                lane = self.lane(Path(temporary))
                self.seed_native(lane)
                alias = lane.scratch / "alias"
                alias.symlink_to(lane.scratch if ancestor else lane.documents, target_is_directory=True)
                lane.documents = alias / "Documents" if ancestor else alias
                receipt = lane.preserve_failure_evidence(quiescent=True)
                self.assertIn("unsafe", receipt["limitation"])
                self.assertTrue(all(row["status"] == "rejected" for row in receipt["files"]))

    def test_existing_forensic_destination_is_never_reused_even_if_symlinked(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            self.seed_native(lane)
            (lane.out / failure.DIRECTORY).symlink_to(lane.documents, target_is_directory=True)
            with self.assertRaises(FileExistsError):
                lane.preserve_failure_evidence(quiescent=True)
            self.assertEqual((lane.documents / "rune-qualification.json").read_bytes(), FIRST_FAILURE)
            self.assertFalse((lane.documents / "receipt.json").exists())

    def test_staging_moves_only_the_background_inode_without_reading_before_quiescence(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            source = lane.documents / "rune-background.json"
            source.write_bytes(b"original late acknowledgement")
            original_inode = source.stat().st_ino
            with patch.object(qualify._failure, "original_bytes", side_effect=AssertionError("no live read")):
                lane.stage_failure_background()
            staged = lane.documents / failure.STAGING_DIRECTORY / "rune-background.json"
            self.assertEqual(staged.stat().st_ino, original_inode)
            self.assertFalse(source.exists())
            source.write_bytes(b"later seal acknowledgement")
            lane.preserve_failure_evidence(quiescent=True)
            self.assertEqual(self.retained(lane, "rune-background.before-seal.json").read_bytes(),
                             b"original late acknowledgement")
            self.assertEqual(self.retained(lane, "rune-background.json").read_bytes(), b"later seal acknowledgement")

    def test_preexisting_staging_directory_is_never_reused_or_collected(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            original = lane.documents / "rune-background.json"
            original.write_bytes(b"original")
            staged = lane.documents / failure.STAGING_DIRECTORY
            staged.mkdir()
            (staged / "rune-background.json").write_bytes(b"CREDENTIAL-SENTINEL")
            with self.assertRaises(FileExistsError):
                lane.stage_failure_background()
            self.assertEqual(original.read_bytes(), b"original")
            lane.preserve_failure_evidence(quiescent=True)
            self.assertFalse(self.retained(lane, "rune-background.before-seal.json").exists())
            self.assert_private_allowlist(lane)

    def test_copy_error_for_one_file_does_not_discard_other_native_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            self.seed_native(lane)
            real_write = qualify._failure.write_private

            def write(destination, name, data):
                if name == "rune-qualification.json":
                    raise PermissionError("private-path-sentinel")
                return real_write(destination, name, data)

            with patch.object(qualify._failure, "write_private", side_effect=write):
                receipt = lane.preserve_failure_evidence(quiescent=True)
            self.assertEqual(receipt["files"][0]["status"], "rejected")
            self.assertEqual(self.retained(lane, "rune-tree.json").read_bytes(), b'{"synthetic":"tree"}')
            self.assertNotIn("private-path-sentinel", json.dumps(receipt))

    def test_changed_source_is_rejected_instead_of_labelled_a_final_capture(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            self.seed_native(lane)
            with failure.directory(lane.documents) as source:
                original_stat = (lane.documents / "rune-qualification.json").stat()
                (lane.documents / "rune-qualification.json").write_bytes(b"changed")
                changed_stat = (lane.documents / "rune-qualification.json").stat()
                with patch.object(failure.os, "fstat", side_effect=[original_stat, changed_stat]):
                    with self.assertRaisesRegex(ValueError, "changed during capture"):
                        failure.original_bytes(source, "rune-qualification.json")

    def test_missing_container_is_explicit_and_unowned_launch_has_no_forensic_capture(self):
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            lane.documents = None
            receipt = lane.preserve_failure_evidence(quiescent=True)
            self.assertIn("container unavailable", receipt["limitation"])
            self.assertTrue(all(row["status"] == "rejected" for row in receipt["files"]))
        with tempfile.TemporaryDirectory() as temporary:
            lane = self.lane(Path(temporary))
            lane.diagnostic_prefixes = []
            self.assertIsNone(lane.preserve_failure_evidence(quiescent=True))
            self.assertFalse((lane.out / failure.DIRECTORY).exists())

    def test_seal_receipts_retained_independently_when_trace_and_tree_are_missing(self):
        for prefix in ("", "resume-"):
            with self.subTest(prefix=prefix), tempfile.TemporaryDirectory() as temporary:
                lane = self.lane(Path(temporary), prefix)
                self.seed_native(lane, tree=None)
                (lane.documents / (prefix + "rune-connection-diagnostics.jsonl")).unlink()
                pending = prefix + "rune-connection-diagnostics.status.pending"
                (lane.documents / pending).write_bytes(b'{"complete":')
                lane.failure_evidence_requested = True
                lane.simctl = Mock()
                lane.cleanup()
                self.assertEqual((lane.out / failure.DIRECTORY / pending).read_bytes(), b'{"complete":')
                self.assertFalse((lane.out / "evidence.json").exists())
                self.assertEqual(lane.failure_evidence_receipt["status"], "UNVALIDATED")


if __name__ == "__main__":
    unittest.main()
