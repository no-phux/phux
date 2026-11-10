"""Pinned, isolated SwiftPM build of RuneTailnet for the direct UIKit build."""

import hashlib
import io
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import urllib.request
import zipfile

PACKAGE = Path(__file__).resolve().parent / "Tailnet"
ARTIFACT_URL = (
    "https://github.com/no-phux/libtailscale/releases/download/"
    "tailscalekit-3af8bca979a5/TailscaleKit-3af8bca979a5.xcframework.zip"
)
ARTIFACT_SHA256 = "610fe434360393508d61138da3ba86ca01c4640dc6e95341c1ec1dcf4e07631d"
TAILSCALE_REVISION = "3af8bca979a5e3f7e93e603fbc71b2c02b2512ad"
GO_VERSION = "1.27.2"


def run(command, env, out, name, timeout=1800, stdout_only=False):
    command = [str(value) for value in command]
    (out / f"{name}.command.json").write_text(json.dumps(command, indent=2) + "\n")
    with (out / f"{name}.log").open("w") as log, \
            (out / f"{name}.stderr.log").open("w") as diagnostics:
        subprocess.run(command, env=env, cwd=out, check=True, timeout=timeout,
                       stdout=log, stderr=diagnostics)
    output = (out / f"{name}.log").read_text()
    return output if stdout_only else output + (out / f"{name}.stderr.log").read_text()


def verify_archive(archive):
    with archive.open("rb") as handle:
        digest = hashlib.file_digest(handle, "sha256").hexdigest()
    if digest != ARTIFACT_SHA256:
        raise ValueError("TailscaleKit archive checksum mismatch; refusing to extract or link")
    with zipfile.ZipFile(archive) as package:
        for entry in package.infolist():
            path = Path(entry.filename)
            if path.is_absolute() or ".." in path.parts:
                raise ValueError("unsafe TailscaleKit archive path")


def artifact(out, env):
    archive = out / "TailscaleKit.xcframework.zip"
    if not archive.exists():
        supplied = env.get("RUNE_TAILSCALE_ARCHIVE")
        partial = archive.with_suffix(".partial")
        if supplied:
            shutil.copyfile(supplied, partial)
        else:
            with urllib.request.urlopen(ARTIFACT_URL, timeout=120) as response:
                with partial.open("wb") as handle:
                    shutil.copyfileobj(response, handle)
        verify_archive(partial)
        partial.rename(archive)
    verify_archive(archive)  # Also validate an existing cache, before trusting it.
    extracted = out / "artifact"
    if extracted.exists():
        shutil.rmtree(extracted)  # Only our disposable, checksum-derived output.
    extracted.mkdir()
    run(["/usr/bin/ditto", "-x", "-k", archive, extracted], env, out, "extract")
    frameworks = list(extracted.rglob("TailscaleKit.xcframework"))
    if len(frameworks) != 1:
        raise ValueError("expected exactly one TailscaleKit xcframework")
    return frameworks[0]


def framework_slice(xcframework, target):
    with (xcframework / "Info.plist").open("rb") as handle:
        libraries = plistlib.load(handle)["AvailableLibraries"]
    variant = "simulator" if target == "simulator" else None
    matches = [item for item in libraries
               if item["SupportedPlatform"] == "ios"
               and item.get("SupportedPlatformVariant") == variant
               and "arm64" in item["SupportedArchitectures"]]
    if len(matches) != 1:
        raise ValueError("missing or ambiguous arm64 TailscaleKit framework slice")
    item = matches[0]
    result = xcframework / item["LibraryIdentifier"] / item["LibraryPath"]
    if result.name != "TailscaleKit.framework" or not result.is_dir():
        raise ValueError("missing TailscaleKit framework")
    return result


