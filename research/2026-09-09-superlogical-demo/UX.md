---
audience: humans, contributors, agents
stability: scratch
last-reviewed: 2026-09-09
---

# Reconstructed UX and visual design

**TL;DR.** The demo uses restrained dark terminal chrome, a host-grouped session
switcher, a compact command palette and a reusable directory picker. Local and
remote contexts keep the same layout and actions. This document separates
observed flows from proposed state handling and gives editable, legible mockups
for the major surfaces without claiming pixel-perfect source fidelity.

## Information architecture

**O:** the normal window has four levels:

1. macOS window controls at top left.
2. Session identity/host context followed by pill-like tabs and an add control.
3. Pane headers with a shell label and compact trailing toolbar.
4. Terminal content occupying almost the entire remaining window.

The host/session hierarchy appears in a popover from the session control,
not a permanently exposed sidebar. The global command palette is a separate,
approximately centered-upper popover. Add Host and Rename reuse a small input
plus action-row pattern. The directory picker extends that pattern with a list.

## Primary user flows

### A. Add a host and work remotely

```text
Local shell → global command palette → Add Remote Host
→ type hostname → choose Connect to "<host>"
→ new remote session → remote shell → split → two shells
```

Evidence: S04–S09. The session adopts a generated name (`drifting-cedar` in the
footage), and the remote host becomes visible in the session selector. No
password or host-key confirmation is shown. That absence does not establish
how a previously unknown identity or host is authenticated.

### B. Rename and switch context

```text
Remote session → command palette → Rename Session
→ type Demo → confirm rename → header updates
→ session switcher → local session → remote Demo
```

Evidence: S09–S12, S15–S16. Rename occurs through the command palette, not a
demonstrated session-row context menu. The switcher groups sessions under
`This Mac` and the remote hostname. Active/selected rows have a bright teal fill,
and shortcut hints appear on the right. Selection and active-session indicators
can overlap; the exact hover/focus treatment is uncertain at source resolution.

### C. CLI creates an initially empty GUI workspace

```text
Local CLI → target remote host → session new → opaque ID returned
→ selector gains crisp-sierra → select it → Empty session
→ New Tab / Cmd+T → shell appears
```

Evidence: S17–S19. Empty-state copy reads `Empty session` and `Open a new tab to
start a terminal.` A centered teal `New Tab` button provides the primary action.
The new tab need not imply a new session: those are visibly separate levels.

### D. Close the client, then resume

```text
Demo with two distinct panes → Cmd+Q → desktop
→ relaunch → reconnect → Demo with previous panes and content
```

Evidence: S20–S22. The screen briefly returns to the local context during
startup before the remote view is restored. There is no recoverable dedicated
reconnection banner design. Do not mock the incidental transition as a required
product loading state. A deliberate connection status treatment is proposed below.

### E. Delete a non-active session through the CLI

```text
Local CLI → session kill session:<id> on remote host
→ reopen session selector → crisp-sierra absent; Demo remains
```

Evidence: S23–S24. No GUI confirmation, toast or undo is exercised. Neither
the kill of an actively displayed session nor its fallback destination is shown.

### F. Jump to a directory, locally or remotely

```text
Focused terminal → Cmd+Shift+G → picker starts at its directory
→ type or navigate path → list current destination and child directories
→ confirm selected destination → new tab at that path on same host
```

Local evidence: S25–S27 (`ghostty` → `macos`). Remote evidence: S28–S31
(`/` → `/proc` → `/home` → user home). The new remote tab becomes full-width;
the original split tab is still visible and can be revisited. This is a new
terminal action, not a demonstrated `cd` sent to the existing shell.

## Keyboard evidence

