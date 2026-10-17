---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-17
---

# Desktop framework toolchain

**TL;DR.** The desktop builds against GPUI Kit 0.7.3 on
[gpui-fast](https://github.com/longbridge/gpui-fast) 0.1.9, a GPUI fork with
retained views, layout and text measurement, at the Zed revision of the
`gpui-pre` 0.3.9 snapshot Kit pins. Kit's `gpui-pre` crates are served by
small compatibility crates in `compat/` that re-export gpui-fast. Five
gpui-fast crates carry reviewed local patches. `source.json` pins each
crates.io archive and each patch by SHA-256; `bootstrap.py` materializes the
patched copies under `gpui/` (ignored build state), and `native/Cargo.toml`
points `[patch.crates-io]` at them and at `compat/`.

## Bootstrap

`just desktop-source` runs `python3 clients/desktop/toolchain/bootstrap.py`.
It downloads `gpui-fast`, `gpui-fast-apple`, `gpui-fast-macos`,
`gpui-fast-linux` and `gpui-fast-wgpu` from
crates.io (cached under `clients/desktop/.cache/crates`), verifies them,
applies `patches/` in order, and moves the result to `gpui/`. The output is a
pure function of the pins. An existing `gpui/` is verified against a fresh
materialization and never overwritten: any difference fails, so unrecorded
edits stay in place for inspection. `--check` verifies without writing.

Every other crate in the graph, including Kit itself, resolves from
crates.io through `native/Cargo.lock`, built `--locked`.

## gpui-fast and Kit

GPUI Kit depends on GPUI through `gpui-pre`, `gpui-pre-macros`,
`gpui-pre-platform` and `gpui-pre-web` at the exact version it pins. Cargo's
`[patch]` replaces a package only with one of the same name and a matching
version, so `compat/` holds one crate per name at that version, each
re-exporting the published gpui-fast crate of the same library name
(`pub use gpui_fast::*`) and forwarding its features. They are adapted from
gpui-fast's own `compat/` directory, which depends on gpui-fast by path; these
depend on the crates.io release. The support crates gpui-fast itself uses
(`gpui-pre-util`, `gpui-pre-sum-tree`, `gpui-pre-collections` and the rest)
are the genuine 0.3.9 snapshot crates, shared with Kit.

gpui-fast redraws a view only when something it read changed: an entity or
global it read, a `ListState` or `ScrollHandle` it tracked, the pointer, its
bounds or hover, or a `cx.notify()`. The window's root view is retained like
any other. State outside those (a runtime frame, an `Arc<Mutex<_>>`, the
clock) must notify the entity that renders it. Running with
`GPUI_VIEW_RETENTION=0` draws every view from scratch each frame, which rules
retention in or out when something looks stale.

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

## Upgrading GPUI Kit or gpui-fast

Kit pins its GPUI snapshot exactly, and a gpui-fast release names the
snapshot crates it builds on (`gpui-pre-util` and siblings, `=0.3.x`). Move
them together: bump `gpui-kit`, `gpui-pre` and `gpui-pre-platform` in
`native/Cargo.toml` and the version of every crate in `compat/` to the
snapshot Kit names, the gpui-fast requirement in `compat/` to a release built
on that snapshot, and the five archive pins in `source.json` to that
release. Re-run `--edit`, resolve whatever the patches no longer apply to,
`--export`, then `cargo update` the host lock and run the desktop gates.

## Linux submission and capture bridge

X11 and Wayland report native visibility from their existing mapped/occlusion
and xdg-shell suspended state. As with GPUI's `WindowVisibility`, compositing
X11 window managers and older Wayland compositors cannot report all occlusion.
An unconfigured Wayland surface is not visible.

Each successful wgpu submission gets a process-unique id and an
`on_drawable_reported` callback with `confirmed = false`. Failed acquisition,
rendering, or device recovery drops the callback. Neither wgpu `present()` nor
a Wayland frame callback proves that the display showed that exact drawable;
the bridge never calls a submission a confirmed presentation. macOS's Metal
presented-time confirmation is unchanged. gpui-fast's opt-in composed windows
retain their existing `draw_composed` path and do not report drawable receipts.

`render_to_image` renders the scene into an offscreen texture using the live
window's core and sprite atlas, then uses the headless renderer's bounded
readback path. Readback converts the GPU's premultiplied colors to straight
alpha for RGBA/PNG consumers, including linear-space unpremultiplication for
sRGB targets. It never acquires or presents a swap-chain texture, acknowledges
a recovery fence, or consumes errors needed by normal device recovery. Capture
excludes native surfaces, compositor decorations and effects and may wait for
the GPU.

Path intermediates contain premultiplied color. Their shared wgpu blend state
uses source-over for both RGB and alpha, including transparent native windows,
cached layer tiles and captures; adding destination alpha would corrupt the
straight-alpha result. This correction leaves opaque destination pixels
unchanged. Quad/path overlap tests compare transparent PNG compositing against
direct rendering on opaque white in both blend modes and all four capture
formats.

**Recovery limitation:** native projection recovery currently requires
`on_drawable_presented`, which accepts only confirmed presentation. Linux's
submission-only receipts deliberately cannot clear that fence. A different
recovery policy or real native presentation feedback must be designed outside
this bridge before claiming equivalent recovery behavior.

The wgpu patches include submission-receipt, shared-atlas, and translucent
capture/PNG-compositing tests. Run the dependency's unit tests on Linux:

```sh
mise exec -- python3 clients/desktop/toolchain/test-wgpu.py
# Optional: retain build outputs in a caller-owned, isolated directory.
CARGO_TARGET_DIR=/path/to/isolated/target mise exec -- \
  python3 clients/desktop/toolchain/test-wgpu.py wgpu_renderer::tests
```

The script verifies/materializes pinned sources, copies wgpu into a disposable
standalone Cargo workspace with the patched GPUI dependency, and reuses the
native lockfile's versions in a temporary lockfile. It provisions shared
rounded-clip fixtures and downloads the three inherited font fixtures at an
immutable Zed revision. Additional arguments are forwarded to
`cargo test --workspace --lib --features test-support`; no committed lockfile
or materialized source is changed. Capture tests require Vulkan/GL (lavapipe
is sufficient); the Linux backend does not use macOS Metal.

## macOS prerequisites

GPUI is built with `runtime_shaders`: Metal shaders compile when the app
starts, so a build does not need Apple's offline Metal compiler. Xcode's SDK
and command line tools are still required.

## Provenance and redistribution

Sources: <https://crates.io/crates/gpui-fast> (Longbridge's fork of Zed's
`crates/gpui`, Apache-2.0, from <https://github.com/longbridge/gpui-fast>),
<https://crates.io/crates/gpui-pre> (the snapshot crates gpui-fast and Kit
share, Apache-2.0) and <https://github.com/longbridge/gpui-kit>
(Apache-2.0). `compat/` is adapted from gpui-fast's `compat/` (Apache-2.0).
Every resolved dependency keeps its own license.
`scripts/notices.ts` writes the notices of the actual crate graph into
`Phux.app`.

`gpui_util` and `gpui_shared_string` (published as `gpui-pre-util` and
`gpui-pre-shared-string` 0.3.9) once carried no license in Zed. Their
Apache-2.0 label now rests on upstream, not on the republisher's manifest.
Zed [#63573](https://github.com/zed-industries/zed/pull/63573)
(`ac5af8b9e1`) declared `license = "Apache-2.0"` for both, and each crate
directory carries `LICENSE-APACHE`. Both packages record
`zed-rev = 93d745668805` in `package.metadata.gpui-pre`, a revision after
that change, where both upstream manifests declare Apache-2.0. A `gpui-pre` bump repeats
this check for any crate that was not Apache-2.0 upstream at its new pin.
