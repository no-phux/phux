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
invariant is pinned by `settings_theme_tests.zig`, "the settings surface stays
readable when the theme is broken", which measures the tree rather than
comparing against the constants — so a future palette swap still has to keep it
readable.

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
