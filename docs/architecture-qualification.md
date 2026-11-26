---
group: internals
order: 9
---

# Architecture qualification

Required proof for the [architecture end state](./architecture-end-state.md), **not shipping behavior**. “Existing” means inspected source/tests; none ran here. Missing runners are required coverage. Use isolated servers/stores, never production. Source-name/length/default tests, structural/generated agreement and compilation cannot prove outcomes.

## Evidence and current execution routes

Each family needs a selectable scenario, stimulus/failure cut, expected observation and result. Retain commit **and tree** (dirty-source digest when applicable), locks/engine revision, features/profile/target, compiler/Zig/SDK, OS/hardware, peer versions, artifact/binding hashes, command/filter/repetitions. Record omissions; skipped platforms/zero selected cases and parent receipts cannot qualify changed artifacts.

Crash/store evidence: durable records/receipts/journal before/after, affected set, process attempts, live handles and notifications. Client evidence: actions/outcomes, generations and visible results. Frozen inputs/independent expectations cannot come from the implementation under test. Missing receipt never proves nonexecution.

Commands below do not claim complete coverage; observed runs are in [evidence](./architecture-qualification-evidence.md). Recipes in `just/`, [development](./development.md) and [CI](./ci.md) define operational details.

| Surface | Current route; limit |
| --- | --- |
| Root builds | `just build`, `just build-lean`, `just build-release`: locked `phux` + `phux-mcp`; lean disables defaults. Compilation is not behavior. |
| Root tests | `just test`: workspace nextest, `--features phux-server/measure`. `just e2e`: ignored server/PTY cases, serial/retried. `just test-archive`: `target/nextest/phux-tests.tar.zst`; preserve source identity. `just test-cargo` omits ignored/feature-gated cases. |
| Narrow | `cargo nextest run --locked -p phux-server --lib state::org::lifecycle`; substitute `runtime::organization`, `runtime::revocation_conformance`, `hub::relay`, `runtime::upgrade`, `upgrade::blob::golden`. Paste: `cargo nextest run --locked -p phux-client-runtime control_plane::paste`. |
| Binding | `cargo test --locked -p phux-client-ffi --no-default-features --features uniffi --lib` (`binding-uniffi-test`, `just/gates.just`); not foreign-app proof. |
| Browser | `just web-vt-check`: Node ABI/VT wasm tests; `PHUX_WEB_ENGINE=1` adds pinned engine reproduction. `just web-client-check`: wasm tests, shipping `wasm-pack build --target web --release --out-dir pkg --locked`, then Chrome via `bun tools/main.ts web-browser`. `just web-check` runs both; Node is not paint proof. |
| Rune | `just rune-check`: separate locked workspace/all-feature tests plus driver tests (`clients/rune/scripts/check.sh`); root never reaches it. `just rune-gallery /absolute/scratch/gallery` paints, not OS interaction. |
| Packaged Rune | `bash clients/rune/scripts/qualify.sh ARCHIVE PHUX EVIDENCE_DIR`: same-source isolated server/archive/headless frames, available Linux X11/Wayland branches. Name branches executed. |
| Perf | `just e2e`: server `measure`; `just stress`: `phux-server/stress,phux-terminal/stress`, allocation tests. `just perf-icount`: x86_64/Nix. `just perf-echo`, `just perf-rune-echo`, macOS `just perf-rune-photon`: distinct latency paths; photon needs native permissions/unlocked/uncovered display. |

Independent build shapes: default/lean; headless MCP without TUI/VT; `phux-client` + `phux-mcp` with `--all-targets --no-default-features --features phux-client/testkit`; `phux --no-default-features --bin phux`; `phux-tui --all-targets --no-default-features`; runtime engine/headless no-default; UniFFI above; wasm; Rune/iOS. Workspace feature union cannot substitute. `bun tools/main.ts build-features-check` (`tools/src/cargo/build-features.ts`) inspects production graphs, including engine-free server/terminal-interface/config, not behavior.

