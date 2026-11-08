#!/usr/bin/env python3
"""Build a UIKit host and the real Rune Rust staticlib, without Xcode projects."""

import argparse
import json
import os
from pathlib import Path
import plistlib
import shlex
import shutil
import subprocess
import sys
import tomllib

REPO = Path(__file__).resolve().parents[3]
RUNE = REPO / "clients/rune"
IOS = RUNE / "ios"
MIN_IOS = "26.0"
BUNDLE_ID = "sh.phux.rune.ios"


def apple_environment(out):
    env = {k: v for k, v in os.environ.items() if not k.startswith("PHUX_")}
    tmp = out / "toolchain-tmp"
    tmp.mkdir(exist_ok=True)
    env["TMPDIR"] = str(tmp)
    result = subprocess.run(
        ["/bin/bash", "-ec", 'source "$1"; env -0', "apple-env",
         str(REPO / "scripts/lib/apple-toolchain-env.sh")],
        env=env, check=True, capture_output=True, timeout=30,
    )
    return dict(item.decode().split("=", 1) for item in result.stdout.split(b"\0") if item)


def run(command, env, out, name, timeout=120):
    command = [str(arg) for arg in command]
    (out / f"{name}.command.json").write_text(json.dumps(command, indent=2) + "\n")
    with (out / f"{name}.log").open("w") as log:
        subprocess.run(command, cwd=REPO, env=env, stdout=log,
                       stderr=subprocess.STDOUT, check=True, timeout=timeout)
    return (out / f"{name}.log").read_text()


def native_static_flags(output):
    # rustc's order and duplicates matter. Never eval compiler output.
    lines = [line.split("native-static-libs:", 1)[1].strip()
             for line in output.splitlines() if "native-static-libs:" in line]
    if len(lines) != 1:
        raise ValueError("expected exactly one rustc native-static-libs record")
    flags = shlex.split(lines[0])
    if not flags:
        raise ValueError("rustc returned no native static libraries")
    return flags


def bundle_plist(version, simulator, fonts):
    return {
        "CFBundleIdentifier": BUNDLE_ID, "CFBundleExecutable": "PhuxRune",
        "CFBundleName": "Phux Rune", "CFBundleDisplayName": "Phux Rune",
        "CFBundlePackageType": "APPL", "CFBundleShortVersionString": version,
        "CFBundleVersion": version.split("-", 1)[0], "MinimumOSVersion": MIN_IOS,
        "CFBundleSupportedPlatforms": ["iPhoneSimulator" if simulator else "iPhoneOS"],
        "LSRequiresIPhoneOS": True, "UIDeviceFamily": [1, 2],
        "UILaunchScreen": {}, "UIAppFonts": fonts,
        "UISupportedInterfaceOrientations": ["UIInterfaceOrientationPortrait"],
    }


def copy_resources(app, env, out, target):
    fonts = sorted((RUNE / "assets/fonts").glob("*.ttf"))
    if not fonts:
        raise ValueError("no bundled fonts")
    resources = [REPO / "LICENSE", REPO / "NOTICE", *fonts,
                 *sorted((RUNE / "assets/fonts").glob("*-OFL.txt"))]
    for source in resources:
        shutil.copy2(source, app / source.name)
    # Resolve from the bridge manifest: the desktop root graph can omit native
    # bridge-only dependencies. Reuse the packaging notice renderer/reader.
    metadata = run(["cargo", "metadata", "--locked", "--format-version", "1",
                    "--filter-platform", target, "--manifest-path", IOS / "bridge/Cargo.toml"],
                   env, out, "notice-metadata", timeout=300)
    metadata_path = out / "notice-metadata.json"
    metadata_path.write_text(metadata)
    renderer = json.dumps(str(RUNE / "scripts/third-party.ts"))
    reader = json.dumps(str(RUNE / "scripts/notice-texts.ts"))
    script = f"""
import {{ canonicalLicenses, renderNotices, shippedCrates }} from {renderer};
import {{ runeNoticeReader }} from {reader};
const crates = shippedCrates(await Bun.file(process.argv[1]).text());
if (!crates.some(c => c.name === 'phux-rune-ios')) throw new Error('missing bridge notices');
const read = runeNoticeReader(crates, canonicalLicenses());
process.stdout.write(renderNotices(crates, read, 'Phux Rune'));
"""
    notices = run(["bun", "-e", script, metadata_path], env, out,
                  "third-party-notices", timeout=300)
    if not notices.strip():
        raise ValueError("empty third-party notices")
    (app / "THIRD-PARTY-NOTICES.txt").write_text(notices)
    return [font.name for font in fonts]


