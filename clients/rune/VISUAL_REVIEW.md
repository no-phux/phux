# Visual review: improve a real task every tick

Every upstream follower tick ships one contained, evidence-based visual or UX
improvement, including bumps and quiet runs. Passing rendering tests, moving
pins and preserving identical pixels are maintenance, not that improvement.
Fix real friction, not arbitrary cosmetic churn. [END_STATE](./END_STATE.md)
says where every change is headed.

Tests establish behavior and geometry, not an aesthetic score. Inspect the
images and explain whether the experience improved.

## Pick one category

Journal the category and next category. Rotate in this order; override for a
stronger observed problem and explain why. Revisit skipped categories.

| Category | Questions for before/after judgment |
| --- | --- |
| Alignment and spacing | Are related items aligned/grouped with consistent, economical margins? |
| Hierarchy and readability | Are the next action, names and status clear without decoding icons/chords? |
| Density and responsive layout | Do long names, wrapping, scrolling and the last row work at small sizes? |
| Navigation and discoverability | Can users find actions, orient themselves and leave without losing work? |
| Input and touch ergonomics | Are phone targets at least 44×44 logical points, separated and reachable? Is input ownership correct? |
| Feedback, empty/error/loading | Are state and next safe action clear without raw errors or secrets? |
| Motion and perceived responsiveness | Is response immediate without jumps/stale frames, including reduced motion? |
| Accessibility and contrast | Are names, roles, focus and contrast clear in both themes, without colour-only status? |

Preserve the good command palette, terminal input ownership, literal pane
arrangement, native safe areas and production isolation. No forced menu API,
new framework, AI dependency or scoring system for a small improvement.

## Capture real application states

From the repository root of the change worktree:

```sh
bash clients/rune/scripts/visual-review.sh /absolute/home/scratch/TICK/before
# Implement and test the selected improvement, then:
bash clients/rune/scripts/visual-review.sh /absolute/home/scratch/TICK/after
```

Use fresh absolute directories outside Git. Capture before implementation;
for bumps, baseline includes the old pin. Record both revisions and dirty
diff identities. Run new fixtures against baseline in an isolated worktree;
distinguish fixture-only from app changes. Candidate captures are not "before".

The runner reuses real Rune `Runtime<Desk>`/GPU exporters for Settings,
navigation, the command palette (`palette_visual_review_frames`), resources
and `phone_inbox_visual_review_frames`, not mock HTML
or a second UI. Extend coverage near behavior tests. Use deterministic IDs,
realistic text, fixed timing and explicit settling. Seed inventory, then use
actual handlers/events for the interaction under review.

Output: PNGs, matching element/accessibility trees and `manifest.json` with
source revision/dirty state, scenario/state, theme, logical viewport, scale,
paths and exporter. PNG dimensions equal logical dimensions × scale. Missing
frames/trees or failed exporters fail the run. No credentials, signing/device
data, live user content or private links in fixtures or public evidence.

### Required core gallery and selected-flow matrix

Keep a populated core gallery every tick, not just the selected happy path:

| Core surface | Realistic content and states |
| --- | --- |
| Settings | Grouped sections and current values; category drill-down; long labels; typed search/no match; in-progress update feedback. |
| Phone navigation | Several sessions and panes with long names/folders; mixed working/attention/quiet states; current pane; scrolled bottom; matching search with keyboard and no matches. |
| Attention and contrast | Populated desktop sidebar and selected tab with a waiting agent; Inbox closed, keyboard-focused via Focus Sidebar then Shift+Tab, and opened with Space (native button activation). Normal and forced colours, light/dark, 1100×720 and 640×360, at true 2×. Phone Inbox remains in the existing core matrix; phone tabs have no attention glow. |
| Command palette | Populated terminals with working, blocked and done agents, sessions, commands; empty list, typed matches, no match, Themes and Sessions modes; phone keyboard up and largest text. 1100×720, 640×360, 393×852, 320×568, light/dark, 2×. |
| Resources | Actual decoded image beside a structured native document/accessibility tree; readable text and labels, not an empty placeholder. |
| Phone Inbox | Populated questions and approvals, long agent/detail text, suggested replies; focused reply with keyboard; empty state and a safe dismiss path. Delivery cases include pending, acknowledged, refused and unconfirmed answers, plus a newer draft and another question before an older receipt. |