## Twelve operation scenarios

Source prefixes: **S** = `crates/phux-server/src/`, **ST** = `crates/phux-server/tests/`, **R** = `crates/phux-client-runtime/`, **C** = `crates/phux/src/commands/`, **CT** = `crates/phux/tests/`, **B** = `clients/phux-web/`, **N** = `clients/rune/`. Paths below exist; no unified cross-client twelve-operation harness is claimed. Its adapter corpus is required coverage. Unsupported surfaces refuse without hidden work.

| Intent | Required observation/cut; current seam |
| --- | --- |
| 1. Create | Durable admission before engine, not observed start; store refusal/lost reply. `S/state/org/operation.rs`, `R/src/control/commands/spawn.rs`. |
| 2. Ensure-session | Exact existing/created receipt, no reseed, replay after name reuse, storage refusal and revoked/disconnected authority. `S/runtime/organization/tests/create.rs`, `S/state/org/tests.rs`, core session tests. |
| 3. Select-view | Local focus/scroll/search, not membership/execution; two views. `N/src/desk/splits.rs`, `B/tests/session/splits.rs`, `R/src/engine/owner/views.rs`. |
| 4. Attach | Authorized exact generation; staging invisible until ready; disconnect/late chunks. `R/tests/it/control_plane/views.rs`, `S/runtime/apps/terminal/attach/tests/publication_recovery.rs`. |
| 5. Raw input | Bounded latency/order/local queue result under saturation/reconnect; not all keys receipted. `R/src/control/input.rs`, `ST/terminal/input.rs`. |
| 6. Acknowledged paste | Original atomic payload/id survives lost reply; shared acknowledged/raw choice. `R/tests/it/control_plane/paste.rs`, `C/paste.rs`. |
| 7. Close-view | Retire view/callback/interest, not execution. Runtime views above; `B/src/session/pane_replies.rs` currently couples failed split to legacy kill. Compensation must explicitly cancel exact created activation. |
| 8. Terminate | Admit exact activation/revision before end; replay cannot signal replacement. `S/runtime/organization/tests/session.rs`. |
| 9. Purge | Full descendant set, child-first close, retained removal during outage. `S/state/org/activation.rs`, `ST/lifecycle/agent_session/family.rs`. |
| 10. Session-kill | Explicit override; persist set before close/mirror/announce; replay closes once. `S/state/org/operation.rs`, `S/runtime/organization/tests/session.rs`. |
| 11. Restore | Import inert; disabled/changed integration requires current trust/raw approval. `C/archive.rs`, `CT/automation_e2e/workspace_archive_e2e.rs`. |
| 12. Relaunch | Fresh admission/exact old activation; ambiguity cannot grant retry. `S/state/org/operation.rs`, `S/reconcile.rs`, `ST/lifecycle/crash_restart.rs`. |

Parity compares **intent plus facts**: interactive paste across TUI/Rune/FFI/browser; supported create/ensure through CLI/MCP/headless/native; native route/trust decisions; view closure. Intentionally distinct: raw/acknowledged, user/automation paste, create/ensure, select/attach, close-view/terminate/purge, restore/relaunch, hot handoff/cold restart. Gestures, painting, browser origin and credential acquisition stay platform-owned. No universal event bus or forced native-runtime wasm driver. Work/organization retain separate authorities; wakeups trigger exact level reads, not global transactions/fabricated success.

## Acceptance families

### 1. Atomic admission

**Existing:** `S/runtime/organization/tests/session.rs`: refused-store kill closes nothing; fenced terminate after commit. `S/state/org/operation.rs`: private serialized authority/choreography owner. `ST/lifecycle/agent_session/family.rs`: children; core `crates/phux-core/tests/domain/commands/organization.rs`: atomic moves/revisions. Route: organization/root/e2e.

