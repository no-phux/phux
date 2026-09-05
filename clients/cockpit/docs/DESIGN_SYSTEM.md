# The chrome register

`phux-cockpit-2q8`. What every band, panel, control and gutter in this app's
CHROME is allowed to be, and the measurement or citation behind each number.

This is not a literature review. Every value here is either a token the SDK
already ships, a number measured on this machine, or a threshold from a
standard — and the ones that are taste say so.

**Scope: chrome only.** The terminal grid — cell metrics, the font, the
painter — is out of scope and stays out. See "The grid is not on the grid".

---

## 0. The one rule

**Nothing in the chrome is a number somebody liked.** Every extent traces to
the Geist theme pack's own tokens (`canvas.DesignTokens`), the 4pt grid, or a
stated threshold. When those disagree with each other, the SDK's token wins,
because the toolkit paints the controls and a value it did not choose is a
value it will fight — `header_height` is 50 because `tabs_trigger_height` is 50,
and it is the one extent in the app that does not divide by four.

The SDK's own guidance, quoted:

> **One size register per row**: every control class shares the control height
> at a given register […] so a toolbar/filter row reads as one height exactly
> when every control in it carries the SAME `size` — mixing `size="sm"` buttons
> with a default field renders two heights in one row, and hand-sized pressable
> panels (`height="30"`) never land on the scale.

— `native skills get native-ui`

---

## 1. The register this app is on

`cockpitTokens` resolves `.pack = .geist`, so these are the live numbers, read
out of `primitives/canvas/themes/geist.zig` at the pinned SDK:

| token | value |
|---|---|
| `spacing` | xs **4**, sm **8**, md **12**, lg **16**, xl **24** |
| `radius` | sm/md/lg **6**, xl **12** (floating surfaces) |
| `metrics.control_height_sm` | **32** |
| `metrics.control_height` | **40** |
| `metrics.control_height_lg` | **48** |
| `metrics.tabs_trigger_height` | **50** |
| `metrics.tabs_indicator_thickness` | **2** |
| `metrics.icon_text_step` | **2** (icon extent = companion text size + this) |
| `typography.label_size` | **13** |
| `typography.body_size` | **14** |
| `stroke.hairline` / `.focus` / `.focus_offset` | **1** / **2** / **2** |
| `motion` | fast **150**, normal **200**, slow **300**, easing `standard` |
| `min_pointer_hit_target` | **18** |

Two derived constants this app adds, and nothing else:

```
chrome_band_height   = 40   // one default-register control
chrome_band_inset    =  4   // spacing.xs shoulders -> a 32pt sm control fits exactly
chrome_control_extent= 32   // control_height_sm
chrome_icon_extent   = 16   // see section 4
chrome_gap           =  8   // spacing.sm
```

**A band is exactly one default-register control tall, and it hosts small-register
controls with 4pt of shoulders.** 40 = 32 + 4 + 4. That single sentence is the
whole band system: the search band, the config-notice band, and the tab strip's
row all obey it, and there is no fourth padding to remember.

---

## 2. Spacing: a 4pt grid, with 8 as the preferred step

Scale: **4, 8, 12, 16, 24** — the SDK's `SpacingTokens` verbatim.
20 is legal (16 + 4) where 16 is too tight and 24 too loose; nothing else is.

Why a grid at all, from the source everyone cites and few read: the argument is
density math (1pt = 4px at @2x, 9px at @3x, so an odd base half-pixels under
scaling), that common screen sizes divide by 8, and — the honest one —
*"By removing 7 of every 8 spacing options, you reduce the amount of fiddling
available to you"* ([Jackson, *The 8-Point Grid*](https://spec.fm/specifics/8-pt-grid)).

