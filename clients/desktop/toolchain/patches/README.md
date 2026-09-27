---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-27
---

# Local GPUIX and Zed patches

**TL;DR.** Ordered, reviewed local patches over the pinned GPUIX and Zed
revisions in `../source.json`, which also records each patch's SHA-256. Source
bootstrap verifies the pinned inputs first, then applies the series. None has
been published upstream.

## Series

| Patch | Applies to | What it does |
|---|---|---|
| `0001-native-extensions.patch` | GPUIX | Public facade over GPUIX's private custom-element traits, opaque view, callback alias and exact GPUI crate, plus a startup-only installer that runs after built-ins and seals on first view/registry creation. No GPUI behavior changes. |
| `0002-multi-window.patch` | GPUIX | One embedded macOS application with a GPUI window per renderer. Scroll, list, automation, painted-text, selection and search state is keyed by window; view release fences queued callbacks; `QuitMode::Explicit` lets `tick()` return false with no windows and reopen later. Non-macOS keeps the single-window path. |
| `0003-solid-native-elements.patch` | GPUIX | `registerCustomElementType` / `isCustomElementType` in `@gpuix/native/host`, Solid mounting of registered tags, and `GpuixRenderer.hasCustomElementType` so a missing native factory fails instead of rendering `gpui::Empty`. |
| `0004-close-window-on-reset.patch` | GPUIX | `resetRender()` closes a live macOS window before dropping the Solid root; dropping it during thread-local teardown panicked in the profiler journal. JS only. |
| `0005-drawable-presented.patch` | Zed | `Window::on_drawable_presented` and AppKit visibility, the presentation receipt described in [PRESENTATION.md](../../native/PRESENTATION.md). |

## Rules

- Change a patch only together with its hash in `source.json`; bootstrap
  rejects partially applied or independently edited patches and preserves
  unexpected source edits instead of resetting them.
- Generate patches against a real Git checkout of the pinned baseline plus the
  earlier patches. `git apply` inside an untracked directory of another
  repository can silently skip paths.
- Unified diffs keep a leading space on empty context lines; `.gitattributes`
  disables whitespace fixes for `*.patch`.