**Required:** locked descendants, overlapping parent/child/duplicate targets, session-kill override, stale activation/revision and changed affected set. Refuse stores before each irreversible effect: no partial close/receipt/memory/publication. Acceptance proves exact set, child-first close, receipt/journal order, mirror/effect transfer. Replay/concurrency cannot widen sets or repeat effects. Operation ownership hides choreography without weakening assertions.

### 2. Execution truth

**Existing:** `S/state/org/lifecycle/tests.rs`: SQLite evidence refusal/duplicates/fences; `S/runtime/upgrade/tests/pending_evidence.rs`: handoff. `ST/lifecycle/crash_restart.rs`: lost/unobserved/no relaunch. `S/reconcile.rs` still maps Starting without identity to `NeverLaunched`, not proof of nonexecution. Route: lifecycle/upgrade/e2e.

**Required:** crash after admission/reservation/construction/OS attempt before start persistence; refuse each start/end/failure commit, including generic failures. Probe unreadable/reused PID/boot, duplicates/reordering. Never-attempted needs sufficient durable evidence; otherwise unknown survives server/JSON/bindings/recording/keeper. No invented exit zero. Retry evidence, never execution; absent receipt/identity grants no relaunch. Handoff flushes/quiesces or refuses. Retain attempt counts/outcomes per cut.

### 3. Recoverable retirement

**Existing:** `S/state/org/activation.rs::retire_record` attempts removal after end and logs refusal; lifecycle evidence retries do not prove removal recovery. Session-kill suppresses already-durable retirement. Route: focused lifecycle/organization and parent/child tests.

**Required:** outage after live teardown/before removal, repeated refusal, restart with pending removal and replacement activation before recovery. Assert inspectable pending retirement or durable settlement, never warning-only loss or unexplained live/durable divergence. Recovery cannot remove a replacement or repeat teardown. Retained intent, retry ownership, diagnostics and complete outage scenarios require implementation; they are not established by existing report tests.

### 4. Authority

**Existing:** `S/runtime/revocation_conformance/in_flight.rs`: revoked frames, replay/hub teardown, surviving human session; `S/policy/tests/holds.rs`, `S/transport/tests/revocation/credentials.rs`: hold/credential cuts. Route: revocation conformance/root.

**Required:** revoke/narrow while held/staged/queued, between resolution and admission, and before callback installation. Serving host rechecks current grant at execution admission; client preflight is not authorization. Late work installs no consumer/lease/attachment. Verify exact subscription teardown while existing execution survives revocation unless separately terminated by an authorized operation. Socket closure alone is insufficient evidence.

### 5. Federation

**Existing:** `S/hub/relay/tests/capabilities.rs` and `events.rs`: bits/incarnation/pending-spawn teardown; `S/runtime/commands/kill_merge_tests.rs`: per-host versus legacy results; `ST/federation/hub_relay_federation/conditional_kill.rs`: remote fence. Route: relay/federation.

**Required:** disconnect without detach, whole-client/link teardown, peer restart reusing numeric IDs/lower lease sequences. Epoch retirement removes proxy registrations, leases, negotiated capabilities/catalog, cursors and pending requests; late old callbacks cannot reinstall them. Configured host survives, negotiated facts do not. Assert independent per-host results without global transaction; retain old-peer behavior. Incarnation tests alone do not prove these complete link-epoch cuts.

### 6. Input

**Existing:** `R/tests/it/control_plane/paste.rs`: trust/size/order/satellite. TUI `crates/phux-tui/src/attach/input_dispatch/tests_events/input_delivery_prediction.rs`; `S/runtime/apps/terminal/input_lane/tests/acknowledged.rs`; `S/runtime/connections/commands/input_receipt_capacity_tests.rs`. Route: paste/binding/browser/Rune/e2e.

**Required:** small/large user paste and unsafe automation across local/satellite/old peers and equivalent callers. Compare chosen path, atomic payload and order. Saturate queue/receipts, lose reply after write, reconnect same/replacement incarnation and expire dedupe window. Preserve original identity; distinguish unavailable/refused, locally queued, acknowledged and unknown delivery. Unknown never permits fresh resubmission. Raw keys keep direct bounded delivery, not universal receipts; PTY flush is not child consumption.

