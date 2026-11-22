#!/usr/bin/env python3
"""Reproduce the small public-source license manifest from the SDK inventory.

Input: scripts/licenses/tailscalekit.json from no-phux/phux-mobile commit
bbf510b1a5ec0d939f0a5f39b3165f620f5d0971. The build fetches only the pinned
public upstream license texts, not that private repository or any app code.
"""
import argparse
import hashlib
import json
from pathlib import Path

INVENTORY_SHA256 = "616afdd686ed6fcb59ca98481e92e75783ab20dd627ed2e285cc8c3c90eb9cd4"
REVISION = "3af8bca979a5e3f7e93e603fbc71b2c02b2512ad"


def generate(inventory):
    data = inventory.read_bytes()
    if hashlib.sha256(data).hexdigest() != INVENTORY_SHA256:
        raise ValueError("not the pinned TailscaleKit license inventory")
    records = json.loads(data)
    versions = {record["name"]: record["version"] for record in records}
    if versions.get("libtailscale") != REVISION[:12] or versions.get("go") != "1.27.2":
        raise ValueError("TailscaleKit or Go version mismatch")
    result = {"artifact_revision": REVISION, "go_version": "1.27.2",
              "inventory_sha256": INVENTORY_SHA256, "modules": []}
    for record in records:
        result["modules"].append({key: record[key] for key in ("name", "version", "license")})
        result["modules"][-1]["text_sha256"] = hashlib.sha256(record["text"].encode()).hexdigest()
    return json.dumps(result, indent=2) + "\n"


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("inventory", type=Path)
    args = parser.parse_args()
    print(generate(args.inventory), end="")
