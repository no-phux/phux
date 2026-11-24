---
group: internals
order: 0.5
---

# Architecture end state

**Accepted implementation target for phux-33bjb, not shipped behavior.** Wave 0
specifies the destination; waves 1–7 materialize it. Current contracts remain in
[architecture](./architecture.md), [decisions](./decisions.md),
[organization](./organization.md), [organization store](./organization-store.md),
[embedding](./embedding.md) and [scripting](./scripting.md). Deliberate changes are
labelled below. [Qualification](./architecture-qualification.md) supplies the
required consumer-visible proof.

The destination is host-authoritative resources, operation-owned transitions,
truthful evidence, generation-owned live state, portable client decisions and
platform-owned execution/presentation. Wire, JSON, binding and storage are
independent projections; bounded native terminal data stays direct.

Logical modules, not one crate/thread/actor per noun. Retain correct code; delete
replaced policy. Counts are leads, not defects.

## Constraints that remain true

- Serving host authorizes under current policy; client admission is usability.
  Connection revocation does not itself terminate existing execution.
- Database commit cannot atomically commit arbitrary OS effects. Missing evidence
  stays unknown; missing receipts/process identity never authorize another launch.
- Durable keys, allocation instances, activations, host incarnations, connection/
  link epochs, stream/bootstrap generations and views are different lifetimes.
- Preserve exact atomic affected-set/current-grant checks, child-first closure,
  arrangement locks/session-kill override/protected Intake, commit-before-effects/
  publication, bounded receipts, stale-writer fencing, hot/cold restart distinction
  and pure domain/engine seams. Preserve session-kill's proven exceptional ordering.
- One transaction authority can serialize cross-table facts without separate locks.
  libghostty's `!Send` state stays engine-thread-owned: no avoidable allocations,
  copies, hops or round trips. Preserve batching/borrowing and backpressure.
- Clients vary scheduling/painting, not equivalent intent policy. Work/organization
  retain separate authorities: no global transaction or universal event bus.
- Independent releases, wire 1.x compatibility, frozen stored/upgrade formats
  remain obligations, not waived by a future major.

## Operation meanings

Normative destination meanings, not proposed runtime types/wire commands.
“Commit” includes transition, retained receipt and journal position before effects/
publication. Exact execution means authority/resource key/activation, plus applicable
live incarnation/allocation fences. Receipts are bounded, not perpetual exactly-once.

| Meaning | Authority | Exact identity and lifetime | Linearization point | Observable outcome and uncertainty |
| --- | --- | --- | --- | --- |
| **create** | Host current create/organize and kind admission | New durable resource, placement, operation id and initial activation | Membership/placement/launch-intent commit before engine construction | Committed intent is not start/success. Failure stays inspectable; lost reply requires retained receipt/state reconciliation, not another create. |
| **ensure-session** | Host resolves existence; shared client intent | Authority-scoped selector resolves durable session key; creation retains operation id | Serialized existing-session resolution or atomic create commit | Report existing/created and exact key; resource launch is a separate stage. Never queue creation while reporting unavailable. |
| **select-view** | Local view owner | View id and current resource/stream generation; view lifetime | Matching live view's local selection update | No membership, attach or execution change; stale view is unavailable, not implicit create/kill. |
| **attach** | Host current watch/stream grant | Exact session/resource plus connection, attachment and stream/bootstrap generation | Installation of admitted generation-owned interest | Admission is not bootstrap/presentation. Disconnect retires interest, not execution/history; stale completions cannot reinstall it. |
| **raw-input** | Host current input grant/lease; client readiness/order gate | Live execution, incarnation, attachment/input generation; no durable receipt | Local queue acceptance only; host/PTY effect is separate | Refused/unavailable versus queued; queue is not admission/delivery/read/success. Loss may leave unknown delivery: never blindly resend. |
| **acknowledged-paste** | Host input grant/lease/trust; portable delivery selection | Original input operation scoped to server incarnation/execution; terminal ordering fence | Server acknowledged-input admission and subsequent delivery settlement, not client enqueue | Refused/not applied, acknowledged delivery and unknown differ; reconcile original identity. User/untrusted, size and peer/satellite fallback remain explicit; raw cannot overtake unresolved input. |
| **close-view** | Local view and subscription owners | Exact view and its generation-owned stream interest | View retirement and fenced interest release | Retire callbacks/interest, not execution. Failed-create compensation must explicitly cancel the exact created activation. |
| **terminate** | Host current grant and exact affected set | Exact execution, applicable live handle/incarnation/instance fences | Durable termination admission before cancellation/effect handoff | Accepted is not observed end. Reconcile lost reply; never retarget replacement activation. Plan descendants and close child-first. |
| **purge** | Host removal authority/lifecycle rules | Durable resource and required activation/revision fences, bound descendants | Durable removal/retirement commit before publication | Refuse unsafe removal of possibly live execution. After teardown retain pending retirement until settled; preserve retained outcomes until permitted purge/expiry. |
| **session-kill** | Host session authority; lock override, protected Intake | Durable session, operation id, atomically resolved members/descendants | Durable commit under affected-set serialization before teardown | Commit precedes child-first close/suppression/mirror/effect/publication. Replay never recloses; no partial host-local admission. Cross-host outcomes remain independent. |
| **restore** | Import inert; host current execution authorization | Archive provenance; new exact creation/activation, never old PTY | Import commit distinct from authorized execution-intent commit | Import/readiness/admission/execution differ. Current trusted integration or exact raw-recipe approval; readiness failure does not prove nonexecution. |
| **relaunch** | Host current launch/lifecycle policy | Existing durable resource, expected activation/revision, new operation/activation | Authorized new-intent commit after old execution eligible and conflicting engine released | No automatic ambiguous retry/reused-PID adoption. Intent differs from start/end; resume tokens are data, not executable authority. |

