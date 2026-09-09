# Native cockpit spike: findings

Date: 2026-07-27
Toolchain: Zig 0.16.0

> **Read this as history.** The spike ran 2026-07-27 to 2026-08-02 in a scratch
> repo with no remote. Its conclusions were adopted and its code was migrated
> into this repository, which now ships as a released product. Where a claim
> below is stated in the present tense about "the spike", it describes that
> scratch tree, not `main`. Section 9 records what became of it.

---

## 1. Verdict

**GO, with caveats.** Two independent libghostty-vt terminal panes render into one
`gpu_surface`, laid out by the canvas alongside ordinary native widgets, with focus
routing, under a budget policy derived from the framework's own constants. The build
compiles and 68 of 70 headless tests pass (2 env-gated skips), verified by direct
run rather than by agent report.

Verification coverage is **thinner than planned** and the report says so where it
matters. Both adversarial validators and the automated report agent died on transient
API errors (529). One validator had already committed a substantial probe suite
before dying; the remaining verification in this document was performed by hand.
Section 3 marks every claim by how it was established.

The single most important thing learned is not in the original plan: **the spike's
premise is partly obsolete in our favour.** Since `examples/terminal` was written,
native-sdk absorbed the terminal into its own widget tree — there is a first-party
`canvas.terminal_grid` painter with a per-pane `id_base` that is already multi-grid
safe, and a `<terminal pty={key}>` markup widget. The spike forked the older example
painter and hand-added `id_base`. That was the right call for learning the grain of
the code, and is probably the wrong call to carry forward. See section 7.

---

## 2. What was built

Five commits on a verbatim fork of native-sdk `examples/terminal` (`a7509a7`):

| Commit | What |
|---|---|
| `b2741ed` | Baseline fork, byte-identical to upstream except the `native_sdk` path. 48 tests. |
| `54f9f80` | Two panes, two rects, budgets partitioned by an explicit policy. |
| `4fafca7` | Tests pinning ids, budgets, rects, accessibility, pixels. 54 tests. |
| `a172afd` | A native widget header beside the panes, plus focus routing. 59 tests. |
| `bc993c8` | Adversarial validator probes. 70 tests. |

There are uncommitted additions in `src/adversarial_tests.zig` (75 lines) from the
validator run that died mid-flight: a higher-content proof-shot test. Review before
committing.

---

## 3. What actually works

Claims are labelled **[run]** if a command was executed and its output read directly
for this document, **[test]** if pinned by a test in the suite, **[reported]** if it
rests on a build agent's account and was not independently re-run.

- **[run]** `zig build` succeeds. ~54s cold.
- **[run]** `zig build test -Dplatform=null` exits 0. `Build Summary: 18/18 steps
  succeeded; 68/70 tests passed (2 skipped)`. Zero `error:` lines. Run twice with
  identical results. (Note: the log emits a `failed command:` line while still
  exiting 0 with all steps green — a stderr artifact of the test runner's `--listen`
  protocol when a test prints. It is not a failure, and it did mislead the
  orchestrator once. Trust the exit code and the summary, not that line.)
- **[run]** Pane independence is structural, not asserted: `Model` holds
  `panes: [pane_count]Pane` (`main.zig:226`), each `Pane` owning
  `session: *grid.Session` (`main.zig:163`), and each `grid.Session` owns its own
  `vt.Terminal`, `vt.TerminalStream`, and `vt.RenderState`. Two panes cannot share
  terminal state by construction.
- **[run]** The budget policy is derived from framework constants, not magic numbers:
  `chrome_command_envelope = max_canvas_commands_per_view - widget_command_reserve`,
  `pane_text_reserve` from `canvas.max_display_list_text_bytes`, `pane_glyph_budget`
  from `native_sdk.runtime.max_canvas_glyphs_per_view`, `pane_cell_ceiling` from
  `grid.max_cells` (`main.zig:70-89`).
- **[test]** Distinct id namespaces per pane, judged by `DisplayList.diff` rejecting
  duplicate ids.
