# SDK patches

Historical and pending changes Cockpit needs in the Native SDK. Nothing in this
directory is applied automatically: the pin in `build.zig.zon` is a tarball
sha, and a patch that the build silently applied would make the sha a lie.

To try one, clone the pinned sha into a sandbox and point the build at it:

```sh
./scripts/build-automation-cli.sh                 # clones .zig-cache/pinned-sdk at the pinned sha
cp -R .zig-cache/pinned-sdk .zig-cache/pinned-sdk-patched
git -C .zig-cache/pinned-sdk-patched apply ../../docs/sdk-patches/<name>.patch
# then, temporarily, in build.zig.zon:
#   .native_sdk = .{ .path = ".zig-cache/pinned-sdk-patched" },
```

`.zig-cache/` is gitignored, so the sandbox never becomes a commit. Revert
`build.zig.zon` before committing anything.

---

## symlink-safe-file-write.patch

Landed in `phall1/native@71bbce51` and pinned by Cockpit. The patch preserves
the reviewed `e577c644` and `584ffb64` raw-file write fixes on the current
upstream-v0.9.5 Cockpit lineage.

Whole-file writes now open or create the requested parent before writing, so
an existing symlinked parent remains usable and missing descendants below it
can be created. The final path component is retained and replaced atomically
instead of following a symlink into its target. Unprivileged writes still
authorize the fully resolved target and use the verified opened parent
capability, while writes to FIFOs and other special files inspect and stream
through the exact no-follow-opened handle.

The patch includes the regression matrix for existing and partially missing
symlinked parents, absolute and trailing-separator paths, unbound and confined
final symlinks, and opened FIFO handle identity.

---

## raise-pty-ceiling.patch

Landed in `phall1/native@87917c45` and pinned by Cockpit. The patch remains as
the measured rationale and as a reproducible view of the fork change.

`native_sdk.max_effect_ptys`: **4 -> 32**, plus a comptime budget on what the
pty table costs inline.

### The problem

The SDK keeps ONE fixed pty table for the whole process
(`Effects.pty_slots`, `src/runtime/effects.zig`). At 4, the fifth `ptySpawn`
anywhere in the process is refused with a `.rejected` exit. Cockpit is a
terminal multiplexer: it offers 16 tabs, 16 panes per tab, 5 windows and a
32-slot terminal registry, all of them on top of a table that backs four
shells.

phux-cockpit-pg1 made that failure honest — Cockpit now refuses the fifth
terminal rather than opening a pane no shell can back. This patch is the other
half: the honest number was still four, and four is not a multiplexer.

Two things were unreachable underneath it and become reachable again:
`max_windows` (5 windows need 5 shells) and `topology.max_tabs` (16 tabs need
16 shells). `window_limit_refused` was dead code.

### Is 4 load-bearing, or arbitrary?

**Arbitrary.** Audited against the pinned tree at `f3678832`:

| Claim | Evidence |
|---|---|
| No bitmask over pty slots | No `StaticBitSet`, `@Vector`, or slot bitmask in `src/runtime/`; every slot operation is a linear scan (`findPtySlot`, `findIdlePtySlot`, `idlePtySlotCount`, `ptyOccupiesKey`, all in `effects.zig`) |
| No `select()` / `fd_set` / shared poll array | Each pty polls its OWN two fds on its own io thread: `src/runtime/pty.zig`, `var fds = [2]Pollfd{ nudge_fd, parent }` then `c.poll(&fds, nfds, -1)`. There is no process-wide multiplexer to overflow |
| Slot index does not saturate | `Entry.slot_index` is a `u16` (`effects.zig`) — 65535, not 4 |
| Nothing asserts the value | No `@compileError` or `std.debug.assert` mentions `max_effect_ptys`; `effects_pty_tests.zig` is written generically against it (`while (key < 7 + max_effect_ptys - 1)`) |
| No serialized form | `session_replay.zig` keys pty records by the app's `u64` key, never by slot index |

The only consumers that size anything are `Effects.pty_slots`
(`[max_effect_ptys]PtySlot`), `terminal_session.max_sessions`
(`[max_sessions]Entry`, 16 bytes each), and `ts_core_host.zig`'s wire-key
registry (~272 bytes each). All linear, none structural.

The doc comment on the old constant says as much in its own words: "One live
terminal surface plus a background job or two is the realistic shape." That is
a statement about the expected app, not about the code.

### Why 32

Measured, not preferred. Every number below came out of the real bundle or the
real compiler.

**Per LIVE pty**, from `scripts/drive-shell-ceiling.sh --want 4 --measure`
against `zig build package -Dautomation=true`:

```
MEASURED shells=1 rss_kib=153952 threads=23 fds=63
MEASURED shells=2 rss_kib=161072 threads=24 fds=66
MEASURED shells=3 rss_kib=163840 threads=25 fds=69
MEASURED shells=4 rss_kib=166592 threads=26 fds=72
```

