---
audience: contributors, agents
stability: stable
last-reviewed: 2026-08-11
---
# phux Experience and Visual Design System

**TL;DR.** phux protects a person's work and attention. The first experience
moves from familiarity through safety, magic, confidence, and respect without
a captive tutorial. Guidance appears at the moment of need, remembers what was
learned, and leaves. The visual system supports that promise with quiet,
terminal-native surfaces and restrained electric-lime signals.

## 1. Experience Contract

The product promise is exact: **work is safe; phux gets out of the way.** Every
interaction should make one or both halves more credible. A feature that is
powerful but makes the person wonder whether their process, input, or terminal
state survived is not ready. A feature that proves its value once and keeps
asking for attention is not finished.

Design for the person's likely emotional state, not only the system state:

| Person's state | Product response |
|---|---|
| Cautious in an unfamiliar tool | Preserve terminal conventions and show a recognizable working surface before explaining phux. |
| Worried about losing work | State what remains safe, avoid destructive defaults, and make detach, interruption, and recovery legible. |
| Curious about the difference | Reveal one real shared-terminal capability in context; do not stage a decorative product tour. |
| Learning a control | Give one short, actionable cue beside the relevant object, then let the person act. |
| Focused on their own work | Remove teaching, decoration, and status that do not require a decision. |
| Recovering from failure | Preserve what can be preserved and offer the smallest safe next action without blame. |

These rules govern the TUI, browser surfaces, documentation examples, demos,
and future clients. They do not define commands or keybindings. The current
invocation facts belong in [Quickstart](./docs/QUICKSTART.md) and the generated
[CLI reference](./docs/reference/cli.md).

### Quiet, Respect, and Restraint

- Default to the person's terminal content, not phux chrome.
- Spend attention only on a decision, a changed safety condition, or a useful
  capability available in the current moment.
- Show one primary message and one primary action. Put detail behind an
  explicit request.
- Keep success quiet. Prefer a stable state change over a toast announcing
  that the state changed.
- Do not animate ongoing work merely to prove that phux is alive.
- Never use urgency, celebration, streaks, or completion theater to drive
  engagement.
- Let every transient surface be dismissed immediately. Dismissal must not
  activate a covered UI control; when ordinary terminal input closes a lesson,
  that input continues to the terminal instead of being swallowed.

## 2. The First Five Minutes

The first run is an emotional arc, not a setup funnel. Time is a guardrail,
not a timer: advance when the person reaches the moment, and omit a beat when
their actions show they already understand it.

| Beat | Intended feeling | Experience requirement | Failure signal |
|---|---|---|---|
| Familiarity | "This is still my terminal." | Open on a usable terminal surface with conventional focus, input, and legible chrome. Require no account, configuration choice, or tour before work begins. | The first screen is about phux rather than the person's shell or process. |
| Safety | "My work stays here." | Make continuity visible at the first relevant boundary. Explain what happened to the terminal in plain language whenever a view closes, reconnects, or cannot proceed. | The person hesitates because detach, close, quit, and kill appear interchangeable. |
| Magic | "That is the same live work." | Demonstrate the shared-terminal promise through a real second view, consumer, or agent action connected to the current terminal. Preserve enough context that cause and effect are obvious. | The demonstration looks like copied output, a canned animation, or a separate session. |
| Confidence | "I can do the next thing myself." | Teach one control in response to intent, then let the person complete a meaningful action without assistance. Keep a discoverable route back to help. | Success depends on remembering a sequence shown earlier or escaping a wizard. |
| Respect | "It trusts me now." | Stop introductory guidance after use or dismissal. Return the full surface to the person's work and keep advanced capability available on demand. | Hints repeat, badges accumulate, or the product asks for setup unrelated to current work. |

Do not force all five beats into one session. A person who opens phux during an
incident gets familiarity and safety first; magic and teaching wait. A person
who arrives through an existing shared terminal can begin at magic. The order
expresses trust priorities, not a mandatory route.