Only an exact, visible, unclipped presentation receipt may clear matching input
uncertainty: connection epoch, resource/stream generation, frame and delivery
fence must still match. Acquisition/build/paint is not presentation; an old receipt
cannot clear a newer fence. The uncertainty remains terminal-wide even when a
particular view presents it.

## Criticism ledger and required disposition

**Source-proven** means the linked source/docs establish a structure or behavior,
not that an adversarial scenario was executed. **Inferred** names a risk requiring
qualification, not a demonstrated exploit. **Policy** identifies a deliberate
behavioral decision, not an accidental implementation cleanup. Earlier diagnostic
SCC/count reports are investigation leads; they are not current measured facts.

1. **Physical decomposition is not semantic decomposition.** Source-proven:
   [`RemoteAttach`](../crates/phux/src/commands/remote_target.rs) carries recording
   specification across workflows, not proof of a defect/current SCC. Required:
   cohesive clusters, directed/justified cross-owner contracts; private cycles
   may remain. Reduce caller knowledge, not file size.
2. **Storage ownership is not operation ownership.** Source-proven:
   [`contexts`](../crates/phux-server/src/runtime/apps/context.rs) expose shared
   state; [`session_kill`](../crates/phux-server/src/runtime/organization/mutate.rs)
   coordinates persist/suppress/close/mirror/effect/announce. Preserve correctness
   while making full admitted workflows private: current grant/exact set, commit,
   effects/publication; no routine raw-table or prepared-state access.
3. **Logging is not a closed failure protocol.** Source-proven: retries in
   [`lifecycle.rs`](../crates/phux-server/src/state/org/lifecycle.rs) differ from
   warning-only refused failure/removal commits in
   [`effects.rs`](../crates/phux-server/src/runtime/organization/effects.rs) and
   [`activation.rs`](../crates/phux-server/src/state/org/activation.rs).
   Inferred outage risk: stranded intent. Required: one exact-activation evidence
   ingress, recoverable inspectable retirement; retry commits, never execution.
4. **Missing evidence is not nonexecution.** Source-proven:
   [`spawn.rs`](../crates/phux-server/src/runtime/apps/terminal/commands/spawn.rs)
   builds before start reporting; [`reconcile.rs`](../crates/phux-server/src/reconcile.rs)
   maps Starting without identity to NeverLaunched. Inferred crash-window risk:
   overstated certainty. Required across server/client/recording/keeper: distinguish
   intent, provably unattempted, known attempt, observed end and unknown. Never
   invent exit zero or relaunch permission.
5. **Numeric identity is not lifetime.** Source-proven:
   [`federation.rs`](../crates/phux-server/src/state/federation.rs) proxy/lease keys
   lack link epoch. Inferred replacement/reused-id/late-work risk, not demonstrated
   bypass. Required: epoch-owned incarnation, capabilities/catalog, proxies, leases,
   requests/cursors; teardown retires all, late work cannot reinstall them.
