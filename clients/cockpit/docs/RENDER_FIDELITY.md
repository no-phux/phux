# Render fidelity: what each instrument can and cannot see

`phux-cockpit-2ml.3`. Written after establishing, with measurements rather than
argument, exactly which defect classes the automated screenshot path is blind
to and what can be done about it without a Screen Recording permission.

Read this before trusting a screenshot, and before writing a test that claims
to check what the terminal looks like.

---

## The short version

| Instrument | Sees | Blind to | Needs TCC |
|---|---|---|---|
| `native automate screenshot` | display list, layout, colour choices, which commands were emitted | everything the real macOS rasterizer does: CoreText outlines, hinting, font smoothing, CG blend arithmetic, device colour space | no |
| `scripts/host-raster-check.sh` | the real host rasterizer's output for a fixed row | layout, what the app actually emitted, anything outside one command | no |
| `scripts/capture-gpu-ink.sh` | the app's real composited frame: the host rasterizer AND the layout that fed it | anything the composite pass does differently from the shipping present path — it is a prototype flag, not the default; needs a patched SDK | no |
| eyes on glass / screen capture | everything | nothing | **yes** |

The middle row was empty when this document was first written, and section 2
is the survey that emptied it. Section 6 is how it got filled: one SDK-side
patch, kept at docs/sdk-patches/composite-cell-grid.patch, plus the
measurements proving the capture sees a defect the screenshot does not.

---

## 1. Why the reference screenshot cannot see a rasterizer defect

`native automate screenshot <label>` does not photograph anything and does not
read the GPU. The verb lands in `runtime/flow.zig:944`, is served by
`publishAutomationScreenshot` (`runtime/flow.zig:1043`), and the pixels come
from `renderCanvasScreenshot` (`runtime/canvas_frame.zig:801`, body at `:819`).
That function re-plans the view's retained display list as a full repaint and
rasterizes it with `canvas.ReferenceRenderSurface`
(`runtime/canvas_frame.zig:843-849`) — a pure-Zig CPU renderer.

Its glyphs come from `primitives/canvas/font_ttf.zig`, a std-only TrueType
outline parser, filled by a vector rasterizer held in per-thread scratch
(`primitives/canvas/reference.zig:61-79`). CoreText is never called on this
path. The PNG carries no colour-space chunk (`primitives/canvas/png.zig:55-77`).

The app's real frames take an entirely different route. Cockpit presents on the
**packet** path (`gpu_present_path=packet` in the automation snapshot, now asserted
by `scripts/automate-smoke.sh`), where the AppKit host decodes the packet
and rasterizes each command with CoreText into a `CGBitmapContext`
(`platform/macos/appkit_host.m:5256` for the cached raster, `:5473` for the
direct full-surface draw, `:4569` for the composite scratch). All three set
`CGContextSetShouldSmoothFonts`, added for `phux-cockpit-aht`. That call does
NOT fix anything: see "The smoothing calls are a no-op" below, where the two
shipping SDK commits are measured against each other and come out identical.

So: two renderers, two glyph rasterizers, and the screenshot only ever shows
the one that is not on screen.

### Proven, not asserted

Reintroducing the `aht` defect (`CGContextSetShouldSmoothFonts(..., true)` →
`false` at the three call sites above, in a local copy of the pinned SDK) and
building the app against it:

```
binary                                     reference screenshot sha256
8ed94141881ba893…  fixed SDK               6b4f3b582a7bbf926d0605f44bf9b7af1f07c84c86b61c6f84c4acba66fcd3d2
57ba94b9290ad790…  smoothing disabled      6b4f3b582a7bbf926d0605f44bf9b7af1f07c84c86b61c6f84c4acba66fcd3d2
```

Two different binaries, one byte-identical screenshot. The screenshots were
taken twice per run one second apart and matched within each run, so the
comparison is not measuring capture jitter. Same numbers on the same basis
(`scripts/measure-png-ink.m`, whole image): `mean_luma=21.6076 solid=132410
lit=177883` for both.

That result stands. It proves the screenshot is blind to the rasterizer. It
does NOT prove the smoothing change fixed anything, and the next section is
why.