- **[test]** Command floor holds under adversarial load; per-pane rect containment;
  both panes present in the accessibility tree; per-pane cursor cue.
- **[test]** Layout agreement: `paneFrames` and the widget tree independently derive
  the same rectangles. The slice-3 agent did not accept a first-try pass — it
  instrumented the test (`paneFrames=(8,36,478,596) tree=(8,36,478,596)`), then ran a
  negative control moving the top edge 3pt and confirmed the test fails. Not vacuous.
- **[test]** Keyboard input routes only to the focused pane; click routes focus.
- **[reported]** Measured two-pane adversarial load: pane 0 painted 15 rows, pane 1
  painted 22, combined 1488 of the 1792 command envelope. Neither pane starved.
- **[reported]** One real Metal frame captured showing the header, the button, and
  both pane cursors.

### Known defect, characterised and pinned

Wheel routing resolves the target pane by containment against `paneFrames`, which
needs a surface size that only arrives on the first `.viewport` message. Before the
surface is measured, a wheel over pane 1 scrolls pane 0:

```
MEASURED wheel routing (surface measured):   pane0 178->178  pane1 178->172
MEASURED wheel routing (surface UNMEASURED): pane0 178->172  pane1 178->178  <- WRONG pane
```

The validator pinned this with a test that **asserts the broken behaviour**
(`ADVERSARIAL: DEFECT - before the surface is measured the wheel hit test is...`), so
the suite stays green while documenting the flaw. That is a defensible choice for a
spike and an unacceptable one for the product. Fix before N panes: the defect scales
with pane count, and a cockpit's first interaction is often a scroll.

---

## 4. What broke, and what it cost

**Toolchain skew, as predicted.** native-sdk requires Zig 0.16.0. phux's `flake.nix:43`
pins `zig_0_15`, and the shell `zig` is 0.15.2. Resolved by staging 0.16.0 via
`nix build nixpkgs#zig_0_16` and passing the absolute path everywhere. Two Zig
toolchains is now a permanent condition of this project, not a transient annoyance.
It is survivable and it is real overhead.

**`zig build run` is not a usable liveness gate here.** Slice 2 got a completely empty
log and exit 124; slice 3 got 791 bytes and an app that exited at ~0.5s while the
grep-based pass criterion still reported success. The honest gate is
`NATIVE_SDK_GPU_FRAME_TRACE=1 timeout N ./zig-out/bin/terminal` on a binary built
*without* `-Dplatform=null`. A stale null-platform binary produces
`error: UnsupportedViewKind`, which looks exactly like a render regression and is not.

**GPU presentation cannot be proven from an agent session.** Across a 45s live run,
45 `gpu_surface_frame` events and 49 `effects_wake` fired with zero errors, but 44 of
45 frames traced `path=occluded` — the window sits behind others, so the compositor
skips the present. One genuine capture exists. Runtime liveness is well evidenced;
sustained Metal presentation is not, and no agent claimed it was.

**Headless tests cannot see Metal.** The PNG path renders through the CPU reference
renderer (`src/primitives/canvas/reference.zig`), so it proves display-list-to-pixels,
not Metal parity. `gpu_nonblank` — the one signal that the real GPU drew — is stamped
only by the platform hosts and is unavailable under `-Dplatform=null`. A Metal-only
regression would pass the entire suite.

**Two ptys cannot be proven live, only inferred.** The runtime event log does not name
the pty key. The inference is strong (a steady ~1 Hz `effects_wake` matching pane 1's
`date` ticker, which an idle shell does not produce) and the headless tests do prove
two ptys under the fake executor. But there is no direct live proof.

**An honesty note worth keeping.** The slice-2 agent initially wrote command counts
derived by arithmetic rather than measurement, then caught itself, instrumented the
tests, measured 1488/644/964 against the claimed 1490/646/966, corrected the notes and
amended the commit. The numbers in the repo are real. This is recorded because the
same temptation applies to everything downstream.

**Orchestration cost.** 13 agents, ~101 minutes, 1.76M subagent tokens, 746 tool calls
for the productive run. Three agents lost to API 529s across two attempts; the verify
and report phases never completed and were done by hand.

