---
audience: contributors
stability: stable
last-reviewed: 2026-07-27
---

# 0060 — Self-contained session recording

**TL;DR.** Recording is a consumer-side projection over the already-normative
`ATTACH_TERMINAL` observer contract: zero wire change, zero protocol version
bump. GIF and APNG are encoded in-process — no `agg`, no `gifski`, no
`ffmpeg` — and glyphs come from a vendored 1-bit bitmap font, which keeps every
output pixel an exact cell color and makes GIF quantization lossless. The
default asciicast version is 2.

Status: Accepted
Date: 2026-07-27

## Context

phux had no way to produce a shareable artifact of a session. The README's
demo asset was recorded with third-party tooling, and
[`../docs/consumers/tui.md`](../docs/consumers/tui.md) carried a design sketch
for a server-side recorder (`phux capture --record`, `phux play`) that was
never built.

Two facts constrained the shape of the real thing. First, the reference server
already exposes exactly the subscription a recorder needs:
[`../docs/spec/L1.md`](../docs/spec/L1.md) §5.1 specifies `ATTACH_TERMINAL` as
a non-resizing observer that primes the caller with one authoritative
`TERMINAL_SNAPSHOT` and then streams `TERMINAL_OUTPUT` deltas, with no
session-scoped `ATTACH` and no viewport read. Second, the server now rejects
any client whose protocol `major.minor` differs, so a minor version bump is a
hard, no-grace, fleet-wide break rather than a soft upgrade.

## Decision

1. **Recording is consumer-side.** Both surfaces live in the client and the
   binary. `phux rec TARGET -o PATH` subscribes as an `ATTACH_TERMINAL`
   observer and projects the frames into asciicast events; `phux --rec PATH`
   tees the attach driver's single composited `RenderSink`. Neither adds a
   frame type, a command tag, a `ServerFeature` bit, or a spec sentence.
2. **No external binaries, ever.** GIF and APNG are produced in-process from
   two permissively licensed, pure-Rust crates (`gif`, `png`). The pipeline
   replays the captured bytes through libghostty offline and rasterizes each
   sampled frame itself.
3. **Glyphs come from a vendored 1-bit bitmap face** (Spleen 8x16,
   BSD-2-Clause) plus procedural drawing for `U+2500..=U+259F`, not from a
   TTF rasterizer.
4. **The archival artifact is the `.cast`.** Every export goes through one, so
   an animation is always re-derivable at a different frame rate or idle limit
   without re-recording. Both surfaces default to **asciicast v2**;
   `--cast-version 3` is opt-in.
5. **Input is never captured.** No `i` events, on either surface, with no
   opt-in flag. Passwords do not belong in a recording.

## Why

**Consumer-side.** A server-side recorder is the more capable feature — it
survives the recorder process dying and can record a session nobody is
attached to — but it costs a new command tag, a new feature bit, a spec
change, and a `0.6.0` to `0.7.0` minor bump. Under the hard version gate that
bump means a `0.7.0` client cannot talk to a `0.6.0` server at all, for every
user, to gain a recorder. The observer contract already delivers the bytes;
paying a fleet-wide break for durability we do not yet need is the wrong
trade at this maturity.

**No external binaries.** The premise of the feature is that `phux rec` works
on a machine with nothing else installed. Beyond that, the obvious candidates
are license-blocked: asciinema's own `agg` is GPL-3.0-or-later and the
`gifski` encoder under it is AGPL-3.0-or-later, both incompatible with phux's
`MIT OR Apache-2.0` even as a vendored fork. Hand-rolling a GIF89a container
was rejected separately: the 255-byte sub-block chunking, LZW code-width
growth, and clear-code reset are precisely the details that yield a file that
plays in one decoder and breaks in another, and that is a bug class we would
own forever for no user-visible gain.