### The smoothing calls are a no-op

`CGContextSetShouldSmoothFonts(ctx, false)` really does thin the glyphs, and the
harness really does see it. But no build was ever in that state. The state
before the fix was the calls being ABSENT, and absent is not false — on this
`CGBitmapContext` the default is smoothing ENABLED.

Three measurements, one harness, one basis, `scripts/host-raster-check.sh`:

| SDK | smoothing calls | mean_luma | solid | lit |
|---|---|---|---|---|
| `e8bd84886` (0.7.1 and 0.8.0 shipped this) | absent | 45.7978 | 4179 | 5103 |
| `f3678832f` (the current pin) | set `true` | 45.7978 | 4179 | 5103 |
| `f3678832f`, locally edited | set `false` | 37.1124 | 3081 | 4272 |

Reproduce the first two rows with

```
./scripts/host-raster-compare.sh e8bd84886
```

The two shipping commits are identical to four decimal places on every
statistic. The +35%/-26.3% that justified the fix was measured against the
third row: a state that exists only in a local edit made to demonstrate the
knob. **Cockpit's glyph weight has not changed since 0.7.1.** Any report that
the text still looks the same after the fix is correct, and is not a second
defect.

The comment in `build.zig.zon` claiming macOS font smoothing "is off by default
on a transparent backing" is false as measured here; the row above is the
refutation.

This is the general trap, and it is what `scripts/host-raster-compare.sh`
exists to close: a control you constructed tells you the knob is wired up. It
cannot tell you the knob was ever in the other position. Compare two commits
that shipped.

Meanwhile the same defect, measured through the host's own rasterizer, is
large — see section 3.

---

## 2. Capture paths evaluated, and why each one is or is not viable

The goal was a capture of the app's REAL pixels that runs without the macOS
Screen Recording permission, since TCC is unavailable in CI.

**`CGWindowListCreateImage`, `CGDisplayCreateImage`, `ScreenCaptureKit`,
`screencapture(1)`** — all TCC-gated since macOS 10.15. Not usable in CI. The
SDK does create an `SCStream` (`appkit_host.m:7856-7928`) but configures it 2x2
and drops every non-audio buffer at `:7931`; it is the audio capture service and
yields no image.

**`NSView cacheDisplayInRect:` / `CALayer renderInContext:`** — no TCC, but the
canvas is a `CAMetalLayer` and neither reads a Metal drawable. Not present in
the SDK either, so it would need in-process code the app cannot inject.

**`gpu_sample` in the automation snapshot** — this one IS a real GPU readback,
blitted from the presented drawable (`appkit_host.m:6337-6344`) and published as
`gpu_sample=0xff090b0f`. It is one pixel at the surface centre. Real, TCC-free,
automation-reachable, and far too coarse to measure glyph weight. Worth knowing
it exists; not an instrument.

**`NATIVE_SDK_GPU_SHOT_DIR`** — the SDK's only real-pixel-to-PNG path
(`appkit_host.m:5132-5155`): it reads the composited canvas texture back with
`getBytes` and writes a PNG. No TCC, no focus, no window required. This looked
like the answer.

It is not available to Cockpit. The dump only fires from the GPU composite pass
(`appkit_host.m:5030`), which is gated on `NATIVE_SDK_GPU_COMPOSITE=1`, and that
pass refuses any command whose kind is not in its known-kind list at
`appkit_host.m:4714-4716`:

```objc
const BOOL knownKind = [kind hasPrefix:@"fill_rect"] || … || [kind isEqualToString:@"draw_image"];
if (!knownKind) return 0;
```

`cell_grid` is not in that list, and a Cockpit terminal frame is nothing but
`cell_grid` commands. Measured consequence, from the automation snapshot with
the app running the same pane both times:

```
NATIVE_SDK_GPU_COMPOSITE unset   gpu_present_path=packet
NATIVE_SDK_GPU_COMPOSITE=1       gpu_present_path=pixels  present_fallback=missing_service
                                 present_fallback_frames=147   shots written: 0
```

Every packet present is refused and the runtime falls back to the CPU **pixels**
path — which is the reference renderer. So turning composite mode on does not
capture the real rasterizer; it *replaces* it. That is a real SDK bug, filed as
`phux-cockpit-2ml.7` and then as `phux-cockpit-g1z`.

