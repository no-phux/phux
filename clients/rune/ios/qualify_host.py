"""Host-side fixture for iOS qualification: which simulator, and the server env.

The lane never selects a user's simulator or inherits a production phux profile.
"""


def version_tuple(value):
    return tuple((list(map(int, value.split("."))) + [0, 0])[:3])


def available_ios_runtime(runtime):
    return (runtime.get("isAvailable") and ".iOS-" in runtime["identifier"]
            and version_tuple(runtime["version"]) >= (26, 0, 0))


def simulator_pair(runtimes, device_types):
    """Use runtime-supported IDs, not a device name guessed from OS version."""
    phones = {d["identifier"] for d in device_types if d.get("productFamily") == "iPhone"}
    available = filter(available_ios_runtime, runtimes)
    for runtime in sorted(available, key=lambda r: version_tuple(r["version"])):
        compatible = [d for d in runtime.get("supportedDeviceTypes", [])
                      if d["identifier"] in phones]
        if compatible:
            device = min(compatible, key=lambda d: (d["name"] != "iPhone 17", d["name"]))
            return runtime["identifier"], device["identifier"]
    raise ValueError("no available iOS >=26 runtime with an advertised compatible iPhone")


def server_environment(scratch):
    home = scratch / "home"
    home.mkdir()
    config = scratch / "config"
    config.mkdir()
    prefs = scratch / "prefs"
    prefs.mkdir()
    return {"HOME": str(home), "XDG_CONFIG_HOME": str(config),
            "XDG_DATA_HOME": str(scratch / "data"), "XDG_STATE_HOME": str(scratch / "state"),
            "PATH": "/usr/bin:/bin", "SHELL": "/bin/sh", "TERM": "xterm-256color",
            "TMPDIR": str(scratch), "PHUX_PROFILE": "rune-ios-qualification",
            "PHUX_WS_SECURE": "1",
            "RUNE_PREFS_DIR": str(prefs)}