---

## 5. The budget-partitioning answer

This was the predicted hard part. The answer is that the three budgets have **different
scopes**, and a naive `budget / panes` for each is wrong:

- `command_budget` is **builder-cumulative** — it bounds the whole display list, so
  pane *i* must receive a running ceiling, not a slice. Implemented as
  `paneCommandBudget(i) = envelope - (pane_count - 1 - i) * (envelope / pane_count)`
  (`main.zig:71-72`), a floor-and-slack scheme: an early quiet pane leaves headroom a
  later busy pane can use, but no pane can starve its successor.
- `text_reserve` and `glyph_budget` are **per-paint-local** — these do divide, against
  the shared 32 KiB text store and the per-view glyph atlas.

One real fix fell out that neither candidate plan predicted: `row_reserve` had to widen
from `cols * 4 + 8` to `cols * 8 + 8`. Measured, not argued — a 40-column screen of
U+256C reached 964 commands against an 896 budget under the old reserve, 644 under the
new one. Box-drawing content was silently over-committing.

**The glyph budget remains uncalibrated and is the largest open technical risk.** The
test asserts *our* accounting, not the runtime's: `view_canvas.zig` charges only
pre-shaped `.glyphs` arrays while the painter emits `.text`, so the runtime's per-view
atlas accounting still sees zero entries for a terminal pane. The chosen value is
strictly more conservative than the status quo, and nothing in the spike probed the
real atlas. The honest test is two panes of disjoint heavy content — CJK in one,
powerline in the other — on a live run.

---

## 6. Scaling to a real cockpit

The policy above is written in terms of `pane_count` and divides cleanly, but the
ceilings are absolute and will bind well before a large fleet:

- `max_cells` now matches the full bounded grid (`320 * 96 = 30,720`) rather
  than imposing a second 7,168-cell shape cap. Command, text, glyph, and path
  budgets still bind well before a large fleet can paint full-fidelity grids.
- The command envelope divides, but the measured two-pane adversarial load already
  consumed 1488 of 1792. Eight panes of comparable content do not fit.
- The glyph atlas is shared per view and is the least understood ceiling.

The conclusion is not "a fleet grid is impossible" — it is that **a fleet grid cannot
be N full terminals at full fidelity on one surface**. The plausible shapes are
per-pane fidelity tiers (the focused pane full, the rest degraded to a thumbnail or a
last-N-lines summary), or more than one window. Both are ordinary product decisions,
but they need to be made deliberately rather than discovered at pane six.

---

## 7. The phux seam

The best news in the spike. The ingestion point is one line and knows nothing about
PTYs (`cockpit/src/grid.zig:172-173`):

```zig
pub fn feed(session: *Session, bytes: []const u8) void {
    session.stream.nextSlice(bytes);
}
```

Everything PTY-specific lives above this in the effect vocabulary. Swapping the byte
source for phux `PANE_OUTPUT` frames is a change of *caller*, not of the terminal
stack. This is exactly the ADR-0013 asymmetry landing on the FFI boundary as intended:
Zig owns everything from VT bytes rightward, Rust owns everything leftward, and Rust
never parses a byte.

Minimum C ABI for `phux-client-ffi`:

```c
typedef struct phux_client phux_client;

// Lifecycle. Rust owns the transport, the reconnect loop, and resolution.
phux_client *phux_client_connect(const char *socket_path, char **err_out);
void         phux_client_free(phux_client *);

// Server -> app. Opaque VT bytes; the app feeds them straight to Session.feed.
// Returns 0 when the queue is empty. `bytes` stays valid until the next call.
typedef struct {
    uint64_t       pane_id;
    uint64_t       seq;
    const uint8_t *bytes;
    size_t         len;
} phux_pane_output;
int phux_client_poll_output(phux_client *, phux_pane_output *out);

// Control-plane events: pane opened/closed/resized, agent state, asks.
// Small, versioned, CBOR or JSON — NOT terminal state.
int phux_client_poll_event(phux_client *, const uint8_t **buf, size_t *len);

// App -> server. Input atoms, already libghostty's own types (ADR-0008).
int phux_client_send_key(phux_client *, uint64_t pane_id, const uint8_t *ev, size_t len);
int phux_client_send_mouse(phux_client *, uint64_t pane_id, const uint8_t *ev, size_t len);
int phux_client_resize(phux_client *, uint64_t pane_id, uint16_t cols, uint16_t rows);

// Readiness for the app's event loop. See the wakeup note below.
int phux_client_fd(phux_client *);
```

