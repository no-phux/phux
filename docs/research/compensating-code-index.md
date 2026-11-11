# Compensating code: cluster index

Part of [Compensating code in phux](./compensating-code.md), research at
`ed579facc7ec`. One line per cluster: estimated lines, mostly non-test, then
where to start reading. Paths are abbreviated. Each section names its base
directory, and a path that does not start with a crate or top-level
directory is under that base.

## Server state, core, resource, tree

Base: `crates/phux-server/src/`. Other crates are named.

| Cluster | Lines | Start at |
| --- | --- | --- |
| Three identities per entity | 1,500 | `phux-server/src/id_bridge.rs`, `state/id_space.rs`, `org_wire.rs` |
| Process-scoped wire ids need incarnation tokens | 700 | `state/conditional_kill.rs`, `hub/operation_fence.rs`, `delegation.rs` |
| Legacy client model translated in the server | 1,100 | `state/legacy_snapshot.rs`, `state/layout_translation.rs` |
| Re-exec upgrade serializes memory across versions | 2,600 | `upgrade/blob.rs`, `state/upgrade_blob/` |
| Store, memory and engines as three truths | 900 | `state/org.rs`, `reconcile.rs`, `org_store/sqlite.rs` |
| A close has several asynchronous owners | 750 | `state/bindings.rs`, `state/reap.rs`, `state/resource_table.rs` |
| Detached tasks need generation fences | 450 | `state/lease_table.rs`, `phux-resource/src/lib.rs` |
| Hub: one link identity for all consumers | 1,300 | `state/satellite_spawns.rs`, `hub/metadata_mirror.rs` |
| Hub: one upstream stream fanned out | 2,400 | `hub/relay/` |
| One never-await state mutex | 500 | `state/mod.rs:281-372`, `state/events/subscription.rs` |
| File and OS truth is polled | 1,400 | `auth/reloading.rs`, `workload/reload.rs`, `autosave.rs` |
| Child supervision against OS semantics | 1,400 | `plugin_startup/reap.rs`, `plugin_startup/exit_watch.rs` |
| Platform and library workarounds | 900 | `daemon.rs`, `transport/webtransport/` |
| Inference from the kernel and old data | 900 | `phux-resource/src/proc_query.rs`, `cwd_query.rs` |
| Guards and ratchets | 1,100 | `phux-frozen/src/lib.rs`, `phux-resource/tests/substrate_boundary.rs` |
| Swallowed failures | 300 | 143 `let _ =` sites |

## Server runtime

Base: `crates/phux-server/src/runtime/`, with or without the leading `runtime/`.

| Cluster | Lines | Start at |
| --- | --- | --- |
| Output generation and gap resync | 2,000 | `runtime/pump.rs`, `apps/terminal/attach/output_pump_resync.rs` |
| Writer-side generation fence | 450 | `connections/writer/generation.rs` |
| Attach as a multi-actor transaction | 1,200 | `session_attach/`, `apps/terminal/attach/pump_loop.rs` |
| Many independent closers | 900 | `connections/pane_exit.rs`, `committed_close.rs` |
| Wire id versus core id | 150 sites | `resource_from_wire`, `intern_resource_wire` |
| Retry-safe commands | 1,400 | `operation_dedupe.rs`, `apps/terminal/input_lane/acknowledged.rs` |
| One current-thread runtime | 1,800 | `apps/terminal/input_lane/`, `command_tasks.rs` |
| Hub consumers as one identity | 1,000 | `commands/satellite.rs`, `commands/satellite_lease.rs` |
| `SET_METADATA` as a command channel | 900 | `connections/metadata_mutation.rs`, `connections/session_create.rs` |
| Store, registry and engines | 700 | `organization/mutate.rs`, `organization/effects.rs` |
| Engine facts copied into the registry | 450 | `apps/terminal/attach/target.rs`, `apps/terminal/agent_state.rs` |
| Hot upgrade by re-exec | 1,750 | `upgrade/validate.rs`, `upgrade/mod.rs` |
| Authority frozen at `HELLO` over edited files | 430 | `revocation.rs` |
| Paths and environment as identity | 450 | `connections/socket.rs`, `process_env.rs` |
| Timers that recheck on firing | 350 | `apps/terminal/commands/input_lease.rs`, `approvals.rs` |
| Supervising external processes by polling | 700 | `apps/terminal/standing.rs`, `service_http/opencode.rs` |
| Source-scanning guards | 725 | `apps/seam_ratchet_tests.rs`, `apps/seam_tests.rs` |
| Best-effort sends | spread | 126 `let _ =`, 28 `try_send`, 37 timeouts |

