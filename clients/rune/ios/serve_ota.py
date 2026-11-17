#!/usr/bin/env python3
"""Serve only an OTA site's public files, with a live scheduled-download page.

Reuse the existing loopback listener behind private HTTPS. Published build
folders stay immutable; /scheduled/ selects the newest complete build on read.
This server never signs, installs, pairs, or reads private delivery receipts.
"""
import argparse
from functools import partial
import html
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
import io
from pathlib import Path
import plistlib
import re
import urllib.parse
import zipfile

from package_ota import BUNDLE_ID, checked_base_url

INFO = "Payload/Phux Rune.app/Info.plist"


def version_key(value):
    if not isinstance(value, str) or not re.fullmatch(r"[0-9]+(?:\.[0-9]+){0,2}", value):
        raise ValueError("invalid numeric bundle version")
    parts = tuple(int(part) for part in value.split("."))
    return parts + (0,) * (3 - len(parts))


def latest_release(directory, base_url):
    """Read complete published children, never private packaging output."""
    scheduled = directory / "scheduled"
    if not scheduled.is_dir() or scheduled.is_symlink():
        return None
    releases = []
    for child in scheduled.iterdir():
        if not child.is_dir() or child.is_symlink():
            continue
        files = [child / name for name in ("index.html", "manifest.plist", "PhuxRune.ipa")]
        if not all(path.is_file() and not path.is_symlink() for path in files):
            continue
        public = base_url + "scheduled/" + urllib.parse.quote(child.name, safe="") + "/"
        try:
            if files[1].stat().st_size > 65536:
                continue
            manifest = plistlib.loads(files[1].read_bytes())
            if len(manifest["items"]) != 1:
                continue
            item = manifest["items"][0]
            metadata = item["metadata"]
            packages = [asset["url"] for asset in item["assets"]
                        if asset["kind"] == "software-package"]
            if (metadata["bundle-identifier"] != BUNDLE_ID
                    or metadata["kind"] != "software"
                    or packages != [public + "PhuxRune.ipa"]):
                continue
            build = metadata["bundle-version"]
            build_order = version_key(build)
            with zipfile.ZipFile(files[2]) as archive:
                if archive.namelist().count(INFO) != 1 or archive.getinfo(INFO).file_size > 65536:
                    continue
                info = plistlib.loads(archive.read(INFO))
            if (info["CFBundleIdentifier"] != BUNDLE_ID
                    or info["CFBundleVersion"] != build
                    or info["CFBundleSupportedPlatforms"] != ["iPhoneOS"]):
                continue
            version = info["CFBundleShortVersionString"]
            order = (version_key(version), build_order)
            releases.append((order, child.name, {
                "version": version, "build": build, "url": public,
                "install": "itms-services://?action=download-manifest&url="
                           + urllib.parse.quote(public + "manifest.plist", safe=""),
            }))
        except (OSError, ValueError, KeyError, TypeError, IndexError, zipfile.BadZipFile):
            # An incomplete copy must not replace the last usable download.
            continue
    return max(releases, key=lambda release: release[:2])[2] if releases else None


def download_page(release):
    if release:
        version, build = html.escape(release["version"]), html.escape(release["build"])
        content = (f"<p class=version>Version {version}<span>Build {build}</span></p>"
                   f"<a class=install href='{html.escape(release['install'], quote=True)}'>Install on iPhone</a>"
                   f"<p><a href='{html.escape(release['url'] + 'PhuxRune.ipa', quote=True)}'>Download IPA</a></p>")
    else:
        content = "<p class=version>No phone build published yet.</p><p>Check this same page again later.</p>"
    return ("<!doctype html><html lang=en><head><meta charset=utf-8>"
            "<meta name=viewport content='width=device-width,initial-scale=1'>"
            "<meta name=color-scheme content='light dark'><title>Phux Rune downloads</title>"
            "<style>html{color-scheme:light dark}body{font:17px/1.55 system-ui,sans-serif;"
            "margin:0;padding:48px 24px}main{max-width:30rem;margin:auto}"
            "h1{font-size:2.4rem;line-height:1.1;letter-spacing:-.04em;margin:12px 0 24px}"
            ".eyebrow,footer{opacity:.65}.eyebrow{font-size:.8rem;letter-spacing:.08em;text-transform:uppercase}"
            ".version{font-size:1.25rem;font-weight:600;margin:28px 0}.version span{display:block;"
            "font-size:.85rem;font-weight:400;opacity:.65}.install{display:block;padding:14px 20px;"
            "border-radius:12px;background:light-dark(#17191d,#eee);color:light-dark(#fff,#17191d);"
            "text-decoration:none;font-weight:650;text-align:center}a{color:inherit}"
            "a:focus-visible{outline:3px solid #6897e8;outline-offset:4px}footer{font-size:.85rem;"
            "border-top:1px solid light-dark(#ddd,#444);margin-top:36px;padding-top:20px}"
            "</style></head><body><main><div class=eyebrow>iPhone · Private downloads</div>"
            "<h1>Phux Rune</h1><p>The newest published phone build, always at this link.</p>"
            + content
            + "<footer><p>Turn on Tailscale and open this page in Safari on your registered iPhone. "
            "Tap Install and approve the iOS prompt. Update in place—don’t delete the app first.</p>"
            "<p>Save this page with Safari’s Share → Add to Home Screen. "
            "Future published builds appear here automatically.</p></footer></main></body></html>")


class Handler(SimpleHTTPRequestHandler):
    extensions_map = {**SimpleHTTPRequestHandler.extensions_map,
                      ".plist": "text/xml", ".ipa": "application/octet-stream"}

    def __init__(self, *args, directory, base_url, **kwargs):
        self.base_url = base_url
        super().__init__(*args, directory=str(directory), **kwargs)

    def send_head(self):
        path = urllib.parse.urlsplit(self.path).path
        if path == "/scheduled":
            self.send_response(308)
            self.send_header("Location", self.base_url + "scheduled/")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return None
        if path not in ("/scheduled/", "/scheduled/index.html"):
            return super().send_head()
        release = latest_release(Path(self.directory), self.base_url)
        content = download_page(release).encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(content)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.end_headers()
        return io.BytesIO(content)


def serve(directory, base_url, port):
    directory = Path(directory).resolve(strict=True)
    if not directory.is_dir():
        raise ValueError("directory must be the existing public OTA site")
    handler = partial(Handler, directory=directory, base_url=checked_base_url(base_url))
    with ThreadingHTTPServer(("127.0.0.1", port), handler) as server:
        print(f"OTA downloads listening on 127.0.0.1:{server.server_port}", flush=True)
        server.serve_forever()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, required=True, help="public site only, never packaging output")
    parser.add_argument("--base-url", required=True, help="existing private HTTPS URL mapped to this directory")
    parser.add_argument("--port", type=int, required=True, help="existing loopback listener port")
    args = parser.parse_args()
    serve(args.directory, args.base_url, args.port)


if __name__ == "__main__":
    main()
