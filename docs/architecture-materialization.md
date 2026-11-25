---
group: internals
order: 0.55
---

# Architecture materialization

The [accepted end state](./architecture-end-state.md) defines the destination,
not shipped behavior. [Qualification](./architecture-qualification.md) defines
the consumer-visible evidence required at every integration boundary.

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

Dependencies are producer contracts, not global work barriers. Wave 1 contracts
precede their consuming 2A/2B/2C tracks; independent ownership work in waves 3–6
can proceed against established contracts. Each integration waits for its exact
producer and consumer proofs, including execution and generation guarantees
where it consumes them. Proof construction starts in wave 0 and accompanies
each owner; wave 7 qualifies the complete product, not its first exercised path.

## No-MVP finish

Every disposition/operation/proof is required end to end, in actual clients.
No optional follow-ups or unexecuted platform passes. No universal framework,
new emulator/transport, state facade, universal DTO or actor per table.
Finish: truthful outcomes, explicit lifetimes, supported evolution, native performance.