def build_rust(out, env, triple, release):
    command = ["cargo", "rustc", "--manifest-path", RUNE / "Cargo.toml", "-p",
               "phux-rune-ios", "--lib", "--locked", "--target", triple]
    if release:
        command.append("--release")
    output = run([*command, "--", "--print", "native-static-libs"],
                 env, out, "rust", timeout=2400)
    flags = native_static_flags(output)
    profile = "release" if release else "debug"
    library = out / "cargo" / triple / profile / "libphux_rune_ios.a"
    if not library.is_file():
        raise ValueError(f"cargo did not produce {library}")
    return library, flags, profile


def build_swift(out, env, app, sources, library, flags, simulator, release):
    sdk = "iphonesimulator" if simulator else "iphoneos"
    sdk_path = run(["/usr/bin/xcrun", "--sdk", sdk, "--show-sdk-path"],
                   env, out, "sdk").strip()
    swift_target = f"arm64-apple-ios{MIN_IOS}" + ("-simulator" if simulator else "")
    module_cache = out / "module-cache"
    module_cache.mkdir(exist_ok=True)
    linker_flags = [arg for flag in flags for arg in ("-Xlinker", flag)]
    run(["/usr/bin/xcrun", "--sdk", sdk, "swiftc", "-target", swift_target,
         "-sdk", sdk_path, "-module-cache-path", module_cache,
         "-import-objc-header", IOS / "Bridge.h", "-parse-as-library",
         "-O" if release else "-Onone", *sources, library, *linker_flags,
         "-o", app / "PhuxRune"], env, out, "swift", timeout=600)


def build(out, target, release):
    sources = sorted((IOS / "Sources").glob("*.swift"))
    if not sources or not (IOS / "Bridge.h").is_file():
        raise ValueError("the native Swift sources and Bridge.h must exist before building")
    out.mkdir(parents=True, exist_ok=True)
    app = out / "Phux Rune.app"
    app.mkdir()  # Never overwrite a prior build or somebody else's bundle.
    env = apple_environment(out)
    env.update(IPHONEOS_DEPLOYMENT_TARGET=MIN_IOS, CARGO_TARGET_DIR=str(out / "cargo"))
    simulator = target == "simulator"
    triple = "aarch64-apple-ios-sim" if simulator else "aarch64-apple-ios"
    library, flags, profile = build_rust(out, env, triple, release)
    build_swift(out, env, app, sources, library, flags, simulator, release)
    fonts = copy_resources(app, env, out, triple)
    version = tomllib.loads((RUNE / "Cargo.toml").read_text())["package"]["version"]
    with (app / "Info.plist").open("wb") as handle:
        plistlib.dump(bundle_plist(version, simulator, fonts), handle)
    # A device build is an unsigned development bundle, not an installable IPA.
    if simulator:
        run(["/usr/bin/codesign", "--force", "--sign", "-", "--timestamp=none", app],
            env, out, "sign")
        run(["/usr/bin/codesign", "--verify", "--strict", app], env, out, "verify-sign")
    revision = run(["git", "rev-parse", "HEAD"], env, out, "revision").strip()
    (out / "build.json").write_text(json.dumps({
        "schema": 1, "app": str(app), "target": triple, "minimum_ios": MIN_IOS,
        "profile": profile, "revision": revision, "native_static_flags": flags,
        "signed": simulator,
    }, indent=2) + "\n")
    return app


def absolute_path(value):
    path = Path(value)
    if not path.is_absolute():
        raise argparse.ArgumentTypeError("use an absolute output path")
    return path.resolve()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=absolute_path, required=True)
    parser.add_argument("--target", choices=("simulator", "device"), required=True)
    parser.add_argument("--release", action="store_true")
    args = parser.parse_args()
    print(build(args.out, args.target, args.release))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        sys.exit(f"iOS build failed: {error}")