Retain the exporters' existing sizes. The gallery must include 1100×720
desktop, the existing minimum desktop case (640×360), 393×852 phone and
320×568 small phone, with light and dark represented. Phone-only surfaces
need not become desktop designs; desktop-only fixtures need not pretend to
be native phone screens.

For the **selected flow**, compare matched before/after states at every
applicable size above in light and dark. Include populated, constrained
long-content, scrolled and interactive/focused states, plus empty/error/loading
states where the flow owns them. For phone editing, show keyboard-open as
well as keyboard-closed, the final reachable control and content above the
keyboard. Add a selected-flow 2× capture with true runtime scale, not a resized
PNG. For native layout, exercise the existing safe-area/keyboard-reduced
viewport too; desktop drawn keys are not UIKit proof.

Do not multiply every screen by every case. Cover the gallery and selected
flow, explain inapplicable cases and extend the nearest exporter for gaps.
Do not lower the requirement or build a giant screenshot framework.

Settings size-control cases use the integrated app view, its actual text
scale and explicit scroll reveal of the selected button. Compare standard
and largest iOS accessibility text on both phone widths, including reduced
native-sized 2× viewports; desktop controls use the desktop's largest 140%
text scale, not an iOS setting. Check both buttons' complete visible bounds,
label/value containment and non-overlap, real touch/keyboard activation and
range limits. A 44-point visible fragment of a taller button is not a pass.

Theme-row cases use the integrated view at those same scales, with real search
and palette preview/cancel/accept. Retain whole-row focus reveal **and** an
explicit heading-aligned scroll: a description taller than the viewport can
reveal its bottom on focus, hiding the title/value being judged. Heading frames
show reading composition; they do not claim automatic whole-row reveal is fixed.

Accessory cases use integrated Settings editing at standard, AX2, AX3 and
AX5 text sizes, both strip edges and a fully revealed Tab, both phone widths
and native keyboard-reduced 2× viewports. Check the complete pinned contextual
action/Done targets, their text/icon containment, and every strip control
revealed individually. Retain Sessions editing as a surrounding composition
check: Sessions and Actions deliberately have no accessory bar, and Return
finishes their search without clearing it or changing the terminal keyboard.
Check that the persistent overflow indicator appears only when needed,
follows the actual offset and consumes only the existing bottom inset, never
the keys' hit area or more content height.
Reveal the pad, then exercise its hold/drag/cancel without scrolling its
ancestor or leaking input into the terminal. Done preserves the field draft,
sheet and terminal keyboard preference. Use Settings-to-terminal transitions
for the fixed-clock resize check; retain the older list transitions too.
Retain the whole-create-action check
above the native keyboard, not just a visible fragment. Keep the desktop
drawn-keyboard cases too; their inherited large-text glyph clipping is not
native UIKit behavior or evidence that the accessory fix repaired that stand-in.

Sessions canvas cases add an empty inventory and a long unbroken filter at
true 2× on both native-sized phone widths. At AX5, compare the whole Connections
destination after explicit keyboard-focus/reveal requests and the long-filter editor plus the
explicitly scrolled final action. Preserve requested text sizes and complete
controls; an offscreen fragment is not a pass. Explicit bottom scrolls are not
claims of Tab-driven focus reveal. Long-filter cases use the keyboard-reduced
320×340 and 393×480 viewports. Ordinary UIKit Glass remains separate
from the large-text opaque inline action.

Sessions search recovery cases retain matching, no-match, long and blurred
filters, standard and AX5 text, both phone widths, full and keyboard-reduced
native-sized viewports, light/dark and true 2×. Compare the complete Clear search
target and remaining editor width. Exercise touch, accessibility Click and real
Tab/Enter/Space: clearing restores the first results, cancels composition and
keeps replacement typing in the editor. A stale native edit cannot restore the
old query; a covering modal and cancelled press cannot clear it. Preserve the
terminal's keyboard preference, selected pane and server arrangement.

