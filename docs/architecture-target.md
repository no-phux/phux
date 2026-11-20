# Architecture target

A maintainer should be able to change a rule once, exercise the change
through each supported caller, and predict what a person will see after a
disconnect, refusal, or restart. A caller should be able to ask phux what
happened and get an answer that identifies the host, resource, operation,
and known outcome at the precision the operation provides.

This is the target for Beads epic `phux-lpf5v`. [Architecture](./architecture.md)
and [decisions](./decisions.md) describe the current system. The target uses
their existing seams and gives each shared rule a clear home.

## One owner for each decision

A rule has one implementation and one vocabulary. Other modules pass it
facts, translate requests into its vocabulary, or present its results.
One module can own the rule while several modules own facts that it needs.
For example, a peer's capability bit, a server's authority grant, and a
resource engine's availability answer three different questions. Collapsing
them into one flag would lose information.

| Decision | Owner at the target | What callers supply or consume |
| --- | --- | --- |
| Identity and authorization | The serving host resolves the authoritative subject and checks the current grant. The home keeper owns its work records. The kind catalog describes wire subjects and verbs. | Carry the caller, serving route, and exact target. Preserve stable keys, live handles, activations, and connection epochs as distinct identities. |
| Organization and effects | Domain rules own valid membership, arrangements, and lifecycle transitions. The server resolves the live effects and admits their exact affected set. The engine owns its supervised process and delivery facts. | Request a typed intent. Consume its refusal, receipt, or outcome without reconstructing the rule locally. |
| Durability and observation | The organization store owns committed state, durable receipts, and the journal position that goes with the commit. The keeper owns work and its receipts. The server journal publishes ordered events and declares gaps. | Read committed state, or resume and reread on a gap. Use executor evidence to learn whether an effect completed. |
| Shared client intent | The shared client path owns decisions common to equivalent requests, including capability selection, request identity, and handling an unknown outcome. | CLI, MCP, TUI, Rune, browser, and FFI retain their own presentation, input gestures, and platform integration. A transport adapter supplies I/O and negotiated facts. |
| Plugin shortcuts | Configuration owns manifest and binding interpretation. The plugin runtime owns action execution. | Entry points read the same effective key policy when they offer those shortcuts. A palette owns labels, focus, and when it captures a target. |
| Wire and compatibility | A wire declaration owns each known bit, name, and encoded form. The serving implementation owns what it advertises. The compatibility adapter translates supported older forms into the canonical intent at the edge. | Consumers use the declared codec and effective capabilities. Old wire, persisted data, and downgrade behavior stay covered by their existing contracts. |

The useful distinction is between a rule and a projection of it. A CLI exit
code, a menu label, and a wire response may all differ in shape while faithfully
representing the same outcome. A shortcut chooser and a state-changing command
may have different jobs even when both say “close.” Share policy after the
identity, effects, and result contract agree.

## Follow one operation through failure

For a state-changing intent, resolve the authoritative identities under the
serving host's rules. Admit the precise affected set that execution will use.
Validate the caller, revision or activation fences, and policy against that
set before any irreversible work. Where a durable transition applies, commit
the transition, receipt, and corresponding journal position together. Mirror
the committed state and execute the admitted effects in their required order.
External effects such as starting or signalling a process can fail after
commit. Qualify their outcome with what the engine can prove.