Two constraints found during recon that shape this:

1. **Wakeups.** native-sdk's runtime owns the event loop. The clean integration is a
   `ModuleRegistry` extension module (`src/extensions/root.zig`) holding the socket and
   posting to the effect channel, which is why `phux_client_fd` is in the sketch.
2. **Frame size.** The channel post path has a hard 4096-byte bound
   (`effects.zig:907`) with no override field. phux `PANE_OUTPUT` frames will exceed
   this. Either chunk in the producer, or bypass the channel by feeding `Session`
   directly from the module. **Measure throughput before committing to channels** —
   this is the one thing most likely to force a design change.

---

## 7a. The first-party terminal stack: take the painter, refuse the session store

Investigated 2026-07-27 after the findings above. native-sdk's terminal support comes
in two separable halves, and the right answer differs for each.

**Take `canvas.terminal_grid` (978 lines).** It is a pure painter:
`paint(grid: TerminalGrid, builder, options)` over a plain snapshot — `rows` of
`TerminalCell{cp, cluster, fg, bg, underline, wide}`. It contains **zero** references
to `vt.`, ghostty, or `Terminal`. `id_base` is first-class (`paintIdBase`,
`reserved_id_offset`), so the spike's hand-threaded 2^40 stride across five emission
sites is deletable. Box-drawing code points "render as GEOMETRY at exact cell bounds,
never font glyphs" with a dedicated `path_reserve`, which should retire both `box.zig`
and the `row_reserve` widening. Its doc names third-party producers explicitly:
snapshots are "Produced outside the canvas (the runtime session adapter, a test, a
docs scene)."

**Refuse `src/runtime/terminal_session.zig` (1569 lines) and the `<terminal pty=>`
widget.** It is a framework-owned store of libghostty-vt sessions keyed by pty, and it
duplicates most of our fork. Two independent blockers for phux:

1. **A hard four-session ceiling.** `max_sessions = effects.max_effect_ptys`, and
   `effects.zig:921` is `pub const max_effect_ptys: usize = 4`. No override field. A
   cockpit fleet needs more than four panes.
2. **No inbound byte injection.** `PtyGateway` exposes only `write` (app to pty) and
   `resize`. There is no public feed/ingest entry point on the module — ingestion is
   driven internally by the framework's own pty effects. phux bytes arrive from a
   socket, and this path has no door for them.

So the fork was not wasted: its `Session` half is load-bearing precisely because phux
needs an unbounded, externally-fed session that the framework's store will not give us.
Its painting half is dead weight.

**The port, therefore:** keep `grid.Session` (our libghostty ownership, our byte source,
no pane ceiling). Delete our painting, run-merging, id-threading, and box drawing. Write
a `Session -> TerminalGrid` snapshot adapter. Call `canvas.terminal_grid.paint`.

Tradeoff this accepts: our fork is damage-driven and re-emits only dirty rows. The
first-party painter takes a full resolved snapshot each frame and lets the retained
renderer diff display lists by command id. We trade our damage tracking for the
framework's diffing — simpler and upstream-tracking, but it moves a per-frame snapshot
allocation into the N-pane perf question. Measure it at pane six, not pane two.

---

## 8. Recommended next slice

**Migrate off the forked example painter and onto the first-party
`canvas.terminal_grid` before doing anything else — painter only, per section 7a.**