## Terminal, agent rules, agent session, agent host

Base: `crates/phux-terminal/src/`. Other crates are named.

| Cluster | Lines | Start at |
| --- | --- | --- |
| Terminal on loan during native capture | 450 | `phux-terminal/src/actor/canonical.rs`, `actor/native/finish.rs` |
| Native history cursors in three maps | 700 | `actor/native/cursors.rs`, `native_state.rs` |
| Resync after resize or gap | 350 | `actor/io/resize.rs`, `actor/run_loop.rs:123-287` |
| Reverse-rendering grid into VT | 1,300 | `grid/synthesizer*` |
| Dirty bits have one reader | 450 | `grid/synthesizer/incremental.rs`, `grid/reference.rs` |
| State-sync loss and stale-ack fences | 300 | `actor/run_loop_tick.rs`, `actor/consumers.rs` |
| Child lifetime observed by polling | 450 | `actor/io/lifecycle.rs`, `actor/hangup.rs` |
| Reader and writer share one file description | 150 | `actor/spawn/write.rs` |
| Parked and hot reader slots | 250 | `actor/park.rs`, `actor/park/hot.rs` |
| Three PTY write paths | 600 | `actor/fast_pty.rs`, `ordered_input.rs`, `input/mod.rs` |
| Agent identity by process sniffing | 450 | `agent_detect/identify.rs`, `agent_detect/identity.rs` |
| Agent state by screen scraping | 1,900 | `agent_detect/tick.rs`, `phux-agent-rules/rules/*.toml` |
| Several partial sources arbitrated | 700 | `phux-agent-session/src/state.rs`, `agent_detect/detector.rs` |
| Edge events over droppable sinks | 90 | `actor/events.rs` |
| Facts the engine does not announce | 250 | `actor/events.rs:243-378` |
| portable-pty mismatch | 1,100 | `portable-pty-adopt/src/lib.rs`, `actor/fd_shrink.rs`, `actor/spawn/exit.rs` |
| Kernel and tool workarounds | 500 | `actor/spawn/write.rs:173-258`, `actor/pane_scope.rs` |
| Fairness on one biased select | 300 | `actor/run_loop_service.rs` |
| Delivery across replacement and upgrade | 350 | `phux-agent-session/src/session/mod.rs`, `phux-agent-host/src/session/mod.rs` |

## Client core, runtime, client, FFI, dial, quictun

Base: `crates/`. Bare `control/`, `session/framed.rs`, `runtime.rs`, `engine.rs`, `publication.rs` and `reconnect.rs` are under `crates/phux-client-runtime/src/`; bare `session/` otherwise is under `crates/phux-client-core/src/`.

| Cluster | Lines | Start at |
| --- | --- | --- |
| Replica generations and resync by redial | 3,000 | `phux-client-core/src/session/bootstrap.rs`, `session/output.rs` |
| Attach barrier and cross-stream order | 600 | `session/attach.rs`, `phux-client-runtime/src/session/framed.rs` |
| Unknown delivery | 3,000 | `phux-client-core/src/input_replay.rs`, `org_ops.rs`, `control/input_delivery.rs` |
| Incarnation-scoped recycled ids | 500 | `control/responses.rs:35-120` |
| Connection epoch and reset lists | 400 | `control/lifecycle.rs:365-473` |
| Lossy events and unversioned reads | 4,000 | `control/roster.rs`, `control/topology.rs`, `phux-client/src/agent_wait.rs` |
| Reply-less writes need read-backs | 900 | `phux-client-core/src/rename.rs`, `phux-client/src/layout_ops/ops.rs` |
| Feature-bit dual paths | 4,500 | `phux-client/src/layout_ops.rs`, `phux-client-core/src/layout/legacy/` |
| Key space versus handle space | 1,200 | `phux-client-core/src/layout/wire.rs` |
| Three stream admission origins | 400 | `control.rs:239-259`, `control/commands.rs` |
| Predictive echo | 2,500 | `phux-client-core/src/predict/` |
| Re-deriving server-known facts | 2,500 | `phux-client/src/run.rs`, `agent_meta.rs`, `wait.rs` |
| libghostty ownership semantics | 1,500 | `phux-client-runtime/src/engine.rs`, `publication.rs` |
| Two socket lanes and two ABI encodings | 24,000 in `phux-client-ffi` | `phux-client-ffi/src/c/`, `phux-client-ffi/src/uniffi/` |
| Consumer contract | 700 | `runtime.rs:302-435`, `control/agent_delivery.rs` |
| Failure classification by string | 200 | `reconnect.rs:86-106` |
| Transport workarounds | 1,000 | `quictun/src/window.rs`, `quictun/src/forward.rs` |

