# Decisions

Settled questions, with the reasoning that settled them. A decision here is not
permanent — it is *closed pending new information*. Reopen one by adding
information, not by re-litigating what is already written down.

---

## Ligatures: NO

**Decided 2026-08-06.** Do not implement programming ligatures.

The packed cell-grid model **permits** them. An N-cell cluster generalizes the
wide/spacer mechanism already in `CellWidth`: the head cell owns the ligature
glyph, continuations ink nothing, positions stay index-derived, and selection
and cursor keep addressing individual cells. The primitive is not the obstacle.

The obstacle is **who decides what ligates**. That requires GSUB `liga`
shaping. CoreText has it; the SDK's own `font_ttf.Face` (`glyphIndex`,
`advance`, `outline`) does not. If the AppKit host shapes and the CPU reference
renderer does not, the two diverge — and because automation screenshots render
through the reference path, that divergence would be **invisible in every
screenshot while being wrong on real glass**. That is the exact failure mode
this whole line of work exists to avoid. It is the same blind spot that hid the
glyph-smoothing defect (see below) for three diagnoses.

Doing it properly means: a GSUB ligature-substitution reader in `font_ttf.zig`
so the ENGINE decides once; cells carrying glyph IDs rather than only cluster
bytes (a ligature is a specific glyph, not a string); then the wire format,
both renderers, and the glyph-atlas key path following. That is a project, not
an increment.

It is also moot today: the app deliberately bundles **JetBrains Mono NL**, the
explicit no-ligature variant.

If ligatures are ever wanted, do it engine-side with explicit glyph IDs. Never
by letting each renderer shape for itself.

---

## Scrollback search matches case-insensitively, everywhere

**Decided 2026-08-10.** One rule for every provider, stated once in
`provider_contract.search_case_sensitive`.

The pinned `vt.search` matcher is ASCII case-insensitive throughout, so the
local side cannot currently be anything else. The remote phux side can do
either, so it is the one that has to be told — it reads the shared constant
rather than taking a caller's bool, which makes the two sides structurally
incapable of drifting apart.

A case **toggle** remains a legitimate feature. It needs case sensitivity in
the ENGINE first, at which point the constant becomes a default. It cannot be
built by letting the providers diverge.

---

## An emptied window closes — including the main one

**Keep-empty exception, 2026-09-11:** [ADR-0114](../../../docs/adr/0114-cockpit-closes-terminals-and-detaches-windows.md)
retains an Empty session view after a keep-empty session's last terminal ends.
Cmd+W there closes the client window without ending the named session. The
last-window close still quits. Close Pane/Tab ends work; OS-window close detaches
Phux views and preserves their shared layout.

**Decided 2026-08-10.** When a window's last tab closes, that window goes away.

macOS apps genuinely vary here, and the main window used to stand on its web
surface instead. Closing is the majority behaviour and, more importantly, the
one that keeps a single rule for every window: the thing you emptied is the
thing that goes away. Standing meant the identical gesture — cmd+W on a
window's last tab — closed a secondary window but left the main one on screen
showing something nobody asked for.

The last-window case is unchanged: it closes **and** quits.

---

## Config diagnostics get a dismissible band, never a modal

**Decided 2026-08-12.** A config that produced diagnostics raises one line of
chrome above the terminal, naming the LINE NUMBERS, dismissed by a press
anywhere in it. The startup log keeps every diagnostic in full sentences.

The log alone was the bug. From a bundled `.app` `std.log` lands in the unified
log, so a mistyped key produced a terminal that quietly behaved differently and
a user with no reason to open Console. But the opposite failure is worse and is
the one a dialog would have caused: most diagnostics are **benign** —
`unsupported_key` fires for `font-family` on a config that is otherwise
perfect — and nothing about a setting that did not apply justifies standing
between someone and a prompt. So the band takes no keyboard, holds no chord,
and leaves Escape to the search field, the palette, and the shell.

It NAMES LINE NUMBERS because that is the only part a user can act on; "your
config has a problem" sends them back to the file to hunt. With exactly one
problem there is room to name the problem too, and it does.

It takes its room out of the **content rect**, exactly as the scrollback search
band does, rather than floating like the palette. Painter, hit-test tree, and
PTY sizing pump all derive from `workspaceChromeIn`, and the palette floats only
because it is transient enough that two SIGWINCHes per summon would be the
larger cost. This band appears once per launch at most, so the honest layout is
worth its one resize — and taking room is also what guarantees no press can fall
through it into a grid painted underneath.