| Shortcut | Recoverable meaning | Confidence |
|---|---|---|
| Cmd+Shift+P | Open global command palette | O, overlay around S10 |
| Cmd+K | Open session switcher | O, S12/S23/S24 |
| Cmd+1 / Cmd+2 | Choose local/remote session in this demonstration | O; exact global indexing/context rules U |
| Cmd+T | New tab, including from empty session | O, S18–S19 |
| Cmd+Shift+G | Go to Directory | O+N, S25/S28 |
| Cmd+Q | Quit application, leaving session alive | O, S21 |
| Enter | Confirm palette action/path | O in rename/directory sequence |
| Cmd+[ | Focus moves between panes in the recorded split | O around 03:20; complete focus keymap U |

Control-key overlays during command editing are shell/editor interaction;
do not assume every displayed chord is an application shortcut. Escape to
dismiss, arrow-key list navigation and focus restoration are sensible **I**
defaults for reconstruction but are not fully demonstrated by this clip.

## Component anatomy and approximate geometry

Measurements below are visual estimates in the **encoded 540×360 frame**, not
native display pixels or CSS pixels. The app occupies roughly x=12–529,
y=8–343. Desktop wallpaper remains visible at the edges.

| Component | Observed visual structure | Reconstruction guidance (I) |
|---|---|---|
| Window | Rounded dark translucent blue-green surface, fine outline | Large app canvas with subdued border and 12px CSS radius |
| Top chrome | About 20 encoded px tall, small traffic lights, session then tabs | 38–42px CSS header for legible reference mockups |
| Tab | Rounded pill with terminal icon and path/title | Single-line ellipsis; active fill distinguishable from inactive |
| Pane header | Roughly 12 encoded px, shell label left, icons right | 28px CSS header; controls require real labels/tooltips in implementation |
| Split | Near 50/50 vertical division, thin border | Independent focus and content; preserve layout across tab switch |
| Command palette | Centered around x=200–343; content-dependent height | 310–360px CSS width, input first, action list below |
| Session switcher | Anchored near x=55; roughly 85 encoded px wide | 245px CSS popover, host labels above session rows |
| Directory picker | Same upper-middle anchor, roughly 143 encoded px wide | 350px CSS panel, scrollable list, current path above children |
| Empty session | Centered icon, title, supporting text, teal CTA | Clear one-action recovery path; show shortcut adjacent |

**O:** the terminal background is dark navy/teal, terminal text pale green/gray,
and selection bright turquoise. The shell prompt can include purple accents.
The UI uses small proportional text; terminal content uses a monospace face.
**U:** exact font family, font size, transparency, theme name and native scale.

### Proposed mockup tokens

These are editable approximations, not extracted official tokens:

```css
--canvas: #071f27;
--pane: #092b31;
--panel: #10262f;
--border: #29454e;
--text: #d1e1df;
--muted: #97adb2;
--accent: #46ceb0;
--selection-text: #062d29;
--shell-text: #b5d5bc;
--prompt-accent: #c1a0dd;
```

Use system sans-serif for chrome and a system monospace stack for the terminal.
The mockup increases type and hit-target sizes, uses representative CLI output,
and replaces ambiguous tiny icons with descriptive buttons. The video inset,
wallpaper and keystroke overlay are recording context rather than product UI.

## Mockup surfaces and behavior

The [interactive viewer](index.html#mockups) contains eight selectable scenes:

| Scene | Reference | What can be exercised |
|---|---|---|
| Local terminal | S01/S03 | Switch to remote context or open palette |
| Remote split | S07/S20/S22 | Two-pane composition and host/session context |
| Session switcher | S09/S12/S17 | Select local, Demo or empty crisp-sierra |
| Command palette | S04/S10 | Filter actions and open Add Host, Rename, directory or empty session |
| Add host | S05 | Edit hostname; simulated connection opens remote shell context |
| Rename | S11 | Change Demo display name in mockup state |
| Empty session | S18 | New Tab produces a shell |
| Directory picker | S25–S31 | Change local/remote example paths; open destination as a new tab |

All interactions are **I simulations** grounded in these observations. These are
editable HTML/CSS mocks, not captured screenshots of the original application.
They intentionally do not implement a terminal emulator, CLI protocol or remote
filesystem. The gallery preserves the source evidence next to the reconstruction.

### Exported mockups

The same scenes are available as PNGs, with a reconstruction label baked into
each image. See the [overview contact sheet](mockups/contact-sheet.jpg), or open
individual frames:

- [Local terminal](mockups/01-local.png)
- [Remote split](mockups/02-remote.png)
- [Session switcher](mockups/03-sessions.png)
- [Command palette](mockups/04-commands.png)
- [Add host](mockups/05-host.png)
- [Rename session](mockups/06-rename.png)
- [Empty session](mockups/07-empty.png)
- [Directory picker](mockups/08-directory.png)

## Implied design completions

The following are **I**, useful requirements for a buildable UX rather than
claims that Superlogical already implements them:

| Situation | Proposed behavior |
|---|---|
| Connect is pending | Keep entered host visible and expose cancellable progress |
| Connect fails | Explain the failed stage and let the user retry without retyping |
| Remote goes away | Preserve visible output/context; distinguish stale view from live input |
| Remote lists directories slowly | Show progress without changing host or jumping selection |
| Directory missing/denied | Inline path-specific error; keep parent navigation usable |
| Directory result arrives after switching host | Discard stale result using host/path/request identity |
| Session killed elsewhere while selected | Show explicit ended state and a route to another session |
| No filter matches | Clear empty-result message and preserve input |
| Keyboard-only use | Visible focus, list navigation, Enter activation, Escape dismissal, focus return |
| Screen-reader use | Label host/session context, actions, selected row and connection status |
| Small window | Bound popovers to viewport; truncate paths with access to full text |

These proposed failure cases are not additional mocked source screens. The
interactive artifact focuses on the demonstrated happy paths. Richer flows need
product decisions, especially active-session deletion, close-vs-kill semantics,
reconnect editing behavior and how a tab inherits host/user/CWD context.