## Review behavior as well as pixels

Sidebar keyboard cases start in a non-leading split pane. Compare idle,
Focus Sidebar, Shift+Tab to Inbox and Space activation at both desktop sizes,
light/dark, forced colours and 140% interface zoom, at true 2×. The keyboard ring
must mark the actual grouped row without selecting
another pane or changing its arrangement. Behavioral tests additionally cover
agent-led identity, missing members, unplaced/document rows, keyless sessions
and unchanged terminal input. These are desktop keyboard cases; the core phone
gallery remains surrounding nonregression evidence, not a phone-sidebar claim.

Sidebar hierarchy cases add actual project/linked-worktree sessions, naturally
visible descendants, flat singles, real inline splits, remote-machine previews
and empty inventory. `single` and `split` replace obsolete `collapsed` and
`children` scenes; local folding no longer exists. Retain
the app's macOS traffic-light reservation, standard and 140% interface zoom,
forced colours, both desktop widths and real 2× backing scale. Zoom changes
the logical viewport and effective scale; it must not resize a finished PNG or
loosen the geometry contract. Compare arrow-free leading edges, section rhythm,
unboxed attention summaries and the complete machine menu. Check optional
keep-awake controls, independent session/resource drag ownership, Left/Right split-member navigation,
focus, and unchanged terminal input and arrangement. A retained but covered
sidebar element is not a valid pointer target: open the narrow drawer through
its handler after initial layout and verify the actual hit path.
The current inline split member must remain identifiable when forced colours
make every caption the same ink: retain its quiet underline and accessible
"Current pane" description. A pending or cancelled selection must not move
that cue before the shown resource changes. Keep the containing row's single
keyboard stop and the members' existing hit bounds.

Reconnect feedback cases retain an attached terminal, then exercise an ordinary
drop, a long explanation and a real failed pairing status. Compare top, scrolled
bottom and keyboard-focused Retry on both phone widths and native keyboard-reduced
2× viewports at standard/AX5 text sizes, light and dark. Require complete recovery
targets above the accessory and unchanged terminal geometry while scrolling.
Keep desktop/minimum and 140% interface-zoom captures as nonregression evidence.
The live-server comparison induces a real loss only after witnessing attachment;
run it separately from GPU exporters to avoid capture-window contention.

ACP multi-field form cases retain field-heading and focused-editor scrolls,
selected choices, final actions, required errors and in-flight responses.
Compare ordinary/long content at standard and largest native text sizes,
including both keyboard-reduced phone widths at true 2× and the unchanged
desktop composition. Independently finish touches on held Submit and Cancel:
neither may create an error, change a draft or keyboard preference, or send
input. Keep genuine enabled validation. Record extra scroll cost and inherited
oversized-heading/header clipping rather than calling spacing a fix for them.

Use the trees and behavior tests to check focus/name/role, clipping/scroll
reachability, actual hit bounds, keyboard routing and action semantics. A
44-point painted button with a smaller effective hit target does not pass.
Exercise activation and dismissal, not just the resting appearance. For
motion, compare defined intermediate and settled frames and relevant timing
evidence; a still image cannot prove smoothness or physical-device feel.

The first worked example (November 17) is **phone Inbox ergonomics**: 44-point
targets, plain approval names, a visible Close matching existing Escape
dismissal, wrapping long suggestions/ACP controls, and a typed reply anchored
above the keyboard while questions scroll. It does not change Send, controller
code or draft lifetime. Prove existing dismissal/draft semantics, scrolling,
keyboard clearance and no input leaking into the terminal. Do not force menu
API adoption. This is not a standing instruction to churn Inbox afterward.

