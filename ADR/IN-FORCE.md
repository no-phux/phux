---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-09
---

# Decisions in force

**TL;DR.** The ADRs that currently govern each part of phux, grouped by
topic and listed newest decision first, with one line on what each one
decides. Superseded and deprecated ADRs are omitted; ADRs still under
review sit in the trailing Proposed block. Start here to learn what is
settled and follow a link for the why. The README index is the numeric
view; a docs gate keeps the two in sync.

A topic lists only ADRs whose `Status:` is `Accepted` or
`Accepted (forward-compat)`. When an ADR is accepted, add its line to the
topic it governs; when it is superseded or deprecated, delete its line.
The `adr-in-force-sync` gate in `scripts/check-docs.sh` fails any tree
where a live ADR is missing from this file, listed twice, or listed after
it stopped being in force. Where an older ADR in a topic has been amended
by a newer one, the newer line is the operative reading.

## Identity and kinds

- [0104](./0104-parent-bindings-are-l1-lifecycle.md) A parent is bound at spawn; closing it closes every child with `ParentClosed`, atomically.
- [0102](./0102-resources-the-server-serves-kinds.md) The server serves resources of open kinds; `ResourceId` replaces `TerminalId`, with Terminal as kind 0.
- [0064](./0064-playback-as-a-pane.md) `phux play` creates a real Terminal fed from a cast; no wire change.
- [0062](./0062-headless-resize-and-window-size-policy.md) An explicit headless resize applies now but does not outrank the `window-size` policy.
- [0056](./0056-cross-session-terminal-move.md) `MOVE_RESOURCE` re-parents a live Terminal across sessions on L1; geometry stays L3.
- [0050](./0050-explicit-spawn-ownership.md) `SPAWN_RESOURCE` may name an owning Terminal; placement remains a client-written L3 concern.
- [0027](./0027-terminal-references-and-l3-links.md) A Terminal is one identity with one geometry; views, tags, and links are client-side.
- [0015](./0015-protocol-layering.md) The wire is L1 Terminals, L2 Collections, L3 metadata; sessions and windows are conventions.
- [0011](./0011-protocol-core-independence.md) `phux-protocol` and `phux-core` share no dependency edge; `IdBridge` is their meeting point.

## Wire and codecs

- [0086](./0086-shared-render-pool.md) One pooled libghostty render trio lives in `phux-protocol` behind the `server` feature.
- [0061](./0061-capabilities-add-versions-break.md) New wire surface ships as a negotiated capability; a `major.minor` mismatch is rejected.
- [0060](./0060-self-contained-session-recording.md) Recording is a consumer-side projection over the attach contract; encoders run in-process.
- [0059](./0059-sandboxed-chunked-file-upload.md) `PUT_FILE` sends bounded chunks into a server-chosen sandbox directory under the command envelope.
- [0034](./0034-kitty-graphics-image-passthrough.md) Kitty graphics prefer Unicode placeholders; the client repaints from its cell grid only.
- [0024](./0024-wire-owns-input-atoms.md) The wire protocol owns its input atoms instead of reusing libghostty's types.
- [0021](./0021-control-plane-commands.md) CLI verbs ride `COMMAND`/`COMMAND_RESULT`; selectors resolve client-side against `GET_STATE`.
- [0013](./0013-libghostty-bytes-on-wire.md) Terminal content crosses the wire as VT bytes; input stays structured events.
- [0008](./0008-use-libghostty-types-directly.md) Plain libghostty types are re-exported, not mirrored; only multiplexer domain types are phux's own.
- [0006](./0006-input-mirrors-libghostty.md) Wire input events wrap libghostty atoms with terminal addressing and phux framing.

## Server process and actor model

- [0105](./0105-sessions-can-outlive-their-last-window.md) A keep-empty session survives its last window until an explicit kill; default sessions still cascade.
- [0096](./0096-always-on-performance-telemetry.md) Performance telemetry is always on, in-process, and read back through `GET_PERF`.
- [0088](./0088-adopting-a-live-server-into-supervision.md) `install --adopt` arms a unit rather than loading it; the incumbent keeps its panes.
- [0083](./0083-in-place-supervisor-unit-reconcile.md) `service reconcile` patches only the installed unit's restart-policy keys and reloads nothing.
- [0080](./0080-socket-lifecycle-and-instance-isolation.md) Liveness is a connect probe; every build resolves a profile that isolates its directories.
- [0073](./0073-service-managed-pane-login-shell.md) A service-managed server spawns command-less panes as login shells; hand-started servers do not.
- [0063](./0063-ephemeral-server-lifetime.md) `--exit-after-idle` is an opt-in exit condition beside the last-pane self-exit.
- [0055](./0055-always-on-server-and-ssh-bootstrapped-enrollment.md) The server runs under a generated service unit; `enroll` mints credentials over ssh.
- [0032](./0032-graceful-server-upgrade.md) The server re-execs in place with inherited fds; sessions survive a binary upgrade.
- [0028](./0028-runtime-log-control.md) Logs are leveled `tracing`; input atoms narrate their shape and never their text.
- [0014](./0014-server-terminal-pane-actor.md) Each Terminal is one `spawn_local` actor on the single current-thread runtime.
- [0005](./0005-relationship-to-zmx-and-zmosh.md) zmx and zmosh are pattern sources, not shared code or forks.
- [0004](./0004-libghostty-vt-as-grid.md) Per-pane server state is a `libghostty_vt::Terminal`, not a hand-written grid.
- [0003](./0003-server-process-model.md) One server per user holds every session in a single process.

