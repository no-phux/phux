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
ADR-0086 files with zero git conflicts). The Status column is the base
status word plus at most one relationship clause (supersedes / superseded
by / amends / builds on), about 80 characters at most. It is navigation,
not a summary: the TL;DR lives in the ADR.
-->

| # | Decision | Status |
|---|----------|--------|
| [0001](./0001-language-rust.md) | Use Rust | Accepted |
| [0002](./0002-diff-based-protocol.md) | Diff-based wire protocol, not VT byte replay | Superseded by [0013](./0013-libghostty-bytes-on-wire.md) |
| [0003](./0003-server-process-model.md) | Single server, many sessions | Accepted |
| [0004](./0004-libghostty-vt-as-grid.md) | libghostty-vt is the canonical grid | Accepted |
| [0005](./0005-relationship-to-zmx-and-zmosh.md) | Relationship to zmx and zmosh | Accepted |
| [0006](./0006-input-mirrors-libghostty.md) | Input event types re-export libghostty-vt's atoms | Accepted (amended by [0024](./0024-wire-owns-input-atoms.md)) |
| [0007](./0007-mosh-class-transport-and-satellites.md) | Mosh-class transport semantics and satellite forward-compat | Accepted (forward-compat; superseded in part by [0098](./0098-workload-proof-and-closed-scope-authority.md)) |
| [0008](./0008-use-libghostty-types-directly.md) | Use libghostty-vt's types directly; stop reimplementing them | Accepted (amended by [0024](./0024-wire-owns-input-atoms.md)) |
| [0009](./0009-phux-vs-mux-positioning.md) | phux vs coder/mux: positioning | Accepted |
| [0010](./0010-frontend-agnostic-tmux-cc-reserved.md) | phux is TUI-first, non-TUI not precluded; tmux control mode reserved as compat option | Accepted (forward-compat; superseded in part by [0017](./0017-tui-not-protocol-privileged.md)) |
| [0011](./0011-protocol-core-independence.md) | `phux-protocol` and `phux-core` are independent; `IdBridge` is their only meeting point | Accepted |
| [0012](./0012-binary-split-tree-layout.md) | Window layout is a binary split tree, not n-ary | Accepted (superseded in part by [0015](./0015-protocol-layering.md)) |
| [0013](./0013-libghostty-bytes-on-wire.md) | Libghostty bytes on the wire; structured input remains | Accepted (supersedes [0002](./0002-diff-based-protocol.md)) |
| [0014](./0014-server-terminal-pane-actor.md) | Server-side `Terminal` placement: per-pane PaneActor on a `LocalSet` | Accepted |
| [0015](./0015-protocol-layering.md) | Protocol layering: L1 substrate, L2 collections, L3 metadata | Accepted (superseded in part by [0030](./0030-engine-delegated-wire-and-projection-consumers.md)) |
| [0016](./0016-terminal-id-as-wire-primary.md) | `TerminalId` as the wire primary; `PaneId` is a consumer-side alias | Superseded by [0102](./0102-resources-the-server-serves-kinds.md) |
| [0017](./0017-tui-not-protocol-privileged.md) | The reference TUI is not protocol-privileged | Accepted (supersedes in part [0010](./0010-frontend-agnostic-tmux-cc-reserved.md)) |
| [0018](./0018-lazy-state-synchronization.md) | Lazy state synchronization is the wire's long-arc shape | Accepted (builds on [0013](./0013-libghostty-bytes-on-wire.md)) |
| [0019](./0019-tui-multi-pane-rendering.md) | Multi-pane TUI rendering: layout persistence, wire shape, and chrome | Accepted |
| [0020](./0020-layered-render.md) | Layered render: ratatui chrome over libghostty pane interiors | Accepted |
| [0021](./0021-control-plane-commands.md) | Control-plane commands and client-side selector resolution | Accepted (superseded in part by [0030](./0030-engine-delegated-wire-and-projection-consumers.md)) |
| [0022](./0022-tool-for-agents.md) | phux as a tool for agents | Accepted |
| [0023](./0023-config-ux-philosophy.md) | Config UX: pure-config, defaults as a live base layer | Accepted (builds on [0017](./0017-tui-not-protocol-privileged.md)) |
| [0024](./0024-wire-owns-input-atoms.md) | The wire protocol owns its input atoms | Accepted (amends [0006](./0006-input-mirrors-libghostty.md), [0008](./0008-use-libghostty-types-directly.md)) |
| [0025](./0025-browser-web-client.md) | Browser web client over a WebSocket transport | Accepted (builds on [0017](./0017-tui-not-protocol-privileged.md), [0024](./0024-wire-owns-input-atoms.md)) |
| [0026](./0026-overlays-theme-stack-single-dispatch.md) | Overlays: one theme, a real stack, and a single dispatch path | Accepted (builds on [0020](./0020-layered-render.md)) |
| [0027](./0027-terminal-references-and-l3-links.md) | Terminals are referenced, not owned: views, links, and L3 tags | Accepted (builds on [0017](./0017-tui-not-protocol-privileged.md), [0015](./0015-protocol-layering.md)) |
| [0028](./0028-runtime-log-control.md) | Runtime log control | Accepted (forward-compat; builds on [0024](./0024-wire-owns-input-atoms.md)) |
| [0029](./0029-one-cursor-authority-and-repaint-scheduler.md) | One cursor authority and a repaint scheduler | Accepted (builds on [0020](./0020-layered-render.md)) |
| [0030](./0030-engine-delegated-wire-and-projection-consumers.md) | Engine-delegated wire and projection consumers | Superseded by [0102](./0102-resources-the-server-serves-kinds.md) |
| [0031](./0031-remote-consumer-auth-and-encryption.md) | Remote-consumer authentication and encryption (no SSH tunnel) | Proposed |
| [0032](./0032-graceful-server-upgrade.md) | Graceful server upgrade (sessions survive a binary update) | Accepted |
| [0033](./0033-input-authority-and-process-signals.md) | Input authority leases and process signals ("take the wheel + kill") | Accepted |
| [0034](./0034-kitty-graphics-image-passthrough.md) | Kitty graphics / image passthrough through the cell renderer | Proposed |
| [0035](./0035-agent-asked-event.md) | Agent-asked event: a pending human-answerable question on the wire | Accepted |
| [0036](./0036-agent-asked-detection.md) | Agent-asked detection sources | Accepted |
| [0037](./0037-overlay-network-reachability.md) | Overlay-network reachability for remote self-host consumers | Accepted (forward-compat; builds on [0007](./0007-mosh-class-transport-and-satellites.md), [0031](./0031-remote-consumer-auth-and-encryption.md)) |
| [0038](./0038-hub-satellite-auth.md) | Hub-to-satellite authentication | Accepted (builds on [0031](./0031-remote-consumer-auth-and-encryption.md)) |
| [0039](./0039-layered-config.md) | Layered config: an ordered `extends` stack with explicit array append | Accepted |
| [0040](./0040-agent-identity-metadata.md) | Agent identity and lifecycle are an L3 metadata record | Accepted |
| [0041](./0041-managed-plugin-installs.md) | Managed plugin installs: snapshot fetches, system tools, one lockfile | Accepted |
| [0042](./0042-launch-executor.md) | Launch executor: a CLI verb that spawns an integration template | Accepted |
| [0043](./0043-state-diff-output-mode.md) | State-diff output mode and loss-tolerant reference advance | Accepted |
| [0044](./0044-dedicated-input-lane.md) | Dedicated input lane: route input off the single runtime thread | Accepted |
| [0045](./0045-client-side-copy-mode.md) | Client-side copy-mode over the consumer's own engine | Accepted (builds on [0030](./0030-engine-delegated-wire-and-projection-consumers.md)) |
| [0046](./0046-server-side-agent-state-detection.md) | The server derives agent state; detection is level-triggered | Accepted (builds on [0040](./0040-agent-identity-metadata.md)) |
| [0047](./0047-ci-metrics-branch.md) | CI metrics recorded to an orphan `ci-metrics` branch | Superseded by [0082](./0082-retire-the-ci-metrics-store.md) |
| [0048](./0048-drag-to-resize-and-default-mouse-capture.md) | Drag-to-resize panes and default outer-terminal mouse capture | Accepted |
| [0049](./0049-client-local-focus-and-advisory-attention.md) | Client-local focus and advisory agent attention | Accepted (builds on [0019](./0019-tui-multi-pane-rendering.md)) |
| [0050](./0050-explicit-spawn-ownership.md) | Explicit spawn ownership, client-owned placement | Accepted |
| [0051](./0051-outbound-dial-out-connector-transport.md) | Outbound dial-out (connector) transport mode | Accepted (builds on [0037](./0037-overlay-network-reachability.md)) |
| [0052](./0052-connector-route-identity-and-config.md) | Connector route identity, registration, and config surface | Accepted (builds on [0051](./0051-outbound-dial-out-connector-transport.md)) |
| [0053](./0053-acknowledged-idempotent-input.md) | Acknowledged idempotent input batches | Accepted (builds on [0021](./0021-control-plane-commands.md), [0024](./0024-wire-owns-input-atoms.md), and [0044](./0044-dedicated-input-lane.md)) |
| [0054](./0054-worktree-bound-sessions.md) | Worktree-bound sessions by name convention | Accepted |
| [0055](./0055-always-on-server-and-ssh-bootstrapped-enrollment.md) | Always-on server and ssh-bootstrapped enrollment | Accepted (superseded in part by [0080](./0080-socket-lifecycle-and-instance-isolation.md), [0096](./0096-always-on-performance-telemetry.md)) |
| [0056](./0056-cross-session-terminal-move.md) | Cross-session Terminal move | Accepted (builds on [0050](./0050-explicit-spawn-ownership.md)) |
| [0057](./0057-minimal-reference-relay.md) | A minimal reference relay in-tree | Accepted (builds on [0051](./0051-outbound-dial-out-connector-transport.md), [0052](./0052-connector-route-identity-and-config.md)) |
| [0058](./0058-right-click-context-menus.md) | Right-click context menus for panes, windows, and sessions | Accepted |
| [0059](./0059-sandboxed-chunked-file-upload.md) | Sandboxed chunked file upload | Accepted (builds on [0007](./0007-mosh-class-transport-and-satellites.md), [0021](./0021-control-plane-commands.md), and [0031](./0031-remote-consumer-auth-and-encryption.md)) |
| [0060](./0060-self-contained-session-recording.md) | Self-contained session recording | Accepted (builds on [0013](./0013-libghostty-bytes-on-wire.md)) |
| [0061](./0061-capabilities-add-versions-break.md) | Capabilities add, versions break | Accepted (builds on [0060](./0060-self-contained-session-recording.md)) |
| [0062](./0062-headless-resize-and-window-size-policy.md) | Headless resize and the window-size policy | Accepted (builds on [0027](./0027-terminal-references-and-l3-links.md)) |
| [0063](./0063-ephemeral-server-lifetime.md) | Ephemeral server lifetime | Accepted (builds on [0003](./0003-server-process-model.md)) |
| [0064](./0064-playback-as-a-pane.md) | Playback as a pane | Accepted (supersedes in part [0060](./0060-self-contained-session-recording.md)) |
| [0065](./0065-one-cli-grammar.md) | One CLI grammar | Accepted |
| [0066](./0066-host-namespace.md) | One `phux host` namespace over the split machine registries | Accepted |
| [0067](./0067-cache-preserving-agent-fleet-context.md) | Cache-preserving agent fleet context | Accepted (builds on [0040](./0040-agent-identity-metadata.md), [0046](./0046-server-side-agent-state-detection.md)) |
| [0068](./0068-native-agent-session-restore.md) | Native agent-session restore | Accepted (builds on [0040](./0040-agent-identity-metadata.md), [0042](./0042-launch-executor.md)) |
| [0069](./0069-generated-reference-docs.md) | Generated reference docs from the compiled binary | Accepted |
| [0070](./0070-native-engine-state-bootstrap.md) | Native engine-state bootstrap and client-owned history | Accepted (builds on [0013](./0013-libghostty-bytes-on-wire.md)) |
| [0071](./0071-what-phux-1-0-commits-to.md) | What phux 1.0 commits to | Proposed (builds on [0061](./0061-capabilities-add-versions-break.md)) |
| [0072](./0072-prune-policy-vocabulary-keep-the-seam.md) | Prune the policy vocabulary, keep the authorization seam | Proposed (amends [0031](./0031-remote-consumer-auth-and-encryption.md)) |
| [0073](./0073-service-managed-pane-login-shell.md) | Login-shell semantics for service-managed pane spawns | Accepted (builds on [0055](./0055-always-on-server-and-ssh-bootstrapped-enrollment.md)) |
| [0074](./0074-self-update-trust-boundary.md) | The self-update trust boundary | Accepted (builds on [0071](./0071-what-phux-1-0-commits-to.md)) |
| [0075](./0075-agent-name-addressing.md) | Agent names are addressable, and a withdrawn name is refused | Proposed (builds on [0021](./0021-control-plane-commands.md), [0040](./0040-agent-identity-metadata.md)) |
| [0076](./0076-agent-prompt-and-lifecycle-wait.md) | Prompting an agent is acknowledged; waiting on one is event-driven | Proposed (builds on [0053](./0053-acknowledged-idempotent-input.md), [0046](./0046-server-side-agent-state-detection.md)) |
| [0077](./0077-agent-read-surface.md) | The agent read surface: sources, soft wrap, and truncation | Accepted (builds on [0022](./0022-tool-for-agents.md)) |
| [0078](./0078-alternate-screen-history.md) | Harvesting alternate-screen history | Proposed (builds on [0077](./0077-agent-read-surface.md)) |
| [0079](./0079-fatal-signal-terminal-restore.md) | Fatal-signal terminal restore | Accepted |
| [0080](./0080-socket-lifecycle-and-instance-isolation.md) | Socket lifecycle and instance isolation | Accepted (amends [0055](./0055-always-on-server-and-ssh-bootstrapped-enrollment.md)) |
| [0081](./0081-overlay-auto-listen-and-one-command-pairing.md) | Overlay auto-listen and one-command pairing | Accepted (builds on [0037](./0037-overlay-network-reachability.md), [0031](./0031-remote-consumer-auth-and-encryption.md)) |
| [0082](./0082-retire-the-ci-metrics-store.md) | Retire the CI metrics store; the run page is the dashboard | Accepted (supersedes [0047](./0047-ci-metrics-branch.md)) |
| [0083](./0083-in-place-supervisor-unit-reconcile.md) | In-place supervisor unit reconcile | Accepted (builds on [0080](./0080-socket-lifecycle-and-instance-isolation.md)) |
| [0084](./0084-starting-an-agent-in-an-existing-shell.md) | Starting an agent in an existing shell | Accepted (builds on [0042](./0042-launch-executor.md)) |
| [0085](./0085-hook-sourced-agent-state.md) | Hook-sourced agent state is detector evidence | Accepted (builds on [0040](./0040-agent-identity-metadata.md), [0046](./0046-server-side-agent-state-detection.md)) |
| [0086](./0086-shared-render-pool.md) | The pooled libghostty render trio lives in `phux-protocol` | Accepted |
| [0087](./0087-elastic-status-bar-space.md) | Elastic status-bar space is row-wide slack, not slot layout | Proposed (builds on [0071](./0071-what-phux-1-0-commits-to.md)) |
| [0088](./0088-adopting-a-live-server-into-supervision.md) | Adopting a live server into supervision | Accepted (builds on [0055](./0055-always-on-server-and-ssh-bootstrapped-enrollment.md), [0080](./0080-socket-lifecycle-and-instance-isolation.md)) |
| [0089](./0089-three-zone-attention-sidebar.md) | The sidebar is a bounded attention inbox, not a structural list | Accepted |
| [0090](./0090-confirmation-gated-predictive-echo.md) | Predictive echo returns to the alt screen via confirmation-gated display | Accepted |
| [0091](./0091-certificate-names-the-advertised-address.md) | The certificate names the advertised address, once, at generation | Accepted (builds on [0031](./0031-remote-consumer-auth-and-encryption.md)) |
| [0092](./0092-durable-work-coordinator-authority.md) | The coordinator owns durable work | Proposed (amends [0009](./0009-phux-vs-mux-positioning.md)) |
| [0093](./0093-remote-target-as-a-resolution-ladder.md) | `--remote user@host` is a resolution ladder, not a new transport | Accepted |
| [0094](./0094-explicit-per-pane-scrollback-byte-ceiling.md) | Per-pane scrollback is bounded in bytes, by phux, explicitly | Accepted |
| [0095](./0095-the-blackbird-boundary.md) | Blackbird is a peer ledger, not a phux client | Accepted (builds on [0040](./0040-agent-identity-metadata.md)) |
| [0096](./0096-always-on-performance-telemetry.md) | Performance telemetry is always on, in-process, and one command away | Accepted |
| [0098](./0098-workload-proof-and-closed-scope-authority.md) | Workload proof and closed-scope authority | Accepted (forward-compat; amends [0031](./0031-remote-consumer-auth-and-encryption.md)) |
| [0099](./0099-ci-aggregate-gate-and-action-supply-chain.md) | CI: one aggregate merge gate, immutable action pins, and shared lane setup | Accepted |
| [0100](./0100-the-tui-is-its-own-crate.md) | The TUI is its own crate | Accepted (builds on [0020](./0020-layered-render.md)) |
| [0101](./0101-the-settings-page-edits-the-file.md) | The settings page edits the file | Accepted (builds on [0023](./0023-config-ux-philosophy.md)) |
| [0102](./0102-resources-the-server-serves-kinds.md) | Resources: the server serves kinds; Terminal is the first | Accepted (supersedes [0016](./0016-terminal-id-as-wire-primary.md), [0030](./0030-engine-delegated-wire-and-projection-consumers.md)) |
| [0103](./0103-agent-session-resource-and-producer-fed-streams.md) | Agent session resource and producer-fed streams | Accepted (amends [0040](./0040-agent-identity-metadata.md)) |
| [0104](./0104-parent-bindings-are-l1-lifecycle.md) | Parent bindings are L1 lifecycle | Accepted (builds on [0102](./0102-resources-the-server-serves-kinds.md)) |
| [0105](./0105-sessions-can-outlive-their-last-window.md) | Sessions can outlive their last window | Accepted (amends [0063](./0063-ephemeral-server-lifetime.md)) |
| [0106](./0106-identity-is-the-serving-user.md) | Identity is the serving user; whoami reports it | Accepted (builds on [0003](./0003-server-process-model.md)) |

## When to write an ADR

- Picking between viable approaches with long-term consequences.
- Closing off a design space (deciding *against* something).
- Anything you'd want to explain to a new contributor on day one.

## When NOT to write an ADR

- Bug fixes.
- Refactors that don't change behavior.
- Anything purely internal to a single function.

## Format

The template, the `Status:` vocabulary, the 150-line cap, and the
supersede-don't-amend rule are defined once, in
[`docs/CONVENTIONS.md`](../docs/CONVENTIONS.md) under "ADR template". Copy
the template from there; this file carries only the index.
