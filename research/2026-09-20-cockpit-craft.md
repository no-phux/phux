---
audience: contributors, agents
stability: scratch
last-reviewed: 2026-09-20
---

# Cockpit craft: elegant to use, elegant to change

**TL;DR.** Cockpit's next design emphasis is the everyday experience around the
terminal: intuitive settings, precise geometry, clear interaction states and
quiet native chrome. Its implementation deserves the same care: legible file
organization, cohesive modules and obvious ownership. These notes capture the
direction and source-backed opportunities, not a live visual audit or a shipped
contract.

## Intent and scope

The user's emphasis: "grid spacing, proper geometry, proper intuition" and
"our code should be elegant as well, file trees, clear." Working on the app
should feel as considered as using it. This is a product-quality requirement,
not a request for ornamental effects or a repository-wide file shuffle.

Phux core and the TUI remain the main bet. Develop the existing Native Cockpit
as a client people enjoy using and showing to friends. Creator amplification is
an opportunity, not an acceptance criterion. Chrome craft comes before expanding
the proposed agent-attention experience; attention should inherit good controls.

Tracking: **phux-3gpg**; this notes capture: **phux-3gpg.1**. Beads owns scope,
status and follow-up work. This file holds design reasoning. Initial source
baseline: `f6b10675`, branch `docs/cockpit-craft-notes`. The earlier closed
`phux-cockpit-2q8` established a chrome register; build on that work.

## Existing sources and observed opportunities

The [product direction](../clients/cockpit/docs/PRODUCT_DIRECTION.md) owns the
terminal-first ambition; [design system](../clients/cockpit/docs/DESIGN_SYSTEM.md)
owns chrome tokens and sizing; [decisions](../clients/cockpit/docs/DECISIONS.md)
owns settled tradeoffs. These notes propose refinements, not replacement rules.

Source inspection establishes:

- [Settings markup](../clients/cockpit/src/windows/components/cockpit-settings.native)
  renders editable values through repeated Edit/Reset actions, a revealed text
  input and Preview, followed by panel-level Save. It already has search,
  categories and theme choices. The opportunity is better interaction design,
  not adding another settings framework.
- [Settings vocabulary](../clients/cockpit/src/settings.ts) includes font,
  appearance, cursor, shell and shortcut concerns. Each choice need not look
  like an arbitrary string simply because configuration stores a string.
- [Commands](../clients/cockpit/src/command-catalog.ts) already include search,
  navigation, configuration and updates. Improve their presentation and
  discoverability before inventing duplicate command surfaces.
- The design system already specifies a Geist register, spacing ladder and
  geometry checks. Token compliance alone does not establish hierarchy, optical
  balance or intuitive behavior.

The user's dissatisfaction is the motivation. Hover quality, actual contrast,
animation timing and current on-glass geometry have not been evaluated here.
Existing features in this checkout are not proof of public-release availability.

## Visual and interaction direction

**Geist restraint, macOS interaction discipline, terminal-first density.**
Make the app attractive at rest, obvious under the pointer and immediate under
the keyboard. Most of the personality should come from proportion, typography,
alignment and excellent behavior rather than persistent decoration.

### Geometry that holds together

- Use the existing token ladder for spacing, control registers and radii.
  Group related controls more tightly than unrelated groups; equal spacing
  everywhere erases hierarchy even when every number is legal.
- Keep chrome geometry and terminal cell geometry distinct. Snap the container
  to the chrome system; preserve font-derived cell dimensions inside it. Do
  not force terminal rows onto the spacing grid or derive toolbar heights from
  cell metrics.
- Share resolved layout between painting, pointer targets and viewport sizing.
  Follow the compiled-markup and workspace-projection contract; do not add a
  second set of geometry formulas for new controls.
- Make icon/text baselines, button silhouettes, tab labels and inset edges
  align perceptually. Optical corrections should be explicit and justified,
  not unexplained offsets scattered through handlers.
- Reserve space for changing affordances. A close icon appearing on hover
  should not shift a tab title; a status label should not move nearby actions.
- Distinguish visible size from hit area. Compact desktop controls can be easy
  to acquire without oversized chrome. Validate resize edges, dividers and
  adjacent targets together; do not blindly import mobile target sizes.
- Check minimum windows, long labels, overflow, split layouts and backing-scale
  changes. Floating surfaces should stay on screen and visually belong to the
  trigger that opened them.

### A complete state language

| State | What a person should understand |
|---|---|
| Hover | This can be acted on. |
| Pressed | My action registered. |
| Selected | This is the current tab or chosen value. |
| Keyboard focus | My next key acts here. |
| Attention | This item needs me. |
| Disabled | This action is presently unavailable. |
| Busy/error | The action is pending, or something needs correction. |

States must compose: a selected tab can also have keyboard focus and need
attention. Do not make all three the same accent fill. Maintain static cues,
readable text and inactive-window distinctions. Hover never steals keyboard
selection; asynchronous updates never silently change the action's target.

### Settings that speak the user's language

Proposed controls, subject to checking the pinned SDK's actual capabilities:

| Value | Intended interaction |
|---|---|
| Supported font family | Picker showing the available choices |
| Font size | Editable number and increment/decrement controls |
| System appearance / boolean preferences | Clearly labeled switches |
| Theme | Visual swatches/previews with persistent selection indication |
| Cursor shape | Illustrated choices with text labels |
| Shortcut | Chord capture with conflict feedback and cancel |
| Shell command / free-form values | Text editing with appropriate validation |
| Reset | Quiet contextual action for a changed value |

Preserve honest setting scope: a local scratch-shell setting cannot appear to
configure a remote Phux server. Keep preview, saved value, cancel and reset
semantics explicit. Better controls should fit the current transaction model;
do not quietly change persistence semantics to make the UI look simpler.

### Tabs, buttons, menus and motion

Prioritize coherent title/session/tab hierarchy, predictable truncation, stable
close targets and a readable focused pane. Use borders for meaningful structure
and elevation for floating surfaces; avoid framing every piece of content.

Give actions a hierarchy. Routine Edit/Reset buttons should not compete with
the action that completes a task. Icon-only actions need meaningful accessible
names and useful tooltips. Align menu shortcuts, group related verbs and make
empty/loading/error states intentional. Escape restores the originating focus;
menus close predictably; click-away never accidentally types into a terminal.

Motion should explain origin or confirm action. Frequent terminal and keyboard
navigation remains immediate. Any transition should be interruptible and respect
reduced motion. Adopt existing motion tokens first; evaluate native feel rather
than copying a web spring or universal scale-on-press recipe.

## Equal craft inside the code

The desired developer experience: a contributor can find a behavior, identify
its owner, change it locally and verify it without understanding the whole app.

- **Deep modules:** a small interface hides meaningful behavior. A settings
  editor should own its draft/validation/commit interaction rather than make
  every view coordinate those rules. Build from the existing appearance and
  settings modules instead of adding another store.
- **One owner per fact:** UI draft, committed configuration, focus, terminal
  identity and geometry have explicit owners. Views project values and express
  intent. Paint and hover paths do not become authority for execution state.
- **Locality:** shared control semantics should be fixed once across windows.
  Share meaningful templates and policy; keep effectful native work at its
  existing seam. More wrapper layers are not automatically cleaner.
- **Legible names and trees:** organize by responsibility and actual change
  patterns. Avoid generic dumping grounds or splitting a cohesive module into
  many tiny forwarding files just to reduce line counts.
- **Explicit state transitions:** prefer named states, guard clauses and
  readable predicates over flag combinations and dense expressions. Preserve
  error, pending and stale outcomes rather than hiding them in visual code.
- **Tests at useful interfaces:** exercise the same behavior callers use.
  Refactoring preserves behavior and public interfaces unless a specific
  change is intentional. Measure touched branching functions before and after
  using project-native complexity tooling; smaller numbers alone are not taste.

Current orientation points, not a prescribed replacement tree:

| Responsibility | Existing entry points under `clients/cockpit/` |
|---|---|
| Window composition and shared chrome | `src/app.native`, `src/windows/` |
| App coordination and UI state | `src/core.ts`, `src/appearance.ts`, `src/settings.ts` |
| Commands and navigation | `app.zon`, `src/commands.ts`, `src/window-navigation.ts` |
| Typed native seam | `src/protocol.ts`, `src/native_extension.zig` |
| Geometry and native execution | `src/cockpit/native/`, `src/cockpit/` |
| Provider identity and transport adapters | `src/providers/` |
| Behavioral evidence | `src/tests/`, focused native tests and `scripts/` |

Judge a proposed move by whether it reduces the places a reader must visit.
Keep generated files visibly tied to their generator; a tidy tree that conceals
the actual edit point is a regression. Extract around proven responsibilities,
not a speculative future renderer or universal UI framework.

## References and evidence for the next conversation

Use [UI Skills](https://www.ui-skills.com/) selectively:
[Jakub Krehel's better-ui](https://www.ui-skills.com/skills/jakubkrehel/better-ui)
for optical alignment, state feedback and surface craft;
[Emil Kowalski's motion guidance](https://www.ui-skills.com/skills/emilkowalski/improve-animations)
for frequency, restraint and interruptibility. Consulted 2026-09-20. Their CSS,
DOM and animation recipes require translation to Native SDK. These links do
not install skills, invoke workflows or override project tokens.

The first concrete design discussion should use the actual Settings surface
and a terminal window's tabs/buttons in every meaningful state. Compare the
number of actions, visual hierarchy, geometry and code ownership together.
Keep source observations separate from live defects and from desired behavior.

Follow [rendering evidence](../clients/cockpit/docs/RENDER_FIDELITY.md): CPU
reference screenshots cannot prove CoreText/Metal appearance. A future craft
slice needs real-app observation with the correct build/PID, scoped behavioral
checks, and a diff whose ownership is easier to explain. Setup and checks live
in [SETUP](../docs/SETUP.md#cockpit); this notes-only capture requires docs checks.

The acceptance question is the same on both sides: **does this make the next
action obvious, and remove effort without hiding essential information?**