Exactly +1 OS thread and exactly +3 descriptors per shell, and ~2.7 MiB rss.
The thread and fd deltas land on the nose of what the code predicts (one
`std.Thread.spawn` per pty in `effects.zig`; parent pty fd plus both ends of
the nudge pipe), which is the check that the measurement is measuring the
right thing rather than measuring noise.

**Per SLOT** (paid whether or not the slot is used), from the compiler, by
setting the new budget to 1 and reading the error:

```
error: pty table is 209664 bytes (32 slots x 6552); budget is 1.
```

So `@sizeOf(PtySlot)` is 6552 bytes: 209,664 bytes of inline `Effects` at 32
slots, up from 26,208 at 4. The 256 KiB staging block and 64 KiB outbound
block are NOT in that — they are heap, allocated at spawn and freed at retire,
so they scale with live sessions rather than with the table size.

**Against the machine**: `kern.maxfilesperproc` is 184320 and `ulimit -n` is
1048576, so 96 descriptors for 32 live shells is not a constraint at any
plausible number.

**Why 32 and not 8 or 16**: 32 is `local.max_terminals`, the terminal registry
Cockpit already declares. The bug being fixed is not "the number is small", it
is "the number is invisible": the app refused at a ceiling it never chose and
could not name. At 32 the binding constraint moves back into the app —
`topology.max_tabs` (16) and `layout.max_panes` (16) are what a person now
hits, and both are Cockpit's own. Worst case, all 32 live at once: ~86 MiB
rss, 32 threads, 96 descriptors.

### What the patch adds besides the number

`max_effect_pty_table_bytes` (512 KiB) and a `comptime` block that checks
`@sizeOf(PtySlot) * max_effect_ptys` against it. The SDK had no size assertion
on `Effects` at all, and `Effects` is stack-allocated in the SDK's own suite
(`var fx = DirectFx.init(...)` in `effects_pty_tests.zig`) — the one place
this table's growth could actually break something. The assert was watched
failing (the error text above is that run), so it is known to be able to fail.

### Consequences for consumers

`local.max_live_shells` already reads `native_sdk.max_effect_ptys` with no
literal in between, so Cockpit follows the pin with no second edit. That is
deliberate and load-bearing: a hardcoded duplicate is exactly how
phux-cockpit-pg1 happened.

One thing DOES change for any consumer's tests: with the table larger than the
app's tab and pane ceilings, no single repeated chord reaches the shell
ceiling any more. Cockpit's suite grew `support.fillLiveShells` for that.

---

## composite-cell-grid.patch

`cell_grid` added to the GPU composite pass's known-kind list in
`src/platform/macos/appkit_host.m`. One list entry, plus the comment
explaining why it belongs there.

### What it unlocks

`NATIVE_SDK_GPU_SHOT_DIR` — the SDK's only real-pixel-to-PNG path, and the only
capture of Cockpit's actual presented frames that needs no Screen Recording
permission. `docs/RENDER_FIDELITY.md` section 2 concluded it was "not available
to Cockpit". That conclusion was correct about the shipped SDK and wrong about
what it would take to change: the blocker is a single missing entry in a list,
not a missing capability.

### Why it is safe rather than a hack

The known-kind list is not a statement about what the composite pass can DRAW.
Both bitmap paths under it — `rasterCacheBuildEntryForCommand:` and
`compositeScratchTextureForCommand:` — dispatch through
`NativeSdkPacketDrawCommandBody`, which has drawn `cell_grid` since the kind
existed, and `NativeSdkPacketCommandRasterCacheable` already answers YES for
it. The kind was drawable all along; only the gate said otherwise.

Leaving it out is not graceful degradation. A refusal returns 0, the engine
reads that as "this host cannot present packets", and falls back to its own CPU
reference renderer (`recordCanvasPacketFallback` ... `.missing_service` in
`runtime/canvas_frame.zig`). For an app whose frames are ENTIRELY `cell_grid` —
a terminal — every present is refused, so composite mode REPLACES the CoreText
rasterizer instead of compositing it. That is the measured behaviour
`RENDER_FIDELITY.md` recorded: `gpu_present_path=pixels`,
`present_fallback=missing_service`, 147 fallback frames, zero shots.

### Measured, with the patch applied

Real bundle, `zig build package -Dautomation=true`, pin `f3678832f` plus this
one hunk, driven by the run recorded in `phux-cockpit-wmi`:

```
gpu_present_path=packet   present_fallback=none   gpu_nonblank=true
shots written: 3 (p1, p30, p60)
```

No fallback at all, and the dumps hold the real composited texture. Filed
upstream-side as `phux-cockpit-2ml.7`.

### How to use it

```sh
./scripts/build-automation-cli.sh
cp -R .zig-cache/pinned-sdk .zig-cache/pinned-sdk-patched
git -C .zig-cache/pinned-sdk-patched apply ../../docs/sdk-patches/composite-cell-grid.patch
zig fetch --save=native_sdk .zig-cache/pinned-sdk-patched     # revert before committing
zig build package -Dautomation=true
NATIVE_SDK_GPU_COMPOSITE=1 NATIVE_SDK_GPU_SHOT_DIR=<dir> \
  PHUX_COCKPIT_CONFIG=<config> ./zig-out/package/phux-cockpit.app/Contents/MacOS/phux-cockpit
```