### 7. Presentation

**Existing:** `R/src/engine/tests/views/presentation.rs`: readers/views/destruction; `N/src/desk/frame.rs`: exact drawn receipt. FFI `crates/phux-client-ffi/src/uniffi/wire/client.rs::render_projection` acknowledges acquisition, a mismatch. Route: engine/binding/browser/native.

**Required:** hidden/clipped frames never clear uncertainty; stale connection/resource/stream/frame/fence receipts are rejected. Race old paint against newer uncertainty, reconnect and callback teardown. Two views preserve local preferences while respecting terminal-wide uncertainty. Only visible/unclipped presentation of the exact publication releases its fence; projection acquisition/build does not. Add actual FFI/browser/native negative cases. Uncorrelated screenshots do not prove receipts.

### 8. Client parity

**Existing:** `R/tests/it/control_plane/{spawns,views,paste}.rs`, `N/src/desk/resources/tests/receipts.rs`, `B/tests/session/splits.rs`: separate consumers, not unified parity. Binding sentinel and browser cleanup remain mismatches. Route: root CLI/MCP/runtime/TUI, binding/browser/Rune/native.

**Required:** equal typed intent, facts/capabilities/time and outcome sequence through CLI/MCP/headless/TUI/Rune/FFI/browser where supported. Compare actual decisions/canonical results, then representative transports/renderers. “Unavailable” cannot hide queued creation. Failed split leaves original usable and cannot incidentally kill another view/resource activation. Preserve local gestures/preferences and peer fallbacks. Wire the twelve-operation corpus and delete replaced policy paths; linking a shared library is insufficient.

### 9. Trust

**Existing:** `R/src/dial/tests.rs`: pin/token/relay naming; `C/archive/model/tests.rs`, `CT/automation_e2e/workspace_archive_e2e.rs`: archives, not new restore policy. [Organization](./organization.md) describes current distinctions. Route: dial/archive/e2e/native enrollment.

**Required:** equal native route/credential facts yield equal planner verdicts. Missing/revoked credentials, route changes and cancellation cannot disclose secrets. Import is inert; restore/relaunch needs current trusted template or exact raw-recipe approval. Conversation readiness is not trust. Test disabled/changed integration, rejection/approval, recipe mutation invalidating approval and admission after restart. This policy change needs product/security acceptance, not a behavior-preserving-refactor claim.

### 10. Format evolution

**Existing:** protocol `crates/phux-protocol/tests/it/wire_contract/{newer_server,tail_extension}.rs`, `ST/protocol/newer_client*.rs`, `crates/phux-frozen/src/lib.rs`, `S/upgrade/blob/golden_tests.rs`. Keeper `crates/phux-work/src/start/binding.rs` still stores public `ExecutionRef`; sampled wire schema is no independent nested oracle. Route: protocol/server/keeper/upgrade.

**Required:** public DTO addition changes no keeper stored bytes/values; keeper owns its execution pin/frozen reader. Test independent released old/new client/server/hub pairs, populated frozen values, malformed/truncated bytes, unknown fields/tags/bits/nested records. Unsupported request differs from unreadable reply/unknown outcome; unreadable success never becomes refusal/retry. Hot-upgrade engine rebinding differs from cold reconciliation. Preserve distinct core/wire IDs with exhaustive correspondence.