**A 1-bit font.** This is the decision a future contributor is most likely to
try to "improve", so the mechanism matters. With a 1-bit face every output
pixel is exactly some cell's resolved foreground or resolved background —
rendering introduces no new colors. The distinct-color count of a whole
recording therefore stays under 256 for essentially every real session, GIF's
global color table becomes an exact table, and quantization degenerates to a
hash-map lookup with no dithering and no loss. An antialiasing rasterizer
blends foreground into background at 256 coverage levels and spawns up to 256
intermediate colors *per (foreground, background) pair*, which forces real
quantization, forces dithering, and compresses far worse under LZW because
flat blocks stop being flat.

**asciicast v2 as the default.** Upstream states that v3 is *not* backward
compatible with v1 or v2: the header schema changed and event times became
relative intervals rather than absolute offsets. v2 is read by asciinema CLI
2.x and 3.x, player 2.6 and later, and server builds back to 2017; v3 requires
CLI 3.0, player 3.10.0, and a 2025 server. There is no consumer that reads v3
but not v2, so v2 is strictly the more portable default and v3 is a flag.

## Tradeoffs

- No recording of a detached session, none that survives the recorder process
  dying, and no server-side retention or lifecycle. Recording is bounded by
  the life of the process that asked for it.
- Timing resolution is the server's output pacing (default 60 Hz,
  [`../docs/spec/proto.md`](../docs/spec/proto.md) §8.1), not per-byte. There
  is no per-byte timestamp on the wire and adding one would be a wire change,
  so sub-frame keystroke cadence is not recoverable and a fast flood replays
  chunkier than it looked live.
- A recording started mid-session opens on the current viewport, because the
  observer's priming snapshot is requested without scrollback.
- Glyph coverage is narrow: Latin, Greek, Cyrillic, Braille, box drawing, and
  Powerline. CJK and other wide glyphs and color emoji render as tofu boxes.
  That is a permanent consequence of decision 3, not a gap to be closed with a
  bigger bitmap.
- Heavy box strokes are approximated as two pixels and double strokes as two
  one-pixel lines, because an 8-pixel-wide cell admits no third option.
- The vendored font carries a BSD-2-Clause obligation that cargo-deny cannot
  see, discharged by hand in `LICENSE-SPLEEN`.

## Alternatives

- **Server-side recorder.** Rejected for the version-gate cost above. It stays
  the natural v2 if durable, unattended recording is ever asked for; it would
  be additive over this decision, not a replacement for it.
- **Shell out to `agg`, `vhs`, or `ffmpeg`.** Rejected: absent on a clean
  machine, and the asciinema toolchain is GPL/AGPL.
- **Hand-rolled GIF89a writer.** Rejected: roughly 250 feasible lines whose
  failure mode is a file that plays in one viewer and not another.
- **`fontdue` or `ab_glyph` rasterizing a TTF.** Rejected on the color-count
  argument above. `fontdue` is additionally rejected for declaring its own
  maintenance status experimental and carrying dozens of `unsafe` sites into a
  workspace whose library crates are `#![forbid(unsafe_code)]`.
- **Dithering the palette.** Rejected: terminal content is flat blocks, and
  Floyd-Steinberg adds per-pixel noise that hurts both the visual result and
  inter-frame compression. Median cut is the upgrade path if someone ever
  records sixel or photographic content.
- **Shipping a player (`phux play`).** Rejected as out of scope: `asciinema
  play` already exists, and a `.cast` is the interoperable artifact precisely
  so we do not have to. Tracked as deferred work. **Revisited by
  [ADR-0064](./0064-playback-as-a-pane.md)**, which splits the rejection in
  two: a shell-level viewer stays unbuilt for exactly this reason, while
  playback *as a pane* — a Terminal whose PTY is fed from the cast, and which
  every existing verb can drive — ships, because `asciinema play` cannot
  produce that object.

## Related

- ADR-0003 — one server per user, one current-thread runtime.
- ADR-0013 — libghostty bytes on the wire; the recorder consumes them directly.
- ADR-0017 — the TUI is not protocol-privileged, and neither is the recorder.
- ADR-0032 — graceful upgrade; a reconnect continues one recording.
