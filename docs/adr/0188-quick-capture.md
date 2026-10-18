---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-10-18
---

# 0188 — Quick capture saves intent before dispatching work

**TL;DR.** Capture is server-side intake, not a desktop note or an agent
session. Save one thought, resolve a destination using explicit rules, then
dispatch with a receipt. Enter follows the result; Command/Control+Enter stays put.
The desktop spike proves launch and focus using existing spawn commands;
durable capture and cross-device synchronization remain proposed.

Status: Proposed
Date: 2026-10-18

## Context

The clicks between noticing a change and starting an agent lose short-lived
thoughts. Capture must not demand a project picker before accepting text.
Desktop, mobile, and TUI must ultimately read the same saved thought and outcome.

The desktop launcher uses Kit Command for search and Kit Input for text entry.
`app/overlays/launcher.rs` owns palette modes; `settings/keymap.rs` catalogs
commands. `hotkeys.rs` currently registers only quick-terminal global chords.
The desktop-install-app recipe's Intake is the server's designated persistent
Session, not a note store. The attention Inbox is a projection, not intake.

[Agent identity](./0040-agent-identity-metadata.md) and
[detection](./0046-server-side-agent-state-detection.md) are superseded in scope
by the [AgentSession stream](./0103-agent-session-resource-and-producer-fed-streams.md).
An AgentSession is a live child of a Terminal, not durable user intent. Generic
L3 metadata is live sharing, not crash-persistent storage. `resource_kinds`
advertises resource operations; it does not promise capture storage.

## Decision

### Surface and submission

- Desktop: Command+Shift+Space opens Capture directly inside Phux. Command
  Palette, then `!`, also enters Capture in one additional keystroke.
  Capture is a catalog command and visible mode button.
  Reuse the existing sheet and Kit's auto-growing textarea and input group,
  not a second launcher framework or a handwritten editor. Keep the editor
  across dismissal so selection and undo survive alongside the draft.
- Later: an opt-in global capture chord uses the existing hotkey registrar
  and raises only the capture sheet. No new default OS-wide chord is reserved.
- Mobile: a persistent Capture affordance and share-sheet text enter the same
  server contract. TUI: a capture action opens a one-line editor. Neither is
  implemented by the desktop spike.
- Enter means dispatch and navigate after a confirmed spawn/delivery receipt.
  Shift+Enter adds a line, matching the conversation composer.
  Command+Enter (macOS) / Control+Enter dispatches without changing terminal/tab/app
  focus, empties the capture field on confirmation, and keeps it ready for the next thought.
  This is not “run headlessly”: both modes launch the same interactive agent.
- Keep the draft on refusal. While awaiting a receipt, suppress duplicate
  submits. Preserve edits made in flight. Never silently retry an ambiguous
  delivery or report “agent working” from a spawn receipt alone.

### Server-side record and routing contract (not yet implemented)

Use the existing `phux-work` keeper as the single durable writer, reached
through an authenticated phux-server facade; never expose the keeper's private
local protocol to phones. Add one bounded Capture entity, not a resource kind:

```
Capture { id, revision, text, created_at, source_context?, route?, outcome }
source_context = { authority, session_key?, resource_key?, cwd? }
route = { authority, session_key?, resource_key?, cwd, provider, intent, reason }
outcome = saved | dispatching | dispatched(result_ref) | refused(reason) | unknown
```

Text is UTF-8, at most 16 KiB, with no NUL. Create accepts an operation id and
text even without a destination. Dispatch accepts capture id, expected revision,
and operation id; commit saved intent before launching. Reuse keeper transaction,
revision and persisted operation-result patterns. Same operation/body replays;
different body conflicts. A saved thought is not automatically an Objective:
“later” can remain unassigned, and a note need not be a task.

The server exposes create/read/list/dispatch plus cursor-based change delivery
over existing authenticated transports, behind a new negotiated capability.
Reconnect uses a snapshot plus cursor; gaps require a fresh snapshot. One home
authority owns each capture. Other devices connect there; this is not multi-master
or automatic federation. Authorization, bounds, wire/spec changes and mobile/FFI
bindings are prerequisites for claiming sync, not inferred from L3 subscriptions.

Resolve in order: explicit stable destination; exact repo/session/pane-name rule
when unique; captured focused-pane cwd; otherwise saved/unrouted. Resolve names
to authority-qualified stable keys before dispatch. Never pick an arbitrary
worktree or another machine's local path. `bug`/`idea` label intent; explicit
`later` saves without launch. A future System 1/Jev hook returns a typed route
proposal with evidence over allowed destinations; code validates it and explicit
rules win. No classifier credentials, model call, or generic plugin framework now.

### Launch, placement and the spike boundary

Use argv, never shell interpolation: OpenCode `opencode --prompt TEXT`;
Claude Code `claude TEXT`; Pi `pi TEXT` (positional first prompt). Preserve
normal trust and permission prompts; never add `--auto` or bypass flags.
The spike implements only OpenCode. Installed V2 CLI help confirms `--prompt`;
`opencode run TEXT` is noninteractive and is not this UX.

Launch a Terminal in the resolved session/cwd through existing SPAWN_RESOURCE.
The integration subsequently registers its AgentSession child and publishes
`phux.agent/v1`; capture must not fabricate native session identity. Existing
agent routing must use acknowledged agent delivery, never blind paste into an
apparently idle pane. Existing-session delivery is deferred in the spike.

The desktop spike resolves a leading `@session` against the connected server,
requiring exactly one distinct known local cwd there; otherwise it uses the
focused local pane's session/cwd. Unknown or ambiguous destinations refuse and
keep text. Names containing spaces, repo/pane selectors, remote forwarding,
keyword-intent classification and new worktree creation are deferred.

Enter places the acknowledged, ready Terminal in a new local tab and focuses
it. Command/Control+Enter creates no placement: it remains discoverable in server topology
and the sidebar/Go To from other clients. Focus is client-local, consistent with
[ADR-0049](./0049-client-local-focus-and-advisory-attention.md).
Existing spawn idempotency protects same-incarnation replay only; it is not
durable capture deduplication or proof the first prompt reached a provider.

## Why

Prove the short path with existing command, input, spawn and placement seams
before introducing shared authority. Durable intent belongs outside ephemeral
terminal lifetime. Reusing the keeper avoids another database/daemon while
keeping save, launch and provider admission honest, separate outcomes.

## Tradeoffs

The spike's draft survives modal dismissal, not process exit; only launched
resources are server-visible. It has no durable capture history or synced drafts.
Spawn success can still lead to an agent login/trust/startup failure. The UI must
not promise durable fire-and-forget until the proposed record service ships.

## Alternatives

Desktop-only notes lose mobile/TUI parity. A fake AgentSession or Intake shell
couples saved intent to a process. An opaque L3 list loses atomicity/durability.
A universal classifier or autonomous inbox dispatcher adds risk before routing
rules and receipts are proven. None is needed for the launch spike.