## TUI and config

Base: `crates/phux-tui/src/`; bare `driver/` and `input_dispatch/` are under its `attach/`. Config crates are named.

| Cluster | Lines | Start at |
| --- | --- | --- |
| Many writers to one screen | 900 | `phux-tui/src/attach/render.rs:256-320`, `attach/paint.rs` |
| Repaint-level classification | 450 | `attach/repaint.rs`, `driver/loop_state/input/view.rs` |
| Paint pacing and reply inference | 500 | `attach/repaint.rs:78-247` |
| Undeclared host terminal | 650 | `attach/terminal_probe.rs`, `attach/notify/deliver.rs` |
| Restoration on every exit path | 600 | `attach/driver/terminal.rs` |
| Two geometries | 500 | `attach/reflow.rs`, `attach/driver/viewport.rs` |
| Arrangement: two generations and a read fence | 1,300 | `attach/arrangement_link.rs`, `driver/loop_state/arrangement.rs` |
| Attach bootstrap ordering | 450 | `driver/loop_state/bootstrap.rs`, `driver/native.rs` |
| Satellite orphans and strays | 1,000 | `attach/driver/orphans.rs` |
| Request and reply correlation | 500 | `attach/driver/subscriptions.rs` |
| Session switch rebuilds, then carries | 700 | `driver/loop_state/handover.rs`, `driver/loop_exit.rs` |
| Client replicas of server-wide state | 900 | `attach/driver/subscriptions.rs`, `driver/loop_state/peers.rs` |
| Version-skew branches | 400 | `driver/loop_state/mod.rs:365-385` |
| Chrome emitters and wide glyphs | 300 | `render/chrome/dividers.rs`, `render/chrome/status_bar/cells.rs` |
| Input disambiguation | 200 | `input_dispatch/dispatch/mouse.rs` |
| Config vocabulary duplicated | 900 | `phux-config/src/vocab.rs`, `theme_slots.rs` |
| Merge before the schema is known | 800 | `phux-config-core/src/layer.rs`, `phux-config/src/check/` |
| Config aliases and migrations | 250 | `phux-config/src/distro.rs`, `distro/switching.rs` |
| Config files with no owning process | 500 | `phux-config/src/toml_registry.rs` |
| Dev and production isolation by inference | 700 | `phux-config-core/src/instance.rs`, `socket.rs` |

## CLI, MCP, work store, mission, git, usage, plugin

Base: `crates/phux/src/`; bare `commands/` paths and bare verb files are under its `commands/`. Other crates are named.

| Cluster | Lines | Start at |
| --- | --- | --- |
| Server existence as a socket convention | 1,000 | `crates/phux/src/commands/server/spawn.rs`, `server/ensure.rs` |
| CLI and server replaced independently | 1,300 | `commands/upgrade.rs`, `commands/attach.rs` |
| Supervisor unit as a rendered copy | 1,100 | `commands/service.rs`, `commands/service/reconcile.rs` |
| Install-time artifacts not rewritten | 900 | `commands/agent/shim.rs`, `src/skill/setup.rs` |
| `doctor` | 2,000 | `commands/doctor.rs`, `commands/doctor/` |
| Work-store keeper spawned by first caller | 450 | `phux-work/src/client.rs`, `service.rs` |
| Mail take without an acknowledgement | 350 | `phux-work/src/coord/mail.rs`, `service/waiters.rs` |
| Task state projected into a second store | 850 | `commands/mission_project.rs` |
| `mission start` with no shared transaction | 350 | `commands/mission_start.rs` |
| Commands without replies | 1,200 | `commands/agent/record.rs`, `agent/start.rs`, `run.rs` |
| Four idempotency ledgers | 500 | `phux-mcp/src/operations.rs` |
| One capability, several surface definitions | 3,500 | `phux-mcp/tests/parity.rs`, `src/refdocs/` |
| Agent state inferred from outside | 1,500 | `commands/agent/detect.rs`, `agent/hook_payload.rs` |
| Vendor usage scraping | 1,900 | `phux-usage/src/` |
| Remote registry duplicates far-host truth | 900 | `commands/remote_target.rs`, `enroll/workload.rs` |
| Federation returns partial views | 400 | `commands/mod.rs:285-326`, `partial.rs` |
| Stored layouts not maintained | 250 | `commands/archive/snapshot.rs` |
| Child tools that hang | 1,000 | `phux-git/src/bounded.rs`, `commands/agent/tree/` |
| Dev and production on one machine | 200 | `src/lib.rs:263-270` |

