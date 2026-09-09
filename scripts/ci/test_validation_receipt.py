#!/usr/bin/env python3
"""Validation reuse must fail closed for mismatched trees and run provenance."""

from io import BytesIO
import json
import unittest
import zipfile

import validation_receipt as receipts


class ValidationReceiptTests(unittest.TestCase):
    def setUp(self):
        self.expected = {"schema": 1, "tree": "a" * 40, "workflow": ".github/workflows/ci.yml"}
        self.run = {
            "conclusion": "success", "event": "pull_request",
            "head_sha": "c" * 40,
            "path": self.expected["workflow"],
            "head_repository": {"full_name": "owner/project"},
            "repository": {"full_name": "owner/project"},
        }
        self.artifact = {"id": 2, "workflow_run": {"id": 3}, "expired": False, "size_in_bytes": 400}
        self.body = dict(self.expected)
        self.commit_tree = self.expected["tree"]

    def fetch(self, path, binary=False):
        if binary:
            archive = BytesIO()
            with zipfile.ZipFile(archive, "w") as files:
                files.writestr("receipt.json", json.dumps(self.body))
            return archive.getvalue()
        if path.endswith("/3"):
            return self.run
        if "/git/commits/" in path:
            return {"tree": {"sha": self.commit_tree}}
        return {"artifacts": [self.artifact]}

    def test_successful_exact_tree(self):
        self.assertEqual(receipts.find_validation(self.expected, "owner/project", self.fetch), 3)

    def test_changed_tree_is_not_reused(self):
        self.body["tree"] = "b" * 40
        self.assertIsNone(receipts.find_validation(self.expected, "owner/project", self.fetch))

    def test_forged_receipt_from_unrelated_source_commit_is_not_reused(self):
        self.commit_tree = "d" * 40
        self.assertIsNone(receipts.find_validation(self.expected, "owner/project", self.fetch))

    def test_failed_inflight_wrong_workflow_and_fork_are_not_reused(self):
        for field, value in [("conclusion", "failure"), ("conclusion", None),
                             ("event", "workflow_dispatch"), ("path", "other.yml"),
                             ("head_repository", {"full_name": "fork/project"})]:
            with self.subTest(field=field, value=value):
                previous = self.run[field]
                self.run[field] = value
                self.assertIsNone(receipts.find_validation(self.expected, "owner/project", self.fetch))
                self.run[field] = previous

    def test_expired_and_oversized_artifacts_are_not_reused(self):
        self.artifact["expired"] = True
        self.assertIsNone(receipts.find_validation(self.expected, "owner/project", self.fetch))
        self.artifact["expired"] = False
        self.artifact["size_in_bytes"] = 100000
        self.assertIsNone(receipts.find_validation(self.expected, "owner/project", self.fetch))

    def test_archive_cannot_supply_extra_files(self):
        archive = BytesIO()
        with zipfile.ZipFile(archive, "w") as files:
            files.writestr("receipt.json", json.dumps(self.body))
            files.writestr("unwanted.sh", "exit 1")
        self.assertIsNone(receipts.read_receipt(archive.getvalue()))


if __name__ == "__main__":
    unittest.main()
