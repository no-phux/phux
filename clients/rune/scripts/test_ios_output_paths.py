"""Trusted output normalization must not normalize untrusted forensic sources."""
import json
from pathlib import Path
import tempfile
import unittest

from test_ios_build import load

qualify = load("qualify")
failure = load("failure_evidence")
REPORT = b'{"schema":1,"error":"original synthetic native failure"}'


class OutputPathTests(unittest.TestCase):
    def lane(self, root):
        physical = root / "physical"
        physical.mkdir()
        alias = root / "trusted-output-alias"
        alias.symlink_to(physical, target_is_directory=True)
        output = alias / "out"
        output.mkdir()
        lane = qualify.Qualification(root / "App", root / "phux", output, root)
        lane.documents = root / "Documents"
        lane.documents.mkdir()
        lane.diagnostic_prefixes = [""]
        return lane, output

    def test_trusted_output_parent_alias_matches_cli_and_retains_original_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            # Only the controlled fixture root is canonical; the supplied output
            # deliberately contains an alias just like /var or /tmp on macOS.
            lane, supplied = self.lane(Path(temporary).resolve())
            (lane.documents / "rune-qualification.json").write_bytes(REPORT)
            self.assertEqual(lane.out, qualify.absolute_path(str(supplied)))
            self.assertNotEqual(lane.out, supplied)
            receipt = lane.preserve_failure_evidence(quiescent=True)
            retained = supplied / failure.DIRECTORY / "rune-qualification.json"
            self.assertEqual(retained.read_bytes(), REPORT)
            self.assertEqual(receipt["files"][0]["status"], "UNVALIDATED")
            self.assertEqual(retained.stat().st_mode & 0o777, 0o600)

    def test_source_container_ancestor_and_leaf_aliases_remain_rejected(self):
        for kind in ("container", "ancestor", "leaf"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary).resolve()
                lane, supplied = self.lane(root)
                secret = root / "private-configuration.json"
                secret.write_bytes(b"PRIVATE-CREDENTIAL-SENTINEL")
                source = lane.documents / "rune-qualification.json"
                source.write_bytes(REPORT)
                alias = root / "untrusted-source-alias"
                if kind == "container":
                    alias.symlink_to(lane.documents, target_is_directory=True)
                    lane.documents = alias
                elif kind == "ancestor":
                    alias.symlink_to(root, target_is_directory=True)
                    lane.documents = alias / "Documents"
                else:
                    source.unlink()
                    source.symlink_to(secret)
                untrusted = lane.documents
                receipt = lane.preserve_failure_evidence(quiescent=True)
                self.assertEqual(lane.documents, untrusted)
                self.assertEqual(receipt["files"][0]["status"], "rejected")
                self.assertFalse((supplied / failure.DIRECTORY / source.name).exists())
                self.assertNotIn("PRIVATE-CREDENTIAL-SENTINEL", json.dumps(receipt))
                self.assertEqual(secret.read_bytes(), b"PRIVATE-CREDENTIAL-SENTINEL")

    def test_canonical_output_replaced_by_symlink_after_construction_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            lane, _ = self.lane(root)
            (lane.documents / "rune-qualification.json").write_bytes(REPORT)
            redirect = root / "redirect"
            redirect.mkdir()
            lane.out.rename(root / "original-out")
            lane.out.symlink_to(redirect, target_is_directory=True)
            with self.assertRaises(OSError):
                lane.preserve_failure_evidence(quiescent=True)
            self.assertEqual(list(redirect.iterdir()), [])
            self.assertIsNone(lane.failure_evidence_receipt)


if __name__ == "__main__":
    unittest.main()