def copied_package(out, xcframework):
    # Put even manifest caches and Package.resolved rewrites under out.
    destination = out / "package"
    if destination.exists():
        shutil.rmtree(destination)  # Our source mirror, never the worktree.
    shutil.copytree(PACKAGE, destination, dirs_exist_ok=True,
                    ignore=shutil.ignore_patterns(".build", ".swiftpm", "__pycache__"))
    manifest = (PACKAGE / "Package.swift").read_text()
    start = manifest.index("        .binaryTarget(")
    end = manifest.index("        .target(name:", start)
    # The downloaded bytes were checked above. A local binary target prevents
    # SwiftPM from downloading a second copy into a global artifact cache.
    relative = os.path.relpath(xcframework, destination)
    local = f'        .binaryTarget(name: "TailscaleKit", path: {json.dumps(relative)}),\n'
    (destination / "Package.swift").write_text(manifest[:start] + local + manifest[end:])
    return destination


def fetched_license(module):
    name, version = module["name"], module["version"]
    raw_sources = {
        "libtailscale": f"https://raw.githubusercontent.com/no-phux/libtailscale/{TAILSCALE_REVISION}/LICENSE",
        "go": f"https://raw.githubusercontent.com/golang/go/go{GO_VERSION}/LICENSE",
    }
    url = raw_sources.get(name, f"https://proxy.golang.org/{name}/@v/v{version}.zip")
    with urllib.request.urlopen(url, timeout=120) as response:
        data = response.read(100_000_001)
    if len(data) > 100_000_000:
        raise ValueError(f"license source too large for {name}")
    if name in raw_sources:
        return data.decode(errors="replace").strip()
    prefix = f"{name}@v{version}/"
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        candidates = ("LICENSE", "LICENSE.txt", "LICENSE.md", "COPYING", "LICENCE")
        for filename in candidates:
            path = prefix + filename
            if path in archive.namelist():
                return archive.read(path).decode(errors="replace").strip()
    raise ValueError(f"no upstream license for {name} {version}")


def tailscale_notices(out):
    inventory = json.loads((PACKAGE / "license-sources.json").read_text())
    if inventory["artifact_revision"] != TAILSCALE_REVISION or inventory["go_version"] != GO_VERSION:
        raise ValueError("TailscaleKit license inventory version mismatch")
    modules = inventory["modules"]
    versions = {module["name"]: module["version"] for module in modules}
    if versions.get("libtailscale") != TAILSCALE_REVISION[:12] or versions.get("go") != GO_VERSION:
        raise ValueError("TailscaleKit license module version mismatch")
    cache = out / "licenses"
    cache.mkdir(exist_ok=True)
    parts = []
    for module in modules:
        path = cache / (module["text_sha256"] + ".txt")
        text = path.read_text() if path.exists() else fetched_license(module)
        if hashlib.sha256(text.encode()).hexdigest() != module["text_sha256"]:
            raise ValueError(f"license checksum mismatch for {module['name']}")
        path.write_text(text)
        parts.append(f"\n{module['name']} {module['version']} ({module['license']})\n{text}\n")
    return "".join(parts)


def notices(checkouts, tailscale):
    resolved = json.loads((PACKAGE / "Package.resolved").read_text())
    parts = ["RuneTailnet third-party notices\n"]
    for pin in resolved["pins"]:
        name = pin["identity"]
        checkout = checkouts / name
        paths = sorted(path for path in checkout.iterdir()
                       if path.is_file() and path.name.upper().startswith(("LICENSE", "NOTICE")))
        if not paths:
            raise ValueError(f"missing license notices for pinned package {name}")
        parts.append(f"\n{name} {pin['state']['version']} ({pin['state']['revision']})\n")
        for path in paths:
            parts.append(f"\n{path.name}\n{path.read_text()}\n")
    parts.append(tailscale)
    return "".join(parts)


def build_paths(out, target, release):
    if target not in ("simulator", "device"):
        raise ValueError("tailnet target must be simulator or device")
    triple = "arm64-apple-ios26.0" + ("-simulator" if target == "simulator" else "")
    return triple, "release" if release else "debug"