The spike forked `examples/terminal`'s painter and hand-threaded an `id_base` through
five emission sites plus a 2^40 stride helper. It works, and it taught us the grain of
the code. But the framework now ships a painter with per-pane `id_base` already built
in, and every hour spent maintaining our fork is an hour spent diverging from a
pre-1.0 project that is moving fast. The whole architectural argument for this design
was that the native layer stays thin and disposable; carrying a forked 55KB renderer
contradicts that.

Order of work:

1. Port to `canvas.terminal_grid`. Expect the `id_base` patch, the `row_reserve`
   widening, and the box-id stride to become unnecessary. Keep every test — they are
   the real asset from this spike and they should pass against the new painter or tell
   us something important.
2. Fix the wheel-routing defect and convert the DEFECT test into a correctness test.
3. Calibrate the glyph budget with the CJK-versus-powerline live run.
4. Then `phux-client-ffi`, designed against section 7 and validated by pointing one
   pane at a real phux server.

Step 1 may substantially shrink the diff against upstream, which changes the cost
estimate for everything after it. It should happen before the ABI work, not after.

---

## 9. The port, executed (2026-07-27 to 2026-08-02)

All four steps of section 8 were completed on the spike branch
`port/first-party-terminal-grid`: the first-party painter, the wheel-routing and
budget work, the glyph-budget calibration, and the production `phux-client-ffi`
host.

**That branch no longer exists, and nothing is owed to it.** Its work was
migrated into this repository as PR #7 on 2026-08-03 and has been developed here
ever since. The branch was removed on 2026-08-09 after a symbol-by-symbol audit
confirmed the migration was complete; its history is preserved at the annotated
tag `spike/first-party-terminal-grid`, so every commit cited below is still
reachable (`git show d4ccb84^:src/box.zig` still works).

The audit is worth recording, because "the spike had a file main does not" is an
easy and wrong conclusion to draw from a directory listing. The spike's flat
`src/phux_*.zig` became `src/providers/phux/*.zig`, and 13 of its 75 exported
symbols appear absent by name. All 13 are renames, not losses:

| Spike | Mainline |
|---|---|
| `sendPasteOwned` | `sendPaste` (`providers/phux/host.zig`, re-exported by `provider.zig`) |
| `clearSelectionOwned` / `releaseAnchorOwned` | `clearSelection` / `releaseAnchor` — the `*Owned` suffix was dropped once `provider.ReplicaOwner` made ownership a parameter type |
| `finishAttachBarrier` | the `attach_barrier_seen` state machine |
| `explicitResizeTerminal` | `viewportResize` → `phux_client_terminal_resize` |
| `pruneRemovedPanes` | `pruneRemoved` |
| `ensurePane` / `findPane` / `indexOfOwner` / `replicaOwner` | subsumed by `provider.ReplicaOwner` and `provider.TerminalRef` |
| `asC` / `fromC` | per-type converters such as `toCAnchor` |

Mainline additionally carries `providers/phux/presentation.zig` and
`providers/phux/provider.zig`, which the spike never had, and a
`max_search_results` cap it never had. The migration was a strict superset.

### What landed

`621bd31` — `Session.snapshot` projects the live libghostty viewport into
`canvas.TerminalGrid`. Buffers are owned per session and rewritten in place each
frame, so the painter's producer-owned-lifetime contract is satisfied with no
allocation in the frame path.

`d4ccb84` — `paint()` became a thin delegate. Deleted: run merging, per-row command
ids, the glyph probe set and its hand-added 4x subpixel charge, text-byte
preflighting, the `row_reserve` widening, the hand-built 2^40 id stride across five
emission sites, and `box.zig` entirely. `grid.zig` went 1156 -> 916 lines; `box.zig`
was 358 lines. Cell metrics now come from `canvas.terminalCellMetrics`, the seam the
framework shares with its own grid sizing, so painted cells and the cols/rows pushed
to the pty cannot drift apart.

`0f4ecb9` — the wheel-routing defect from section 3 is fixed and its DEFECT test
inverted into a REGRESSION test that dispatches no frame at all. `Model.surface_size`
is seeded from the window's configured size; `app.zon` pins `restore_state = false`
with `center_on_primary`, so that is the same number the shell config hands the
platform rather than a guess.

