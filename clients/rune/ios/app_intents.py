"""The app's App Intents metadata (Metadata.appintents), as Xcode writes it:
without it the system never sees the app's shortcuts, so the Action Button
can't offer Dictate (Sources/Dictation.swift)."""

import json
from pathlib import Path

MODULE = "PhuxRune"
# What the compiler records constant values for: the App Intents types the
# metadata processor reads (Xcode passes the same list).
PROTOCOLS = ["AppIntent", "EntityQuery", "AppEntity", "TransientEntity", "AppEnum",
             "AppShortcutProviding", "AppShortcutsProvider", "AnyResolverProviding",
             "AppIntentsPackage", "DynamicOptionsProvider", "IntentValueQuery", "Resolver"]


def consts(out):
    return out / f"{MODULE}.swiftconstvalues"


def swift_flags(out):
    """The swiftc flags that name the module and emit its constant values."""
    protocols = out / "app-intents-protocols.json"
    protocols.write_text(json.dumps(PROTOCOLS))
    return ["-module-name", MODULE, "-wmo", "-emit-const-values-path", consts(out),
            "-Xfrontend", "-const-gather-protocols-file", "-Xfrontend", protocols]


def metadata(out, env, run, app, sources, sdk_path, swift_target, minimum_ios):
    source_list = out / "app-intents-sources.txt"
    source_list.write_text("".join(f"{source}\n" for source in sources))
    const_list = out / "app-intents-consts.txt"
    const_list.write_text(f"{consts(out)}\n")
    swiftc = run(["/usr/bin/xcrun", "--find", "swiftc"], env, out, "find-swiftc", stdout_only=True).strip()
    version = run(["/usr/bin/xcodebuild", "-version"], env, out, "xcode-version", stdout_only=True).split()[-1]
    run(["/usr/bin/xcrun", "appintentsmetadataprocessor", "--output", app,
         "--toolchain-dir", Path(swiftc).parents[2], "--module-name", MODULE, "--sdk-root", sdk_path,
         "--xcode-version", version, "--platform-family", "iOS", "--deployment-target", minimum_ios,
         "--target-triple", swift_target, "--binary-file", app / MODULE,
         "--source-file-list", source_list, "--swift-const-vals-list", const_list, "--force"],
        env, out, "app-intents", timeout=300)
    if not (app / "Metadata.appintents" / "extract.actionsdata").is_file():
        raise ValueError("the App Intents metadata was not written")