Dismissal is **for the launch, not forever**. Persisting "seen it" would need
state on disk that a re-read config file silently invalidates: the file is
parsed fresh every start, so the only honest memory of a notice is one that dies
with the process.

A second finding came out of building it, recorded here because it explains why
`Diagnostic` owns its bytes: the text used to be a slice borrowed from the
source, and `main.readConfig` reads the file into a buffer local to the read and
returns the `Config` **by value** — so every quoted key in the startup log was
read out of a dead stack frame, before the band existed to make it worse. It is
a bounded copy now. See `config_tests.zig`'s "a diagnostic outlives the bytes it
was parsed from".
## The settings surface paints in fixed colours, never in the theme's

**Decided 2026-08-12.** `view.zig`'s `settings_*` constants are literals, and
they must stay literals.

The settings panel is what you open when the terminal has become unreadable.
Painting it with the tokens the user is trying to fix would mean the one
configuration it exists to rescue — text you cannot see — makes the rescue
itself invisible. That is not hypothetical: `phux-cockpit-aht` is four rounds
of "the text is see-through or black or something".

Today `cockpitTokens` is already independent of the user's config; only
`terminalTokensFrom` applies `foreground`/`background`, and only the grids
paint with those. That is **not** a reason to lean on the tokens in the panel.
A theming feature is precisely the change that starts colouring the chrome, and
on the day it does, every other panel can afford to follow the theme and this
one still cannot.

Every colour in the block is measured against the ground it sits on with the
same WCAG formula the panel reports, and clears AA (4.5:1) on both grounds:
ink 19.03, muted ink 9.08, pass green 12.38, fail red 7.55 on `#101010`. The
invariant is pinned by `semantic_theme.zig`, "Settings rescue text remains
readable on every semantic surface", which measures token contrast on every
semantic surface rather than comparing against the constants — so a future
palette swap still has to keep it readable.

---

## Settings rows disclose details; they do not dump the schema

**Decided 2026-09-15.** A Settings row shows title, effective value, a short
timing subtitle, and actions. Default, applicability, timing, and the
configured value live in one native accordion, one open at a time. Syntax
appears only while editing; provenance and connection change-route stay
read-only. The editor stays outside the accordion so collapsing details cannot
hide an in-flight Preview. Search is an ordinary `input` (not `search-field`,
whose Escape-clears fights Escape-to-Cancel) and, when the hit is only in
concealed copy, opens that row's details. IDs 0..14, appearance records 0..10,
request bytes, rollback/Save, and About (section 5) stay on the same wire.

---

## A theme sets three colours, and the ANSI-16 palette is not among them

**Decided 2026-08-12.** `theme = <name>` carries `background`, `foreground`,
and `selection_background` only.

Those three reach the terminal through the **design tokens**, which
`terminalTokensFrom` rebuilds from `model.config` on every frame and
`Session.snapshot` pushes into the emulator's defaults on every frame. That is
what makes a theme change repaint LIVE with nothing to invalidate — there is no
stored copy of a theme colour anywhere for a stale value to hide in. Verified
on the real Metal surface: no theme samples `0xff090b0f`, `theme = phux-light`
samples `0xfffbfcfe`, and `theme = phux-light` plus `background = #0000ff`
samples `0xff0000ff`.

The palette, the cursor colour and the cursor style are excluded because they
land in the **emulator** (`applySessionConfig`) rather than in the tokens: they
are written once per session, so a theme change would need an explicit
re-apply — and the re-apply has an unsolved half, since switching from a theme
that sets slot 1 to one that does not cannot un-set it. The dynamic palette
takes overrides and offers no revert. A knob that applies but cannot un-apply
is the trap `Diagnostic.Kind.unsupported_key` exists to avoid. The ANSI-16
slots are also libghostty's own defaults read back verbatim, and a terminal red
should stay a terminal red.

**Explicit keys outrank the theme**, and the precedence is resolved at the READ
site (`Config.resolvedForeground` and friends) rather than at parse time.
Deciding at parse time would make the file order-sensitive: `foreground` above
`theme` would lose and the same two lines swapped would win, which is a rule
nobody can hold in their head and nothing in the file makes visible.

---

## Glyph rasterization enables macOS font smoothing

**Decided 2026-08-10**, in the SDK fork rather than here.