6. **Shared libraries do not guarantee shared decisions.** Source-proven:
   [`input.rs`](../crates/phux-client-runtime/src/control/input.rs) chooses paste
   delivery/order; FFI [`client.rs`](../crates/phux-client-ffi/src/uniffi/wire/client.rs)
   ensure/select/spawns locally, returning zero after queued creation. Required:
   portable typed identity/order/capability/fallback/outcomes. Keep create/ensure,
   view/terminate, raw/acknowledged and user/untrusted distinct; linkage is not parity.
7. **Queue/admission/delivery/presentation differ.** Source-proven: FFI
   `render_projection` acknowledges acquisition/build, contrary to
   [embedding](./embedding.md#frames-wakes-and-input). Required: explicit
   refused/unavailable/queued/acknowledged/unknown and exact presentation receipts.
   No discarded results/acquisition acknowledgment; preserve Rune's correct contract.
8. **View failure is not resource death.** Source-proven: browser
   [`pane_refusal`](../clients/phux-web/src/session/pane_replies.rs) legacy-kills an
   unpresented split. Required: separate execution/interest/view, explicit exact
   activation cancellation for failed-create compensation, not incidental kill.
   Preserve runtime's separation.
9. **Portable semantics must not own scheduling.** Source-proven:
   [`stepped.rs`](../crates/phux-client-runtime/src/control/frames/stepped.rs) owns
   connection locking/engine round trip. Required: facts/clocks → bounded semantic
   work; drivers own I/O, locks, time/backoff, handoff/callback retirement.
   Fence completions; preserve batching, event loops and `!Send`; no universal bus.
10. **Representations have independent contracts.** Source-proven: wire
    [`Refusal`](../crates/phux-protocol/src/wire/frame/org/refusal.rs) depends on
    public error projection; keeper
    [`binding.rs`](../crates/phux-work/src/start/binding.rs) stores public
    `ExecutionRef`. Required: lower pure validation, separate host filesystem/
    authority checks, outward conversions, keeper pin/frozen storage; JSON must
    not migrate storage. Preserve independent core/protocol and distinct-ID proof.
11. **Archives are not perpetual execution authorization.** Source-proven:
    [organization](./organization.md#restore-and-executable-authority) documents
    trusted-integration terminal resume versus ACP archived-recipe execution.
    **Policy change:** new execution requires current trusted-template admission
    or exact raw-recipe approval. This accepted product/security target does not
    allege current unauthorized execution. Import/readiness/trust stay separate;
    cutover explicitly updates user docs and acceptance evidence.
12. **Temporal state needs cohesive owners, not field wrappers.** Source-proven:
    [`TerminalActor`](../crates/phux-terminal/src/actor/mod.rs) combines terminal/
    process/bootstrap/input/consumer facts;
    [`PeerWatch`](../crates/phux-tui/src/attach/driver/subscriptions.rs) combines
    watches/presentation. Concentration is not proof each field needs an actor.
    Required: private bootstrap/publication, process, input-ordering, watch and
    view-preference owners; efficient shared ordering/data, no invalid production
    optional states solely for tests.
13. **Structural/generated agreement is not behavioral proof.** Source-proven:
    [`wire_schema.rs`](../crates/phux-protocol/src/wire/decode/wire_schema.rs) derives
    top-level metadata/presence from tables/samples;
    [CI](./ci.md#native-apple-qualification) names green checks without Apple work.
    Required: independent nested/frozen/peer oracles, exact source/artifact/platform/
    scenario provenance. Structural/file-growth gates supplement behavior; actual
    native proof precedes integration, not optional capacity follow-up.


## Ten logical owners

| Owner | Responsibility and seam |
| --- | --- |
| 1. Domain transitions | Pure membership/arrangement/lifecycle, durable identities and refusals; existing core. |
| 2. Host operation authority | Current subject/grant, affected set, transaction/receipt/journal, mirror/effect/publication; private workflows, no mega-dispatcher. |
| 3. Execution lifecycle | Reservations, observed evidence/backlog, retirement, reconciliation, upgrade quiescence; engine facts, durable host interpretation. |
| 4. Live generations | Connection/attachment/stream/bootstrap/link facts, cancellation and stale-result rejection. |
| 5. Resource kinds/terminal engine | Existing kind contracts; engine owns PTY/libghostty/encoding/native capture; explicit host assembly. |
| 6. Portable client semantics | Shared session/intent decisions, explicit facts/clocks/identity and typed outcomes; extend existing kernels/policies. |
| 7. Platform drivers/views | I/O/locks/scheduling, gestures/presentation/search/selection/scroll, teardown/callbacks. Shared native trust planning; platform credentials/origin constraints. |
| 8. Contract projections | Wire/JSON/binding/storage owners and outward conversions; compatibility adapters, not competing semantic authority. |
| 9. Work authority | Keeper transactions/projection; events wake exact level reads; no global transaction or invented work success. |
| 10. Qualification | Scenarios, independent oracles, same-source native/foreign provenance, performance/backpressure and change-locality proof. |

## Backward prerequisites

One intent must survive entry point, disconnect, crash, peer replacement and
presentation failure. Working backward:

1. Policy-free adapters require shared typed intents.
2. Intent parity requires canonical outcomes, identities, trust and capabilities.
3. Truth requires separate execution/presentation versus acceptance/queue evidence.
4. Evidence requires generation-owned callbacks/attachments/leases/publications.
5. Failure survival requires closed admission/effect/evidence/retirement protocols.
6. Evolution requires separate semantic/public/wire/stored ownership.
7. Deletion requires independent scenario/peer/frozen/platform consumer proof.

## Dependency-ordered materialization

Every exit is required. Wave 0 is a specification prerequisite, not completion.

| Wave | Work and mandatory exit |
| --- | --- |
| **0 — contracts/proof corpus** | Fix twelve meanings, identities, linearization/outcomes, evidence/policy ledger and consumer scenario seams. Exit: no ambiguous verb/success; preserve correct contracts. |
| **1 — values/formats** | Lower pure validation, reverse wire/public error dependency, typed outcomes/epoch/presentation evidence, keeper pin/frozen storage readers and kind correspondence. Exit: JSON cannot migrate storage, host independent of presentation validation, distinct malformed/unsupported/refused/unknown. No TLV rewrite/core-protocol merger. |
| **2A — execution lifecycle** | Unified evidence, retained retirement, honest reconciliation, activation fences and justified attempt evidence. Upgrade flushes/quiesces or refuses. Exit: every admission/engine/evidence/removal failure cut inspectable without duplicate/ambiguous replay. |
| **2B — live generations** | Own registrations/cancellation and all negotiated link facts. Exit: disconnect without detach, peer replacement/reused ids and late completions cannot retain/reinstall old authority; configured hosts survive. |
| **2C — portable decisions** | Share paste order/delivery, create/ensure/close, trust and outcomes with explicit clocks/capabilities. Exit: equivalent native/TUI/browser/binding policy, preserved raw latency/untrusted rules/fallbacks, no hidden creation reported unavailable. |
| **3 — host operations** | Encapsulate complete choreography under existing serialization; narrow admitted operation/capture/resource contexts and kind seams; private temporal owners. Exit: one ordering owner, no unrelated-table access, unchanged atomicity/hot-path budgets. |
| **4 — drivers/all callers** | Move locks/clocks/engine round trips outward; migrate CLI/MCP/headless/TUI/Rune/FFI/browser, retain platform enrollment/credentials/gestures/paint; supported binding outcome transition. Exit: no semantic connection locks or duplicate policy; delete replaced paths at cutover. |
| **5 — presentation/trust** | Matching visible/unclipped receipts, no FFI acquisition acknowledgment; independent view/interest, exact failed-create compensation; current trusted-template or exact raw-recipe approval. Exit: stale/hidden frames cannot clear uncertainty, views cannot incidentally kill resources, disabled-integration/archive cases follow explicit new policy. |
| **6 — compatibility/deletion** | Complete wire/JSON/binding/storage/old-peer/frozen-upgrade projections and independent nested probes. Delete obsolete modules/exports/decisions/conversions/redundant internal tests. Exit: one semantic path; legitimate released readers remain until their support contract ends. |
| **7 — qualification/integration** | Actual entry points and real native/foreign clients, exact source/artifact identity: peer/storage/crash/upgrade/authority, bounded queues/memory, hardware latency/allocations and locality. Exit: applicable native proof before integration, all callers cut over, obsolete paths deleted, live docs updated. |

Wave 1 contracts precede parallel 2A/2B/2C; all three integrate through their exact
outcomes/generations before wave 3. Waves 3 → 4 → 5 → 6 → 7 complete the cutover.
Proof construction starts in wave 0 and accompanies each owner; wave 7 is the
whole-product qualification, not the first time behavior is exercised.

## No-MVP finish

Every disposition/operation/proof is required end to end, in actual clients.
No optional follow-ups or unexecuted platform passes. No universal framework,
new emulator/transport, state facade, universal DTO or actor per table.
Finish: truthful outcomes, explicit lifetimes, supported evolution, native performance.