## 3. Teaching in the Moment

phux teaches at the point where intent and opportunity meet. It does not use a
captive wizard, a checklist that blocks the terminal, or a tour anchored to
controls the person has not tried to use.

### Guidance Rules

- Trigger guidance from a visible moment: first prefix use, an attempted
  action, a newly available shared capability, or a recoverable error.
- Place the cue beside the object or status it explains. Do not move focus to
  a separate teaching surface.
- State the action first and the reason second. Keep the default cue to one or
  two lines.
- Offer at most one new concept at a time. If several apply, choose the one
  that unblocks the person's current intent.
- Never disable ordinary controls until a lesson is completed. Trying the real
  action is the lesson.
- Make help browsable on demand so quiet defaults do not make the product
  opaque.
- Treat dismissal as a valid outcome, not a failed conversion.

### Memory of Learned Guidance

Guidance needs memory or it becomes nagging. Give each lesson a stable identity
and remember, per person or client, whether it was shown, dismissed, or
demonstrated through successful use.

- Successful use marks the lesson learned and suppresses its introductory cue.
- Explicit dismissal suppresses the cue even when no action followed.
- A timeout or lost focus does not imply learning; it only closes the surface.
- A materially changed control or safety contract may version the lesson and
  show a revised cue once. Cosmetic copy changes do not reset it.
- Provide a clear way to review guidance and reset learned state without
  resetting unrelated configuration.
- Keep this memory local and minimal. It records lesson state, not command
  history, terminal content, or a behavioral profile.

## 4. Humane Errors and Recovery

An error interrupts somebody who was trying to do something else. Lead with
their work, not the subsystem that failed. Every user-facing error answers, in
this order:

1. What could not happen?
2. What happened to the person's work or requested change?
3. What is the smallest safe recovery action?
4. Where can they inspect technical detail if recovery fails?

Use direct language: "The view disconnected. The terminal is still running."
Avoid blame ("you entered"), vague failure ("something went wrong"), raw error
chains as the headline, and reassurance the system cannot prove. If phux does
not know whether work is safe, say that plainly and avoid retrying a write that
might duplicate input.

Recovery behavior follows these rules:

- Preserve terminal state, typed but unsubmitted input, focus, and layout when
  the failed operation does not require discarding them.
- Keep the last valid configuration or view active when a replacement fails.
- Make retry idempotent where possible. When it is not, explain the duplication
  risk before offering retry.
- Put destructive recovery behind a consequence-specific confirmation. Do not
  use generic "Are you sure?" prompts.
- Return the person to the interrupted context after recovery; do not send them
  to a dashboard or setup flow.
- Keep diagnostics available without forcing log paths, protocol codes, or
  implementation terms into the primary message.

Error colors communicate severity, not blame. Use `--status-error` only when
the person must act or an operation failed. Use `--status-warning` for a changed
safety condition or a choice with consequences. Neutral interruptions use the
ordinary text and border tokens.

## 5. Experience QA

Exit codes, protocol assertions, and snapshots remain necessary, but they do
not prove the experience contract. Every user-facing acceptance pass includes
a real rendered surface or transcript and evaluates the moment from the
person's point of view. Automated checks may verify the evidence, but the
report must expose what the person saw.

| Moment under test | Question to answer | User-visible evidence |
|---|---|---|
| Familiarity | Can a new person begin terminal work without first making a phux decision? | A cold-start capture from launch through the first ordinary command. |
| Safety | At each leave, disconnect, and recovery boundary, can the person tell what remains running before acting? | The exact before, interruption, and return states, including the safety message. |
| Magic | Is it unmistakable that two consumers are acting on the same live terminal rather than copies? | A continuous capture with a causally clear action in one view and result in the other. |
| Confidence | Can the person complete the next relevant action after one contextual cue, with no hidden prerequisite? | A first-use trace showing cue, action, outcome, and the route back to help. |
| Respect | After use or dismissal, does guidance stay gone while the capability remains discoverable? | A repeat-session capture plus the persisted lesson state visible through a supported inspection surface. |
| Recovery | Does failure preserve context and offer a specific safe next step? | A fault-injected capture showing the attempted action, preserved work, remedy, and successful return. |

