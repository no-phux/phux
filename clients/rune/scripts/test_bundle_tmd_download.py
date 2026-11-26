"""Mirror pin, transport version, and compressed-cache resource boundaries."""
from pathlib import Path
import re
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import bundle_tmd


class MirrorDownloadTests(unittest.TestCase):
    def test_bootstrap_preserves_the_official_aro_package_identity(self):
        repo = Path(__file__).resolve().parents[3]
        manifest = (repo / "dev/zig-mirrors/build.zig.zon").read_text()
        self.assertIn("https://deps.files.ghostty.org/aro-d0c8c4d9c55daa7ef6e40cf0f630a5b5e900989b.tar.gz", manifest)
        self.assertIn(' .hash = "aro-0.0.0-JSD1QtuBNwCASyBtNF3pqTl_W3oAJQGEVyFAtrBSE_Pa"', manifest)
        self.assertEqual(re.findall(r'\.url\s*=\s*"([^"]+)"', manifest), [bundle_tmd.ARO_URL])
        self.assertEqual(re.findall(r'\.hash\s*=\s*"([^"]+)"', manifest), [bundle_tmd.ARO_HASH])
        self.assertEqual(bundle_tmd.MAX_ARCHIVE_SIZE, 64 * 1024 * 1024)

    def test_unexpected_mirror_or_package_paths_are_rejected_before_download(self):
        for name, value in (
            ("ARO_URL", bundle_tmd.ARO_URL.replace("https:", "http:")),
            ("ARO_URL", bundle_tmd.ARO_URL.replace("deps.files.ghostty.org", "example.com")),
            ("ARO_URL", bundle_tmd.ARO_URL + "?redirect=1"),
            ("ARO_URL", bundle_tmd.ARO_URL.replace("/aro-", "/../aro-")),
            ("ARO_HASH", "../" + bundle_tmd.ARO_HASH),
            ("ARO_HASH", "/" + bundle_tmd.ARO_HASH),
            ("ARO_HASH", bundle_tmd.ARO_HASH + "/child"),
            ("ARO_HASH", bundle_tmd.ARO_HASH[:-1]),
        ):
            with self.subTest(name=name, value=value), tempfile.TemporaryDirectory() as directory:
                cache = Path(directory) / "global"
                with patch.object(bundle_tmd, name, value), patch.object(bundle_tmd.subprocess, "run") as run:
                    with self.assertRaisesRegex(ValueError, "invalid aro"):
                        bundle_tmd.seed_aro_cache(cache)
                    run.assert_not_called()
                self.assertFalse(cache.exists())

    def test_curl_version_floor_for_bounded_unknown_length_transfers(self):
        for version in ("8.4.0", "8.4.1", "8.10.0", "9.0.0"):
            with self.subTest(version=version), patch.object(bundle_tmd.subprocess, "run") as run:
                run.return_value = subprocess.CompletedProcess([], 0, f"curl {version} (test) libcurl/{version}\n")
                bundle_tmd.require_bounded_curl()
                run.assert_called_once_with(
                    ["curl", "--disable", "--version"], check=True,
                    capture_output=True, text=True, timeout=10,
                )

    def test_old_malformed_or_timed_out_curl_never_downloads_unknown_length_response(self):
        failures = (
            "curl 8.3.0\n", "curl 7.88.1\n", "", "not curl\n",
            "curl 8.4\n", "curl 8.4.0-DEV\n", "curl 8.4.0/invalid\n",
            subprocess.TimeoutExpired(["curl", "--disable", "--version"], 10),
            subprocess.CalledProcessError(1, ["curl", "--disable", "--version"]),
        )
        for failure in failures:
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                cache = Path(directory) / "global"
                with patch.object(bundle_tmd.subprocess, "run") as run:
                    if isinstance(failure, Exception):
                        run.side_effect = failure
                        error = type(failure)
                    else:
                        run.return_value = subprocess.CompletedProcess([], 0, failure)
                        error = ValueError
                    with self.assertRaises(error):
                        bundle_tmd.seed_aro_cache(cache)
                    self.assertEqual(run.call_count, 1, "no unknown-length transfer before the version gate")
                    self.assertEqual(run.call_args.args[0], ["curl", "--disable", "--version"])
                self.assertFalse(cache.exists())

    def test_mirror_size_bound_includes_exact_limit_and_rejects_overflow(self):
        for size in (3, 4):
            with self.subTest(size=size), tempfile.TemporaryDirectory() as directory:
                cache = Path(directory) / "global"

                def curl(command, **kwargs):
                    if "--version" in command:
                        return subprocess.CompletedProcess(command, 0, "curl 8.4.0\n")
                    Path(command[command.index("--output") + 1]).write_bytes(b"x" * size)
                    return subprocess.CompletedProcess(command, 0)

                with patch.object(bundle_tmd, "MAX_ARCHIVE_SIZE", 3), \
                        patch.object(bundle_tmd.subprocess, "run", side_effect=curl):
                    if size == 3:
                        bundle_tmd.seed_aro_cache(cache)
                    else:
                        with self.assertRaisesRegex(ValueError, "size limit"):
                            bundle_tmd.seed_aro_cache(cache)
                self.assertEqual(list(cache.iterdir()), [cache / "p"])