Every glyph the AppKit host draws lands in a `CGBitmapContext`, and font
smoothing — macOS's stem darkening — is off by default on the transparent
backing those contexts use, so all terminal text rendered systematically thin.

Measured with `scripts/measure-glyph-smoothing.m`, 13pt, bundled JetBrains Mono
NL: fully-solid stem pixels rise **2341 → 3164 (+35.2%)** at scale 2 and
**635 → 940 (+48.0%)** at scale 1, purely from enabling smoothing.

Filling an opaque ground under the cells was **considered and rejected**: on
the same harness it moves solid stem pixels by −0.3% (2341 → 2334), so it buys
no weight while adding a full-screen fill to every frame — which would have
cost real latency against a frame budget that is already over.

The CPU reference renderer never touches CoreText and blends coverage itself,
so it does not share the defect and **no reference screenshot can catch a
regression here**. Re-measure with the harness rather than eyeballing a
screenshot.

---

## The pty ceiling: 32, and it belongs to the app

**Decided 2026-08-12** (phux-cockpit-ipg, on top of phux-cockpit-pg1).

The SDK keeps ONE fixed pty table for the whole process and it held **four**
shells. Cockpit is a multiplexer offering 16 tabs, 16 panes per tab, 5 windows
and a 32-slot registry on top of it. pg1 made the refusal honest; it did not
make it right.