Wire: `PHUX_UPDATE_WIRE_SCHEMA=1 cargo nextest run -p phux-protocol wire_schema_matches` updates `docs/spec/wire-schema.json`; type new fields. `just docs-gen`: `docs/reference/` and `docs/spec/{config,plugin}.schema.json`. Public schemas: `crates/phux-protocol/src/api/schemas.rs`; MCP: `crates/phux-mcp/tests/golden/`. Independent frozen/peer oracles must check nested grammar/refusals/values. Fixtures stay append-only ([compatibility](./compatibility.md#persisted-formats)); populate new fields without erasing old expectations.

### 11. Native performance and platform qualification

**Existing:** `ST/perf/main.rs`: PTY settle, not photons; `crates/phux-terminal/tests/perf_bursty_output.rs`: synthesis allocations/damage; `crates/phux-terminal/benches/capture.rs`: fanout. `just/perf.just`: echo/headless/photon. Not blanket native proof.

**Required:** compare optimized before/after artifacts on representative hardware. Record corpus/grid/views/clients, repetitions/warmup, p50/p95/p99, allocations/copies/hops and queue/memory/backpressure. Predeclare budgets for flooding, stalled readers, many views, resize/bootstrap/history and sustained input. Preserve libghostty owner-thread ordering/borrowed batching without added hot-path allocation/copy/hops. Compilation/idle frames/coarse settle cannot replace latency/allocation proof.

Current artifact/surface routes:

```sh
bash packaging/build-mobile-ffi-xcframework.sh --full --out /absolute/scratch/apple-ffi
bash packaging/build-mobile-ffi-android.sh --out /absolute/scratch/android-ffi
python3 clients/rune/ios/build.py --target simulator --out /absolute/scratch/ios-sim
python3 clients/rune/ios/build.py --target device --out /absolute/scratch/ios-device
cargo build --locked -p phux
python3 clients/rune/ios/qualify.py --app '/absolute/scratch/ios-sim/Phux Rune.app' \
  --phux "$PWD/target/debug/phux" --out /absolute/scratch/ios-proof
python3 clients/rune/ios/qualify.py --app '/absolute/scratch/ios-sim/Phux Rune.app' \
  --phux "$PWD/target/debug/phux" --transport wss --out /absolute/scratch/ios-wss-proof
```

FFI outputs: matching `Generated/PhuxFFI.swift`/`PhuxFFI.xcframework`, or Kotlin/`jniLibs/{arm64-v8a,x86_64}/libphux_client_ffi.so`, plus design/provenance/hashes. Apple smoke only calls `bridgeReady()`; Android cross-build is not interaction proof. Foreign-language scenarios must execute exact artifacts, outcomes/presentation/callback retirement included. Rune UIKit/Metal is not UniFFI proof.

Rune iOS owns simulator/same-source server; executes paired QUIC/WSS input, presented-grid marker, background/resume and saved-credential reconnect. Retain reports/screenshots and `.github/workflows/rune-build.yml`'s `qualification-ios-native` artifacts/source identity. Driver tests/bundles/IPAs/install pages are not device proof. Missing hardware/window/foreign-app cases remain required; device/security actions need user authorization.

Current `.github/workflows/rune-macos.yml` standalone PR green deliberately covers **no Apple execution**. Called qualification covers merge groups/applicable release PRs; push/nightly/dispatch is separate. Direct main can integrate before native failure blocks release. Destination: **applicable same-source native/client qualification before integration**. Skips/pool scarcity/parent receipts/device compilation cannot waive it.

### 12. Change-locality rehearsals

**Existing:** structural guards prove no semantic locality; no completed corpus claimed. Seams: `S/runtime/organization`, `crates/phux-terminal-api`, `crates/phux-resource`, runtime input, protocol and keeper.

**Required:** rehearse new kind, input policy, refusal, value-preserving stored evolution and equivalent workflow independently. Retain patches, owner rationale, consumer/projection changes and behavior/compatibility results. One rule/owner; only real projections/adapters change elsewhere. No unrelated client policy edits, universal bus or whole-state facade. Exercise failures/delete replaced paths; metrics cannot pass.

## Integration verdict

All twelve families and applicable supported operation/client/platform combinations need exact-source/artifact evidence, independent compatibility/frozen-value proof, native budgets and locality rehearsals before integration. Missing runners/cases remain required implementation, never borrowed passes. Shipping claims change only after implementation and qualification.