## Rune, web, vt-web

Base: `clients/rune/src/`. Other clients are named.

| Cluster | Lines | Start at |
| --- | --- | --- |
| A dead connection rebuilds the window | 700 | `clients/rune/src/desk/link.rs`, `desk/link/replacement.rs` |
| Failures classified by substring | 150 | `desk/link/standing.rs` |
| Ids and receipts scoped to a connection | 600 | `desk/events/receipts.rs`, `desk/resources.rs` |
| Split as spawn plus read-modify-write | 800 | `desk/splits.rs`, `clients/phux-web/src/session/pane_replies.rs` |
| What to show re-derived after attach | 690 | `desk/home.rs`, `phux-web/src/session/attach.rs` |
| Two inventories | 200 | `desk/resources/satellite.rs` |
| Attention folded on the client | 700 | `activity.rs`, `alerts.rs`, `desk/fleet.rs` |
| No system text input | 1,800 | `desk/native_input/`, `ios/Sources/RuneInput.swift` |
| Phone stand-ins | 1,300 | `ui/phone/keyboard.rs`, `desk/phone/touch.rs` |
| Resize issued from layout | 210 | `desk/phone/refit.rs` |
| Pointer and key routing gaps | 350 | `desk/input/modal.rs`, `desk/input/selection.rs` |
| Rendering and resource gaps | 450 | `desk/resources/surface.rs`, `grid/glyphs.rs` |
| Unreleased upstream framework | 520 | `scripts/rune-upstream/tick.sh` |
| Local machine through the CLI | 2,000 | `launch.rs`, `machines.rs`, `onboarding.rs` |
| Three artifacts, one wire major | 900 | `updates/install.rs`, `desk/updates.rs` |
| Browser workarounds | 500 | `phux-web/src/client/keyboard.rs`, `client/connect.rs` |
| Hand-transcribed C ABI over wasm | 250 | `clients/phux-vt-web/src/lib.rs` |

## Integrations, plugins, examples, distros

Base: `integrations/`. Other top-level directories are named.

| Cluster | Lines | Start at |
| --- | --- | --- |
| Subprocess as API | 600 | `integrations/runtime/src/runner.ts`, `adapter.ts` |
| Re-validating CLI output | 1,150 | `runtime/src/schemas.ts`, `resources.ts` |
| Version and capability sniffing | 150 | `runtime/src/agent-session.ts`, `claude/scripts/phux-hook.sh` |
| Agent-record ownership per harness | 1,200 | `{pi,omp,opencode-v2}/src/lifecycle.ts` |
| Host event ordering and API drift | 650 | `omp/src/lifecycle.ts`, `opencode-v2/src/active-pane.ts` |
| Upgrade and keeper retry | 180 | `runtime/src/agent-session.ts:28-86` |
| Stateless shell hooks | 1,150 | `claude/scripts/phux-hook.sh`, `grok/scripts/phux-hook.sh` |
| Mail delivery per harness | 450 | `runtime/src/mail.ts`, `claude/scripts/phux-mail.sh` |
| Self-pane guard | 120 | `runtime/src/tool-support.ts:68-74`, `pi/src/tools.ts:213-223` |
| Target selection against stale ids | 500 | `pi/src/target-store.ts` |
| Client-side pane ownership | 210 | `opencode-v2/src/terminals.ts` |
| Fleet awareness versus prompt caching | 430 | `runtime/src/awareness.ts` |
| Transcript through `agent emit` | 470 | `runtime/src/transcript.ts` |
| GUI pane view from CLI snapshots | 760 | `opencode-v2/src/gui-service.ts`, `gui/terminal.ts` |
| Two tool catalogs | 1,200 | `pi/src/tools.ts`, `runtime/src/tools.ts` |
| Three packaging modes, committed output | 600 | `opencode-v2/index.js`, `runtime/dist/` |
| nvim focus and placement | 250 | `plugins/nvim/src/daemon.rs` |
| Shell glue around the CLI | 500 | `plugins/agent-tools/scripts/`, `examples/agents/lib.sh` |