**Correction, 2026-08-12 (`phux-cockpit-wmi`).** The conclusion that followed
from this — "no SDK affordance captures Cockpit's real frames without TCC" —
was right about the shipped SDK and wrong about what it would cost to change.
The blocker is the missing list ENTRY, not a missing capability: both bitmap
paths under that gate already draw `cell_grid`, and
`NativeSdkPacketCommandRasterCacheable` already answers YES for it. Adding the
kind to the list is one line. With `docs/sdk-patches/composite-cell-grid.patch`
applied to a sandbox copy of the pin, the real bundle reports
`gpu_present_path=packet present_fallback=none` and writes real composited PNGs
at presents 1, 30, 60, …

That is how `wmi` was confirmed and fixed on glass: two runs of the same
binary, same driving script, one config key apart, each asserting
`publisher_pid` against the pid it launched. See the patch's entry in
`docs/sdk-patches/README.md` for the recipe and its traps (`-p1.png` is the
BOOT frame — empty).

The gap this leaves is narrower than the old conclusion: the dump is a readback
of the canvas TEXTURE, so it still cannot see window compositing or display
colour conversion. Section 5 is about that remaining sliver, not about the
whole frame.
Reproduced against the pinned SDK, `publisher_pid` in the snapshot checked
against the pid we launched on both runs:

```
composite unset   gpu_present_path=packet  present_fallback=none            shots written: 0
composite=1       gpu_present_path=pixels  present_fallback=missing_service shots written: 0
```

**Conclusion, as it stood.** No SDK affordance captures Cockpit's real frames
without TCC. A negative result, with the code that makes it negative named
above.

**Conclusion now.** That paragraph was a statement about the pinned SDK, not
about macOS, and the gate it named is one condition long. Section 6 lifts it
with `docs/sdk-patches/composite-cell-grid.patch`. The survey above still
stands for everything else: with an unpatched SDK there is still no capture,
and the four TCC-gated paths are still TCC-gated.

---

## 3. What was built instead: `scripts/host-raster-check.sh`

If the app's frames cannot be captured, the rasterizer can still be exercised
directly — which is where the whole `aht` defect class lives.

`scripts/measure-host-raster.m` `#include`s the **pinned SDK's own**
`src/platform/macos/appkit_host.m` and calls its real
`rasterCacheBuildEntryForCommand:` with a `cell_grid` command built the way the
packet decoder builds one (`appkit_host.m:3613-3690`), plus a `draw_text`
command for contrast. It registers the app's own JetBrains Mono face through the
host's `native_sdk_appkit_register_font`, and compiles with the same flags
`build/app.zig:1157` uses.

This is not a replica. The context creation, the smoothing calls, the CoreText
draw, the face resolution, the two-pass cell order and the colour space are the
host's own code. A future SDK bump that changes any of them moves these numbers
without this repo being touched. `scripts/host-raster-check.sh` refuses to run
against a checkout at any commit other than the one `build.zig.zon` pins, or
against that commit with tracked or untracked source changes. Ignored build
artifacts do not make the checkout dirty.

It needs no window, no focus, no Screen Recording permission, and no running
app, so it runs in CI.

### It catches the defect the screenshot misses

Same harness, same font, same row, only the SDK's three smoothing calls differ:

```
$ ./scripts/host-raster-check.sh --min-solid 4000                       # pinned SDK
kind=cell_grid width=768 height=36 mean_luma=45.7978 solid=4179 lit=5103
kind=draw_text width=768 height=36 mean_luma=36.5460 solid=4068 lit=5021
ok: cell_grid solid=4179 >= 4000
exit=0

$ PHUX_COCKPIT_SDK_SRC=<copy with smoothing disabled> \
      ./scripts/host-raster-check.sh --min-solid 4000
kind=cell_grid width=768 height=36 mean_luma=37.1124 solid=3081 lit=4272
kind=draw_text width=768 height=36 mean_luma=27.4578 solid=3026 lit=4117
FAIL: cell_grid solid=3081 is below the pinned floor 4000.
exit=1
```