**4 was arbitrary, not load-bearing.** Nothing in the SDK encodes it: no
bitmask over slots, no `fd_set`, no shared poll array, no saturating slot
index (`Entry.slot_index` is a `u16`). Every slot operation is a linear scan,
and each pty polls only its own two descriptors on its own io thread. The old
doc comment argued from the expected *app* ("one live terminal surface plus a
background job or two"), never from the code.

**32, because it is `local.max_terminals`** — the registry Cockpit already
declares. The bug was never that the number was small; it was that the number
was **invisible**. The app refused at a ceiling it had not chosen and could
not name. At 32 the binding constraint moves back inside Cockpit, where
`topology.max_tabs` and `layout.max_panes` are both 16 and both nameable.

Measured, not preferred — per live shell, from the shipped bundle:
**+1 OS thread, +3 descriptors, ~2.7 MiB rss**. Per slot, from the compiler:
`@sizeOf(PtySlot)` = 6552 B, so the table costs 209,664 inline bytes at 32
against a `kern.maxfilesperproc` of 184320 and a 512 KiB comptime budget.
Re-derive with `./scripts/drive-shell-ceiling.sh --want 8 --measure`.

**Consequence worth knowing:** no single repeated chord reaches the shell
ceiling any more — cmd+T stops at 16 tabs and cmd+D stops at 16 panes, both
short of 32. Tests that want the ceiling use `support.fillLiveShells`.

The change is in the SDK, so it lives at `docs/sdk-patches/` until the pin
moves. `local.max_live_shells` derives from `native_sdk.max_effect_ptys` with
no literal in between, which is the part that must not be undone: a hardcoded
duplicate is exactly how pg1 happened.

Raising the table is not permission to adopt the framework terminal store.
That split is the next decision.

---

## Native SDK is the shell; libghostty-vt Session is the engine

**Decided 2026-09-14.** Cockpit architecture package 2 from Metal/Foreman.

The pinned Native SDK fork is **shell only**: windows, chrome (`.native` /
TypeScript), the event loop, `gpu_surface`, and `canvas.terminal_grid.paint`.
It does not own product-pane cell state.

`src/terminal/` libghostty-vt `Session` is the **engine**: cell state, damage,
scrollback, selection. Providers feed it VT bytes (local PTY or phux FFI). The
engine projects a `canvas.TerminalGrid`; the shell paints it.

**Refuse forever** for product panes: the framework store
`runtime/terminal_session.zig` and the `<terminal pty=>` markup widget.
Inbound feed is missing — phux bytes arrive from a socket, not an SDK pty
effect — and the store's old four-pty ceiling is why the fork raised
`native_sdk.max_effect_ptys` to 32 for Cockpit's own table, not a reason to
take the store. Historical write-up: [FINDINGS.md](../FINDINGS.md) §7a.

`local.max_live_shells` stays derived from `native_sdk.max_effect_ptys` with
no literal in between. `scripts/check-shell-engine.py` fails if product source
reintroduces the widget or the store.

---

## VT bytes never ride the Native SDK 4096 effect channel

**Decided 2026-09-14.** Cockpit architecture package 4 from Metal/Foreman.

The channel post path has a hard 4096-byte bound with no override
([FINDINGS.md](../FINDINGS.md) §7). phux `PANE_OUTPUT` frames exceed it.
Chunking those bytes through the channel is forbidden — that is the thing
the queues exist to avoid.

Production `providers/phux` stays: the extension module owns the socket;
complete frames cross bounded reusable queues; only a one-byte wake is
posted; the UI thread drains and feeds the engine (`phux_client_feed_frame`
on this thread, `Session.feed` for local panes). Channel `event.bytes` is
the wake, never VT.

`scripts/check-vt-channel.py` fails if product phux source posts anything
but that wake, or if a phux channel handler feeds `event.bytes` as VT.

---

## State is said with the accent, not with elevation: SETTLED

**Decided 2026-08-14.** `phux-cockpit-2q8`.

The selected tab, and the switcher's cursor row, each carry an accent marker on
top of their lighter fill. That is deliberately TWO signals, reversing a comment
that had argued for one, and the reason is a measurement rather than a taste.

The fill difference was the whole signal. Measured against the app's own tokens:

```
surface_subtle  on surface  = 1.07 : 1     the selected tab
surface_pressed on surface  = 1.26 : 1     the switcher's cursor row
border          on surface  = 1.61 : 1
```

WCAG 2.1 SC 1.4.11 asks **3:1** of *"visual information necessary to indicate
state"*. Text is excluded from that criterion, so the brighter label was never
the thing under test; the fill was, and it is not close.

**It cannot be fixed by choosing a better grey.** Material's dark-theme
elevation model expresses depth as a white overlay whose alpha rises with
elevation — 5% at 1dp through 16% at 24dp — and the whole of that range spans
**1.00:1 to 1.60:1**. Reaching 3:1 against this ground needs a relative
luminance around 0.121, a mid grey, at which point the selected tab has stopped
being a tab and become a button. Depth in a dark UI is a sub-3:1 signal by
construction, which is also why 1.4.11 exempts elevation as decoration.

So the rule, and it is general: **elevation says near or far; the accent says
here.** One accent verb, already spoken by the focused pane's edge, now spoken
by the tab strip and the switcher too. `accent` on `surface` measures 14.10:1.

Shadow was never an alternative. A shadow works by darkening what is behind it,
and on a `#090b0f` ground there is nothing left to take away — which is *why*
the overlay-lightening model exists. On a GPU canvas a hairline also wins on the
merits: one quad against a multi-tap separable blur and an offscreen target, and
a blurred edge cannot land on the device-pixel grid at @2x while a snapped
hairline can.

The earlier removal of the underline was not wrong about its own evidence: an
accent rule under a rounded pill *is* clipped by the pill's radius. The bar is
inset a full gap on each side now, which clears a 6pt corner entirely.

The `ts-chrome-parity` extension test asserts both halves — that the two fills
are under 1.5:1, so nobody "fixes" this by lightening a surface, and that the
accent clears 3:1 on every ground it lands on. Full derivation and sources in
[docs/DESIGN_SYSTEM.md](DESIGN_SYSTEM.md).

---

## Terminal pixels stay in the native display list beneath markup chrome

**Decided 2026-09-02, by reuse rather than measurement; reopen with a number.**

The shipping TypeScript graph paints grids through
`terminal_painter.paintWindowIndex` on the engine's model, as a variable-length
command prefix under the markup widget tree. The
alternative in `docs/TS_MIGRATION.md` (a `media-surface` leaf fed by a native
RGBA producer) was not built.

Why this way first: it costs no painter code, keeps the packed `cell_grid`
command and its per-row AppKit decoder, keeps the incremental patch path, and
keeps accessibility where it already is. The media-surface route would lose all
four and needs a CPU rasterizer on screen, which is the automation renderer that
`docs/RENDER_FIDELITY.md` says cannot stand in for CoreText. The one thing this
route cannot do is let markup lay out *around* the grids: the strip is 50pt and
the rail 184pt on both sides of the seam by construction
(`workspace_projection.zig`), not by the markup telling the painter.

The baseline, measured 2026-09-02 on this machine by the extension's
`MEASURED: the chrome-prefix paint of a full grid on the engine model` test
(`zig build test -Dplatform=null -Dmeasure=true
-Doptimize=ReleaseFast`): a full 80x24 grid at 1100x640, scale 2, paints as
29 commands in 42 us per paint (472 us in the Debug test build). A surface leaf
would have to rasterize the same grid, upload it, and composite it in less
than that plus the display-list decode it saves, and it would do so without
the incremental cell-patch path. That is the number to beat.

What would reopen it: a measured leaf route under that figure, or a chrome
layout the fixed geometry cannot express.

## The bounded cwd snapshot field stays on protocol version 1

**Decided 2026-09-04.**

The cwd bytes added to each bounded snapshot-tab record do not bump the
TypeScript/native seam above version 1. The encoder and decoder are statically
linked into one Cockpit binary; packets are neither persisted nor exchanged
with an independently deployed peer, and the decoder rejects malformed or
trailing framing. Version 1 therefore names the current lockstep internal seam,
not a compatibility promise to an older producer.

Reopen this when a packet can outlive the process or cross a separately
versioned deployment boundary. That change requires a version bump and an
explicit compatibility policy.

---

## Paint ceilings: Hybrid C (focused full, unfocused degraded)

**Decided 2026-09-14, Cockpit pkg3b / Metal hybrid C. Pin signed 2026-09-15.**

Metal's policy, closed here:

1. Measure paint bind points at N=2/4/8 (and note N=1 / N=16), then bump
   the SDK paint tables so **2–4 full-fidelity 320x96 panes** fit without
   heroic partitioning. Cockpit derives every constant from the SDK and
   the product grid. Do not chase 16 full grids in one envelope.
2. Fidelity tiers: focused pane(s) of the **active** window paint full;
   unfocused panes, and every pane in an inactive window, paint degraded.
   A multi-window "this other window stays full" escape is allowed later,
   not required day one.
3. Kill equal-cut glyphs-with-no-slack. Commands/text/paths/cells may keep
   forward-slack **inside a tier**, not as a fleet-wide equal split.
   `widget_cell_reserve` (`store / 2`) is the SDK's two-pane leftover, not a
   production floor, and Cockpit does not read it.
4. Keep `atlas_variants_per_glyph = 4` alone.
5. Record a regression against "N full panes share one envelope forever."

### What the signed pin holds

Product grid: `session.max_cols=320`, `max_rows=96`, `max_cells=30720`.
SDK pin `phall1/native` @ `ad3f0fae36a7d1380c459c6b23ed12d83cad6a7a`
(Metal Hybrid C signed bump; native PR
[phall1/native#13](https://github.com/phall1/native/pull/13)):

| Table | Value | Notes |
|---|---|---|
| commands / view | 2048 | chrome envelope 1792 after `widget_command_reserve=256` |
| path elements | 2048 | |
| glyphs / view | 8192 | `widget_glyph_budget=7680`; 4 atlas variants/glyph |
| cells / view | 131072 | 20-byte cells; 2560 KiB builder + 2560 KiB retained (4x) |
| text bytes / view | 131072 | interned per row (2x); one unique-CJK 320x96 pane is ~92160 |
| `widget_cell_reserve` | store/2 | **not used** |

`maxFullPanesThatFit = store / max_cells = 4`. Two full product grids
are 61440 cells. Sixteen are 491520. The regression in
`paint_budget.zig` pins both facts: `max_cells * 2 < store` (signed) and
`max_cells * layout.max_panes > store` (forever). Do not flip the second
without an explicit decision to chase 16x.

Packed `cell_grid` is one command per row, so a truecolor or ASCII 320x96
screen costs ~96 row commands plus a small prologue — well inside 1792.
Box-drawing (U+256C, 8 commands/cell) overflows the command envelope at
40x24 already; a full 320x96 box screen cannot fit N=1. Do not bump
commands to chase that.

Unique 3-byte clusters at 320x96 want ~92160 interned bytes, which is
why text moved with the cell bump. Typical ASCII interned cost is the
alphabet, not the cell count.

### Hybrid C on this pin

`src/cockpit/native/paint_budget.zig` derives the split:

- Focused, active window: `full_cells = min(max_cells, store)` = 30720.
  Leftover after one full pane is 100352. `cell_reserve` holds leftover
  for later degraded panes.
- Unfocused / inactive: share leftover, capped at
  `max_cols * (max_rows / 4)` = 320x24 = 7680. The cap binds: leftover
  after one full pane would otherwise paint nearly full. Unused leftover
  after the cap is held so a last thumbnail cannot spend the rest of the
  store.
- Glyphs: focused keeps `widget_glyph_budget` minus degraded holds
  (`glyph_budget * degraded_cells / full_cells`). Not `/ N`.
- Single pane, active window: leftover after the full grid stays in
  `unused_cells` (`store - full_cells`). The pane still paints the whole
  320x96 grid; that leftover is unused slack, not the SDK two-pane floor.

Equal-cut at N=2 would give both panes half the store and starve glyphs.
Hybrid C gives the focused pane the full 96 rows and an honest thumbnail
to the rest.

The SDK painter emits top-first and drops the bottom. Leftover-budget
truncation without a crop is therefore **first-N**, which hides the
prompt. **Last-N crop** is the shipped degraded meaning (Cockpit #656):
snapshot the last `min(allowance/cols, max_rows/4)` rows, move cursor and
select-head with the crop (drop them when they sit above it), then paint.
Thumbnail / lower glyph density is still the mechanical budget; last-N
is which rows those cells show. On this pin the 24-row cap binds, so a
neighbour at N=2 keeps 24 trailing rows, not leftover-limited 6.

`scripts/drive-shell-ceiling.sh` is live macOS PTY evidence (~2.7 MiB rss
per shell, `max_effect_ptys`). It is not a paint bind. Linux hosts cannot
run Cockpit `zig build` (the graph is macOS-only). The paint tables above
are the pinned SDK sources (`src/runtime/canvas_limits.zig`,
`terminal_grid.zig`) at `ad3f0fae`. Runnable measurement is
`scripts/measure-paint-ceiling.sh` on macOS
(`zig build test -Dplatform=null -Dmeasure=true`). Linux source
arithmetic is not enough for this pin move.

### Signed SDK bump (Metal, 2026-09-15)

| Table | Was | Signed | Why |
|---|---|---|---|
| cells | 32768 | **131072** (4x) | 4 x 30720 = 122880, 8192 slack. |
| text | 65536 | **131072** (2x) | one unique-3-byte 320x96 pane is ~92160 bytes. Do not 4x unless two unique-CJK full panes are in scope. |
| glyphs | 8192 | **keep** | bind was equal-cut, not the ceiling. |
| commands | 2048 | **keep** | packed grids at 4x96 rows fit; box-drawing does not at N=1. |
| paths | 2048 | **keep** | same as commands; box geometry. |
| atlas variants | 4 | **keep** | |

Cell memory at 20 B/cell, builder + retained per view:

| Store | Builder | Builder+retained | x5 Cockpit windows | x32 SDK view slots |
|---|---|---|---|---|
| 32768 (previous pin) | 640 KiB | 1280 KiB | 6.3 MiB | 40 MiB |
| 131072 (signed) | 2560 KiB | 5120 KiB | 25 MiB | 160 MiB |

Address space is reserved per view slot; pages are touched as used. 160 MiB
for 32 slots is the conservative envelope, not resident RSS of a 5-window
Cockpit.

Text 65536 → 131072 is +64 KiB x2 x views: +640 KiB across 5 windows.

### Tier thresholds (grounded in leftover after the bump)

| Tier | Who | Cells | Rows at 320 cols | Glyphs (derived) | Commands (packed) |
|---|---|---|---|---|---|
| full | focused, active window | `min(max_cells, store)` | 96 | remainder after degraded holds | `max_rows` hold |
| degraded | unfocused / inactive | `min(leftover/n, max_cols*(max_rows/4))` | **24** (cap binds) | `glyph_budget * cells / full_cells` | `max_rows/4` hold |

After the 4x cell bump, leftover after one full pane is 100352. Without the
7680 cap, three unfocused panes would each get ~33k cells and paint full,
which contradicts the tier. The cap is what keeps "degraded" meaning
degraded once the store can hold 2–4 full grids. Text, glyphs, and paths
use the same degraded-first remainder and saturate so N degraded panes
cannot overflow the store (`plan` at N=8). Last-N crop uses the
same row cap (`max_rows/4`).

### Last-N crop (shipped)

**Decided 2026-09-14, Metal follow-up to Hybrid C.** First-N that hides
the prompt is not the lasting tier. `src/terminal/render.zig` crops the
snapshot (`row_fit = .last_n`) before the SDK painter runs. Production
Hybrid C always sets that fit; `grid.paint` callers keep the default
`from_top` so existing tests still see SDK first-N. Shipped in Cockpit
#656; this pin bump does not change that crop.

Reopen this if a measured bind after the bump disagrees, or if 16 full
panes become a product requirement.

