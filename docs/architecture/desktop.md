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
(`gpui-pre` 0.3.8), `gpui_platform` (`gpui-pre-platform` 0.3.8), and `gpui_kit`
0.7.1. A parent-managed pinned GPUI snapshot patch supplies the
drawable-presented callback; `on_next_frame`, submitted, and completed do not
clear a presentation fence.

The process owns one connection drain, workspace layout, and terminal
entities. It does not keep a second reconnect or input-delivery state machine
beside the runtime. Closing a pane, tab, window, or the app detaches. Only
Terminate Terminal ends execution.

## Package

| Path | Owns |
|---|---|
| `native/src` | GPUI application, connection, workspace, terminal, settings, platform integration. |
| `native/assets/fonts` | Paper Mono files packaged beside the executable. |
| `scripts`, `packaging` | Checkout build, isolated smoke/perf, and `Phux.app` preparation. |
| `toolchain` | Parent-managed GPUI snapshot pin and patch. |
| `dist/app/Phux.app` | Release executable, fonts, icon, ad-hoc signature. No `.node` addon. |

`Phux.app/Contents/MacOS/phux-desktop` is the Rust binary. Dock launch omits
`--phux`; the binary resolves an installed CLI and never replaces it. Isolated
smoke and perf pass `--socket`, `--session`, `--phux`, `--layout`, and
`--evidence`. `--smoke` writes the same evidence and then exits. Qualification
refuses an installed CLI and a production socket.

Layout files stay at
`$XDG_STATE_HOME/phux-desktop/<sha256(JSON.stringify([socket, session]))[:24]>/`.
Unscoped layouts are imported without overwrite. Malformed or future versions
are not rewritten.

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
only visible view, proposes whole cells that fit its painted bounds. Observer
views crop or letterbox the authoritative grid. The desktop connects with a
zero viewport and casts no window-size vote. Quitting leaves explicit sizes in
place. Read back the size the server actually applied.

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

A terminal-started OMP remains the only running agent. When its authenticated
`AgentSession` stream publishes, that pane gains a persistent **Conversation**
button and a matching control in its pane capsule; starting an agent never
changes the selected face or takes keyboard focus. **Terminal** returns to the
same terminal view. A draft and reader position survive face switches.

The connection enables the runtime's `subscribe_agent_sessions` option and
consumes committed `AgentPublication` events on its existing event drain.
`connection/transcript.rs` reuses the core `transcript_entry()` convention,
replaces stable entry ids in place, and bounds each pane's native history to
256 entries / 2 MiB. Prose, thinking, partial/final replies, truncation, tool
summary/status/output, and a lifecycle-only activity timeline are native GPUI
rows. `PHUX_AGENT_TRANSCRIPT=0` on the producer withholds conversation text, not
the lifecycle timeline. Scrolling pauses follow; **Jump to latest** resumes it.

The native Kit textarea accepts multiple lines: **Enter** sends, **Shift+Enter**
adds a line. Sending uses the existing acknowledged `apply_line` operation
(one trusted paste plus Return), addressed to the exact Terminal parent.
The draft clears only after a delivered receipt; refusals and unknown delivery
remain visible with the draft intact. Nothing auto-retries, changes a model,
or starts another prompt. **Stop** / **Escape** sends Escape only while this
agent stream reports working. An ended or closed stream remains readable and
cannot write into the shell that replaced the agent.
Permission/approval blocks disable sending and direct the person to **Terminal**
to answer the existing prompt; the native face does not synthesize approval
controls or type a message into that dialog.

The composer binds serving incarnation, runtime client, terminal, and agent
resource. Connection epoch and runtime input eligibility are checked again
under the control lock immediately before input. A resumed publication keeps
its stable rows; a replacement publication resets them. A new agent resource
gets a fresh transcript/draft/scroll scope, and incarnation replacement drops
old records. The terminal's existing unknown-delivery presentation fence is
never cleared by conversation rendering. No WebView, RPC worker, protocol, or
FFI ABI is added.


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
| Product journeys | Tabs, splits, Ghostty import, settings, agents, and layout restore are in the product contract. Projects/worktree grouping, remote hosts, and approvals are not built. |
| Apple-silicon desktop | Ad-hoc signed, not notarized. The CLI stays a separate install. Linux is not supported. |

[Desktop verification](./desktop-verification.md) owns the evidence required
before calling any of that complete.