Use observable proxies rather than claiming to measure feelings directly.
Hesitation, repeated backtracking, uncertainty about whether a process survived,
and inability to explain what changed are design failures even when every
command exits successfully. Record the tester's answer to the question in the
table alongside the artifact. A pass requires both correct system behavior and
a credible human reading of it.

Review captures at normal terminal size and under constrained width. Include
keyboard-only operation, reduced motion where motion exists, loss of color
distinctions, slow or interrupted transport, and a returning user whose lessons
are already learned. Do not approve the first-run experience using only a
pristine machine and a scripted happy path.

## 6. Atmosphere and Identity

phux is a command surface for people and agents sharing the same live terminal
object. It should feel sharper than tmux, calmer than a dashboard, and more
material than a raw protocol spec. The visual signature is the "wire object":
thin terminal-grid geometry with a single lime path showing that panes, agents,
and clients are holding the same object rather than copying a screen.

## 7. Color

### Palette

| Role | Token | Light | Dark | Usage |
|------|-------|-------|------|-------|
| Surface/primary | `--surface-primary` | `#f8fafc` | `#090b0f` | Documentation page background, outer terminal field |
| Surface/secondary | `--surface-secondary` | `#eef2f7` | `#11141b` | Terminal panes, README demo field |
| Surface/elevated | `--surface-elevated` | `#ffffff` | `#171b23` | Modals, prompt overlays, callouts |
| Text/primary | `--text-primary` | `#0f172a` | `#f4f7fb` | Headlines, status titles, foreground text |
| Text/secondary | `--text-secondary` | `#475569` | `#9aa4b2` | Body copy, inactive pane labels |
| Text/tertiary | `--text-tertiary` | `#64748b` | `#697386` | Muted hints, disabled controls |
| Border/default | `--border-default` | `#cbd5e1` | `#343a46` | Pane dividers, modal borders |
| Border/subtle | `--border-subtle` | `#e2e8f0` | `#242936` | Secondary separators |
| Accent/primary | `--accent-primary` | `#65a30d` | `#bef264` | Default accent, active wire, modal titles |
| Accent/secondary | `--accent-secondary` | `#15803d` | `#86efac` | Key chords, secondary active states |
| Status/error | `--status-error` | `#dc2626` | `#f87171` | Errors, destructive messages |
| Status/warning | `--status-warning` | `#ca8a04` | `#fde047` | Warnings, section headers |

### Rules

- Lime is a signal, not wallpaper. Use it for active objects, command focus,
  agent events, contextual teaching focus, and the README wordmark path.
- Prefer off-black technical surfaces over pure black.
- Keep screenshots and demo assets legible when downscaled to README width.
- Do not rely on lime, red, or yellow alone. Pair color with text, shape, or a
  stable position.

## 8. Typography

### Scale

| Level | Size | Weight | Line Height | Tracking | Usage |
|-------|------|--------|-------------|----------|-------|
| Display | 48px | 700 | 1.05 | 0 | Wordmark, large launch visuals |
| H1 | 36px | 700 | 1.15 | 0 | Page title |
| H2 | 28px | 650 | 1.25 | 0 | Section headers |
| H3 | 20px | 650 | 1.35 | 0 | Panel titles |
| Body | 16px | 400 | 1.6 | 0 | Documentation prose |
| Body/sm | 14px | 400 | 1.5 | 0 | Captions, status text |
| Mono/sm | 13px | 500 | 1.45 | 0 | Commands, pane labels, JSON |

### Font Stack

