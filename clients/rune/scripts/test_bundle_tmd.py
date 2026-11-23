import hashlib
import io
import json
import os
from pathlib import Path
import re
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import subprocess

import bundle_tmd


def fixture(symlink=False):
    data = io.BytesIO()
    with tarfile.open(fileobj=data, mode="w:gz") as archive:
        for name in ("tmd", "LICENSE", "THIRD_PARTY_NOTICES.md", "../ignored"):
            member = tarfile.TarInfo(name)
            text = name.encode()
            member.size = len(text)
            if name == "tmd" and symlink:
                member.type = tarfile.SYMTYPE
                member.linkname = "/outside"
            archive.addfile(member, io.BytesIO(text))
    return data.getvalue()


def source_fixture():
    data = io.BytesIO()
    with tarfile.open(fileobj=data, mode="w:gz") as archive:
        for name in ("LICENSE", "THIRD_PARTY_NOTICES.md"):
            member = tarfile.TarInfo(f"tmd-{bundle_tmd.SOURCE_REV}/{name}")
            member.size = len(name)
            archive.addfile(member, io.BytesIO(name.encode()))
    return data.getvalue()


class BundleTests(unittest.TestCase):
    def test_binary_and_all_notices_are_bundled_without_archive_paths(self):
        data = fixture()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bundle_tmd.unpack(data, hashlib.sha256(data).hexdigest(), root / "bin", root / "notices")
            self.assertEqual((root / "bin/tmd").read_bytes(), b"tmd")
            self.assertEqual((root / "bin/tmd").stat().st_mode & 0o777, 0o755)
            for name in ("tmd-LICENSE.txt", "tmd-THIRD-PARTY-NOTICES.md", "tmd-VERSION.txt"):
                self.assertTrue((root / "notices" / name).is_file())
            self.assertFalse((root / "ignored").exists())

    def test_wrong_digest_or_symlink_fails_before_writing(self):
        for symlink in (False, True):
            data = fixture(symlink)
            digest = hashlib.sha256(data).hexdigest() if symlink else "0" * 64
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                with self.assertRaises(ValueError):
                    bundle_tmd.unpack(data, digest, root / "bin", root / "notices")
                self.assertFalse((root / "bin").exists())

    def test_every_desktop_target_is_pinned(self):
        self.assertEqual(set(bundle_tmd.DIGESTS), {
            ("Linux", "aarch64"), ("Linux", "x86_64"),
        })
        for _, digest in bundle_tmd.DIGESTS.values():
            self.assertRegex(digest, r"^[a-f0-9]{64}$")
        self.assertRegex(bundle_tmd.SOURCE_REV, r"^[a-f0-9]{40}$")
        self.assertRegex(bundle_tmd.SOURCE_SHA256, r"^[a-f0-9]{64}$")
        self.assertEqual(bundle_tmd.MACOS_TARGET, "aarch64-macos.14.0.0")

    def test_macos_build_uses_verified_source_and_the_pinned_compiler_for_14(self):
        data = source_fixture()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with patch.object(bundle_tmd.subprocess, "run") as run:
                with self.assertRaisesRegex(ValueError, "source checksum"):
                    bundle_tmd.build_macos(data, root / "bin", root / "notices")
                run.assert_not_called()

            stages = []

            def compiler(command, **kwargs):
                if command == ["zig", "version"]:
                    repo = Path(__file__).resolve().parents[3]
                    version = json.loads((repo / ".config/zig-toolchain.json").read_text())["version"]
                    self.assertEqual(kwargs["timeout"], 10)
                    return subprocess.CompletedProcess(command, 0, version + "\n")
                self.assertTrue(kwargs["check"])
                if command[0] == "curl":
                    self.assertEqual(command[:2], ["curl", "--disable"])
                    self.assertEqual(command[-2:], ["--", bundle_tmd.ARO_URL])
                    for flag, value in (
                        ("--proto", "=https"), ("--proto-redir", "=https"),
                        ("--connect-timeout", "20"), ("--max-time", "120"),
                        ("--max-filesize", str(bundle_tmd.MAX_ARCHIVE_SIZE)),
                    ):
                        self.assertEqual(command[command.index(flag) + 1], value)
                    for flag in ("--fail", "--silent", "--show-error", "--location"):
                        self.assertIn(flag, command)
                    self.assertNotIn("--retry", command)
                    self.assertEqual(kwargs["timeout"], 120)
                    archive = Path(command[command.index("--output") + 1])
                    self.assertEqual(archive.parts[-3:], ("zig-global-cache", "p", bundle_tmd.ARO_HASH + ".tar.gz"))
                    archive.write_bytes(b"original mirror archive")
                    stages.append("seed")
                    return subprocess.CompletedProcess(command, 0)
                cache = Path(kwargs["env"]["ZIG_GLOBAL_CACHE_DIR"])
                self.assertNotEqual(str(cache), os.environ.get("ZIG_GLOBAL_CACHE_DIR"))
                self.assertEqual((cache / "p" / (bundle_tmd.ARO_HASH + ".tar.gz")).read_bytes(), b"original mirror archive")
                self.assertEqual(stages, ["seed"])
                self.assertEqual(kwargs["cwd"].parent, cache.parent)
                package = kwargs["cwd"] / "zig-pkg"
                self.assertFalse(package.exists(), "only Zig may materialize a package")
                self.assertEqual(kwargs["timeout"], 600)
                stages.append("compile")
                self.assertEqual(command, [
                    "zig", "build", "--release=small", "-Dtarget=aarch64-macos.14.0.0",
                    "--pkg-dir", str(package), "--cache-dir", str(cache.parent / "zig-cache"),
                ])
                output = kwargs["cwd"] / "zig-out/bin/tmd"
                output.parent.mkdir(parents=True)
                output.write_bytes(b"compiled tmd")

            with patch.object(bundle_tmd, "SOURCE_SHA256", hashlib.sha256(data).hexdigest()), \
                    patch.object(bundle_tmd.subprocess, "run", side_effect=compiler), \
                    patch.dict(os.environ, {
                        "ZIG_GLOBAL_CACHE_DIR": "/ambient/global",
                        "ZIG_LOCAL_PKG_DIR": "/ambient/pkg",
                        "ZIG_LOCAL_CACHE_DIR": "/ambient/cache",
                    }):
                bundle_tmd.build_macos(data, root / "bin", root / "notices")
            self.assertEqual((root / "bin/tmd").read_bytes(), b"compiled tmd")
            self.assertEqual((root / "bin/tmd").stat().st_mode & 0o777, 0o755)
            self.assertEqual(stages, ["seed", "compile"])
            version = json.loads((Path(__file__).resolve().parents[3] / ".config/zig-toolchain.json").read_text())["version"]
            self.assertEqual((root / "notices/tmd-VERSION.txt").read_text(), (
                f"tmd {bundle_tmd.VERSION}\nhttps://github.com/jtnk/tmd\n"
                f"Source: {bundle_tmd.SOURCE_REV}\nSource SHA-256: {hashlib.sha256(data).hexdigest()}\n"
                f"Zig: {version}\nTarget: aarch64-macos.14.0.0\n"
            ))
            self.assertEqual((root / "notices/tmd-LICENSE.txt").read_bytes(), b"LICENSE")
            self.assertEqual((root / "notices/tmd-THIRD-PARTY-NOTICES.md").read_bytes(), b"THIRD_PARTY_NOTICES.md")

    def test_failed_mirror_fetch_never_compiles_or_publishes(self):
        data = source_fixture()
        repo = Path(__file__).resolve().parents[3]
        version = json.loads((repo / ".config/zig-toolchain.json").read_text())["version"]
        for failure in (
            subprocess.CalledProcessError(63, ["curl"]),
            subprocess.TimeoutExpired(["curl"], 120),
        ):
            with self.subTest(failure=type(failure).__name__), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                with patch.object(bundle_tmd, "SOURCE_SHA256", hashlib.sha256(data).hexdigest()), \
                        patch.object(bundle_tmd.subprocess, "run", side_effect=[
                            subprocess.CompletedProcess(["zig", "version"], 0, version + "\n"), failure,
                        ]) as run:
                    with self.assertRaises(type(failure)):
                        bundle_tmd.build_macos(data, root / "bin", root / "notices")
                self.assertEqual(run.call_count, 2)
                self.assertFalse((root / "bin").exists())
                self.assertFalse((root / "notices").exists())

    def test_wrong_compiler_never_fetches_or_publishes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            data = source_fixture()
            with patch.object(bundle_tmd, "SOURCE_SHA256", hashlib.sha256(data).hexdigest()), \
                    patch.object(bundle_tmd.subprocess, "run", return_value=(
                        subprocess.CompletedProcess(["zig", "version"], 0, "0.0.0\n")
                    )) as run:
                with self.assertRaisesRegex(ValueError, "repository's Zig"):
                    bundle_tmd.build_macos(data, root / "bin", root / "notices")
            self.assertEqual(run.call_count, 1)
            self.assertFalse((root / "bin").exists())
            self.assertFalse((root / "notices").exists())

    def test_failed_build_or_hash_verification_never_publishes(self):
        data = source_fixture()
        repo = Path(__file__).resolve().parents[3]
        version = json.loads((repo / ".config/zig-toolchain.json").read_text())["version"]
        for failure in (
            subprocess.CalledProcessError(1, ["zig", "build"]),
            subprocess.TimeoutExpired(["zig", "build"], 600),
        ):
            with self.subTest(failure=type(failure).__name__), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                with patch.object(bundle_tmd, "SOURCE_SHA256", hashlib.sha256(data).hexdigest()), \
                        patch.object(bundle_tmd, "seed_aro_cache"), \
                        patch.object(bundle_tmd.subprocess, "run", side_effect=[
                            subprocess.CompletedProcess(["zig", "version"], 0, version + "\n"), failure,
                        ]):
                    with self.assertRaises(type(failure)):
                        bundle_tmd.build_macos(data, root / "bin", root / "notices")
                self.assertFalse((root / "bin").exists())
                self.assertFalse((root / "notices").exists())

    def test_mirror_size_bound_includes_exact_limit_and_rejects_overflow(self):
        for size in (3, 4):
            with self.subTest(size=size), tempfile.TemporaryDirectory() as directory:
                cache = Path(directory) / "global"

                def curl(command, **kwargs):
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

    def test_oversized_mirror_never_compiles_or_publishes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            data = source_fixture()
            with patch.object(bundle_tmd, "SOURCE_SHA256", hashlib.sha256(data).hexdigest()), \
                    patch.object(bundle_tmd, "seed_aro_cache", side_effect=ValueError("aro archive exceeds size limit")), \
                    patch.object(bundle_tmd.subprocess, "run") as run:
                repo = Path(__file__).resolve().parents[3]
                version = json.loads((repo / ".config/zig-toolchain.json").read_text())["version"]
                run.return_value = subprocess.CompletedProcess(["zig", "version"], 0, version + "\n")
                with self.assertRaisesRegex(ValueError, "size limit"):
                    bundle_tmd.build_macos(data, root / "bin", root / "notices")
                self.assertEqual(run.call_count, 1)
            self.assertFalse((root / "bin").exists())
            self.assertFalse((root / "notices").exists())

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

    def test_bootstrap_preserves_the_official_aro_package_identity(self):
        repo = Path(__file__).resolve().parents[3]
        manifest = (repo / "scripts/zig-mirrors/build.zig.zon").read_text()
        self.assertIn("https://deps.files.ghostty.org/aro-d0c8c4d9c55daa7ef6e40cf0f630a5b5e900989b.tar.gz", manifest)
        self.assertIn(' .hash = "aro-0.0.0-JSD1QtuBNwCASyBtNF3pqTl_W3oAJQGEVyFAtrBSE_Pa"', manifest)
        self.assertEqual(re.findall(r'\.url\s*=\s*"([^"]+)"', manifest), [bundle_tmd.ARO_URL])
        self.assertEqual(re.findall(r'\.hash\s*=\s*"([^"]+)"', manifest), [bundle_tmd.ARO_HASH])

    def test_main_requires_version_and_real_plain_markdown_output(self):
        for version, rendered, error, calls in (
            ("tmd 0.2.0\n", "Whisk until smooth.\n", None, 2),
            ("tmd 0.0.0\n", "Whisk until smooth.\n", "version mismatch", 1),
            ("tmd 0.2.0\n", "Whisk until **smooth**.\n", "cannot render", 2),
            ("tmd 0.2.0\n", "", "cannot render", 2),
        ):
            with self.subTest(version=version, rendered=rendered), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                with patch.object(bundle_tmd.sys, "argv", ["bundle_tmd.py", str(root / "bin"), str(root / "notices")]), \
                        patch.object(bundle_tmd.platform, "system", return_value="Darwin"), \
                        patch.object(bundle_tmd.platform, "machine", return_value="arm64"), \
                        patch.object(bundle_tmd, "download", return_value=b"verified by build_macos"), \
                        patch.object(bundle_tmd, "build_macos") as build, \
                        patch.object(bundle_tmd.subprocess, "run", side_effect=[
                            subprocess.CompletedProcess([], 0, version),
                            subprocess.CompletedProcess([], 0, rendered),
                        ]) as run:
                    if error:
                        with self.assertRaisesRegex(ValueError, error):
                            bundle_tmd.main()
                    else:
                        bundle_tmd.main()
                    build.assert_called_once()
                    self.assertEqual(run.call_count, calls)
                    self.assertEqual(run.call_args_list[0].args[0], [str(root / "bin/tmd"), "--version"])
                    for call in run.call_args_list:
                        self.assertEqual(call.kwargs["timeout"], 5)
                    if calls == 2:
                        self.assertEqual(run.call_args.args[0], [
                            str(root / "bin/tmd"), "--print", "--no-images",
                            "--plain-headings", "--width", "60",
                        ])
                        self.assertIn("**smooth**", run.call_args.kwargs["input"])

    def test_packaging_and_inside_out_signing_include_the_helper(self):
        repo = Path(__file__).resolve().parents[3]
        package = (repo / "clients/rune/scripts/package.sh").read_text()
        self.assertEqual(package.count('python3 "$here/scripts/bundle_tmd.py"'), 2)
        signing = (repo / "scripts/ci/sign-macos-desktop.sh").read_text()
        self.assertLess(signing.index("for helper in phux tmd"), signing.index('--timestamp "$app"'))
