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

## Composite cell-grid and shot cadence (landed)

Landed on the current lineage in `phall1/native@5cc3aa75` and pinned by
Cockpit. The composite pass now admits `cell_grid`; a terminal frame no longer
falls back to the CPU reference renderer with
`present_fallback=missing_service`. The capture-cadence hunk landed in
`phall1/native@2d689c76`: `NATIVE_SDK_GPU_SHOT_EVERY=1` lets the capture harness
dump settled content instead of waiting for thirty content-changing presents.
Composite mode remains a prototype flag, off unless set. The historical
measurements and safety argument remain in `docs/RENDER_FIDELITY.md`.
