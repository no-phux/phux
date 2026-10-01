---
audience: contributors, agents
stability: stable
last-reviewed: 2026-09-23
---
# phux Experience and Visual Design System

**TL;DR.** Keep terminal work usable, make process and input state clear,
and offer guidance only when it helps the next action. These design
requirements apply across clients; they are not a claim that every surface
already meets them. The visual system uses terminal-grid geometry, quiet
surfaces, and restrained lime accents.

## 1. Experience Contract

People must be able to tell whether their process, input, and terminal state
survived an operation. If the interface leaves that uncertain when the system
knows the answer, the feature is not ready. Guidance should stop after use or
dismissal, while help remains discoverable.

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
invocation facts belong in [Quickstart](./QUICKSTART.md) and the generated
[CLI reference](./reference/cli.md).

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

Start with a working terminal. Introduce controls when they become relevant,
not on a timer, and skip guidance the person's actions show they do not need.

| Beat | Intended feeling | Experience requirement | Failure signal |
|---|---|---|---|
| Familiarity | "This is still my terminal." | Open on a usable terminal surface with conventional focus, input, and legible chrome. Require no account, configuration choice, or tour before work begins. | The first screen is about phux rather than the person's shell or process. |
| Safety | "My work stays here." | Make continuity visible at the first relevant boundary. Explain what happened to the terminal in plain language whenever a view closes, reconnects, or cannot proceed. | The person hesitates because detach, close, quit, and kill appear interchangeable. |
| Magic | "That is the same live work." | Demonstrate the shared-terminal promise through a real second view, consumer, or agent action connected to the current terminal. Preserve enough context that cause and effect are obvious. | The demonstration looks like copied output, a canned animation, or a separate session. |
| Confidence | "I can do the next thing myself." | Teach one control in response to intent, then let the person complete a meaningful action without assistance. Keep a discoverable route back to help. | Success depends on remembering a sequence shown earlier or escaping a wizard. |
| Respect | "It trusts me now." | Stop introductory guidance after use or dismissal. Return the full surface to the person's work and keep advanced capability available on demand. | Hints repeat, badges accumulate, or the product asks for setup unrelated to current work. |

Do not force all five beats into one session. During an incident, prioritize
continuity and recovery over demonstrations. Someone arriving through an
existing shared terminal may already understand the second-view behavior.

## 3. Teaching in the Moment

Tie guidance to the person's current action. Do not block the terminal with a
wizard, checklist, or tour of controls they have not tried to use.

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

Give each lesson a stable identity. Remember per person or client whether it
was shown, dismissed, or demonstrated through successful use.

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

Lead with the failed action, not the subsystem. Every user-facing error
answers, in order:

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

Every user-facing acceptance pass requires a real rendered surface or
transcript showing what the person saw. Exit codes, protocol assertions,
and snapshots remain necessary but cannot establish usability alone.

| Moment under test | Question to answer | User-visible evidence |
|---|---|---|
| Familiarity | Can a new person begin terminal work without first making a phux decision? | A cold-start capture from launch through the first ordinary command. |
| Safety | At each leave, disconnect, and recovery boundary, can the person tell what remains running before acting? | The exact before, interruption, and return states, including the safety message. |
| Magic | Is it unmistakable that two consumers are acting on the same live terminal rather than copies? | A continuous capture with a causally clear action in one view and result in the other. |
| Confidence | Can the person complete the next relevant action after one contextual cue, with no hidden prerequisite? | A first-use trace showing cue, action, outcome, and the route back to help. |
| Respect | After use or dismissal, does guidance stay gone while the capability remains discoverable? | A repeat-session capture plus the persisted lesson state visible through a supported inspection surface. |
| Recovery | Does failure preserve context and offer a specific safe next step? | A fault-injected capture showing the attempted action, preserved work, remedy, and successful return. |

Record the tester's answer beside each artifact. Use observable evidence:
hesitation, repeated backtracking, uncertainty about process survival, and
inability to explain a change. A pass requires correct system behavior and
an interface the person can understand.

Review captures at normal terminal size and under constrained width. Include
keyboard-only operation, reduced motion where motion exists, loss of color
distinctions, slow or interrupted transport, and a returning user whose lessons
are already learned. Do not approve the first-run experience using only a
pristine machine and a scripted happy path.

## 6. Atmosphere and Identity

phux is a programmable terminal runtime for people and agents. Its interfaces
should make terminal ownership, control, events, and connections legible
without competing with the work. Use thin terminal-grid geometry and a
restrained lime path to show relationships between terminals, clients,
agents, and machines.

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

- Reserve lime for active objects, command focus, agent events, contextual
  guidance, and the README wordmark path.
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

- Use monospace for terminal and protocol surfaces; keep prose readable.
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

### TUI Cell Tokens

The native TUI uses whole terminal cells rather than pixel spacing. Its
implementation tokens live in `render/theme.rs` and the sidebar composer.

| Token | Value | Purpose |
|---|---|---|
| Sidebar automatic width | 25% of viewport, clamped to 28–40 columns | Give names more room on wide screens without making terminal content chase live labels. Explicit positive widths stay fixed. |
| Sidebar gutter | 1 column on either side of the content | Separate labels from the outer edge and divider. |
| Icon column | 1 glyph + 1 space | Align window, agent, roster, and action labels. Use portable text glyphs, no font-specific icons. |
| Window block | 2 rows | Primary label and aligned branch context; a branchless row supplies breathing room. |
| Section gap | 1 row when affordable | Separate attention, local work, and other sessions without stealing the last local window. |
| Selected surface | `#293628` | A quiet full-row selection bed, gutters included; lime marker and bold label carry focus even without color. |
| Chrome surface | `surface` | The top bar and the sidebar share one bed, so the chrome reads as one frame around the panes. The pane rail tees into the sidebar rule. |
| Window tab | ` {index} {badge} {name} ` | One cell of padding either side. The active tab is lime on the selection bed; the index recedes a step behind the name. |
| Right column | flush right | Section counts, agent names, branches, and session histograms share one right edge, so the sidebar reads as a table. |
| Badge vocabulary | `●` `◆` `◐` `○` | Blocked, done and unread, working, idle. One vocabulary on tabs, sidebar rows, histograms, pane titles, and the fleet. |
| TUI structural ink | `#7c8696` | Terminal-cell rules need stronger contrast than pixel borders; all text and rules clear 4.5:1 on the elevated surface. |

The TUI uses the dark palette above: lime focus, mint key chords, slate
secondary text, off-white panel text, yellow attention, and red errors.
Section labels stay neutral; ordinary ongoing work does not animate. The
sidebar and overlays own their foreground and background together so they
remain legible over either a light or dark host terminal. Selected rows use
the selection foreground; branch context never adds terminal `DIM` on top of
an already-muted color. Unchanged sidebar frames emit no bytes; a changed
frame repaints only changed rows, including clearing shortened labels.

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
- **Companion mark**: `docs/assets/fox-mark.*` is the square mark and favicon source.
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
- Keep terminal demo animation readable at its displayed size and pace.
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