def clang_module_flags(out, sdk):
    # Swift modules serialize their C imports. A direct swiftc app build is
    # outside SwiftPM, so supply the same generated C maps and shim maps.
    generated = out / "build/out/Intermediates.noindex" / f"GeneratedModuleMaps-{sdk}"
    maps = sorted(generated.glob("CNIO*.modulemap"))
    checkouts = out / "build/checkouts"
    maps.extend(checkouts / path for path in (
        "swift-atomics/Sources/_AtomicsShims/include/module.modulemap",
        "swift-nio/Sources/CNIOWindows/include/module.modulemap",
    ))
    if not all(path.is_file() for path in maps):
        raise ValueError("missing SwiftPM C shim module maps")
    return [flag for path in maps for flag in ("-Xcc", f"-fmodule-map-file={path}")]


def prepare(out: Path, env: dict, target: str, release: bool) -> dict:
    """Return swift_flags, dynamic frameworks, resource bundles and notices.

    Caller copies resources to the app root, embeds/signs frameworks under
    Frameworks, and appends notices. No signing keys or system VPN are touched.
    RUNE_TAILSCALE_ARCHIVE optionally supplies an offline archive; its checksum
    is still mandatory. All package, build and cache products live under out.
    """
    triple, profile = build_paths(out, target, release)
    out = out.resolve() / "tailnet"
    out.mkdir(parents=True, exist_ok=True)
    env = {key: value for key, value in env.items() if not key.startswith("PHUX_")}
    # SwiftPM executes package manifests on macOS even for an iOS product.
    # An inherited iOS SDKROOT makes those host manifests uncompilable. The
    # target SDK belongs in SwiftPM's --sdk option, not its host environment.
    env.pop("SDKROOT", None)
    env.update(CLANG_MODULE_CACHE_PATH=str(out / "clang-cache"),
               SWIFTPM_MODULECACHE_OVERRIDE=str(out / "swift-cache"))
    xcframework = artifact(out, env)
    framework = framework_slice(xcframework, target)
    package = copied_package(out, xcframework)
    tailscale = tailscale_notices(out)
    sdk = "iphonesimulator" if target == "simulator" else "iphoneos"
    sdk_path = run(["/usr/bin/xcrun", "--sdk", sdk, "--show-sdk-path"], env, out, "sdk", stdout_only=True).strip()
    options = ["--package-path", package, "--scratch-path", out / "build",
               "--cache-path", out / "cache", "--config-path", out / "config",
               "--security-path", out / "security", "--manifest-cache", "local",
               "--disable-keychain", "--disable-netrc", "--force-resolved-versions",
               "--sdk", sdk_path, "--triple", triple, "--configuration", profile,
               "--build-system", "swiftbuild", "--jobs", "8"]
    run(["/usr/bin/xcrun", "--sdk", "macosx", "swift", "build", *options, "--product", "RuneTailnet",
         "-Xswiftc", "-warnings-as-errors"], env, out, "build", timeout=2400)
    products = Path(run(["/usr/bin/xcrun", "--sdk", "macosx", "swift", "build", *options, "--show-bin-path"],
                        env, out, "products", stdout_only=True).strip())
    library = products / "libRuneTailnet.a"
    modules = products / "Modules" if (products / "Modules").is_dir() else products
    if not library.is_file() or not (modules / "RuneTailnet.swiftmodule").exists():
        raise ValueError("SwiftPM did not produce the RuneTailnet static library and module")
    flags = ["-I", str(modules), "-F", str(framework.parent),
             str(library), "-framework", "TailscaleKit", "-Xlinker", "-rpath",
             "-Xlinker", "@executable_path/Frameworks", *clang_module_flags(out, sdk)]
    # Static-package privacy resources live outside the .a. Preserve each
    # bundle's original name so Bundle.module can find it in the app root.
    resources = sorted(products.glob("*.bundle"))
    resources.append(package / "PrivacyInfo.xcprivacy")
    result = {"swift_flags": flags, "frameworks": [framework],
              "resources": resources, "notices": notices(out / "build/checkouts", tailscale)}
    (out / "integration.json").write_text(json.dumps({
        **result, "frameworks": list(map(str, result["frameworks"])),
        "resources": list(map(str, result["resources"]))}, indent=2) + "\n")
    return result
