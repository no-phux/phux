---
audience: contributors
stability: evolving
last-reviewed: 2026-10-08
---

# phux architecture film

**TL;DR.** The landing page pairs an opt-in Psychopomp film with an interactive,
keyboard-accessible SVG explainer. Neither connects to a running phux server.

## Interactive companion

`../src/components/motion/PhuxExplainer.tsx` uses the site's existing React
integration, without a browser animation dependency. Four stories cover
persistence, human/agent coordination, federation, and input/output. A single
clock drives minimum-jerk signal travel; seeking renders the same pose in any
order. Playback pauses offscreen and in hidden tabs. Reduce Motion replaces
travel with still steps; server-rendered prose and docs links survive without JS.

The film uses Psychopomp directly; the SVG companion applies its
[explainer-motion](https://github.com/kitlangton/psychopomp/blob/a17d31587d06de0a6706d527f04212caa38a1df3/.agents/skills/explainer-motion/SKILL.md)
principles: quiet wires, a source/path/destination, continuous motion, and a
local arrival ring. Facts follow [phux concepts](../../CONCEPTS.md), not a
promise of crash recovery, process migration, or universal satellite operations.

Build before browser acceptance:

```sh
cd ..
bun run check
bun run test
bun run build
bun run check:links
bun run verify:motion
```

`verify:motion` serves the built site on an ephemeral loopback port and uses
headless Chrome. Screenshots go to ignored `.motion-check/`, or the directory
selected by `PHUX_MOTION_ARTIFACTS`. It does not allocate live demo sessions.

## Rendered film

`../public/motion/phux-architecture.mp4` is an actually rendered, silent
16-second architecture film: 1920 x 1080, 60 fps, 960 frames, H.264 / yuv420p.
The poster is `../public/motion/phux-architecture-poster.jpg`.
The MP4 is 4,582,037 bytes; SHA-256:
`606a1d4547fe53b4de78ebe9c7e7fde1cd4b385507341590ed3e099ff2cb4592`.

The film establishes human, desktop/web and agent clients, follows structured
input to the server and PTY, follows emitted VT bytes into the persistent
terminal, then detaches a client without removing the server-owned terminal.
Finally, a resource request passes through the federation hub to a remote
machine and a remote view returns. The remote process never moves. Timings are
editorial pacing, not measured latency. Links illustrate responsibilities, not
an exhaustive wire protocol. There are no narration or sound-generation API calls.

## Reproduce

Renderer: [kitlangton/psychopomp](https://github.com/kitlangton/psychopomp), pinned
to `a17d31587d06de0a6706d527f04212caa38a1df3`. The authored Rust Scene Program
is `phux-architecture/src/main.rs`; `phux-architecture.plan.json` is the emitted
plan. Its Cargo manifest is intended to live under Psychopomp's `scenes/`,
where the relative dependency resolves to the pinned renderer's authoring crate.

Requirements: Rust supporting edition 2024, a working wgpu adapter, FFmpeg with
libx264, and ffprobe. The actual render used Rust 1.99.0 and Apple M4 Pro / Metal,
the neutral theme, restrained stage postprocessing, and 24 temporal samples.

From this directory, using a new scratch directory:

```sh
MOTION="$PWD"
SCRATCH="$(mktemp -d)/psychopomp"
git clone --depth 1 https://github.com/kitlangton/psychopomp.git "$SCRATCH"
git -C "$SCRATCH" fetch --depth 1 origin a17d31587d06de0a6706d527f04212caa38a1df3
git -C "$SCRATCH" switch --detach a17d31587d06de0a6706d527f04212caa38a1df3
cp -R "$MOTION/phux-architecture" "$SCRATCH/scenes/phux-architecture"
cd "$SCRATCH"
CARGO_BUILD_JOBS=4 cargo run -p psychopomp-phux-architecture
CARGO_BUILD_JOBS=4 cargo run --release -- plan validate target/phux-architecture.json
target/release/psychopomp plan render target/phux-architecture.json output/study.mp4 --range 2..5.8 --theme neutral
target/release/psychopomp plan render target/phux-architecture.json output/phux-architecture.mp4 --theme neutral
ffprobe -v error -show_streams -show_format output/phux-architecture.mp4
ffmpeg -hide_banner -loglevel error -y -ss 12.29 -i output/phux-architecture.mp4 -frames:v 1 output/phux-architecture-poster.jpg
```

No phux executable or server is used by the scene or renderer. Rendering does not
require credentials. Copy the resulting MP4 and poster into `../public/motion/`
only after inspecting the output.

## Validation performed

The Scene Program compiled and renderer plan validation reported `valid: true`.
A foreground 3.8-second study render passed before the foreground full render.
`ffprobe.json` records the final encoded properties; no audio stream is present
by design. The MP4 was decoded to an 80-frame contact sheet (`contact-sheet.jpg`),
and full-resolution extracted frames were inspected at 4.0, 5.05, 7.8, 12.29 and
15.0 seconds for paths, captions, detach persistence and remote process placement.
This is frame inspection, not a claim of real-time playback review.

Checks passed in the scratch clone:

```sh
CARGO_BUILD_JOBS=4 cargo test --workspace
cargo fmt --check
CARGO_BUILD_JOBS=4 cargo clippy --workspace --all-targets --all-features -- -D warnings
target/release/psychopomp verify baseline --manifest scenes/phux-architecture/verify.json --out target/verify/phux-final
target/release/psychopomp verify compare target/verify/phux-final --manifest scenes/phux-architecture/verify.json
```

The scoped final-scene comparison reported identical plan bytes and 11 identical
frames. A separate shutter snapshot comparison at nine story times reported
zero changed pixels. These repeatability checks are not a before/after regression
baseline for the entire upstream scene catalog. Upstream GPU tests marked
ignored by default were not explicitly enabled. No renderer code was changed.

## License provenance

Psychopomp is MIT licensed; the exact pinned upstream notice is retained in
`LICENSE-psychopomp.txt`. The scene follows upstream's README / hello example
shape and current `psychopomp::score` APIs. Its source is provided under MIT;
see `LICENSE-scene.txt`.

The film uses upstream's bundled CommitMono font, distributed under the SIL
Open Font License 1.1; its full notice is retained in `LICENSE-CommitMono.txt`.
No upstream sound effects, generated narration, footage, icons or portraits
are used. The pinned upstream Cargo.lock governs renderer dependencies; those
dependencies retain their respective upstream licenses.
