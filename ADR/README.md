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
Every ADR has exactly one row here, inserted at its numeric position when
the ADR is written. This is enforced (`adr-index-sync` in
scripts/check-docs.sh): a missing row, a duplicate number, or an
out-of-order row fails `just docs-check`. The row is deliberately a
collision point — two parallel branches claiming the same ADR number
produce a textual conflict on this table at rebase, where the two files
alone would merge silently (it happened: wave 3 created two different
ADR-0086 files with zero git conflicts). The relationship annotations in
the Status column (supersedes / refines / builds on / amends / extends)
are hand-curated from each ADR's body.
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
| [0029](./0029-one-cursor-authority-and-repaint-scheduler.md) | One cursor authority and a repaint scheduler | Accepted (extends [0020](./0020-layered-render.md); shipped — `end_of_frame_cursor` and the `RepaintLevel` accumulator both live, see [0046](./0046-server-side-agent-state-detection.md)) |
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
| [0047](./0047-ci-metrics-branch.md) | CI metrics recorded to an orphan `ci-metrics` branch | Superseded by [0082](./0082-retire-the-ci-metrics-store.md) |
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
| [0066](./0066-host-namespace.md) | One `phux host` namespace over the split machine registries | Accepted |
| [0067](./0067-cache-preserving-agent-fleet-context.md) | Cache-preserving agent fleet context | Accepted (projects [0040](./0040-agent-identity-metadata.md)/[0046](./0046-server-side-agent-state-detection.md) into agent-host context without changing the wire) |
| [0068](./0068-native-agent-session-restore.md) | Native agent-session restore | Accepted (bridges [0040](./0040-agent-identity-metadata.md), [0042](./0042-launch-executor.md), and workspace archives through bounded L3 provenance; adds no wire under [0061](./0061-capabilities-add-versions-break.md)) |
| [0069](./0069-generated-reference-docs.md) | Generated reference docs from the compiled binary | Accepted |
| [0070](./0070-native-engine-state-bootstrap.md) | Native engine-state bootstrap and client-owned history | Accepted (replaces native clients' synthesized-VT bootstrap under [0013](./0013-libghostty-bytes-on-wire.md) with an opaque libghostty READY/history lifecycle; compatibility clients retain synthesized VT, and one PTY retains one authoritative geometry) |
| [0071](./0071-what-phux-1-0-commits-to.md) | What phux 1.0 commits to | Proposed (freezes the consumer surface under semver while the wire keeps its own `0.x` line per [0061](./0061-capabilities-add-versions-break.md); point 6 enumerates the agent verbs, JSON documents, event names, and error codes inside that freeze) |
| [0072](./0072-prune-policy-vocabulary-keep-the-seam.md) | Prune the policy vocabulary, keep the authorization seam | Proposed (prunes the unreferenced half of the vocabulary [0031](./0031-remote-consumer-auth-and-encryption.md) introduced, and keeps the HELLO seam a post-1.0 paired-workload feature must implement) |
| [0073](./0073-service-managed-pane-login-shell.md) | Login-shell semantics for service-managed pane spawns | Accepted (closes the environment gap [0055](./0055-always-on-server-and-ssh-bootstrapped-enrollment.md)'s generated unit opened) |
| [0074](./0074-self-update-trust-boundary.md) | The self-update trust boundary | Accepted (checksum-gated, atomic, never mutates an install another tool owns; delivers the one-command update path [0071](./0071-what-phux-1-0-commits-to.md) puts in 1.0 scope) |
| [0075](./0075-agent-name-addressing.md) | Agent names are addressable, and a withdrawn name is refused | Proposed (adds a `%name` sigil to the client-side resolution of [0021](./0021-control-plane-commands.md), over the identity [0040](./0040-agent-identity-metadata.md) already carries; no wire change, a grammar addition [0071](./0071-what-phux-1-0-commits-to.md) has to carve in, and it owns the write-time safety gate every input verb reads) |
| [0076](./0076-agent-prompt-and-lifecycle-wait.md) | Prompting an agent is acknowledged; waiting on one is event-driven | Proposed (spends [0053](./0053-acknowledged-idempotent-input.md)'s acknowledged batch on the agent surface and subscribes to [0046](./0046-server-side-agent-state-detection.md)'s published record rather than adding a sequence field; the `wait` half shipped, the `prompt` half has not) |
| [0077](./0077-agent-read-surface.md) | The agent read surface: sources, soft wrap, and truncation | Accepted (extends [0022](./0022-tool-for-agents.md)'s read surface with additive JSON keys under [0061](./0061-capabilities-add-versions-break.md), all four shipped at `SCHEMA_VERSION` 3; the alternate-screen harvest it originally carried is split out to [0078](./0078-alternate-screen-history.md)) |
| [0078](./0078-alternate-screen-history.md) | Harvesting alternate-screen history | Proposed (split out of [0077](./0077-agent-read-surface.md) because it is the one read that writes: it narrows the side-effect-free guarantee `docs/spec/L1.md` §6.1 makes for `GET_SCREEN`, acquires [0033](./0033-input-authority-and-process-signals.md)'s input lease, and needs a capability bit under [0061](./0061-capabilities-add-versions-break.md)) |
| [0079](./0079-fatal-signal-terminal-restore.md) | Fatal-signal terminal restore | Accepted (covers the teardown path `RawModeGuard::drop` and the panic hook cannot reach — a SIGSEGV/SIGBUS/SIGABRT out of [0004](./0004-libghostty-vt-as-grid.md)'s native engine, which does not unwind; vendors `phux-crash`, the workspace's only Apache-2.0-ONLY crate, keeping its `unsafe` behind a crate boundary as [0032](./0032-graceful-server-upgrade.md) does for `portable-pty-adopt`) |
| [0080](./0080-socket-lifecycle-and-instance-isolation.md) | Socket lifecycle and instance isolation | Accepted (liveness is a connect probe, not socket existence; every build resolves a profile that scopes socket/runtime/state dirs; supervision corrected under [0003](./0003-server-process-model.md), with the upgrade handoff riding [0032](./0032-graceful-server-upgrade.md)) |
| [0081](./0081-overlay-auto-listen-and-one-command-pairing.md) | Overlay auto-listen and one-command pairing | Accepted (binds [0037](./0037-overlay-network-reachability.md)'s overlay address at startup, gated on [0031](./0031-remote-consumer-auth-and-encryption.md)'s pairing-token store, so `phux pair` is a pure credential operation with no restart; default profile only, per [0080](./0080-socket-lifecycle-and-instance-isolation.md)) |
| [0082](./0082-retire-the-ci-metrics-store.md) | Retire the CI metrics store; the run page is the dashboard | Accepted (supersedes [0047](./0047-ci-metrics-branch.md) — deletes the `ci-metrics` branch, its collector, and the `observatory` lane, keeping only the zero-cost step-summary half; a hosted dashboard, if it returns, is the site's to own) |
| [0083](./0083-in-place-supervisor-unit-reconcile.md) | In-place supervisor unit reconcile | Accepted (applies [0080](./0080-socket-lifecycle-and-instance-isolation.md)'s restart-policy correction to an already-installed unit by patching only those keys — no re-render, no reload, no stopped server; launchd cannot pick it up live and the command says so) |
| [0084](./0084-starting-an-agent-in-an-existing-shell.md) | Starting an agent in an existing shell | Accepted (separates in-place, shell-evaluated startup from [0042](./0042-launch-executor.md)'s direct-argv pane creation; positive OSC 133 prompt evidence gates submission, detector publication supplies identity, and possible delivery retains the bound name) |
| [0085](./0085-hook-sourced-agent-state.md) | Hook-sourced agent state is detector evidence | Accepted (adds capability-gated `REPORT_AGENT_STATE`: hooks publish immediate working/blocked/done edges through the detector without writing a state declaration that disables self-healing) |
| [0086](./0086-shared-render-pool.md) | The pooled libghostty render trio lives in `phux-protocol` | Accepted (one `RenderPool` owns the `RenderState`/`RowIterator`/`CellIterator` trio and the `phux-5pyx` rebuild-on-resize, behind the existing `server` feature; dirty-bit policy stays at the call sites, which deliberately differ) |
| [0087](./0087-elastic-status-bar-space.md) | Elastic status-bar space is row-wide slack, not slot layout | Proposed (defines the `spacer` widget frozen by [0071](./0071-what-phux-1-0-commits-to.md): paid from the row's leftover width, split evenly, zero on an overflowing row — rather than giving `[status]` slots a two-pass width budget) |
| [0088](./0088-adopting-a-live-server-into-supervision.md) | Adopting a live server into supervision | Accepted (no supervisor can restart-manage a pid it did not start, so `install --adopt` transfers the supervision rather than the process: the unit is armed instead of loaded, the incumbent keeps its panes, and the auto-spawn path completes the hand-over) |
| [0089](./0089-three-zone-attention-sidebar.md) | The sidebar is a bounded attention inbox, not a structural list | Accepted (three zones ranked by how much each row wants a human: a capped cross-session queue that contributes zero rows when nothing is blocked, the focused session's windows behind a floor, and one rolled-up line per other session; built from verbs the client already sends, so no wire surface is added) |
| [0090](./0090-confirmation-gated-predictive-echo.md) | Predictive echo returns to the alt screen via confirmation-gated display | Accepted (predictions queue and reconcile on both screens but display on the alternate screen only after the app confirms a non-blank echo, re-locking on contradiction and hiding on a one-second timeout; the adaptive back-off becomes a display lock so its re-arm path can actually fire; upstreams phux-mobile ADR-0019) |
| [0091](./0091-certificate-names-the-advertised-address.md) | The certificate names the advertised address, once, at generation | Accepted (SANs cover the listener's bind address and the overlay address the connect link carries, chosen only when the certificate is minted; an existing certificate is never widened because that rotates the fingerprint every paired device pins, so coverage is reported by `phux pair`, the listener log, and `phux doctor` instead) |
| [0092](./0092-durable-work-coordinator-authority.md) | The coordinator owns durable work | Proposed (would narrowly amend [0009](./0009-phux-vs-mux-positioning.md), scope [0030](./0030-engine-delegated-wire-and-projection-consumers.md) to terminal synchronization, and reuse [0033](./0033-input-authority-and-process-signals.md)/[0053](./0053-acknowledged-idempotent-input.md) rather than create a second runner) |
| [0093](./0093-remote-target-as-a-resolution-ladder.md) | `--remote user@host` is a resolution ladder, not a new transport | Accepted (resolves a target to a `[[remote]]` entry — registry hit, pasted `phux://connect` code, or one-time ssh pairing — then reuses the existing dial; adds no transport, no wire change, and no trust model, and `user@` is a pairing/lookup label rather than a wire identity) |
| [0094](./0094-explicit-per-pane-scrollback-byte-ceiling.md) | Per-pane scrollback is bounded in bytes, by phux, explicitly | Accepted (`defaults.history-limit` is only libghostty's line limit and the engine's own 10_000-byte constructor default was what actually bound retention, so phux sets the byte bound itself and exposes it as `defaults.history-bytes`, default 2 MiB, capped at 64 MiB by `config check`; retention trades directly against attach latency because the native bootstrap materialises every retained page at READY) |
| [0095](./0095-the-blackbird-boundary.md) | Blackbird is a peer ledger, not a phux client | Accepted (the two daemons do not connect: the seam is one optional field in the [0040](./0040-agent-identity-metadata.md) record written by the [0067](./0067-cache-preserving-agent-fleet-context.md) integrations, and the "required by Blackbird ADR-0005" justification is deleted because that document was never written and its architecture is archived) |
| [0096](./0096-always-on-performance-telemetry.md) | Performance telemetry is always on, in-process, and one command away | Accepted |

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
