"""Inspect the metadata on the final read-only image, not a staging folder."""
import subprocess
import sys
from pathlib import Path

from ds_store import DSStore
from mac_alias import Alias

mount = Path(sys.argv[1])
with DSStore.open(str(mount / ".DS_Store"), "r") as store:
    assert store["."]["icvl"] == (b"type", b"icnv")
    window = store["."]["bwsp"]
    assert window["WindowBounds"] == "{{160, 120}, {720, 460}}"
    assert not window["ShowToolbar"]
    assert not window["ShowSidebar"]
    view = store["."]["icvp"]
    assert view["backgroundType"] == 2
    assert view["arrangeBy"] == "none"
    assert view["iconSize"] == 112.0
    alias = Alias.from_bytes(view["backgroundImageAlias"])
    Path(sys.argv[2]).write_bytes(view["backgroundImageAlias"])
    assert alias.target.filename == ".background.tiff"
    assert alias.target.posix_path == "/.background.tiff"
    assert alias.target.carbon_path == b"Phux:.background.tiff"
    assert alias.volume.name == "Phux"
    assert store["Phux.app"]["Iloc"] == (180, 248)
    assert store["Applications"]["Iloc"] == (540, 248)
    assert store["Install Phux.txt"]["Iloc"] == (650, 382)
assert (mount / ".background.tiff").stat().st_size > 1000
assert (mount / ".VolumeIcon.icns").read_bytes()[:4] == b"icns"
finder_info = bytes.fromhex(subprocess.check_output(
    ["xattr", "-px", "com.apple.FinderInfo", str(mount)], text=True
))
assert int.from_bytes(finder_info[8:10], "big") & 0x400, "missing custom volume icon flag"