Why **4** and not 8: the same article already pairs its 8pt UI grid with a 4pt
sub-grid, and the practitioner case against pure 8pt is a specific missing
value — *"16 is too less but 24 is too much"*
([De Schepper, *Goodbye 8-point grid*](https://uxdesign.cc/goodbye-8-point-grid-hello-4-point-grid-1aa7f2159051)).
Apple's own macOS layout numbers are natively 4pt-based: 20pt window margins,
**8pt between related controls**, 20pt between groups.
Tailwind is a pure 4px scale ([discussion #12263](https://github.com/tailwindlabs/tailwindcss/discussions/12263)).

This is a dense desktop tool. The 8pt purist position costs the two values
reached for most.

**2pt is an optical correction, never layout.**

---

## 3. Type

The chrome's type comes from the pack and is not restated here: label **13**,
body **14**, title **20**. That is also `NSFont.systemFontSize` = 13, so chrome
text is the same size as every other window on the user's screen.

**No modular scale.** A 1.2 / 1.25 / 1.333 ratio cannot express an 11→17pt band:
that span is 1.545×, which is 2.4 steps of a minor third. A perfect fourth from
11 gives 11, 15, 20 and cannot produce 12, 13 or 17 at all. Apple's own ladder
(10/11/12/13/15/17) runs at **1.08–1.15 in the label band**, widening to ~1.29
above — an irregular Bringhurst-style scale, not a constant ratio. Even the
person who built modularscale.com concluded he found no correlation between
beautiful layouts and strict ratio adherence
([Rendle, *Typographic scales*](https://robinrendle.com/adventures/typographic-scales/);
[Bringhurst 3.1.1](http://webtypography.net/3.1.1)).

**Line heights do not go on the 4pt grid, and this is not a compromise.** Of
the eleven macOS text styles only three (32, 20, 16) land on a 4pt multiple;
Title 2 is 22, Callout 15, Footnote 13. A baseline grid "does not automatically
give harmony… it's primarily a system for keeping heights consistent", and in
component UI it breaks because controls and images cannot be forced onto it
([Gombáu](https://medium.com/@gombau/the-baseline-grid-friend-or-foe-d4bec9ae595e)).
**Snap containers to the grid; let line boxes be whatever the text needs.**

Leading rises as size falls, which is the opposite of most people's intuition:
Apple runs 13pt→1.231, 12pt→1.250, 11pt→1.273, 10pt→1.300. UI labels sit at the
bottom of Butterick's 120–145% band
([*Line spacing*](https://practicaltypography.com/line-spacing.html)); body prose
sits at the top.

**Chrome prose never runs at pane width.** 45–75 characters is the satisfactory
measure and 66 the ideal ([Bringhurst 2.1.2](https://webtypography.net/2.1.2));
Butterick widens it to 45–90 ([*Line length*](https://practicaltypography.com/line-length.html)).
A 120-column terminal is 60% past the upper bound. Cap chrome text blocks around
66ch regardless of how wide the pane got.

---

## 4. Icons

**Every inline icon in the chrome is 16pt.** One number, three derivations that
agree:

1. The SDK's own rule: extent = companion text size + `icon_text_step`
   = 13 + 2 = **15**.
2. The cap-height recipe: icon = **1.65 × cap height**. SF Pro's cap ratio is
   0.7046, so 1.65cap = 1.163 × font-size = 13 × 1.163 = **15.1**
   ([Shadeed, *The CSS cap unit*](https://ishadeed.com/article/css-cap-unit/)).
3. Carbon ships 16px icons against 14px IBM Plex (cap ≈ 0.698em → ratio 1.64) and
   states they are "optimized to feel balanced" at that pairing
   ([IBM design language](https://design-language-website.netlify.app/design/language/iconography/ui-icons/design/)).

15.1 rounds to 16, which is also the standard icon artboard in every system that
ships one (Carbon 16/20/24/32, Material 24 trim / 20 live) and centres on whole
device pixels at 1x and 2x. **16.**

Material's 24dp default is **not** inherited: Material pairs 24dp with a 14sp
label, ≈2.0× cap, which is sized for a fingertip
([m1.material.io/style/icons](https://m1.material.io/style/icons.html)).

### Optical alignment, and the correction we deliberately do not make

Icons should align to **cap height**, not to the line box. Because SF Pro's
ascent (0.9668em) exceeds its cap height (0.7046em), the cap band's optical
midpoint sits *below* the line box's geometric centre, so a metrically centred
icon reads high — by **+0.33px at 13pt**, growing with size (+0.44px at 17pt).
SF Symbols solves this by defining its scales relative to cap height and
optically centring symbols on it, which is why symbol/text alignment is
automatic there ([WWDC19 s206](https://asciiwwdc.com/2019/sessions/206)).

This app centres on the line box (`cross = .center`) and accepts the 0.33px
error. Correcting it would need `ElementOptions.transform`, and a transformed
node is skipped by the SDK's layout audit (`auditWidgetLayout` returns early on
`nodeTransformed`) — trading a 0.33pt improvement for the loss of the only
machine check we have on that subtree is a bad trade. 0.33 is also below the
audit's own 0.5pt noise floor. **Stated, measured, and declined.**

### Overshoot

Curved glyphs and shapes overshoot flat ones. Measured from the actual TTFs
(`O` bounds vs declared cap height): SF Pro **+1.66%**, SF Mono **+2.08%**,
Menlo **+1.81%**, JetBrains Mono **+1.37%**; flat-topped glyphs measure exactly
0.00%. Published band is 1–3%, with Karow recommending 3% for `O` and 5% for `A`
([Overshoot](https://en.wikipedia.org/wiki/Overshoot_(typography))).

A circle needs **111–113%** of an equivalent square to read the same size:
Material's own keylines use a 20dp circle against an 18dp square (111.1%), and
the equal-area bound is √(4/π) = 112.84%. Two independent numbers a percent
apart is the strongest signal available.

**This app draws no custom circles.** The attention marker is a vector icon
(`circle-dot`) on the SDK's own icon grid, which owns its optical corrections;
reserving a bigger box for it would double-apply the compensation. The rule is
recorded because the next hand-drawn dot or triangle will need it.

---

## 5. Contrast

### Text — WCAG 2.x, as the contract

| criterion | level | floor |
|---|---|---|
| [1.4.3 Contrast (Minimum)](https://www.w3.org/WAI/WCAG21/Understanding/contrast-minimum.html) body | AA | **4.5:1** |
| 1.4.3 large text (≥24px, or ≥18.5px bold) | AA | **3:1** |
| [1.4.6 Contrast (Enhanced)](https://www.w3.org/WAI/WCAG21/Understanding/contrast-enhanced.html) body | AAA | **7:1** |
| [1.4.11 Non-text Contrast](https://www.w3.org/WAI/WCAG21/Understanding/non-text-contrast.html) | AA | **3:1** |

Thresholds are hard floors: the Understanding doc states plainly that 2.999:1
does not meet 3:1. Apple asks for the same 4.5:1 minimum and says to *"strive
for a contrast ratio of 7:1, especially in small text"*
([HIG: Dark Mode](https://developer.apple.com/design/human-interface-guidelines/dark-mode)).

Measured, `cockpitTokens` today — all pass, most clear AAA:

| ink | on `background` | on `surface` | on `surface_subtle` |
|---|---|---|---|
| `text` | 18.33 | 17.15 | 16.05 |
| `text_muted` | 7.81 | 7.31 | 6.84 |
| `accent` | 15.07 | 14.10 | 13.20 |
| `warning` | 14.94 | 13.98 | 13.08 |
| `destructive` | 7.12 | 6.66 | 6.24 |

### Non-text — 1.4.11, and the carve-out that matters

1.4.11 wants 3:1 for *"visual information necessary to indicate state"* and for
*"parts of graphics required to understand the content"*. Its exemption is the
load-bearing part for a terminal:

> If a control has visible content (such as text or a sufficiently contrasting
> icon), which helps users identify the presence of the control, then a border
> or other indication of the overall boundary of the hit area is not required.

So: **separators and control boundaries are decoration here and are exempt**
(every control in this chrome carries a label or a contrasting glyph).
**State is not exempt.** Which tab is selected, which pane has focus, which row
the palette's cursor is on — those need 3:1 or a signal that is not contrast at
all.

### The finding: elevation cannot carry state in a dark UI

Measured, this app's own tokens:

```
surface_subtle  on surface     = 1.07 : 1     <- the SELECTED tab
surface_pressed on surface     = 1.26 : 1     <- the palette's HIGHLIGHTED row
border          on surface     = 1.61 : 1
```

This is not a palette that was chosen badly. It is the ceiling. Material's
dark-theme elevation model expresses depth as a white overlay whose alpha rises
with dp — 5% at 1dp through 16% at 24dp — and **the whole 0→24dp range spans
1.00:1 to 1.60:1** ([Material dark theme](https://m2.material.io/design/color/dark-theme.html),
[MDC-Android](https://github.com/material-components/material-components-android/blob/master/docs/theming/Dark.md)).
Reaching 3:1 against `surface` (L = 0.00699) needs L ≈ 0.121, which is a mid
grey — a "selected" tab that light stops being a tab and becomes a button.

**Therefore: never encode state in elevation alone.** Elevation says *near or
far*. State is said with the accent, and this app has exactly one accent verb —
lime, reserved for "where you are". The selected tab carries the pack's own
2pt indicator (`tabs_indicator_thickness`); the focused pane carries a 1pt
accent edge. `accent` on `surface` measures **14.10:1**, four and a half times
the floor.

Shadows are not an option and never were: a shadow works by darkening what is
behind it, and on a `#090b0f` ground there is nothing left to take away. That is
*why* the overlay-lightening model exists. On a GPU canvas a hairline also beats
a shadow on the merits — one quad against a multi-tap separable blur plus an
offscreen target, and a blurred edge cannot land on the device-pixel grid at @2x
while a snapped hairline can.

### Dark-UI hygiene

- **No pure black ground, no pure white ink.** Material's own codelab records
  that `#FFFFFF` on dark *"would visually 'vibrate'"* and *"appears to bleed or
  blur"* ([codelab](https://codelabs.developers.google.com/codelabs/design-material-darktheme)).
  This app's ground is `#090b0f` and its ink `#f4f7fb` — both off the extremes.
- **Desaturate accents in dark.** Material's rule of thumb is the **200 tone**
  in dark against the 500 tone in light; Apple says *"tint colors get lighter in
  Dark Mode"*. The lime accent is already a high-lightness, moderate-saturation
  tone.
- **Sanity-check with APCA, contract with WCAG 2.** WCAG 2's `+0.05` flare term
  compresses the low end, and its authors' critics state flatly that it
  *"overstates contrast for dark colors to the point that 4.5:1 can be
  functionally unreadable"* and *"cannot be used for guidance designing dark
  mode"* ([APCA in a Nutshell](https://git.apcacontrast.com/documentation/APCA_in_a_Nutshell)).
  Ship the WCAG 2 ratio as the number a test pins — the settings panel's readout
  already does — and treat anything that passes 4.5:1 but lands below |Lc 60| as
  a bug regardless.

---

## 6. Hit targets

| guideline | size |
|---|---|
| [WCAG 2.2 SC 2.5.8 Target Size (Minimum)](https://www.w3.org/WAI/WCAG22/Understanding/target-size-minimum.html) | **24 × 24** — AA |
| [WCAG 2.1 SC 2.5.5 Target Size](https://www.w3.org/WAI/WCAG21/Understanding/target-size.html) | 44 × 44 — AAA |
| Apple HIG, Accessibility, **macOS** | default **28 × 28**, minimum **20 × 20** |
| Apple HIG, Buttons (all platforms) | 44 × 44 |
| SDK `min_pointer_hit_target` | 18 |

Apple's two pages contradict each other; the Accessibility page is the one
written for pointer input, and macOS is *"high-precision input"* at a 1–3ft
viewing distance. **44pt is a touch number and chasing it would destroy the
density that makes a terminal a terminal.**

**House floor: 24 × 24 for any chrome control.** It clears WCAG 2.2 AA exactly,
sits between Apple's macOS minimum (20) and default (28), and leaves the SDK's
own 18pt audit floor a real margin instead of passing it by zero.

2.5.8's escape hatch — an undersized target passes if a 24pt circle centred on
it does not intersect a neighbour's — is available but not used. A rule with an
exception is a rule nobody applies.

Fitts, in the form worth using (MacKenzie's Shannon formulation):
`MT = a + b·log₂(D/W + 1)`. Both terms are inside a log, so **doubling the
target buys the same time as halving the distance** — which is the argument for
spending points on the close affordance rather than on moving it.

---

## 7. Menus and the palette

Hick: `RT = a + b·log₂(n + 1)`. Doubling the option count adds a constant, not a
proportion — 8 items is 3.17 bits, 16 is 4.09.

**Hick does not justify a short command palette, and citing it that way is
wrong.** The law's own statement of scope excludes exactly this case: *"To find
a given word in a randomly ordered word list, scanning of each word in the list
is required, consuming linear time, so Hick's law does not apply"* — but an
ordered list the user can subdivide runs in log time
([Hick's law](https://en.wikipedia.org/wiki/Hick%27s_law)). A fuzzy-filtered
palette is a recall-and-type task whose cost is roughly constant in n. **That is
the argument *for* the palette**, not against its length.

What *does* bound the palette is the visible row count — Miller's 7±2 is about
what can be held in one fixation, and a list taller than the window is a defect
whatever the law says. **House rule: cap the palette's drawn rows and window
them around the cursor, exactly as the tab strip windows around the selection.**
Menus: ≤7 items per group, dividers between groups.

---

## 8. Motion

The pack's own ladder, and the only durations this app may use:

| token | ms |
|---|---|
| `motion.fast_ms` | **150** |
| `motion.normal_ms` | **200** |
| `motion.slow_ms` | **300** |

These sit where the evidence puts desktop UI. Material puts **desktop at
150–200ms**, ~35% faster than mobile, and caps everything at 400ms because
*"exceeding [it] feels too slow"* ([M1 duration & easing](https://m1.material.io/motion/duration-easing.html)).
NN/g measures 100ms as *"the illusion of physically manipulating the object"*,
200–300ms for substantial changes, and *"at 500ms, animations start to feel like
a real drag"* ([NN/g](https://www.nngroup.com/articles/animation-duration/)).

Easing: **decelerate in, accelerate out.** The fast head of an ease-out makes
the response feel immediate while the slow tail lets the eye settle on where the
thing landed; nobody needs to track something that is leaving. Never linear —
that is for indeterminate progress only. Do not put an emphasized curve on
anything under ~300ms; it reads as sluggish at short durations.

### The two absolute rules

1. **The terminal grid never animates. Ever.** Not the cells, not the cursor
   position, not the viewport. `workspaceChrome`'s comment already records why
   the band's own extent may not ease: the painter, the hit targets and the PTY
   sizing pump all derive from one rect, and a moving `content.height` emits a
   `.viewport` per frame, which is a SIGWINCH per frame — three full redraws of
   whatever TUI is running, per tab open.

2. **Reduced motion is honoured by replacement, not by removal.** MDN's guidance
   on `prefers-reduced-motion` is to swap the vestibular trigger for a non-motion
   equivalent — a cross-fade for a transform — not to hard-cut. Apple: *"Make
   motion optional… avoid using it as the only way to communicate important
   information"* and *"generally avoid adding motion to UI interactions that
   occur frequently"*
   ([Motion HIG](https://developer.apple.com/design/human-interface-guidelines/motion)).
   The SDK reports the preference on the appearance channel and
   `MotionTokens.reduced()` zeroes all three durations, so the honest wiring is
   to take the tokens' answer rather than to branch by hand.

What may move: chrome that is not the grid and does not resize it — a hover
wash, a focus ring, a palette reveal. What may not: anything that fires per
keystroke or per frame of terminal output.

---

## 9. The grid is not on the grid

The one place where the 4pt system is asked to do something it cannot, written
down so nobody tries again.

The terminal's cell height is `ascent − descent + lineGap`, a **font metric**
quantized to whole device pixels. It is not leading and there is no ratio to
choose. Measured for the bundled face:

| JetBrains Mono | value |
|---|---|
| advance / em | **0.6000** |
| hhea line height / em | 1.3200 |
| cap / em | 0.7300 |
| x-height / em | 0.5500 |

That 0.6000 is exact, and it is the independent confirmation of the assumption
`terminalCellMetricsFor` already rests on ("cockpit's terminal face IS a 0.6 em
monospace"). It is not a universal: **SF Mono measures 0.6182**, 3% wider, so
code that hardcodes 0.6 for an arbitrary face drifts a cell every 33 columns.
The test "the registered terminal face is a 0.6 em monospace" is what keeps a
font swap from quietly reopening that.

At 13pt the cell is 7.8 × 17.16. **No integer number of rows ever lands on a 4pt
multiple** — two rows is 34.3, three is 51.5, four is 68.6. So:

- Snap the **container** to the 4pt grid.
- Let the row grid float inside it and absorb the remainder.
- **Never derive a chrome spacing value from cell height**, and never derive a
  cell value from a chrome constant. `terminalTokens` exists to keep the two
  token sets apart for exactly this reason.

There is essentially no published writing on aligning proportional chrome to a
monospace cell grid. The rules above are measured house convention, not citation,
and they are labelled as such.

---

## 10. Platform metrics, measured on this machine

macOS 26.5.1. These move between OS releases; **read them at runtime, do not
hardcode them** — the "28pt titlebar / 52pt unified toolbar" figures that circulate
are pre-Tahoe and are now wrong.

| thing | measured |
|---|---|
| `.titled` window chrome, no toolbar | 32.0 pt |
| `.titled` + `.unified` toolbar | 66.0 pt |
| `hidden_inset_tall`, as this app declares it | `chrome_top` = **66**, band = **62** |
| `NSButton` `.regular` / `.large` | 24 / 28 pt |
| `NSColor.separatorColor` (dark) | white at **9.8%** |
| `windowBackgroundColor` (dark) | `#1E1E1E` |
| `controlAccentColor` | `#007AFF` |

The 62pt band is the one this app depends on: `tabsRideTitlebarIn` needs it to
be at least `tab_height + 8`, and it was read off the running app
(`native automate snapshot` → `role=group name="Phux Cockpit window"
bounds=(8,8 1084x62)`) rather than assumed. Traffic lights sit at 20/40/60pt
from the window's left edge, which is where `titlebar_tab_leading_reserve = 78`
comes from.

---

## 11. What proves it

- **`zig build test`** — the register, the ratios and the contrast floors are
  pinned by the `ts-chrome-parity` extension test. A regression test
  here must fail without its fix; a floor that has only ever been seen to pass
  is not evidence.
- **The SDK's layout audit** (`canvas.auditWidgetLayout`) is wired in as a gate.
  It reports text that loses glyphs, siblings that overlap, widgets that escape
  their clip scope, and controls under the pointer floor — with widget-path
  precision, across a sweep of window sizes, densities and a 1.35× pseudo-locale
  text expansion. It is geometry-only and deterministic, and it re-uses the exact
  measurement seam layout and paint use.
- **`./scripts/dev-run.sh --debug`** then read `.dev-run/app.log`. A spacing
  change that broke something shows up as `zero_canvas_layout` /
  `zero_canvas_ui`.
- **Not screenshots.** `native automate screenshot` renders through the CPU
  reference renderer and cannot see anything CoreText does. Read
  [docs/RENDER_FIDELITY.md](RENDER_FIDELITY.md) before writing any test that
  claims to know what this app looks like.

---

## Corrections to things people repeat

Recorded because each one was believed here at some point during this pass.

1. **The macOS titlebar is not 28pt and the unified toolbar is not 52pt.**
   Measured on macOS 26: 32 and 66.
2. **44 × 44 is not the macOS number.** Apple's Accessibility page gives macOS
   default 28, minimum 20.
3. **"White at 8–12%" for separators has an exact answer**: macOS
   `separatorColor` is white at 9.8%.
4. **Material's `#121212` is not what a native Mac app should use.** Apple ships
   `#1E1E1E`. Take Material's elevation-as-lightness *method* and the platform's
   palette.
5. **Hick's law does not justify a short command palette.** The literature
   explicitly excludes searchable and ordered lists. It justifies short *menus*.
6. **A modular scale is the wrong tool for an 11–17pt band.** Every canonical
   ratio is too coarse to express it.
7. **Elevation cannot carry state in a dark UI.** The entire 0→24dp range is
   1.00:1 to 1.60:1, and 1.4.11 asks 3:1 for state.