Motion cases (pane motion, batched swipes, swipes away, every page's own
control and the switcher's card) bind fixed-clock frames to `.motion.json`
sidecars; what each exports and asserts is in
[motion review](./VISUAL_REVIEW_MOTION.md).

Pane-picker cases compare long, similarly named panes at standard and AX5
text sizes, light/dark, both phone widths and native keyboard-reduced 2×
viewports. Retain top, keyboard-focused row and scrolled final-action frames.
Names may be ellipsized, never spilled outside their targets; visible IDs
distinguish similar names and accessible labels retain the full identity.
Require every focused target and its text to fit completely in the scroller,
including Done and Terminal actions. Exercise both final touch actions,
hostile-but-valid names, input trapping and unchanged literal arrangement.
The toolbar remains 44 points: picker typography must not resize the terminal.

Machines colour cases exercise the actual sheet in the desktop-hosted phone,
not the separate native UIKit connection controller. Compare the open palette
and keyboard-focused Auto at both desktop widths and both phone widths,
light/dark, standard/AX5 text, drawn keyboard open/closed and true 2×.
Each revealed phone radio must have a complete visible and effective touch
target of at least 44 points, containing its scaled artwork without overlapping
neighbours. Keyboard reveal must leave the focused Auto above the sheet's edge
fade, not merely inside its clipping rectangle. Retain desktop sizing and radio names/roles, real touch at the
enlarged edge, keyboard/accessibility activation, cancelled touches, the field
draft and terminal input/layout ownership. The inherited large-text sheet
header and drawn-keyboard clipping are not claimed fixed or native iOS proof.

Inspect full images and the changed region's surroundings. Explain upstream
rendering changes too. Reproduce regressions, fix their cause and recapture.
Never delete hard cases, weaken assertions or regenerate until it looks good.

Before merge, obtain **independent fresh-context visual review** of images
and metadata: task, visible improvement, small/keyboard/light/dark/2× cases
and an explicit regression verdict ("none observed" needs evidence).
Code-only review is insufficient. Resolve every finding; recapture fixes
and have changed evidence reviewed again.

## Judgment record

Store the full record beside private scratch evidence; put a concise,
sanitized version in the PR and category/next-category in the journal.

```text
Tick / baseline revision / candidate revision (+ dirty diff identity):
Selected category / next category / rotation override and evidence:
User task and observed before problem:
Contained change and expected user benefit:
Evidence: before/after manifests; matched scenario/frame pairs:
Coverage: desktop/minimum, phone/small, light/dark, keyboard, 2×:
Before judgment: what is hard to see, reach, read or understand?
After judgment: what is visibly easier, and why?
Preserved behavior: focus/input, dismiss/Send, draft, scrolling, pane layout:
Regressions: explicit verdict per affected case; inapplicable cases and why:
Independent reviewer / frame references / verdict / findings and resolution:
Behavior checks and visual exporter commands/results:
Remaining device-only claims or exact external blocker:
```

Acceptance: one defensible visible improvement, relevant matrix, passing
behavior checks and independent no-unresolved-regression verdict. No synthetic
pass percentage. External blockers are blocked, not maintenance-only success.

## Keep qualification and delivery separate

This gallery supplements `just rune-check`, `just rune-deny`, the repository
PR gate, packaged qualification, full native receipts and paired QUIC/WSS
simulator qualification. It never substitutes for them. Screenshots and
simulator keyboard tests do not qualify physical-phone accessibility, OS IME,
scroll feel or device installation.

Native qualification publishes its tree before its readiness report, only
after a drawn frame contains the selected terminal's closed `[[phase-marker]]`
witness. The collector requires that actual tree text and matching phase,
terminal and frame metadata. A live-grid report or correct later screenshot
cannot make an earlier stale tree pass. Keep failed evidence; fix the binding,
not the expected marker or the capture until it looks acceptable.

Scheduled delivery still follows [DELIVERY.md](./ios/DELIVERY.md): exact
qualified main revision, native gates, signed ad-hoc bundle, complete HTTPS
artifact verification, in-place install and independent marketing/build query.
Every tick's private final summary returns the **same existing permanent
tailnet `/scheduled/` page**, selecting the latest qualified, signed,
completely published build. Keep immutable per-build URLs as internal
evidence/fallbacks, not rotating user bookmarks. Verify the stable page even
after a device install; report a failed check honestly. Never put that private
link in a public PR, change hosting or equate a working page with installation.
