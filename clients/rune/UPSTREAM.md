# Rune upstream dependency

Rune tracks `dev`, with an immutable revision in this client's own manifest
and lockfile. The [upstream follower](../../scripts/rune-upstream/README.md)
reviews and qualifies changes rather than treating a successful update as
application compatibility.

## Pin agreement and IME adoption

Keep all four Rune dependencies and the six `[patch.crates-io]` entries on
the same immutable upstream revision: Cargo does not inherit Rune's workspace
patches. All ten pins, including `winit`, use upstream Rune. Upstream
[#1454](https://github.com/anomalyco/rune/pull/1454) supplies `ImeSync`, which
suppresses unchanged cursor-area calls and republishes after movement,
scaling, reset or input-context reentry. The temporary winit fork and its
six-helper-test gate are retired.

Run the actual upstream regressions named in [Develop](DEVELOP.md), from the
pinned upstream workspace. They do not replace client input tests,
real-compositor Wayland CI or packaged/native qualification.

## Phone modal compatibility hold

The pin is held at `d16ece223d6b86c4ab5ee4e95819ed9e511c181d`, immediately
before upstream [#1550](https://github.com/anomalyco/rune/pull/1550)
(`a7eefe09a6cb`). Its stricter modal scope makes the phone's sibling
keyboard/accessory inert. A persistent outer modal also changes nested
focus-return lifetimes; positioned popover anchors are not a layout-neutral
ownership API.

Preserve the existing input tests and geometry when adopting this boundary:
do not exempt arbitrary siblings, bypass inert controls through native event
dispatch, or move keys with a fake popover. A correct adaptation must retain
modal lifetimes, background scroll/focus identity and keyboard/accessory
layout while keeping input out of underlying terminals. Later upstream
changes remain pending that ownership adaptation.