The dump fires on composite present 1 and every 30th after, and the filename
carries the count. `-p1.png` is the BOOT frame — an empty pane. Waiting for
"a PNG exists" reads a blank window and calls it a measurement; wait for
`-p30` or later.
Two hunks in `src/platform/macos/appkit_host.m`, both about the GPU composite
pass (`NATIVE_SDK_GPU_COMPOSITE=1`):

1. `cell_grid` joins the composite pass's known-kind list.
2. The composite screenshot dump's every-30th-present cadence becomes
   `NATIVE_SDK_GPU_SHOT_EVERY` (default 30, unchanged when unset).

### The problem

`NATIVE_SDK_GPU_SHOT_DIR=<dir>` is the SDK's only path from real GPU pixels to
a PNG: `dumpCompositeShotWithPixelWidth:` reads the composited canvas texture
back with `getBytes` and writes it. It needs no Screen Recording permission, no
focus and no visible window, which is exactly what CI needs and exactly what
nothing else on macOS offers — every other real-pixel path (`CGWindowListCreateImage`,
`CGDisplayCreateImage`, ScreenCaptureKit, `screencapture(1)`) is TCC-gated.
docs/RENDER_FIDELITY.md section 2 is the survey.

It was unreachable for Cockpit. The dump fires only from the composite pass,
and that pass refuses any command whose `kind` is outside its known-kind list.
`cell_grid` was not in the list, and a Cockpit terminal frame is nothing but
`cell_grid` commands. A refusal returns 0, which the engine reads as "this host
cannot present packets" and answers with its own CPU reference renderer.

So composite mode did not capture the real rasterizer. It REPLACED it. Measured
on the real bundle, same pane, `publisher_pid` checked against the pid we
launched both times:

```
composite unset   gpu_present_path=packet  present_fallback=none
composite=1       gpu_present_path=pixels  present_fallback=missing_service   shots written: 0
```

An app running under composite mode looked exactly like its own reference
screenshots, which is the one failure a reference screenshot can never report.

### Why the fix is one line of behaviour

The known-kind list is not a statement about what the composite pass can draw.
It is a statement about which kinds have been checked to reach a bitmap through
one of the two CG paths the pass uses, and `cell_grid` already reaches both:

| Claim | Evidence, pinned tree `f3678832` |
|---|---|
| The raster-cache path draws `cell_grid` | `rasterCacheBuildEntryForCommand:` calls `NativeSdkPacketDrawCommandBody`, whose `cell_grid` branch calls `NativeSdkPacketDrawCellGrid` |
| The scratch path draws `cell_grid` | `compositeScratchTextureForCommand:` calls `NativeSdkPacketDrawCommand`, which is a wrapper over the same body |
| The cache already accepts it | `NativeSdkPacketCommandRasterCacheable` returns YES for `cell_grid`, with a comment explaining why a row's raster is a pure function of its command |
| Nothing else refuses it | With only the gate changed, `gpu_present_path` goes `pixels -> packet`, `present_fallback` goes `missing_service -> none`, and PNGs appear. No second refusal surfaced |

The last row is the one that matters: if either CG path could not draw a cell
grid, the pass would still refuse and the fallback would still be `pixels`.

### Why the second hunk

A composite present happens only when the packet content changes, so "every
30th present" is a cadence for an animating view and unreachable for a settled
one. Measured with `NATIVE_SDK_GPU_DRAW_TRACE=1` on a backgrounded Cockpit
whose pane rewrites one row from its pty as fast as the pty will carry it:

```
3 composite presents in 30 seconds
  action=clear ... drawn=27 fill=25   <- the full pass, 25 cell_grid rasters
  action=patch ... drawn=4  hit=2
  action=patch ... drawn=33 fill=31
```

Present 30 does not arrive. Present 1 does, and it is the empty first frame
before any pty output has been drawn — so the fixed cadence writes exactly one
PNG per run and it is blank. That is the difference between a capture
affordance and a capture affordance for animating views only.

With `NATIVE_SDK_GPU_SHOT_EVERY=1` the newest file in the directory is the
newest settled frame, which is what a capture harness wants. Unset, the
behaviour is byte-for-byte what it was.

### What it unlocks, and the proof that it is worth unlocking

`scripts/capture-gpu-ink.sh` is the harness. Against the pinned SDK it refuses
(exit 1) and names this patch; against a build carrying this patch it captures.

The capture sees a rasterizer defect that the reference screenshot cannot. The
demonstration and every number behind it are in docs/RENDER_FIDELITY.md
section 6.

### Scope

This does not make composite mode the default or recommend it for shipping.
`NATIVE_SDK_GPU_COMPOSITE` stays a prototype flag, off unless set; the patch
only stops the prototype from silently disabling the renderer it exists to
composite.
