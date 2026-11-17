# Visual review: improve a real task every tick

Every upstream follower tick ships one contained, evidence-based visual or UX
improvement, including bumps and quiet runs. Passing rendering tests, moving
pins and preserving identical pixels are maintenance, not that improvement.
Fix real friction, not arbitrary cosmetic churn.

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
navigation, resources and `phone_inbox_visual_review_frames`, not mock HTML
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
| Resources | Actual decoded image beside a structured native document/accessibility tree; readable text and labels, not an empty placeholder. |
| Phone Inbox | Populated questions and approvals, long agent/detail text, suggested replies; focused reply with keyboard; empty state and a safe dismiss path. |

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

## Review behavior as well as pixels

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

Scheduled delivery still follows [DELIVERY.md](./ios/DELIVERY.md): exact
qualified main revision, native gates, signed ad-hoc bundle, complete HTTPS
artifact verification, in-place install and independent marketing/build query.
Every tick's private final summary returns the **same existing permanent
tailnet `/scheduled/` page**, selecting the latest qualified, signed,
completely published build. Keep immutable per-build URLs as internal
evidence/fallbacks, not rotating user bookmarks. Verify the stable page even
after a device install; report a failed check honestly. Never put that private
link in a public PR, change hosting or equate a working page with installation.