## State sync and bootstrap

- [0094](./0094-explicit-per-pane-scrollback-byte-ceiling.md) Scrollback is bounded in bytes by `defaults.history-bytes`, 2 MiB by default.
- [0090](./0090-confirmation-gated-predictive-echo.md) Predictive echo shows on the alternate screen only after a confirmed non-blank echo.
- [0070](./0070-native-engine-state-bootstrap.md) Native clients bootstrap from versioned libghostty state; history replicas are client-owned.
- [0043](./0043-state-diff-output-mode.md) `StateSync` output mode emits per-consumer minimum-VT diffs with an ack-advanced reference.
- [0018](./0018-lazy-state-synchronization.md) The wire's long-arc shape is lazy per-consumer synchronization of engine state.

## Input authority

- [0053](./0053-acknowledged-idempotent-input.md) `APPLY_INPUT` is one bounded, acknowledged, idempotent input batch under the command envelope.
- [0044](./0044-dedicated-input-lane.md) Input routing and encoding run on a dedicated thread from published mode snapshots.
- [0033](./0033-input-authority-and-process-signals.md) Exclusive input leases and process signals ride the command envelope as Terminal-scoped verbs.

## Federation and transport

- [0093](./0093-remote-target-as-a-resolution-ladder.md) `--remote user@host` resolves to a `[[remote]]` entry and reuses the existing dial.
- [0081](./0081-overlay-auto-listen-and-one-command-pairing.md) The server auto-binds its overlay listener at startup; `phux pair` only issues credentials.
- [0066](./0066-host-namespace.md) `phux host add|ls|rm|enroll` with `--role` replaces the split remote and satellite verbs.
- [0057](./0057-minimal-reference-relay.md) A single-process, single-tenant reference relay ships in-tree as a self-host tool.
- [0052](./0052-connector-route-identity-and-config.md) Consumers name a tunneled server by TLS SNI; routes bind to tokens at relay enrollment.
- [0051](./0051-outbound-dial-out-connector-transport.md) A server behind NAT dials out to a relay and holds one persistent QUIC tunnel.
- [0037](./0037-overlay-network-reachability.md) Remote self-host consumers reach the server over a WireGuard-class overlay; phux stays overlay-agnostic.
- [0025](./0025-browser-web-client.md) `phux-web` is a Rust-to-WASM consumer over a WebSocket transport beside UDS.
- [0007](./0007-mosh-class-transport-and-satellites.md) Transport is a trait; every identity carries a local or satellite tag from day one.

## Auth and trust

- [0106](./0106-identity-is-the-serving-user.md) A server never switches OS users; `user@host` selects that user's server, and `phux whoami` reports identity.
- [0098](./0098-workload-proof-and-closed-scope-authority.md) Workload clients present mutual Ed25519 proofs and closed, endpoint-owned scopes.
- [0091](./0091-certificate-names-the-advertised-address.md) The certificate names bind and overlay addresses once, at generation, and is never widened.
- [0038](./0038-hub-satellite-auth.md) A hub dials a satellite as an ordinary paired consumer, pinned to its certificate fingerprint.
- [0031](./0031-remote-consumer-auth-and-encryption.md) Remote consumers use TLS over WebSocket with a pairing-issued bearer token in HELLO.

## Agents

- [0103](./0103-agent-session-resource-and-producer-fed-streams.md) `AgentSession` is the second resource kind; its stream is producer-fed and derives agent state.
- [0095](./0095-the-blackbird-boundary.md) phux and Blackbird never connect; one optional field in the agent record joins their ledgers.
- [0085](./0085-hook-sourced-agent-state.md) Hook-reported working, blocked, and done states are detector evidence, published immediately.
- [0084](./0084-starting-an-agent-in-an-existing-shell.md) `phux agent start` types the integration argv into a live shell; detection verifies the kind.
- [0077](./0077-agent-read-surface.md) The read surface grows additive `ScreenState` keys, not a new read-source vocabulary.
- [0068](./0068-native-agent-session-restore.md) A launch records a bounded native session identity; restore rebuilds resume argv from the integration.
- [0067](./0067-cache-preserving-agent-fleet-context.md) Fleet context reaches models as sequenced tail deltas; static prompts never carry live values.
- [0046](./0046-server-side-agent-state-detection.md) The server derives agent state from title and screen; unmatched means `idle`, never `blocked`.
- [0042](./0042-launch-executor.md) `phux launch` spawns an integration template's argv through `SPAWN_RESOURCE`; no shell evaluation.
- [0040](./0040-agent-identity-metadata.md) Agent identity and lifecycle are one L3 record, `phux.agent/v1`, scoped to the Terminal.
- [0036](./0036-agent-asked-detection.md) The `phux-ask` title sentinel triggers `Asked`; hooks are the next authority, scraping the fallback.
- [0035](./0035-agent-asked-event.md) A blocked agent's question is an additive `AgentEvent::Asked` on the agent-event stream.
- [0022](./0022-tool-for-agents.md) phux is the terminal-capability substrate agents use; the CLI plus JSON schema is the contract.
- [0009](./0009-phux-vs-mux-positioning.md) phux is a substrate, not an agent-orchestration product; it absorbs no runner or workspace features.

