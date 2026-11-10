#!/usr/bin/env python3
"""Build a UIKit host and the real Rune Rust staticlib, without Xcode projects."""

import argparse
import importlib.util
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
# The app icon's own ground (#090A0C), also the root view's before a frame.
LAUNCH_GROUND = ("0x09", "0x0A", "0x0C")


def apple_environment(out):
    env = {k: v for k, v in os.environ.items() if not k.startswith("PHUX_")}
    # Compiler/cache helpers bind Unix sockets under TMPDIR. Evidence paths
    # can exceed macOS's socket length limit; build outputs still stay in out.
    env["TMPDIR"] = "/tmp"
    result = subprocess.run(
        ["/bin/bash", "-ec", 'source "$1"; env -0', "apple-env",
         str(REPO / "scripts/lib/apple-toolchain-env.sh")],
        env=env, check=True, capture_output=True, timeout=30,
    )
    return dict(item.decode().split("=", 1) for item in result.stdout.split(b"\0") if item)


def run(command, env, out, name, timeout=120, stdout_only=False):
    command = [str(arg) for arg in command]
    (out / f"{name}.command.json").write_text(json.dumps(command, indent=2) + "\n")
    with (out / f"{name}.log").open("w") as log, \
            (out / f"{name}.stderr.log").open("w") as diagnostics:
        subprocess.run(command, cwd=REPO, env=env, stdout=log,
                       stderr=diagnostics, check=True, timeout=timeout)
    output = (out / f"{name}.log").read_text()
    return output if stdout_only else output + (out / f"{name}.stderr.log").read_text()


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


def bundle_plist(version, simulator, fonts, icons=None):
    return {
        **(icons or {}),
        "CFBundleIdentifier": BUNDLE_ID, "CFBundleExecutable": "PhuxRune",
        "CFBundleName": "Phux Rune", "CFBundleDisplayName": "Phux Rune",
        "CFBundlePackageType": "APPL", "CFBundleShortVersionString": version,
        "CFBundleVersion": version.split("-", 1)[0], "MinimumOSVersion": MIN_IOS,
        "CFBundleSupportedPlatforms": ["iPhoneSimulator" if simulator else "iPhoneOS"],
        "LSRequiresIPhoneOS": True, "UIDeviceFamily": [1, 2],
        "UILaunchScreen": {"UIColorName": "LaunchGround"}, "UIAppFonts": fonts,
        "NSCameraUsageDescription": "Scan a Phux pairing QR to connect without typing credentials.",
        "NSLocalNetworkUsageDescription": "Connect securely to your computer and get its Phux pairing code over SSH.",
        "CFBundleURLTypes": [{"CFBundleURLName": BUNDLE_ID + ".pairing",
                              "CFBundleURLSchemes": ["phux-rune"]}],
        "UIApplicationSceneManifest": {
            "UIApplicationSupportsMultipleScenes": False,
            "UISceneConfigurations": {"UIWindowSceneSessionRoleApplication": [{
                "UISceneConfigurationName": "Rune",
                "UISceneDelegateClassName": "PhuxRuneSceneDelegate",
            }]},
        },
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
                   env, out, "notice-metadata", timeout=300, stdout_only=True)
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
                  "third-party-notices", timeout=300, stdout_only=True)
    if not notices.strip():
        raise ValueError("empty third-party notices")
    (app / "THIRD-PARTY-NOTICES.txt").write_text(notices)
    return [font.name for font in fonts]


def compile_assets(app, env, out, simulator):
    """The home-screen icon, and a launch colour matching the terminal so a
    light-mode phone does not flash white before the first frame."""
    catalog = out / "Assets.xcassets"
    if catalog.exists():
        shutil.rmtree(catalog)  # Our derived catalog, never a source tree.
    info = {"author": "xcode", "version": 1}
    icon = catalog / "AppIcon.appiconset"
    icon.mkdir(parents=True)
    (catalog / "Contents.json").write_text(json.dumps({"info": info}))
    shutil.copy2(IOS / "AppIcon.png", icon / "AppIcon.png")
    (icon / "Contents.json").write_text(json.dumps({"info": info, "images": [{
        "filename": "AppIcon.png", "idiom": "universal", "platform": "ios", "size": "1024x1024"}]}))
    ground = catalog / "LaunchGround.colorset"
    ground.mkdir()
    components = dict(zip(("red", "green", "blue"), LAUNCH_GROUND), alpha="1.000")
    (ground / "Contents.json").write_text(json.dumps({"info": info, "colors": [{
        "idiom": "universal", "color": {"color-space": "srgb", "components": components}}]}))
    partial = out / "assets-info.plist"
    run(["/usr/bin/xcrun", "actool", catalog, "--compile", app, "--platform",
         "iphonesimulator" if simulator else "iphoneos", "--minimum-deployment-target", MIN_IOS,
         "--target-device", "iphone", "--target-device", "ipad", "--app-icon", "AppIcon",
         "--output-partial-info-plist", partial, "--errors", "--warnings"],
        env, out, "assets", timeout=300)
    icons = plistlib.loads(partial.read_bytes())
    if "CFBundleIcons" not in icons or not (app / "Assets.car").is_file():
        raise ValueError("actool did not compile the app icon")
    return icons


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