## Tooling and CI

Base: the repository root.

| Cluster | Lines | Start at |
| --- | --- | --- |
| Several toolchain registries | 1,900 | `mise.toml`, `flake.nix`, `nix/toolchain-pins.nix`, `scripts/check-toolchain-sync.sh` |
| Nix devshell versus the Apple toolchain | 400 | `scripts/lib/apple-toolchain-env.sh` |
| Worktree lease | 420 | `scripts/lib/worktree-owner.sh` |
| Dev and test share production's address space | 400 | `scripts/test-env-runner.sh` |
| Wall-clock test budgets | 800 | `.config/nextest.toml`, `phux-server-testkit/src/builder.rs` |
| `#[ignore]` as lane selector | 260 | `scripts/check-e2e-lanes.sh`, `just/test.just` |
| Diff-routed CI | 1,300 | `tools/src/routing/`, `tools/src/lib/cargo.ts` |
| Cache and disk budget management | 800 | `.github/actions/setup-rust-lane/action.yml`, `scripts/ci/mbx-export-headroom.sh` |
| Committed generated artifacts | 900 | `scripts/generate-install.sh`, `scripts/build-vt-wasm.sh` |
| Prose docs restating code | 2,200 | `scripts/check-docs.sh`, `scripts/check-llms-txt.sh` |
| One version across many locks | 850 | `tools/src/cargo/release-orchestration.ts` |
| Release state by polling GitHub | 1,400 | `tools/src/release/` |
| Engine from a fork with a prebuilt download | 550 | `scripts/check-ghostty-pin.sh` |
| Boundaries enforced by text scans | 2,500 | `scripts/check-file-tokens.sh`, `crates/phux/tests/cli_no_raw_connections.rs` |
| Persisted formats are field names | 450 | `phux-frozen/src/lib.rs`, `scripts/check-frozen-fixtures.sh` |
| Wrappers around unreliable services | 750 | `scripts/npm-audit-gate.mjs` |
| Vendored brand assets | 500 | `scripts/brand.py` |

## Wire protocol

Base: `crates/phux-protocol/src/`. Other crates are named.

| Cluster | Lines | Start at |
| --- | --- | --- |
| Catalog restated about twelve times | 900 | `phux-protocol/src/wire/frame/mod.rs`, `kinds/rules.rs` |
| Ratchets over those lists | 2,000 | `kinds/samples.rs`, `tests/it/kinds_table.rs` |
| Schema reverse-engineered from samples | 1,950 | `wire/decode/wire_schema/` |
| One permanent bit per feature | 560 | `caps/server_features.rs` |
| Positional records under a tagged shell | 1,900 | `wire/info/snapshot_codec.rs`, `wire/info/extension.rs` |
| Window and group model on the wire | 500 | `wire/info/rows.rs`, `ids.rs:47-52` |
| Commands through the metadata store | 1,700 | `wire/frame/metadata_keys.rs`, `kinds/classifiers.rs:239-331` |
| Typed records as string-keyed JSON | 700 | `phux-agent-session/src/agent_record.rs` |
| Strict and tolerant JSON from one type | 150 | `native_agent/strict.rs` |
| Unknown-value policy per enum | 2,000 | `wire/decode/correlation.rs`, `wire/unreadable.rs` |
| Identity and ordering fences | 150 | `wire/field/connection.rs`, `ids/resource.rs` |
| Three id spaces | 1,000 | `phux-server/src/id_bridge.rs` |
| Dead wire surface still decoded | 500 | `kinds/resource_catalog.rs:246-250`, retired tags |
| Several generations of one operation | over 1,000 | `wire/frame/command_tags.rs` |
| Each kind declared in eight places | 300 | `caps/bootstrap.rs`, `caps/resource_kinds.rs` |
