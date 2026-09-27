"""Exercise the desktop host lock drift check against synthetic lockfiles."""

from pathlib import Path
import runpy
import unittest


CHECK = runpy.run_path(str(Path(__file__).with_name("check-desktop-host-lock.py")))
COMPARE = CHECK["compare"]


def lock(*packages: str) -> str:
    return "version = 4\n\n" + "\n".join(package.strip() + "\n" for package in packages)


def registry(name: str, version: str) -> str:
    return f"""
[[package]]
name = "{name}"
version = "{version}"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "0" * 64
"""


def path_crate(version: str, *dependencies: str) -> str:
    lines = "".join(f' "{dependency}",\n' for dependency in dependencies)
    return f"""
[[package]]
name = "root-path"
version = "{version}"
dependencies = [
{lines}]
"""


class HostLockDriftTests(unittest.TestCase):
    def test_matched_locks_are_accepted(self) -> None:
        root = lock(path_crate("0.46.0", "sha2 0.11.0"), registry("sha2", "0.11.0"))
        self.assertEqual(COMPARE(root, root), [])

    def test_patch_drift_within_a_boundary_is_accepted(self) -> None:
        root = lock(path_crate("0.46.0", "thiserror 2.0.21"), registry("thiserror", "2.0.21"))
        host = lock(path_crate("0.46.0", "thiserror 2.0.20"), registry("thiserror", "2.0.20"))
        self.assertEqual(COMPARE(root, host), [])

    def test_moved_boundary_is_reported_and_fixed(self) -> None:
        root = lock(path_crate("0.46.0", "sha2 0.11.0"), registry("sha2", "0.11.0"))
        host = lock(
            path_crate("0.46.0", "sha2 0.10.9"),
            registry("sha2", "0.10.9"),
            registry("sha2", "0.11.0"),
        )
        findings = COMPARE(root, host)
        self.assertEqual(len(findings), 1)
        self.assertIn("sha2 0.10.9", findings[0].message)
        self.assertIsNotNone(findings[0].fix)
        self.assertEqual(COMPARE(root, findings[0].fix(host)), [])

    def test_missing_target_version_is_reported_without_a_fix(self) -> None:
        root = lock(path_crate("0.46.0", "sha2 0.11.0"), registry("sha2", "0.11.0"))
        host = lock(path_crate("0.46.0", "sha2 0.10.9"), registry("sha2", "0.10.9"))
        findings = COMPARE(root, host)
        self.assertEqual(len(findings), 1)
        self.assertIsNone(findings[0].fix)

    def test_path_crate_version_drift_is_reported_and_fixed(self) -> None:
        root = lock(path_crate("0.46.0"), registry("sha2", "0.11.0"))
        host = lock(path_crate("0.45.0"), registry("sha2", "0.11.0"))
        findings = COMPARE(root, host)
        self.assertEqual(len(findings), 1)
        self.assertIn("0.45.0", findings[0].message)
        self.assertEqual(COMPARE(root, findings[0].fix(host)), [])

    def test_dev_only_dependency_difference_is_ignored(self) -> None:
        root = lock(
            path_crate("0.46.0", "sha2 0.11.0", "criterion 0.7.0"),
            registry("sha2", "0.11.0"),
            registry("criterion", "0.7.0"),
        )
        host = lock(path_crate("0.46.0", "sha2 0.11.0"), registry("sha2", "0.11.0"))
        self.assertEqual(COMPARE(root, host), [])


if __name__ == "__main__":
    unittest.main()