def build_swift(out, env, app, sources, library, flags, simulator, release, swift_flags=()):
    sdk = "iphonesimulator" if simulator else "iphoneos"
    sdk_path = run(["/usr/bin/xcrun", "--sdk", sdk, "--show-sdk-path"],
                   env, out, "sdk", stdout_only=True).strip()
    swift_target = f"arm64-apple-ios{MIN_IOS}" + ("-simulator" if simulator else "")
    module_cache = out / "module-cache"
    module_cache.mkdir(exist_ok=True)
    linker_flags = [arg for flag in flags for arg in ("-Xlinker", flag)]
    if simulator:
        # Xcode embeds *simulated* entitlements in Mach-O, rather than
        # asserting an unprovisioned team entitlement in the code signature.
        entitlement = simulator_entitlements(out)
        for flag in ["-sectcreate", "__TEXT", "__entitlements", str(entitlement)]:
            linker_flags.extend(["-Xlinker", flag])
    run(["/usr/bin/xcrun", "--sdk", sdk, "swiftc", "-swift-version", "6", "-strict-concurrency=complete", "-warnings-as-errors", "-target", swift_target,
         "-sdk", sdk_path, "-module-cache-path", module_cache,
         "-import-objc-header", IOS / "Bridge.h", "-parse-as-library",
         "-O" if release else "-Onone", *sources, library, *linker_flags, *swift_flags,
         "-o", app / "PhuxRune"], env, out, "swift", timeout=600)


def simulator_entitlements(out):
    entitlement = out / "simulator-entitlements.plist"
    entitlement.write_bytes(plistlib.dumps({
        "application-identifier": "PHUXSIMDEV." + BUNDLE_ID,
        "keychain-access-groups": ["PHUXSIMDEV." + BUNDLE_ID],
        "com.apple.developer.team-identifier": "PHUXSIMDEV",
        "get-task-allow": True,
    }))
    return entitlement


def prepare_tailnet(out, env, target, release):
    spec = importlib.util.spec_from_file_location("rune_tailnet_build", IOS / "tailnet.py")
    helper = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(helper)
    return helper.prepare(out, env, target, release)


def embed_tailnet(app, payload, env, out, simulator):
    for framework in payload["frameworks"]:
        destination = app / "Frameworks" / framework.name
        destination.parent.mkdir(exist_ok=True)
        shutil.copytree(framework, destination)
        if simulator:
            run(["/usr/bin/codesign", "--force", "--sign", "-", "--timestamp=none", destination],
                env, out, "sign-" + framework.stem)
    for resource in payload["resources"]:
        destination = app / resource.name
        if resource.is_dir():
            shutil.copytree(resource, destination)
        else:
            shutil.copy2(resource, destination)
    with (app / "THIRD-PARTY-NOTICES.txt").open("a") as handle:
        handle.write("\n" + payload["notices"])


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
    payload = prepare_tailnet(out, env, target, release)
    build_swift(out, env, app, sources, library, flags, simulator, release, payload["swift_flags"])
    fonts = copy_resources(app, env, out, triple)
    embed_tailnet(app, payload, env, out, simulator)
    icons = compile_assets(app, env, out, simulator)
    version = tomllib.loads((RUNE / "Cargo.toml").read_text())["package"]["version"]
    with (app / "Info.plist").open("wb") as handle:
        plistlib.dump(bundle_plist(version, simulator, fonts, icons), handle)
    # A device build is an unsigned development bundle, not an installable IPA.
    if simulator:
        run(["/usr/bin/codesign", "--force", "--sign", "-", "--timestamp=none", app],
            env, out, "sign")
        run(["/usr/bin/codesign", "--verify", "--strict", "--deep", app], env, out, "verify-sign")
    revision = run(["git", "rev-parse", "HEAD"], env, out, "revision", stdout_only=True).strip()
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
