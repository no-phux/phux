#!/usr/bin/env python3
"""Sign a device bundle with an existing ad-hoc profile and write a private OTA site.

Never creates certificates, exports private keys, registers devices or installs apps.
The caller supplies an HTTPS directory and hosts only OUT/site, not OUT itself.
"""
import argparse
import datetime
import hashlib
import html
from pathlib import Path
import plistlib
import shutil
import subprocess
import urllib.parse
import zipfile

BUNDLE_ID = "sh.phux.rune.ios"


def checked_base_url(value):
    parsed = urllib.parse.urlsplit(value)
    if (parsed.scheme != "https" or not parsed.hostname or parsed.username is not None
            or parsed.password is not None or parsed.query or parsed.fragment):
        raise ValueError("base URL must be an HTTPS directory without credentials/query/fragment")
    return value.rstrip("/") + "/"


def authorized_entitlements(profile, identity, now=None):
    now = now or datetime.datetime.now(datetime.timezone.utc)
    expires = profile["ExpirationDate"].replace(tzinfo=datetime.timezone.utc)
    if expires <= now:
        raise ValueError("provisioning profile expired")
    entitlements = profile["Entitlements"]
    teams = profile["TeamIdentifier"]
    if len(teams) != 1 or entitlements.get("application-identifier") != f"{teams[0]}.{BUNDLE_ID}":
        raise ValueError("profile does not authorize this exact Rune bundle")
    if entitlements.get("get-task-allow", False) or profile.get("ProvisionsAllDevices", False):
        raise ValueError("requires ad-hoc distribution, not development or enterprise signing")
    if not profile.get("ProvisionedDevices"):
        raise ValueError("ad-hoc profile must name its permitted devices")
    fingerprints = {hashlib.sha1(cert).hexdigest().upper()
                    for cert in profile["DeveloperCertificates"]}
    if identity.upper() not in fingerprints:
        raise ValueError("signing identity is not authorized by this profile")
    return entitlements


def site_documents(base_url, version):
    base_url = checked_base_url(base_url)
    manifest = {"items": [{"assets": [{"kind": "software-package", "url": base_url + "PhuxRune.ipa"}],
                           "metadata": {"bundle-identifier": BUNDLE_ID, "bundle-version": version,
                                        "kind": "software", "title": "Phux Rune"}}]}
    install = "itms-services://?action=download-manifest&url=" + urllib.parse.quote(
        base_url + "manifest.plist", safe="")
    page = ("<!doctype html><meta charset=utf-8><meta name=viewport content='width=device-width,initial-scale=1'>"
            "<title>Install Phux Rune</title><main style='font:20px system-ui;max-width:36em;margin:3em auto;padding:1em'>"
            "<h1>Phux Rune for iPhone</h1><p>Native preview. Open this page in Safari.</p>"
            f"<p><a href='{html.escape(install, quote=True)}'>Install Phux Rune {html.escape(version)}</a></p>"
            "<p>Apple allows installation only on devices registered in this build’s provisioning profile. "
            "This is not TestFlight. Installation and any Developer Mode prompts require your approval.</p></main>")
    return manifest, page


def run(*args):
    try:
        return subprocess.check_output([str(arg) for arg in args], stderr=subprocess.PIPE, timeout=120)
    except subprocess.CalledProcessError as error:
        detail = error.stderr.decode(errors="replace") if error.stderr else "no diagnostic"
        raise RuntimeError(f"{args[0]} failed ({error.returncode}): {detail}") from error


def bundle_identity(app):
    info = plistlib.loads((app / "Info.plist").read_bytes())
    required = {"CFBundleIdentifier", "CFBundleExecutable", "CFBundleVersion"}
    if not required.issubset(info):
        raise ValueError("device bundle is missing required identity/executable/version metadata")
    if info["CFBundleIdentifier"] != BUNDLE_ID or info.get("CFBundleSupportedPlatforms") != ["iPhoneOS"]:
        raise ValueError("input must be the Rune device bundle, not a simulator app")
    name = info["CFBundleExecutable"]
    if not isinstance(name, str) or not name or Path(name).name != name:
        raise ValueError("executable must be a file inside the app bundle")
    return info


def device_bundle_info(app):
    info = bundle_identity(app)
    executable = app / info["CFBundleExecutable"]
    platform = run("/usr/bin/xcrun", "vtool", "-show-build", executable).decode()
    if "platform IOS\n" not in platform or "IOSSIMULATOR" in platform:
        raise ValueError("executable is not an iOS device binary")
    if (app / "Frameworks").exists() or (app / "PlugIns").exists():
        raise ValueError("nested executable signing requires an explicit per-bundle profile")
    if any(path.is_symlink() for path in app.rglob("*")):
        raise ValueError("symlinked bundle content is not supported by this static-app packager")
    return info


def require_devices(profile, devices):
    normalized = {device.replace("-", "").upper() for device in profile["ProvisionedDevices"]}
    requested = {device.replace("-", "").upper() for device in devices}
    if not requested or not requested.issubset(normalized):
        raise ValueError("provisioning profile does not cover every requested device")


def package(app, profile_path, identity, base_url, out, devices):
    base_url = checked_base_url(base_url)
    profile = plistlib.loads(run("/usr/bin/security", "cms", "-D", "-i", profile_path))
    entitlements = authorized_entitlements(profile, identity)
    require_devices(profile, devices)
    info = device_bundle_info(app)
    out.mkdir(parents=True, exist_ok=False)
    payload = out / "Payload"
    signed = payload / "Phux Rune.app"
    shutil.copytree(app, signed)
    shutil.copy2(profile_path, signed / "embedded.mobileprovision")
    xcent = out / "entitlements.plist"
    xcent.write_bytes(plistlib.dumps(entitlements))
    run("/usr/bin/codesign", "--force", "--sign", identity, "--entitlements", xcent,
        "--generate-entitlement-der", "--timestamp=none", signed)
    run("/usr/bin/codesign", "--verify", "--strict", signed)
    signed_entitlements = plistlib.loads(run("/usr/bin/codesign", "-d", "--entitlements", ":-", signed))
    if signed_entitlements != entitlements:
        raise ValueError("signed entitlements differ from the authorized profile")
    site = out / "site"
    site.mkdir()
    with zipfile.ZipFile(site / "PhuxRune.ipa", "w", zipfile.ZIP_DEFLATED) as archive:
        for path in sorted(payload.rglob("*")):
            archive.write(path, path.relative_to(out))
    manifest, page = site_documents(base_url, info["CFBundleVersion"])
    (site / "manifest.plist").write_bytes(plistlib.dumps(manifest))
    (site / "index.html").write_text(page, encoding="utf-8")
    return site


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", type=Path, required=True)
    parser.add_argument("--profile", type=Path, required=True)
    parser.add_argument("--identity", required=True, help="SHA-1 of a local Apple Distribution identity")
    parser.add_argument("--device-udid", action="append", required=True,
                        help="required device coverage; repeat to authorize multiple phones")
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    print(package(args.app.resolve(), args.profile.resolve(), args.identity,
                  args.base_url, args.out.resolve(), args.device_udid))


if __name__ == "__main__":
    main()