## TUI conventions

- [0100](./0100-the-tui-is-its-own-crate.md) The TUI lives in `phux-tui`; `phux-client` is the headless library, dependency one-way.
- [0089](./0089-three-zone-attention-sidebar.md) The sidebar is a bounded attention inbox in three zones, projected client-side.
- [0079](./0079-fatal-signal-terminal-restore.md) An async-signal-safe handler restores the outer terminal after a fatal client signal.
- [0065](./0065-one-cli-grammar.md) `--socket` is one root-level global; alias parity, one `--split`, one JSON error shape.
- [0058](./0058-right-click-context-menus.md) Right-click opens anchored pane, window, or session menus committed through `run_action`.
- [0054](./0054-worktree-bound-sessions.md) `phux worktree` derives a session name from the worktree path; no stored state.
- [0049](./0049-client-local-focus-and-advisory-attention.md) Focus is client-local and never in shared layout metadata; agent attention is advisory.
- [0048](./0048-drag-to-resize-and-default-mouse-capture.md) The client captures outer-terminal mouse by default; divider drags commit through `SET_METADATA`.
- [0045](./0045-client-side-copy-mode.md) Copy-mode is a client-local projection over the pane's own engine, never a wire feature.
- [0029](./0029-one-cursor-authority-and-repaint-scheduler.md) One end-of-frame cursor emitter and one `RepaintLevel` accumulator drained per loop iteration.
- [0026](./0026-overlays-theme-stack-single-dispatch.md) Chrome and overlays share one theme, a real stack, and a single dispatch path.
- [0020](./0020-layered-render.md) ratatui renders chrome around holes that libghostty pane interiors fill.
- [0019](./0019-tui-multi-pane-rendering.md) The layout tree persists as a CBOR L3 blob; edits are client-computed `SET_METADATA` writes.
- [0017](./0017-tui-not-protocol-privileged.md) The reference TUI has no protocol standing; windows, panes, and focus are L3 conventions.
- [0012](./0012-binary-split-tree-layout.md) Window layout is a binary split tree with an open-interval `ratio`; n-ary is rejected.
- [0010](./0010-frontend-agnostic-tmux-cc-reserved.md) phux is TUI-first, non-TUI frontends are not precluded, tmux control mode is off the roadmap.

## Config

- [0101](./0101-the-settings-page-edits-the-file.md) The settings page edits one key per change in `config.toml`; running state never writes back.
- [0041](./0041-managed-plugin-installs.md) `phux plugin install` fetches into one managed directory with system tools and one `plugins.lock`.
- [0039](./0039-layered-config.md) Config is an ordered `extends` stack; arrays replace unless a key opts into `-append`.
- [0023](./0023-config-ux-philosophy.md) One TOML file over embedded defaults that are the live base layer; no imperative mutation.

## Release, CI, and docs

- [0099](./0099-ci-aggregate-gate-and-action-supply-chain.md) One `ci` aggregate context is the merge gate; every action is SHA-pinned; shared lane setup.
- [0082](./0082-retire-the-ci-metrics-store.md) The CI metrics branch, collector, and dashboard lane are gone; the run page suffices.
- [0074](./0074-self-update-trust-boundary.md) `phux update` verifies the checksum before unpacking, swaps atomically, and refuses foreign installs.
- [0069](./0069-generated-reference-docs.md) `docs/reference/` is rendered by the binary and byte-compared by a unit test.
- [0001](./0001-language-rust.md) phux is implemented in Rust.

## Proposed

Drafted and under review; none of these governs anything yet.

- [0092](./0092-durable-work-coordinator-authority.md) Durable objectives, runs, and evidence belong to a coordinator, not to any client.
- [0087](./0087-elastic-status-bar-space.md) The `spacer` widget is paid from the status row's leftover width, split evenly.
- [0078](./0078-alternate-screen-history.md) The server may harvest alternate-screen history by driving the application's own scrollback, opt-in.
- [0076](./0076-agent-prompt-and-lifecycle-wait.md) `agent prompt` submits through `APPLY_INPUT`; `agent wait` needs an observed transition.
- [0075](./0075-agent-name-addressing.md) `%name` selects exactly one agent Terminal client-side; ambiguity or a withdrawn name refuses.
- [0072](./0072-prune-policy-vocabulary-keep-the-seam.md) Prune the unreferenced policy vocabulary from `phux-protocol`; keep the HELLO authorization seam.
- [0071](./0071-what-phux-1-0-commits-to.md) 1.0 freezes the consumer surface under semver; the wire keeps its own `0.x` line.