Resource close is a worked example. A parent can carry bound descendants.
The affected set includes those descendants when the intent closes the parent.
A guarded close that would empty a locked tab is refused without closing
anything. If admission succeeds, close events are child-first, with one close
per affected resource even when the request names both. Explicit session kill
has its own override. A natural exit follows the exit policy. Durable
terminate additionally fences the exact activation and must preserve the
difference between committing a stopping transition and observing the process
stop. These are separate workflow meanings with shared rules where those
meanings agree. See [organization](./organization.md#activations-and-reconciliation).

Failure and observation count as part of the interface. A receipt may prove an
operation was accepted while the process outcome is still pending. A transport
failure may leave delivery unknown. A stale id must not name a replacement
resource or process. A client reconciles from the supported receipt and state
interfaces within their retention and incarnation rules. It never resubmits
uncertain input under a fresh identity. After a journal gap it rereads level
state. For input whose uncertainty fences the UI, presentation of a visible,
unclipped frame supplies the acknowledgment. See [wire receipts](./protocol.md#receipts-retries-and-backpressure),
[durable receipts](./public-api.md#lost-replies-and-retry-scope), and the
[consumer contract](./embedding.md#runtime-consumer-contract).

On a federated route the serving host still checks authority. An operation
whose targets span hosts keeps their actual per-host outcomes when there is no
cross-host commit. A shared client path must preserve these outcomes through
FFI and browser surfaces. See [origin checks](./substrate.md#origin-checks)
and [federation compatibility](./compatibility.md#federation).

## Extend one workflow without spreading policy

Use a small interface that hides the rule, its ordering, and its failure
handling. The test for a useful module is whether a real caller can use it
without learning its internal sequence. A representative feature should
need one policy change, plus the declarations, adapters, and tests that
actually change contracts. The server decides which known features it
supports. A UI owns its interaction. A terminal engine owns its PTY and
libghostty state.

For an extension rehearsal, follow one slot-tab capability from its declaration
through advertisement, negotiation, and a client write. Then trace a resource
kind through its catalog entry, app implementation, and attach. The source of
each rule should be obvious. An existing kind, unrelated transport, or other
client should need edits only for a contract it actually consumes. The
[kind seam](./resource-kinds.md#the-seam) already makes this possible for
several engine decisions. The campaign applies the same standard to the
covered lifecycle, capability, and client rules.

### Change locality baseline

Trace from each supported entry point to the expression that makes the
decision. Count an independently evaluated rule as one decision site and
record every caller that must know its result. Count a UI or wire translation
as an adapter, with separate counts for contracts, fixtures, and generated
output. Trace the same entry points on the candidate revision.

| Rule | Baseline at `dc7126f76` | Comparison to make |
| --- | --- | --- |
| Guarded host-local close | Four handlers call `locked_tab_refusal` with the explicit targets. Admission and teardown build two affected sets. | The four guarded paths reach one admission on the teardown set. Durable terminate, natural exit, and explicit session kill retain their distinct intent. |
| Plugin shortcut merge | One `merge_plugin_bindings` definition in the TUI. Two key-listing commands import its configuration policy through `phux_tui`. | One policy definition in configuration. The two CLI calls and TUI consume it through that owner. |
| Slot-tab arrangement write | The headless `replace` path and runtime `replace_arrangement` independently set the same outgoing capability flag. The spatial command checks support for a directly requested slot operation. | Two write paths use one capability decision for the flag. Verify the separate spatial refusal and server advertisement independently. |

These searches locate the starting points. Follow the calls and inspect the
branches to count decisions. A search match by itself is just a reference.

```sh
base=dc7126f76
git grep -n -e locked_tab_refusal -e 'fn commit_close' "$base" -- crates/phux-server/src
git grep -n -e 'fn merge_plugin_bindings' -e 'use phux_tui::attach::plugin_actions' "$base" -- crates
git grep -n -e 'ServerFeatureExt::SlotTabs' -e 'slot_tabs: self' "$base" -- crates
git diff-tree --no-commit-id -r --no-renames --name-only 33b04d1e0 | wc -l
```

The plugin-key change touched 23 paths by that last count and split four
files for the size gate. The two CLI imports survived the split. A small
private file can still make a rule easier to find, so review the traced
owners and crossings first. Keep the file-size ratchet unless a comparison
shows repeated forced splits that leave decisions more scattered and a
replacement check can catch the same oversized-file regressions. See
[the plugin-key change](https://github.com/no-phux/phux/commit/33b04d1e0)
and [the current gate](../scripts/check-file-tokens.sh).

## Delivery and proof

The server close plan is the first proof that admission and teardown can share
one affected set. Build on that proof for durable terminate and settle what
each close intent promises before sharing client decisions. Plugin keys and
the capability catalog can land independently. Establish the layout migration
before removing its adapter. Compare locality against the original traces
after the slices land. Beads holds their work and status.

| Slice | Acceptance evidence |
| --- | --- |
| Durable terminate (`phux-lpf5v.2`) | Admission and execution consume the same affected set. Drive locked descendants, stale activations, authorization, receipt replay, delayed completion, Lost recovery, and the session-kill override through supported dispatch paths. |
| Plugin keys (`phux-lpf5v.3`) | One configuration implementation supplies resolution to the key commands and applicable TUI paths. Verify explicit `none`, canonical chord precedence, first manifest wins, old argument spellings, platform filtering, and diagnostics. Preserve the palette and process-execution contracts. |
| Client close (`phux-lpf5v.4`) | Record and exercise the actual meanings of single kill, batch kill, keep-empty close-tab, and activation-fenced terminate. Equivalent intents share a decision path where used. FFI and browser results preserve unsupported capability, host and activation identity, unknown outcomes, and partial results where applicable. |
| Capabilities (`phux-lpf5v.5`) | A single declaration supplies extension feature bits, names, and inventory. Equivalent slot-tab selection uses shared policy where the clients issue the same intent. Compare generated wire and capability output byte for byte. Independently exercise implemented and listener-advertised support. |
| Legacy layout (`phux-lpf5v.6`) | Inventory live old-peer and named-projection readers and writers, plus frozen readers. Prove that compatibility requests reach the arrangement authority. Remove live translation after an explicit compatibility decision and proof of a supported migration or alternative. Exercise old-peer writes, downgrade decoding, and hot handoff at the affected seams. |
| Change locality (`phux-lpf5v.7`) | Compare the three traces with the baseline above and show fewer independent decisions where semantics are shared. Explain remaining adapters by their real consumers and show that the extension rehearsal can follow one rule without finding it again elsewhere. |

Use tests at the seam a real caller crosses. The same scenario exercised
through two entry points should produce the same semantic decision, including
refusals and no durable or live mutation on refused admission. Add a
wire-level regression where routing or translation can change the result.
Use existing [compatibility fixtures](./compatibility.md#persisted-formats)
and old-peer and hot-upgrade checks where a migration crosses their formats.
Keep the authority checks for grants, relays, revocation, and local plugin
execution intact. The daemon/terminal dependency seam and the browser's
sans-IO kernel must still build through their intended routes.

Performance is part of the proof. Keep live queues and histories bounded.
For changes on the input, publication, or commit hot paths, compare before
and after on the same workload and target. Use the existing instruction
budgets and the applicable echo or rendering benchmark to catch added
copies, contention, or round trips. Explain measured regressions before
accepting a changed budget. See [development gates](./development.md#validating-a-change)
and [performance](./development.md#performance).

The finish line is landed code, supported migration paths, and passing evidence
for the rows above, including the negative cases. A relocated duplicate, an
adapter that still owns a policy branch, or an old protocol path with no
verified consumer story leaves the relevant slice open.
