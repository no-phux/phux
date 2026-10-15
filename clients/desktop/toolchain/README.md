---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-13
---

# Desktop framework toolchain

**TL;DR.** The desktop builds against GPUI Kit 0.7.1 and the exact GPUI
snapshot it was released with, `gpui-pre` 0.3.8. Three of those crates carry
reviewed local patches. `source.json` pins each crates.io archive and each
patch by SHA-256; `bootstrap.py` materializes the patched copies under
`gpui/` (ignored build state), and `native/Cargo.toml` points
`[patch.crates-io]` at them.

## Bootstrap

`just desktop-source` runs `python3 clients/desktop/toolchain/bootstrap.py`.
It downloads `gpui-pre`, `gpui-pre-apple` and `gpui-pre-macos` from
crates.io (cached under `clients/desktop/.cache/crates`), verifies them,
applies `patches/` in order, and moves the result to `gpui/`. The output is a
pure function of the pins. An existing `gpui/` is verified against a fresh
materialization and never overwritten: any difference fails, so unrecorded
edits stay in place for inspection. `--check` verifies without writing.

Every other crate in the graph, including Kit itself, resolves from
crates.io through `native/Cargo.lock`, built `--locked`.

## Changing the framework

```sh
python3 clients/desktop/toolchain/bootstrap.py --edit    # gpui/ becomes a git repo
# edit under gpui/, then amend the matching patch commit, or add a commit
git -C clients/desktop/toolchain/gpui commit --amend      # or: commit -m 0003-name
python3 clients/desktop/toolchain/bootstrap.py --export  # rewrite patches/ + pins
```

`--edit` lays the series out as one commit per patch over a `base` commit of
the pristine archives. `--export` writes each commit back as
`patches/<subject>.patch`, removes patch files no commit names, and rewrites
`source.json`. The round trip is byte-identical when nothing changed.

## Menu glass preview

The main menu uses `NSGlassEffectView` on supported macOS versions, with a
solid Kit popover surface as the fallback. The platform patch preserves the
existing GPUI rendering view and Metal layer inside the native material.
AppKit hierarchy changes run outside the window-state mutex because they can
synchronously call back into GPUI.

Run the production menu component as a standalone native preview:

```sh
mise exec -- cargo run --manifest-path clients/desktop/native/Cargo.toml \
  --features ui-fixtures --example glass_menu
```

The fixture needs no daemon, socket, installed app, or production state.
It starts in dark mode; set `PHUX_GLASS_PREVIEW_LIGHT=1` for light mode. Host
and pane pages are interactive; command choices dismiss the fixture. The
`ui-fixtures` feature is excluded from shipping builds. Append `-- --bench`
to measure menu creation after warming a GPUI window. This opens no visible
windows and reports elapsed creation time including the initial draw; it does not
measure compositor presentation latency or frame rate.

Surface height and selection use interruptible springs. Entrance and exit
opacity affect the complete native window, including the material. All motion
respects GPUI’s reduced-motion preference and stops scheduling frames once settled. The popup
has its own render tree, so its animation does not redraw the terminal.

GPUI offscreen captures omit native compositor effects. Inspect the live
window for material QA; a scene-only pixel fixture proves layout, not glass.

## Upgrading GPUI Kit

Kit pins its GPUI snapshot exactly. Bump `gpui-kit`, `gpui-pre` and
`gpui-pre-platform` together in `native/Cargo.toml`, update the three
archive pins in `source.json` to the snapshot Kit names, re-run `--edit`, and
resolve whatever the patches no longer apply to. Then `cargo update` the
host lock and run the desktop gates.

## macOS prerequisites

GPUI is built with `runtime_shaders`: Metal shaders compile when the app
starts, so a build does not need Apple's offline Metal compiler. Xcode's SDK
and command line tools are still required.

## Provenance and redistribution

Sources: <https://crates.io/crates/gpui-pre> (a snapshot of Zed's
`crates/gpui`, Apache-2.0) and <https://github.com/longbridge/gpui-kit>
(Apache-2.0). Every resolved dependency keeps its own license; packaging must
collect the notices of the actual linked graph.
