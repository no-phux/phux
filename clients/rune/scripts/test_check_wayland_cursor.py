"""Fail-closed source selection and execution checks for the headless gate."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import check_wayland_cursor as cursor


REVISION = "a" * 40
GIT = "https://example.invalid/rune"
SOURCE = f"git+{GIT}?rev={REVISION}#{REVISION}"
LISTING = "\n".join(f"{name}: test" for name in sorted(cursor.REQUIRED_TESTS))


class CursorCheckerTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR"))
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.module = self.root / cursor.MODULE
        self.module.parent.mkdir(parents=True)
        self.module.write_text("// Fixture path only; never compiled.\n")
        self.tests = self.module.with_name("ime_cursor_tests.rs")
        self.tests.write_text("// Fixture path only; never compiled.\n")
        self.package = {
            "name": "winit",
            "source": SOURCE,
            "manifest_path": str(self.root / "Cargo.toml"),
        }
        self.manifest = {
            "patch": {"crates-io": {"winit": {"git": GIT, "rev": REVISION}}}
        }

    def test_exact_immutable_manifest_source(self):
        self.assertEqual(cursor.pinned_source(self.manifest), SOURCE)

    def test_missing_or_nonimmutable_manifest_revision(self):
        for revision in ("", "main", "a" * 7, "g" * 40, "a" * 41):
            with self.subTest(revision=revision):
                self.manifest["patch"]["crates-io"]["winit"]["rev"] = revision
                with self.assertRaisesRegex(ValueError, "immutable full"):
                    cursor.pinned_source(self.manifest)

    def test_missing_manifest_git(self):
        del self.manifest["patch"]["crates-io"]["winit"]["git"]
        with self.assertRaisesRegex(ValueError, "git source"):
            cursor.pinned_source(self.manifest)

    def test_selects_actual_package_module(self):
        self.assertEqual(
            cursor.cursor_module({"packages": [self.package]}, SOURCE), self.module
        )

    def test_missing_or_ambiguous_package(self):
        for packages in ([], [self.package, self.package]):
            with self.subTest(packages=packages):
                with self.assertRaisesRegex(ValueError, "exactly one"):
                    cursor.cursor_module({"packages": packages}, SOURCE)

    def test_rejects_wrong_or_missing_source(self):
        for source in (
            None,
            "registry+https://example.invalid",
            SOURCE[:-1] + "b",
            SOURCE.replace(GIT, GIT + "-other"),
            SOURCE.replace("?rev=", "?branch="),
        ):
            with self.subTest(source=source):
                self.package["source"] = source
                with self.assertRaisesRegex(ValueError, "exact git/rev"):
                    cursor.cursor_module({"packages": [self.package]}, SOURCE)

    def test_missing_module_or_sibling_tests(self):
        for required in (self.module, self.tests):
            with self.subTest(required=required.name):
                required.unlink()
                with self.assertRaisesRegex(ValueError, required.name):
                    cursor.cursor_module({"packages": [self.package]}, SOURCE)
                required.write_text("// Restored fixture path.\n")

    def test_requires_all_six_semantic_tests(self):
        cursor.verify_listing(LISTING + "\n\n6 tests, 0 benchmarks\n")
        missing = LISTING.splitlines()[1:]
        for listing in (
            "",
            "0 tests, 0 benchmarks",
            "\n".join(missing),
            LISTING + "\ntests::unexpected: test",
            LISTING.replace(": test", ": benchmark"),
        ):
            with self.subTest(listing=listing):
                with self.assertRaisesRegex(ValueError, "expected six"):
                    cursor.verify_listing(listing)

    def test_compiles_original_module_and_runs_serially(self):
        result = subprocess.CompletedProcess([], 0, stdout=LISTING)
        with patch.object(cursor.subprocess, "run", return_value=result) as run:
            cursor.run_tests(self.module)
        compile_args, list_args, test_args = [
            call.args[0] for call in run.call_args_list
        ]
        self.assertEqual(
            compile_args[:5],
            ["rustc", "--edition=2021", "--test", "-Dwarnings", str(self.module)],
        )
        self.assertEqual(compile_args[5], "-o")
        binary = compile_args[6]
        self.assertEqual(list_args, [binary, "--list"])
        self.assertEqual(test_args, [binary, "--test-threads=1"])
        self.assertFalse(
            Path(binary).parent.exists(), "temporary outputs must be cleaned"
        )
        self.assertTrue(all(call.kwargs["check"] for call in run.call_args_list))

    def test_compiler_failure_stops_before_listing_and_cleans_outputs(self):
        failure = subprocess.CalledProcessError(1, ["rustc"])
        with patch.object(cursor.subprocess, "run", side_effect=failure) as run:
            with self.assertRaises(subprocess.CalledProcessError):
                cursor.run_tests(self.module)
        self.assertEqual(run.call_count, 1)
        self.assertFalse(Path(run.call_args.args[0][-1]).parent.exists())

    def test_listing_failure_or_missing_tests_never_runs_binary_tests(self):
        compiled = subprocess.CompletedProcess([], 0)
        failures = (
            subprocess.CalledProcessError(1, ["tests", "--list"]),
            subprocess.CompletedProcess([], 0, stdout="0 tests, 0 benchmarks\n"),
        )
        for failure in failures:
            with self.subTest(failure=failure):
                with patch.object(
                    cursor.subprocess, "run", side_effect=[compiled, failure]
                ) as run:
                    with self.assertRaises((subprocess.CalledProcessError, ValueError)):
                        cursor.run_tests(self.module)
                self.assertEqual(run.call_count, 2)
                self.assertFalse(Path(run.call_args.args[0][0]).parent.exists())

    def test_regression_failure_propagates(self):
        compiled = subprocess.CompletedProcess([], 0)
        listed = subprocess.CompletedProcess([], 0, stdout=LISTING)
        failure = subprocess.CalledProcessError(101, ["tests"])
        with patch.object(
            cursor.subprocess, "run", side_effect=[compiled, listed, failure]
        ):
            with self.assertRaises(subprocess.CalledProcessError):
                cursor.run_tests(self.module)

    def test_main_uses_locked_metadata_from_client_root(self):
        self.root.joinpath("Cargo.toml").write_text(
            f'[patch.crates-io]\nwinit = {{ git = "{GIT}", rev = "{REVISION}" }}\n'
        )
        result = subprocess.CompletedProcess(
            [], 0, stdout=json.dumps({"packages": [self.package]})
        )
        with patch.object(cursor, "CLIENT", self.root):
            with patch.object(cursor.subprocess, "run", return_value=result) as run:
                with patch.object(cursor, "run_tests") as run_tests:
                    cursor.main()
        run.assert_called_once_with(
            ["cargo", "metadata", "--locked", "--format-version", "1"],
            cwd=self.root,
            check=True,
            stdout=subprocess.PIPE,
            text=True,
        )
        run_tests.assert_called_once_with(self.module)

    def test_metadata_failure_never_compiles(self):
        failure = subprocess.CalledProcessError(101, ["cargo", "metadata"])
        with patch.object(cursor.subprocess, "run", side_effect=failure):
            with patch.object(cursor, "run_tests") as run_tests:
                with self.assertRaises(subprocess.CalledProcessError):
                    cursor.main()
        run_tests.assert_not_called()


if __name__ == "__main__":
    unittest.main()