`solid` falls 4179 → 3081, a 26.3% loss of fully-inked pixels, while the
reference screenshot of the same two builds is byte-identical. The assertion was
watched failing and watched passing; a floor that has only ever been seen to
pass is not evidence.

### Pinned baseline

Measured on this machine at the pinned SDK `f3678832f`, JetBrains Mono NL
Nerd Font Mono Regular, 13pt, backing scale 2, 48 columns:

| kind | mean_luma | solid (>127) | lit (>32) |
|---|---|---|---|
| `cell_grid` | 45.7978 | 4179 | 5103 |
| `draw_text` | 36.5460 | 4068 | 5021 |

Deriving command: `./scripts/host-raster-check.sh`

### The floor CI asserts, and how it was chosen

the root `.github/workflows/cockpit-ci.yml` runs the check on every relevant
push and pull request, and `.github/workflows/cockpit-sdk-head.yml` runs it after
repointing `build.zig.zon` at the
fork's branch head. In both jobs,
`scripts/build-automation-cli.sh --checkout-only` materializes the SDK source
declared by the current pin before the check runs:

```
./scripts/host-raster-check.sh --min-solid 4000
```

**The number does not jitter.** Twenty consecutive runs, each recompiling the
harness against the pinned host:

```
for i in $(seq 1 20); do ./scripts/host-raster-check.sh; done | sort | uniq -c
  20 kind=cell_grid width=768 height=36 mean_luma=45.7978 solid=4179 lit=5103
  20 kind=draw_text width=768 height=36 mean_luma=36.5460 solid=4068 lit=5021
```

Twenty out of twenty identical, integers and all four decimal places. An
`-arch x86_64` build of the same harness, run under Rosetta on the same
machine, returns the same three numbers, so the measurement does not even
depend on the ISA it was compiled for.

So the 4.3% between 4179 and 4000 is **not** slack for noise; there is no
noise. It covers the one variable that cannot be measured from a developer
machine: the runner's CoreText, on a macOS build nothing here has ever
rasterized against. The margin costs nothing in power — the defect class this
check exists for lands 4179 → 3081, a 26.3% loss — so no regression that a
floor of 4179 would catch escapes a floor of 4000.

Tighten it only against numbers the CI job has actually printed; the step prints
all six statistics on every run, green or red. Re-derive it, do not adjust it by
feel, if the font, the size or the sample row changes.

### Why a constant and not a stored baseline

The obvious alternative is to check the six numbers into the repo and compare
for equality. It was rejected on three counts:

- An equality check is a claim that a machine this repo has never measured
  produces this laptop's numbers. Nothing above supports that. The first CI run
  would be the experiment, and its failure would be indistinguishable from a
  regression.
- The defect has a direction. Thinner is the bug; thicker never is. An equality
  check goes red on an SDK bump that *improves* glyph weight, and the only
  available remedy is to edit the baseline — which teaches everyone to edit the
  baseline, and that is how a ratchet dies.
- It would be a third place the pinned rasterizer's output is written down,
  beside this file and the CI floor, and the only thing it buys — noticing a
  change that is not a regression — arrives anyway, because the CI step prints
  every statistic into the log on every run, green or red.

### One thing that moves the number and is not a Cockpit regression

`AppleFontSmoothing = 0` in the running user's defaults suppresses smoothing
even when the host explicitly asks for it. Measured against the unmodified pin,
whose three `CGContextSetShouldSmoothFonts(ctx, true)` calls are intact:

| user defaults | mean_luma | solid | lit |
|---|---|---|---|
| unset (this machine — a runner is assumed, not measured) | 45.7978 | 4179 | 5103 |
| `-AppleFontSmoothing 0` | 37.1124 | 3081 | 4272 |
| `-AppleFontSmoothing 1`, `3` | 45.7978 | 4179 | 5103 |
| `-CGFontRenderingFontSmoothingDisabled YES` | 45.7978 | 4179 | 5103 |