A fourth budget tier appeared: `pane_path_reserve`. The first-party painter renders box
drawing as geometry under a path budget, which the fork had no accounting for at all —
it widened its per-column command reserve instead.

### Verification

- `zig build test -Dplatform=null`: exit 0, 18/18 steps, 70/72 tests (2 env-gated skips).
- Real Metal binary builds.
- Both new snapshot tests negative-controlled: perturbing the `cp` projection fails
  them with `expected 104, found 105`. The wheel regression test negative-controlled:
  restoring the zero default fails it with `expected 980, found 0`.
- The adversarial id-uniqueness test needed its band arithmetic repointed at the new
  scheme. Instrumented before changing it, to confirm the duplicate-id check itself
  still passed: no duplicates across 616 commands, both pane bands symmetric at 302.

Those are the spike's numbers, at the spike's size. On `main` at `358b24c` the same
two configurations measure:

```
zig build test -Dplatform=null                 19/19 steps, 329/334 tests (5 skipped)
zig build test -Dplatform=null -Dphux-enabled  21/21 steps, 337/342 tests (5 skipped)
```

Both exit 0. Both still print the stray `failed command: ... --listen=-` line on
stderr while every step is green — see the corrections below; that line has now
misled two separate sessions and is not a failure.

### Production host integration (2026-08-02, migrated 2026-08-03)

`src/providers/phux/host.zig` owns the panic-contained C ABI client on the UI
thread. `src/providers/phux/extension.zig` owns only the TCP/Unix socket worker;
complete, length-prefixed frames cross bounded reusable queues, and a one-byte
native-sdk channel wake asks the UI owner to drain them. Borrowed FFI grids are
copied into reusable projection buffers before the next mutable client call.
Input and local policy paths cover keyboard/IME, paste, focus, pointer, viewport
resize/scroll, opaque document-anchor selection, clipboard, and search. Reconnect
freezes published panes until the replacement attach reaches its ATTACHED
barrier.

The production build is selected explicitly with `-Dphux-enabled=true` and
requires the generated header plus the `ffi-release` static-library directory.
From the current same-checkout layout, the root recipe builds both before
running Cockpit's complete graphs:

```sh
just cockpit-test
```

### The glyph budget: measured, and the answer is split

Section 5 left this as the largest open risk. The port does **not** resolve it — it
inherits it. Source still says the mismatch is real: the first-party painter emits
`builder.drawText` (`terminal_grid.zig:767`) while the runtime charges the atlas from
`value.glyphs.len` (`view_canvas.zig:63`), so terminal panes still contribute **zero**
to the runtime's real `glyph_count`. The budget remains a self-imposed proxy; it is now
the framework's proxy (with its documented `atlas_variants_per_glyph = 4`) rather than
ours, so we no longer maintain it, but nothing validates it against the Metal atlas.

Empirically, however, it holds. A live calibration run — pane 0 flooding 30 distinct
CJK code points, pane 1 flooding 29 distinct box and geometry code points, both in
tight `printf` loops for 25 seconds — produced **55,331 effect wakes and zero atlas,
glyph, budget, limit, or drop lines**. And this run presented **11 of 11 frames
`path=present`** with none occluded: the first sustained real Metal presentation in the
whole spike, which also retires the "GPU presentation cannot be proven from an agent
session" caveat in section 4.

One new observation from that run: frame production fell to 10-11 frames in 25 seconds,
against roughly 1/s in the idle runs. Under extreme output load the frame pump is
starved by the effect loop. Harmless for a spike, and worth understanding before a
fleet of panes is all producing at once.

### Corrections to earlier sections

- Section 3's "the tests fail at HEAD" claim was wrong when first reported. The suite
  exits 0; a stray `failed command:` line on stderr coexists with all steps green, and
  that line misled the orchestrator once. The section as written above is correct.
- Section 6's old 7,168-cell framework ceiling was removed after tall PTYs
  demonstrated that shape-clamping truncated valid viewports. The first-party
  painter now admits the full `320 * 96` bound and degrades on actual paint
  budgets instead.
