"""Write portable Finder metadata without launching or scripting Finder."""
import sys
from pathlib import Path

from ds_store import DSStore
from mac_alias import Alias

mount = Path(sys.argv[1]).resolve()
with DSStore.open(str(mount / ".DS_Store"), "w+") as store:
    store["."]["vSrn"] = ("long", 1)
    store["."]["icvl"] = ("type", b"icnv")
    store["."]["bwsp"] = {
        "WindowBounds": "{{160, 120}, {720, 460}}",
        "ShowStatusBar": False,
        "ShowTabView": False,
        "ShowToolbar": False,
        "ShowPathbar": False,
        "ShowSidebar": False,
        "ContainerShowSidebar": False,
    }
    store["."]["icvp"] = {
        "viewOptionsVersion": 1,
        "backgroundType": 2,
        "backgroundImageAlias": Alias.for_file(str(mount / ".background.tiff")).to_bytes(),
        "arrangeBy": "none",
        "iconSize": 112.0,
        "textSize": 14.0,
        "labelOnBottom": True,
        "showItemInfo": False,
        "showIconPreview": False,
        "scrollPositionX": 0.0,
        "scrollPositionY": 0.0,
    }
    store["Phux.app"]["Iloc"] = (180, 248)
    store["Applications"]["Iloc"] = (540, 248)
    store["Install Phux.txt"]["Iloc"] = (650, 382)