Those are the same numbers as flipping the host's smoothing calls to `false`,
which is worth stating plainly: **the user default overrides the SDK's call.**
The fix in section 1 cannot protect a machine whose owner has turned font
smoothing off. That is a second reason the CI step prints
`defaults -currentHost read -g AppleFontSmoothing` before measuring — a red run
on a runner image that changed its defaults can be told apart from a real
regression by reading the log once, instead of by bisecting the SDK.

### What it cannot see

It proves the rasterizer, not the frame. It cannot see a layout mistake, a
colour chosen wrongly upstream, a command that was never emitted, or a defect in
compositing between commands. Reference screenshots remain the right instrument
for those, and they are trustworthy for exactly that.

### Its two headline statistics are blind to a whole defect class

`solid` (luma > 127) and `lit` (luma > 32) are ABSOLUTE thresholds. They ask
"how bright is this pixel", not "how far is this pixel from the ground it sits
on". Dark ink on a dark ground is real ink that neither counts, so a row of
unreadable text and a row of blank space score identically. Measured, at the
pinned SDK, same row, same font, same scale:

| foreground | solid | lit | distinct | peak_delta |
|---|---|---|---|---|
| `#f4f7fb` default text | 4179 | 5103 | 5521 | 235.79 |
| `#1d1f21` ANSI black (SGR 30) | **0** | **0** | 4870 | **19.86** |

The middle row is the whole of `phux-cockpit-wmi`: text that is fully present
in the raster and 19.86 luminance steps out of 255 away from its background.
`solid` and `lit` say "nothing here", which is also what they say about an
empty row.

`scripts/contrast-floor-check.sh` reports two more statistics for exactly this
reason — `distinct` (pixels more than one 8-bit step from the row's own
background) and `peak_delta` (the furthest any pixel gets from it). Use those
whenever the question is legibility rather than weight.

---

## 3b. `scripts/contrast-floor-check.sh` — colour, not weight

Same `#include` trick, different question. `host-raster-check.sh` holds the
colour fixed at the app's foreground and asks how thickly the rasterizer inks a
glyph. This holds the rasterizer fixed and asks how much of that ink survives a
given foreground on a given background.

Its colours are not declared anywhere in the harness. They come out of a
MEASURED test (`src/tests/minimum_contrast_tests.zig`) that paints a REAL
session through `grid.paint` and prints the resolved foreground it handed the
painter, one line per SGR case per floor. The script greps those lines and
feeds them straight to the rasterizer. So both columns of the table are
produced by code that shipped, and flipping `minimum-contrast` moves the whole
table on its own.

That construction is deliberate and it is the rule `aht` was retracted for
breaking: a fix validated against a hand-edited SDK state no build was ever in
moved zero pixels against the commit that actually shipped. A colour typed into
a harness is a counterfactual. A colour the build printed is evidence.

Measured 2026-08-12 at the pinned SDK `f3678832f`, 13pt, scale 2, 48 columns,
on `#090b0f`:

| SGR | `minimum-contrast = 1` | | `minimum-contrast = 3` | |
|---|---|---|---|---|
| | fg | peak_delta | fg | peak_delta |
| (none) | `#f4f7fb` | 235.79 | `#f4f7fb` | 235.79 |
| `30` black | `#1d1f21` | 19.86 | `#ffffff` | 244.14 |
| `90` bright black | `#666666` | 91.14 | `#666666` | 91.14 |
| `2` faint | `#7f8185` | 118.00 | `#7f8185` | 118.00 |
| `2;34` faint blue | `#455767` | 73.46 | `#ffffff` | 244.14 |

Deriving command: `./scripts/contrast-floor-check.sh`

It proves the rasterizer's response to the colour the projection chose. The
other half of that wire — that the shipping terminal layer asks for the floor
at all — is covered by `zig build test`, which drives the TypeScript extension
through `terminal_painter.zig` and reads the retained scene.

### And the whole chain, once, on the real app

With `docs/sdk-patches/composite-cell-grid.patch` in a sandbox SDK, the same
question was put to the real bundle. One binary, one driving script writing
eight rows each of SGR 30, SGR 90, faint blue and plain text, two runs one
config key apart, `publisher_pid` asserted against the launched pid both times,
measured over fixed pixel bands of the `-p60` dump with
`scripts/measure-png-ink.m`:

| band | `minimum-contrast = 1` | `minimum-contrast = 3` |
|---|---|---|
| SGR 30 | mean_luma 14.57, solid 0, **lit 0** | mean_luma 67.57, solid 10944, lit 15360 |
| SGR 90 | mean_luma 34.22, lit 15519 | mean_luma 33.95, lit 15424 |
| faint blue | mean_luma 19.59, solid 0, lit 7344 | mean_luma 59.27, solid 10304, lit 13663 |

`lit = 0` over 46400 pixels of a band that is full of glyphs is the defect
stated as a number: the text is there and nothing on the screen distinguishes
it from the ground. The SGR 90 row is the control — it moves by 0.8% while the
other two transform, which is the floor discriminating rather than repainting.

---

## 4. Rules that follow

- **Never** conclude "the text renders correctly" from a reference screenshot.
  It cannot support that claim about anything CoreText does.
- When comparing two images, state the metric and keep the basis identical on
  both sides. `scripts/measure-png-ink.m` states its basis in its header and
  reads raw decoded bytes so no colour management moves the numbers.
- Compare a renderer against ITSELF across two builds. The gap between the
  reference renderer and the host rasterizer is legitimate — different outlines,
  different hinting, different stem darkening — and reading a defect out of it is
  how `aht` was misdiagnosed twice.
- `phux-cockpit-aht`'s fix is still unconfirmed ON GLASS. Nothing here changes
  that: it needs a human to grant Screen Recording once. See section 5.

---

## 5. What a human would have to do once

To close the last gap — confirming the app's actual presented frames, including
compositing and display colour conversion — someone has to grant Screen
Recording to the capturing binary once, in System Settings > Privacy & Security
> Screen Recording. TCC is per-binary and survives rebuilds only if the code
signature identity is stable, so grant it to a stable helper (for example
`/usr/sbin/screencapture` invoked from a fixed terminal app), not to a freshly
built app bundle each time.

Once granted, a `screencapture -l<windowid> -o -x` of the Cockpit window, fed
through `scripts/measure-png-ink.m` over a fixed crop, would be automatable on a
developer machine — but not in CI, and not on a fresh machine without a human
click. That is the honest boundary. Everything above it is automated now.

---

## 6. The real-frame capture, and what it can see that a screenshot cannot

`phux-cockpit-g1z`. Section 2 ruled out every capture path on macOS and closed
on the one that was not ruled out by macOS at all — `NATIVE_SDK_GPU_SHOT_DIR`
was blocked by a single condition in the SDK. This section is that condition
removed, and the measurement that says the result is worth having.

The SDK change is `docs/sdk-patches/composite-cell-grid.patch`; that README
carries the argument for it. The harness is `scripts/capture-gpu-ink.sh`. Note
what neither of them is: composite mode remains a prototype flag that is off
unless set, so this captures the app's frame through a pass the app does not
normally present through. Everything below holds for the CoreText rasterization
inside that frame, which is the same code either way.

### The harness refuses in the state that would lie

Against a bundle built on the pinned SDK:

```
$ ./scripts/capture-gpu-ink.sh --bundle <unpatched>
gpu_present_path=pixels present_fallback=missing_service

REFUSING TO CAPTURE: the host is not rasterizing this frame.
gpu_present_path=pixels means every packet present was refused and the
engine fell back to its own CPU reference renderer. Whatever the
composite pass would dump is that renderer, not CoreText.
Fix: docs/sdk-patches/composite-cell-grid.patch (see that README).
exit=1
```

Against a bundle built on a sandbox copy of the pin with the patch applied:

```
$ ./scripts/capture-gpu-ink.sh --bundle <patched>
gpu_present_path=packet present_fallback=none
shots=3
exit=0
```

Red then green, watched in that order. The refusal is not decoration: a capture
taken in the `pixels` state is a picture of the reference renderer filed under
the GPU path's name, which is the exact confusion this whole document exists to
prevent.

### The demonstration

Three bundles, all from the same Cockpit tree, differing only in the SDK source
they were built against:

| bundle | SDK | binary sha256 |
|---|---|---|
| unpatched | the pin, `f3678832` | `b2bcbc1f6b7341c6…` |
| patched | pin + `composite-cell-grid.patch` | `42f01b541ff0cff1…` |
| bold-defect | patched, plus an INJECTED defect | `841053c4b31ba71c…` |

The injected defect is one branch of `NativeSdkCellFaceFor` in
`appkit_host.m`: the `wantBold` case returns the REGULAR face and synthesizes
nothing, so every `\x1b[1m` cell inks at regular weight on the host while the
engine's reference renderer still resolves the registered bold companion.

**Why this defect and not the font-smoothing one.** The smoothing calls are the
textbook example in section 1, and they are the wrong instrument test: measured
between the two states that actually shipped they move ZERO pixels
(`scripts/host-raster-compare.sh`, and the table in section 1). The only
smoothing state that moves pixels is `false`, which no build was ever in — so
demonstrating with it would mean measuring against a counterfactual authored
for the demonstration, which is precisely the mistake this repo published a
retraction for. The bold-face branch has neither problem: it changes what
CoreText is asked to draw, so it cannot be a no-op, and it is the failure the
SDK's own comment beside that code says must not be allowed to happen — "a bold
run that renders bold on the reference path and regular on this one is worse
than no bold at all."

This is an INSTRUMENT SENSITIVITY test. It asks whether the capture moves when
the rasterizer moves. It is not a claim that any shipped build had this defect,
and no fix is being validated against it.

Captured pane: twelve fixed rows of `BOLD bold …  regular regular …` and one
ticking line at the bottom, cropped off every measurement. Numbers are over
`rect=0,60,470,240`, 112,800 pixels, via `scripts/diff-png-region.m` and
`scripts/measure-png-ink.m` on the same basis.

```
                                                    diff_pixels  max_delta   exit
GPU capture,  same build, two independent launches            0          0      0
GPU capture,  patched      vs bold-defect                 12492        105      1
reference,    patched      vs bold-defect                     0          0      0
reference,    same build, two independent launches            0          0      0
```

Ink over the same crop:

| | mean_luma | solid (>127) | lit (>32) |
|---|---|---|---|
| GPU capture, patched | 49.1915 | 18828 | 25704 |
| GPU capture, bold-defect | 44.7964 | 16884 | 23508 |
| reference, patched | 37.8525 | 13848 | 20688 |
| reference, bold-defect | 37.8525 | 13848 | 20688 |

Read the rows down, never across: the GPU column and the reference column are
different rasterizers and the gap between them is legitimate.

**What the four diff rows establish, in the order they have to be established.**
Row 1 is the negative control for the instrument: two launches of the same
build produce byte-identical captures, so the capture has no jitter to
mistake for a finding. Row 4 is the same control for the reference screenshot.
Row 3 is the blindness: two different binaries, one byte-identical screenshot,
identical to four decimal places on every statistic. Row 2 is the finding:
12,492 pixels — 11.07% of the crop — with a peak channel delta of 105.

Row 2 without rows 1, 3 and 4 would be worth nothing. An assertion that has
only ever been seen to fire is not evidence, and `diff-png-region` was watched
returning both 0 and 1 here.

### Reproduce it

```sh
./scripts/build-automation-cli.sh                  # clones .zig-cache/pinned-sdk at the pin
cp -R .zig-cache/pinned-sdk .zig-cache/pinned-sdk-patched
rm -rf .zig-cache/pinned-sdk-patched/zig-out .zig-cache/pinned-sdk-patched/.zig-cache
(cd .zig-cache/pinned-sdk-patched && git apply ../../docs/sdk-patches/composite-cell-grid.patch)
# temporarily, in build.zig.zon:
#   .native_sdk = .{ .path = ".zig-cache/pinned-sdk-patched" },
zig build package -Dautomation=true
./scripts/capture-gpu-ink.sh
```

Revert `build.zig.zon` before committing anything: the pin is a tarball sha and
a locally patched build must not be able to masquerade as it.

### What this still cannot see

It is the composite pass's frame, not the shipping present path's frame, and it
is a texture readback rather than a photograph of the display — so display
colour conversion and anything the window server does after the drawable is
presented are still outside it. Section 5 remains the honest boundary for
those.
