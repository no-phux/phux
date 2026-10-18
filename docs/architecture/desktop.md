---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-14
---

# Desktop architecture

**TL;DR.** The desktop is a Rust GPUI and GPUI Kit application. It links
`phux-client-runtime` in-process and paints immutable grid frames on the GPUI
thread. The shipping app is that executable; there is no Solid shell, JavaScript
bundle, or NAPI addon. Independent views, focused-writable geometry, and
close-detaches remain the product contract.

The [product contract](../consumers/desktop.md) owns terminology and
interaction. [ADR-0165](../adr/0165-gpui-desktop-replaces-the-solid-host.md)
is the host decision. This page owns the seams. It does not record a completed
GPU qualification.

## Host

```text
GPUI Kit shell / workspace / settings / menus
                   |
        Connection + Workspace + Terminal
                   |
         phux-client-runtime Client
                   |
       existing Ghostty / daemon / wire
```

`clients/desktop/native` is its own Cargo workspace so GPUI and GPUI Kit do not
enter every other consumer. The executable is
`native/target/{debug,release}/phux-desktop`. Framework imports are `gpui`
and `gpui_platform` (gpui-fast 0.1.9, a retained-mode GPUI fork, served
under Kit's `gpui-pre` 0.3.9 names) and `gpui_kit` 0.7.3. Views redraw only
when something they read changed or they are notified
(`clients/desktop/toolchain/README.md`). A pinned GPUI patch supplies the
drawable-presented callback; `on_next_frame`, submitted, and completed do not
clear a presentation fence.

The process owns one connection drain, workspace layout, and terminal
entities. It does not keep a second reconnect or input-delivery state machine
beside the runtime. Closing a pane, tab, window, or the app detaches. Only
Terminate Terminal ends execution.

## Package

| Path | Owns |
|---|---|
| `native/src` | GPUI application, connection, workspace, settings, platform integration. |
| `native/phux-gpui` | The embeddable terminal view the app is built on: painter, input, attachments, presentation receipts. |
| `native/phux-gpui/assets/fonts` | Paper Mono and Nerd symbol fallback faces, with their licenses. |
| `scripts`, `packaging` | Checkout build, isolated smoke/perf, and macOS/Linux package preparation. |
| `toolchain` | Parent-managed GPUI snapshot pin and patch. |
| `dist/app/Phux.app` | Release executable, fonts, icon, ad-hoc signature. No `.node` addon. |
| `dist/linux/phux-desktop` | Linux executable, desktop entry, icon and license notices. No bundled coordinator. |

`Phux.app/Contents/MacOS/phux-desktop` is the Rust binary. Dock launch omits
`--phux`; the binary resolves an installed CLI and never replaces it. Isolated
smoke and perf pass `--socket`, `--session`, `--phux`, `--layout`, and
`--evidence`. `--smoke` writes the same evidence and then exits. Qualification
refuses an installed CLI and a production socket.

Layout files stay at
`$XDG_STATE_HOME/phux-desktop/<sha256(JSON.stringify([socket, session]))[:24]>/`.
Unscoped layouts are imported without overwrite. Malformed or future versions
are not rewritten.

Layout and recent-folder saves enqueue immutable snapshots to ordered background
writers. Pending saves to the same path coalesce; serialization, file sync, and
atomic replacement do not block pane placement or command repaint. Window close
and app quit flush outstanding saves. Save failures still surface as errors.

## Identity and independent views

- A qualified terminal is endpoint, serving authority, server incarnation, and
  `ResourceId`, including a satellite route.
- A runtime view is a generation-fenced handle for that terminal.
- A placement is a stable local layout identity and owns one view attachment.
- A frame carries stream, bootstrap, publication generation, and applied
  sequence. Those are not the server incarnation.

Canonical execution, output sequence, and input authority stay terminal-owned.
Scroll, selection, search, and follow-live are view-local. One view closing
does not detach a sibling. A new process is not another view. No second PTY
and no local reflow of a narrower observer.

## Geometry

One calculation maps font metrics, content bounds, scale, cell counts, hit
testing, the IME rectangle, and clipping. The focused writable view, or the
only visible view, proposes whole cells that fit its painted bounds and the
integer cell size those cells were measured at. Observer
views crop or letterbox the authoritative grid. The desktop connects with a
zero viewport and casts no window-size vote. Quitting leaves explicit sizes in
place. Read back the size the server actually applied.

Grid origins and shared cell edges are aligned to device pixels. Solid
Unicode fractional blocks and quadrants cover those edges procedurally rather
than inheriting font side bearings or line-height gaps; other graphemes retain
normal shaping.

Prepared scenes retain exactly matching rows across viewport movement, moving
their glyphs, backgrounds, and decorations together. Changed selection,
color provenance, and solid-cursor ink rebuild the affected rows. Retention
never crosses terminal, stream, bootstrap, font, palette, or geometry changes.

Bundled fallback descriptors retain their embedded font data in the macOS
GPUI patch. A family-name-only CoreText cascade can otherwise choose an
installed font with the same name but a different glyph table.

## Frames, wakes, and input

One listener and one event drain exist per runtime client. Drain, then
invalidate affected terminals. A removed slot clears stale presentation even
when its generation equals the cached number. Never publish a cached frame as
evidence of recovery.

Raw-send success is a local queue result, not delivery. Unknown delivery is
terminal-wide. `acknowledge_projection` runs only from GPUI's drawable-presented
callback, after the window is visible and the terminal bounds are unclipped.
Paint and `on_next_frame` do not clear the fence. A late callback must not
clear a newer fence.

## Native agent conversation

**New ACP Agent** opens the CLI's configured native integration inventory, including
profiles whose operator-installed ACP executable is missing. Availability and actionable
setup details come from `phux launch --list --acp --json`; the app never installs
dependencies, accepts terms, or guesses readiness from executable names. Explicit
launch calls `phux launch --acp INTEGRATION --json --session SESSION` off the UI
thread. Its returned terminal, AgentSession and activation identify the only
conversation that may be revealed automatically. When its authenticated
`AgentSession` stream publishes, that pane gains a persistent **Conversation**
button and a matching control in its pane capsule. Passive discovery never
switches faces or keyboard focus. Project open passes `--prefer-native` to the same CLI.

The ACP worker belongs to the durable server-owned guardian, not the GUI. Closing
desktop/mobile viewers leaves that same provider session running. The terminal face
of an ACP-owned session is diagnostics, not an interactive provider TUI. Existing
hook/extension streams are explicitly labeled **Attached TUI**, retain their
terminal-owned controls, and are never presented as native SDK execution.
**Terminal** returns to the same terminal view; drafts and reader position survive
face switches.

The connection enables the runtime's `subscribe_agent_sessions` option and
consumes committed `AgentPublication` events on its existing event drain.
`connection/transcript.rs` reuses the core `transcript_entry()` convention,
replaces stable entry ids in place, and bounds each pane's native history to
256 entries / 2 MiB. Prose, thinking, partial/final replies, truncation, tool
summary/status/output, and a lifecycle-only activity timeline are native GPUI
rows. `PHUX_AGENT_TRANSCRIPT=0` on the producer withholds conversation text, not
the lifecycle timeline. Scrolling pauses follow; **Jump to latest** resumes it.

While attached, terminal damage notifies the affected cached terminal entities
even in mixed terminal/agent batches. Disconnected panes retain their last frame
without reacquiring a view. Pure VT output keeps the sidebar/titlebar caches
intact after a conversation has been opened; transcript publications and
composer receipts refresh the native face independently.

The Kit textarea accepts multiple lines: **Enter** sends, **Shift+Enter** adds a
line. For ACP-owned streams, the cached core `NativeView` supplies factual
capabilities, activation, current turn, provider session identity and questions.
`ControlPlane::native_agent_command` submits prompt, exact-turn interrupt, and
exact-question answer to the bound AgentSession. It never uses `apply_line`, key
atoms, terminal titles, or inferred approval status for SDK commands.

Dedicated `NativeAgentResult` receipts distinguish queued SDK submission from
provider admission/consumption. Only admitted/completed prompt receipts can clear
the matching unchanged draft. Queued, refused and unknown outcomes preserve it;
nothing retries automatically. A queued answer stays visible and disabled until
the actual SDK closes its request. Core/server reservation chooses one responder
across all desktop/mobile viewers. Disconnection disables writes without PTY
fallback; readable transcript remains.

`conversation/questions.rs` renders confirm, exact-ID select/permission options,
input/editor, multiple questions and JSON Schema form properties with Kit. It
preserves initial text and actual option labels/IDs, supports multiple selection,
and never preselects recommendations or approvals. Provider schemas are validated
on the executing host; unsupported field types are refused explicitly. Permission
details remain visible with the provider request. A closed/changed question drops
its controls and draft; a stale callback cannot answer a replacement request.
Every request also offers explicit **Cancel request**, mapped to the provider's
typed cancellation response rather than a default answer or terminal Escape.

Attached-mode sending still uses acknowledged `apply_line` to its exact Terminal
parent; attached Stop sends Escape only while the structured stream is working.
Its approvals remain in Terminal. This compatibility surface is not an SDK
implementation.

The composer binds server incarnation, runtime client, terminal, agent resource
and SDK activation. Epoch, committed stream cursor and native state are rechecked
under the control lock immediately before submission. New agent/activation gets a
fresh transcript/draft/scroll scope; same-session reconnect retains readable state.
Terminal damage still notifies only affected cached entities, and native receipts
do not clear the terminal's unknown-delivery presentation fence. No WebView or
provider SDK process enters the GUI.


## Tooling

Bun remains for script checks, smoke, and perf. It is not the production host.
`just desktop-check` typechecks and lints those scripts and runs the host-lock
comparison. GPU qualification is `just desktop-framework-check`: it packages
`Phux.app` and runs `scripts/smoke-app.ts` against an explicit same-checkout
CLI. The harness OCRs `presented.png` and checks `layout.json` and
`interaction.json`. A harness-written `result.json` is not success. Missing GPU
access is a blocked run, not a pass.

## Status

| Target | Present gap |
|---|---|
| Native GPUI host | The executable, package, and smoke contract are the implementation. A real presented frame on an isolated server is still the qualification record. |
| Independent views and geometry | Required. Headless models do not prove them. |
| Product journeys | Tabs, splits, Ghostty import, settings, agents, and layout restore are in the product contract. ACP launch/dialog controls require the real-provider and multi-view qualification below before shipment. |
| Apple-silicon desktop | Ad-hoc signed, not notarized. The CLI stays a separate install. Linux is not supported. |

[Desktop verification](./desktop-verification.md) owns the evidence required
before calling any of that complete.