- Primary: system sans-serif (`ui-sans-serif`, `system-ui`, `-apple-system`)
- Mono: system monospace (`ui-monospace`, `SFMono-Regular`, `Menlo`, `monospace`)

### Rules

- Terminal and protocol surfaces may lean mono-heavy; prose should stay calm
  and readable.
- Letter spacing is zero unless a real terminal glyph grid requires otherwise.

## 9. Spacing and Layout

### Base Unit

All spacing derives from a base of 4px.

| Token | Value | Usage |
|-------|-------|-------|
| `--space-1` | 4px | Icon-to-label, hairline offsets |
| `--space-2` | 8px | Compact terminal chrome |
| `--space-3` | 12px | Status groups, modal inner gaps |
| `--space-4` | 16px | Default panel padding |
| `--space-6` | 24px | README asset padding |
| `--space-8` | 32px | Section grouping |
| `--space-12` | 48px | Major front-door rhythm |

### Grid

- Max content width: 1120px for docs and launch assets.
- Terminal surfaces use stable cell grids; avoid layouts that resize around
  dynamic command text.

### Rules

- Use full-width bands or single composed surfaces for launch visuals.
- Do not nest decorative cards inside other cards.

## 10. Components

### Terminal Demo Surface

- **Structure**: dark terminal frame, single status strip, pane grid, command
  transcript, and one accent wire/path.
- **Variants**: static README image, animated GIF, TUI smoke capture.
- **Spacing**: `--space-4` inside the frame, `--space-2` around pane chrome.
- **States**: active pane has lime title/path; inactive panes use secondary
  text and default borders.
- **Accessibility**: alt text must describe the product behavior, not the
  decoration.

### Wordmark

- **Structure**: mono wordmark plus one wire-object mark.
- **Variants**: SVG source, PNG export for surfaces that do not render SVG.
- **Spacing**: clear space at least the height of the mark's inner node.
- **Accessibility**: `alt="phux"` when used as a brand mark.

### Contextual Guidance

- **Structure**: one short instruction, optional reason, and a visible dismiss
  path beside the relevant pane, control, or status.
- **Color**: elevated surface with default border; reserve lime for the exact
  control or object being introduced.
- **Behavior**: never steals terminal focus, blocks input, or obscures the
  output needed to understand the cue.
- **Persistence**: closes on successful use, explicit dismissal, or loss of
  relevance; lesson memory follows section 3.

### Error and Recovery Notice

- **Structure**: failed intent, work-safety statement, primary recovery action,
  and expandable diagnostics.
- **Color**: severity token on the title or border only; do not flood the
  surface with red or yellow.
- **Behavior**: preserves the interrupted context and does not auto-dismiss
  while a safety decision remains.
- **Copy**: names the affected object in user vocabulary and avoids internal
  protocol or process names unless diagnostics are expanded.

## 11. Motion and Interaction

### Timing

| Type | Duration | Easing | Usage |
|------|----------|--------|-------|
| Micro | 120ms | ease-out | Button or focus state |
| Standard | 240ms | ease-in-out | Overlay open/close |
| Demo beat | 800-1400ms | linear or ease-in-out | README GIF command/event reveal |

### Rules

- Animate opacity and transform only in browser-facing assets.
- Terminal demo animation should be readable first, kinetic second.
- Browser-facing surfaces respect reduced-motion.
- Never animate an error continuously. Use a single state transition, then
  hold still for reading and recovery.
- Guidance enters without moving terminal content or changing focus.

## 12. Depth and Surface

### Strategy

Use tonal shift plus 1px borders. Shadows are reserved for modal overlays and
should be subtle enough to disappear in a terminal screenshot.

| Type | Value | Usage |
|------|-------|-------|
| Default border | `1px solid var(--border-default)` | Panes, demo frame |
| Subtle border | `1px solid var(--border-subtle)` | Internal separators |
| Overlay shadow | `0 16px 48px rgba(0,0,0,0.28)` | Help/prompt overlays |
