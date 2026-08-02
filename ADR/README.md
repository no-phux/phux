---
audience: contributors, agents
stability: stable
last-reviewed: 2026-07-21
---

# Architecture Decision Records

**TL;DR.** Index of every decision that has closed off a design space
in phux. Format and `Status:` vocabulary defined in
[`../docs/CONVENTIONS.md`](../docs/CONVENTIONS.md). Read these when
you need to know *why* something is the way it is — the architecture
docs describe *what* the code is.

We write down decisions so future contributors (including future-us) can
understand why the system is the way it is. Format follows [Michael
Nygard's template][nygard].

[nygard]: https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions

## Index

<!--
This table should be generated from the ADR/0*.md files (each file's first
`# ` heading is the title, its `Status:` line is the status) rather than
hand-maintained. It drifted before — the table stopped at 0028 while 0029
and 0030 existed, and 0022 was missing entirely. Regenerate it instead of
editing rows by hand, or the same drift recurs. The relationship
annotations in the Status column (supersedes / refines / builds on /
amends / extends) are hand-curated from each ADR's body.
-->

| # | Decision | Status |
|---|----------|--------|
| [0001](./0001-language-rust.md) | Use Rust | Accepted |
| [0002](./0002-diff-based-protocol.md) | Diff-based wire protocol, not VT byte replay | Superseded by [0013](./0013-libghostty-bytes-on-wire.md) |
| [0003](./0003-server-process-model.md) | Single server, many sessions | Accepted |
| [0004](./0004-libghostty-vt-as-grid.md) | libghostty-vt is the canonical grid | Accepted |
| [0005](./0005-relationship-to-zmx-and-zmosh.md) | Relationship to zmx and zmosh | Accepted |
| [0006](./0006-input-mirrors-libghostty.md) | Input event types re-export libghostty-vt's atoms | Accepted (amended by [0024](./0024-wire-owns-input-atoms.md)) |
| [0007](./0007-mosh-class-transport-and-satellites.md) | Mosh-class transport semantics and satellite forward-compat | Accepted (forward-compat) |
| [0008](./0008-use-libghostty-types-directly.md) | Use libghostty-vt's types directly; stop reimplementing them | Accepted (amended by [0024](./0024-wire-owns-input-atoms.md)) |
| [0009](./0009-phux-vs-mux-positioning.md) | phux vs coder/mux: positioning | Accepted |
| [0010](./0010-frontend-agnostic-tmux-cc-reserved.md) | phux is TUI-first, non-TUI not precluded; tmux control mode reserved as compat option | Accepted (forward-compat) |
| [0011](./0011-protocol-core-independence.md) | `phux-protocol` and `phux-core` are independent; `IdBridge` is their only meeting point | Accepted |
| [0012](./0012-binary-split-tree-layout.md) | Window layout is a binary split tree, not n-ary | Accepted |
| [0013](./0013-libghostty-bytes-on-wire.md) | Libghostty bytes on the wire; structured input remains | Accepted (supersedes [0002](./0002-diff-based-protocol.md)) |
| [0014](./0014-server-terminal-pane-actor.md) | Server-side `Terminal` placement: per-pane PaneActor on a `LocalSet` | Accepted |
| [0015](./0015-protocol-layering.md) | Protocol layering: L1 substrate, L2 collections, L3 metadata | Accepted (L2 tier dissolved by [0030](./0030-engine-delegated-wire-and-projection-consumers.md)) |
| [0016](./0016-terminal-id-as-wire-primary.md) | `TerminalId` as the wire primary; `PaneId` is a consumer-side alias | Accepted |
| [0017](./0017-tui-not-protocol-privileged.md) | The reference TUI is not protocol-privileged | Accepted (refines [0010](./0010-frontend-agnostic-tmux-cc-reserved.md)) |
| [0018](./0018-lazy-state-synchronization.md) | Lazy state synchronization is the wire's long-arc shape | Accepted (builds on [0013](./0013-libghostty-bytes-on-wire.md)) |
| [0019](./0019-tui-multi-pane-rendering.md) | Multi-pane TUI rendering: layout persistence, wire shape, and chrome | Accepted |
| [0020](./0020-layered-render.md) | Layered render: ratatui chrome over libghostty pane interiors | Accepted |
| [0021](./0021-control-plane-commands.md) | Control-plane commands and client-side selector resolution | Accepted (builds on [0017](./0017-tui-not-protocol-privileged.md)) |
| [0022](./0022-tool-for-agents.md) | phux as a tool for agents | Accepted |
| [0023](./0023-config-ux-philosophy.md) | Config UX: pure-config, defaults as a live base layer | Accepted (TUI-local, builds on [0017](./0017-tui-not-protocol-privileged.md)) |
| [0024](./0024-wire-owns-input-atoms.md) | The wire protocol owns its input atoms | Accepted (amends [0006](./0006-input-mirrors-libghostty.md), [0008](./0008-use-libghostty-types-directly.md)) |
| [0025](./0025-browser-web-client.md) | Browser web client over a WebSocket transport | Accepted (builds on [0017](./0017-tui-not-protocol-privileged.md), [0024](./0024-wire-owns-input-atoms.md)) |
| [0026](./0026-overlays-theme-stack-single-dispatch.md) | Overlays: one theme, a real stack, and a single dispatch path | Accepted (builds on [0020](./0020-layered-render.md)) |
| [0027](./0027-terminal-references-and-l3-links.md) | Terminals are referenced, not owned: views, links, and L3 tags | Accepted (builds on [0017](./0017-tui-not-protocol-privileged.md), [0015](./0015-protocol-layering.md)) |
| [0028](./0028-runtime-log-control.md) | Runtime log control | Accepted (forward-compat, builds on [0024](./0024-wire-owns-input-atoms.md)) |
| [0029](./0029-one-cursor-authority-and-repaint-scheduler.md) | One cursor authority and a repaint scheduler | Accepted (forward-compat, extends [0020](./0020-layered-render.md)) |
| [0030](./0030-engine-delegated-wire-and-projection-consumers.md) | Engine-delegated wire and projection consumers | Accepted (supersedes the L2 tier of [0015](./0015-protocol-layering.md)) |
| [0031](./0031-remote-consumer-auth-and-encryption.md) | Remote-consumer authentication and encryption (no SSH tunnel) | Proposed |
| [0032](./0032-graceful-server-upgrade.md) | Graceful server upgrade (sessions survive a binary update) | Accepted |
| [0033](./0033-input-authority-and-process-signals.md) | Input authority leases and process signals ("take the wheel + kill") | Accepted |
| [0034](./0034-kitty-graphics-image-passthrough.md) | Kitty graphics / image passthrough through the cell renderer | Proposed |
| [0035](./0035-agent-asked-event.md) | Agent-asked event: a pending human-answerable question on the wire | Accepted |
| [0036](./0036-agent-asked-detection.md) | Agent-asked detection sources | Accepted |
| [0037](./0037-overlay-network-reachability.md) | Overlay-network reachability for remote self-host consumers | Accepted (forward-compat, builds on [0007](./0007-mosh-class-transport-and-satellites.md), [0031](./0031-remote-consumer-auth-and-encryption.md)) |
| [0038](./0038-hub-satellite-auth.md) | Hub-to-satellite authentication | Accepted (builds on [0031](./0031-remote-consumer-auth-and-encryption.md)) |
| [0039](./0039-layered-config.md) | Layered config: an ordered `extends` stack with explicit array append | Accepted |
| [0040](./0040-agent-identity-metadata.md) | Agent identity and lifecycle are an L3 metadata record | Accepted |
| [0041](./0041-managed-plugin-installs.md) | Managed plugin installs: snapshot fetches, system tools, one lockfile | Accepted |
| [0042](./0042-launch-executor.md) | Launch executor: a CLI verb that spawns an integration template | Accepted |
| [0043](./0043-state-diff-output-mode.md) | State-diff output mode and loss-tolerant reference advance | Accepted |
| [0044](./0044-dedicated-input-lane.md) | Dedicated input lane: route input off the single runtime thread | Accepted |
| [0045](./0045-client-side-copy-mode.md) | Client-side copy-mode over the consumer's own engine | Accepted (builds on [0030](./0030-engine-delegated-wire-and-projection-consumers.md), supersedes the abi epic's server-side selection frames) |
| [0046](./0046-server-side-agent-state-detection.md) | The server derives agent state; detection is level-triggered | Accepted (extends [0040](./0040-agent-identity-metadata.md); implements [0029](./0029-one-cursor-authority-and-repaint-scheduler.md)'s repaint accumulator) |
| [0047](./0047-ci-metrics-branch.md) | CI metrics recorded to an orphan `ci-metrics` branch | Accepted |
| [0048](./0048-drag-to-resize-and-default-mouse-capture.md) | Drag-to-resize panes and default outer-terminal mouse capture | Accepted |
| [0049](./0049-client-local-focus-and-advisory-attention.md) | Client-local focus and advisory agent attention | Accepted (reaffirms [0019](./0019-tui-multi-pane-rendering.md)) |
| [0050](./0050-explicit-spawn-ownership.md) | Explicit spawn ownership, client-owned placement | Accepted |
| [0051](./0051-outbound-dial-out-connector-transport.md) | Outbound dial-out (connector) transport mode | Accepted (walks through [0037](./0037-overlay-network-reachability.md)'s deferred door; builds on [0031](./0031-remote-consumer-auth-and-encryption.md), [0038](./0038-hub-satellite-auth.md)) |
| [0052](./0052-connector-route-identity-and-config.md) | Connector route identity, registration, and config surface | Accepted (settles [0051](./0051-outbound-dial-out-connector-transport.md) open questions 1/4/5) |
| [0053](./0053-acknowledged-idempotent-input.md) | Acknowledged idempotent input batches | Accepted (builds on [0021](./0021-control-plane-commands.md), [0024](./0024-wire-owns-input-atoms.md), and [0044](./0044-dedicated-input-lane.md)) |
| [0054](./0054-worktree-bound-sessions.md) | Worktree-bound sessions by name convention | Accepted (composes existing verbs; adds no server state, consistent with [0009](./0009-phux-vs-mux-positioning.md)) |
| [0055](./0055-always-on-server-and-ssh-bootstrapped-enrollment.md) | Always-on server and ssh-bootstrapped enrollment | Proposed (makes [0031](./0031-remote-consumer-auth-and-encryption.md)/[0037](./0037-overlay-network-reachability.md) usable; mirrors [0038](./0038-hub-satellite-auth.md)'s pin posture) |
| [0056](./0056-cross-session-terminal-move.md) | Cross-session Terminal move | Accepted (opens the door [0050](./0050-explicit-spawn-ownership.md) left shut for existing Terminals; layout stays L3 per [0019](./0019-tui-multi-pane-rendering.md)) |
| [0057](./0057-minimal-reference-relay.md) | A minimal reference relay in-tree | Accepted (implements [0051](./0051-outbound-dial-out-connector-transport.md) and ADR-0052; backs 0051's trust-honesty claim) |
| [0058](./0058-right-click-context-menus.md) | Right-click context menus for panes, windows, and sessions | Accepted |
| [0059](./0059-sandboxed-chunked-file-upload.md) | Sandboxed chunked file upload | Accepted (builds on [0007](./0007-mosh-class-transport-and-satellites.md), [0021](./0021-control-plane-commands.md), and [0031](./0031-remote-consumer-auth-and-encryption.md)) |
| [0060](./0060-self-contained-session-recording.md) | Self-contained session recording | Accepted (a consumer-side projection over [0013](./0013-libghostty-bytes-on-wire.md)'s bytes; claims no protocol standing, per [0017](./0017-tui-not-protocol-privileged.md)) |
| [0061](./0061-capabilities-add-versions-break.md) | Capabilities add, versions break | Accepted (generalizes the version-gate constraint that shaped [0060](./0060-self-contained-session-recording.md); the fleet-wide break it names is what [0032](./0032-graceful-server-upgrade.md) survives) |
| [0062](./0062-headless-resize-and-window-size-policy.md) | Headless resize and the window-size policy | Accepted (settles the explicit-vs-view precedence [0027](./0027-terminal-references-and-l3-links.md) left to a "future resize verb"; takes no wire under [0061](./0061-capabilities-add-versions-break.md)) |
| [0063](./0063-ephemeral-server-lifetime.md) | Ephemeral server lifetime | Accepted (an opt-in exit condition alongside the last-pane self-exit of [0003](./0003-server-process-model.md); survives re-exec via [0032](./0032-graceful-server-upgrade.md)) |
| [0064](./0064-playback-as-a-pane.md) | Playback as a pane | Accepted (supersedes the "shipping a player" rejection in [0060](./0060-self-contained-session-recording.md) for the pane-shaped case only; takes no wire under [0061](./0061-capabilities-add-versions-break.md) and fits the pane with [0062](./0062-headless-resize-and-window-size-policy.md)) |
| [0065](./0065-one-cli-grammar.md) | One CLI grammar | Accepted |

## When to write an ADR

- Picking between viable approaches with long-term consequences.
- Closing off a design space (deciding *against* something).
- Anything you'd want to explain to a new contributor on day one.

## When NOT to write an ADR

- Bug fixes.
- Refactors that don't change behavior.
- Anything purely internal to a single function.

## Template

```
# NNNN — Short title

Status: Proposed | Accepted | Deprecated | Superseded by ADR-NNNN
Date: YYYY-MM-DD

## Context
What is the situation that calls for a decision?

## Decision
What was decided.

## Rationale
Why this and not the alternatives.

## Tradeoffs
What we give up.

## Alternatives considered
Brief sketch of the other candidates and why they lost.
```
