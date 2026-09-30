"""Exercise the desktop host lock drift check against synthetic lockfiles."""

from pathlib import Path
import runpy
import unittest


CHECK = runpy.run_path(str(Path(__file__).with_name("check-desktop-host-lock.py")))
COMPARE = CHECK["compare"]
REQUIRED = CHECK["required_dependencies"]
REQUIRED_PACKAGE = CHECK["required_package"]


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

    def test_missing_required_edge_is_reported_and_fixed(self) -> None:
        # The #904 shape: a root crate gains a dependency whose package the host
        # lock already carries through another crate, but not as this edge.
        root = lock(
            path_crate("0.46.0", "chrono", "nix", "serde"),
            registry("chrono", "0.4.44"),
            registry("nix", "0.31.2"),
            registry("serde", "1.0.228"),
        )
        host = lock(
            path_crate("0.46.0", "chrono", "serde"),
            registry("chrono", "0.4.44"),
            registry("nix", "0.31.2"),
            registry("serde", "1.0.228"),
        )
        required = {"root-path": {"chrono", "nix", "serde"}}
        findings = COMPARE(root, host, required)
        self.assertEqual(len(findings), 1)
        self.assertIn("depends on nix", findings[0].message)
        fixed = findings[0].fix(host)
        self.assertIn(' "chrono",\n "nix",\n "serde",\n', fixed)
        self.assertEqual(COMPARE(root, fixed, required), [])

    def test_missing_edge_to_an_unresolved_package_needs_a_refresh(self) -> None:
        root = lock(path_crate("0.46.0", "nix"), registry("nix", "0.31.2"))
        host = lock(path_crate("0.46.0"))
        findings = COMPARE(root, host, {"root-path": {"nix"}})
        self.assertEqual(len(findings), 1)
        self.assertIsNone(findings[0].fix)

    def test_unrequired_root_only_edge_is_ignored(self) -> None:
        # Optional (feature-gated) and dev edges are in the root lock's union only.
        root = lock(path_crate("0.46.0", "sha2 0.11.0", "uniffi"), registry("sha2", "0.11.0"))
        host = lock(path_crate("0.46.0", "sha2 0.11.0"), registry("sha2", "0.11.0"))
        self.assertEqual(COMPARE(root, host, {"root-path": {"sha2"}}), [])


class RequiredDependencyTests(unittest.TestCase):
    def test_manifest_entries_resolve_to_package_names(self) -> None:
        inherited = {"usage": {"package": "usage-rs", "version": "6"}, "serde": "1"}
        for key_spec, expected in [
            (("serde", "1"), "serde"),
            (("serde", {"workspace": True, "features": ["derive"]}), "serde"),
            (("usage", {"workspace": True}), "usage-rs"),
            (("alias", {"package": "real", "version": "1"}), "real"),
            (("uniffi", {"version": "0.32", "optional": True}), None),
            (("serde", {"workspace": True, "optional": True}), None),
        ]:
            with self.subTest(key_spec=key_spec):
                self.assertEqual(REQUIRED_PACKAGE(*key_spec, inherited), expected)

    def test_repository_manifests_require_unconditional_dependencies_only(self) -> None:
        required = REQUIRED(Path(__file__).resolve().parent.parent)
        self.assertIn("nix", required["phux-config"])
        self.assertNotIn("insta", required["phux-config"])  # dev-dependency
        self.assertNotIn("uniffi", required["phux-client-ffi"])  # optional


if __name__ == "__main__":
    unittest.main()
