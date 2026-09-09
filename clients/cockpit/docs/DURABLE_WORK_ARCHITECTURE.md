# Durable Work Architecture

## Status

This document defines Cockpit's side of the durable-work boundary. Phux
[ADR-0092](https://github.com/no-phux/phux/blob/main/ADR/0092-durable-work-coordinator-authority.md)
is the proposed coordinator-side decision. Until that ADR and its contracts are
implemented, Cockpit makes no durable-work claim.

The immediate product remains a fast native terminal. This architecture prevents
the UI process from becoming a second coordinator while Phux grows the durable
identity, evidence, and orchestration seams the product direction assigns to it.

## Decision

Cockpit is a **command and projection client**. Phux is the **durable work and
execution authority**.

Cockpit does not own authoritative Objective, Run, WorkSession, Artifact,
Signal, binding, event, or evidence storage. It does not operate a second local
PTY daemon for durable work. Durable local execution uses the local Phux
coordinator and the same provider contract as remote or federated execution.

The existing direct local provider remains useful and intentionally ephemeral.
It owns a PTY and emulator inside the Cockpit process, with no detach, app-crash
survival, durable evidence, or process-restoration claim.

## Authorities

| Fact | Authority |
|---|---|
| Objective, Run, WorkSession, Artifact, Signal, and binding identity | Phux coordinator |
| Work lineage, applied Run policy, ordered events, evidence completeness, and retention | Phux coordinator |
| Durable Signal acknowledgement and resolution | Phux coordinator |
| Phux process, PTY, terminal output order, lease, generation, and exit | Owning Phux server |
| Direct local process, PTY, emulator, and output | Cockpit, for this process lifetime only |
| Window, tab, split, focus, viewport, palette, and settings state | Cockpit |
| Rendering, accessibility, native input routing, and disclosure state | Cockpit |
| Rebuildable projection and search caches | Cockpit |

One fact has one writer. Cockpit sends typed commands to the authority and
renders versioned projections returned by it. It never writes an authoritative
current row and then asks Phux to agree.

Cockpit may define product strategy, formation policy, and requested Run policy.
Once selected, Phux owns the authoritative policy revision, admission decision,
enforcement, and result.

## Identity Layers

The layers answer different questions and remain separate:

```text
WorkSessionId          which durable product session?
  -> BindingId         which immutable provider binding in its history?
  -> TerminalRef       which live provider resource is presented now?
  -> ReplicaOwner      which published generation may accept this operation?
  -> tab/pane/window   where is it displayed in this Cockpit instance?
```

`TerminalRef` remains the correct live identity for the current terminal
provider API. `ReplicaOwner` remains an ephemeral stale-operation fence. Neither
is widened into durable product identity.

Tabs, panes, windows, PIDs, PTY keys, sockets, host text, provider enumeration
positions, and effect keys are never durable work identity.

Cockpit does not define the canonical byte encoding for work IDs or events.
Those definitions live in the Phux coordinator/protocol contract and cross the
client FFI as opaque validated values.

## Execution Paths

### Direct Local Terminal

The existing local provider is the zero-coordinator path:

- Cockpit owns the child process, PTY, emulator, and input queue.
- Closing Cockpit or losing the process ends execution.
- Workspace restore creates a fresh process in the saved presentation shape.
- No Objective, Run, WorkSession, artifact, completion, or evidence history is
  synthesized.

This path keeps startup and ordinary terminal development independent of Phux.
Its limitation is explicit rather than papered over with a local database.

### Durable Local Work

Durable local work is created through a local Phux coordinator:

- Phux issues durable identity and owns the process and PTY.
- Cockpit attaches as a consumer through the Phux provider.
- App exit or crash removes the view, not the execution.
- Reattachment requires an authoritative generation and complete bootstrap.
- Output, lifecycle, input results, artifacts, and Signals become durable only
  when the coordinator admits them under its event contract.

There is no Cockpit companion runner. Phux already owns daemon lifecycle, PTY
actors, attach bootstrap, generations, input leases, process-group signals, and
acknowledged idempotent input.

### Federated Work

The home Phux coordinator owns work lineage. The satellite that owns a terminal
remains authoritative for its execution facts. Cockpit receives one projection
that preserves both authorities; it does not reconcile competing stores itself.

## Client Contract

The durable-work FFI must expose capabilities rather than a universal provider
object. Initial read and command surfaces are expected to include:

```text
Read capabilities and limits
Read a complete work projection at cursor N
Observe ordered projection changes from N + 1
Create or modify Objective intent with OperationId and expected revision
Start, cancel, pause, or resume a Run with OperationId and expected revision
Query the authoritative result for an OperationId
Attach or release a WorkSession control/observe lease
Read terminal presentation through the existing terminal provider
Read Artifact manifests and immutable revisions
Acknowledge or suppress a Signal through typed commands
Request a complete resync after a gap or generation change
```

Absence is `unsupported`, not best-effort emulation. Detach survival, server
restart survival, completed replay, input recording, and artifact availability
are independent capabilities.

The work projection is provider-neutral. It may name current terminal bindings,
but it does not expose Cockpit pane structs, widget IDs, or native window state.

## Projection Rules

Cockpit may cache coordinator projections for responsiveness, but caches are:

- rebuildable from a complete coordinator snapshot and cursor;
- namespaced by authenticated coordinator identity;
- invalidated by authority or schema changes;
- never used to accept a mutating command while authority is unavailable; and
- never described as durable evidence.

A new generation remains staged until a complete snapshot and next cursor are
available. Cockpit may keep the prior complete canvas visibly frozen while
recovering. It cannot mix old rows with a new generation or route input into the
frozen view.

Historical replay creates a read-only terminal projection and performs no spawn,
network connection, input, clipboard write, notification, or external
navigation. Phux owns the completion seal and evidence manifest; Cockpit owns
their native presentation.

## Attention

Phux Signals are durable facts and shared attention state. Cockpit projects them
into calm native attention surfaces:

- healthy work adds no standing chrome;
- repeated occurrences collapse by coordinator-provided identity;
- the highest unresolved severity determines the marker;
- acknowledgement is a typed command, not a local counter reset; and
- inspection reveals lineage, source authority, generation, and evidence.

Purely local presentation state remains local: which popover is open, current
focus, hover, palette query, and whether transient explanatory copy is expanded.

## Failure Semantics

| Failure | Cockpit behavior |
|---|---|
| Cockpit exits or crashes during Phux work | Execution remains Phux-owned; reconnect from a fresh bootstrap. |
| Cockpit exits during a direct local terminal | Process ends; never claim detach or restoration. |
| Coordinator disconnects | Freeze the last complete projection, disable mutations, and show the precise recovery state. |
| Terminal owner generation changes | Cancel held input, pointer capture, search, and asynchronous completions; bootstrap again. |
| Coordinator loses the process | Mark owner lost or ended from authoritative state; never adopt a discovered PID. |
| Event cursor gaps or unknown schema appear | Request complete resync; do not guess or skip. |
| Evidence is incomplete or corrupt | Present the explicit gap/corruption state; never label replay complete. |
| A command result is unknown | Preserve `unknown` and query the same `OperationId`; never mint or blindly retry a second operation. |

## Persistence

Cockpit's `workspace.state` remains presentation persistence. It stores bounded
window, tab, pane-tree, focus, divider, and working-directory state under its
existing versioned contract.

It does not contain authoritative work events, artifact bytes, Signals, process
state, provider credentials, or mutable coordinator records.

A future topology version may use tagged leaves:

```text
fresh_local
durable_work_session <opaque WorkSessionId>
```

That version may land only after the coordinator contract proves identity,
lookup, authorization, and complete reattachment. Existing local leaves always
migrate to `fresh_local`; Cockpit never guesses a WorkSession from title, cwd,
PID, ordinal, or previous placement.

## Delivery Order

1. Ratify the Phux coordinator authority ADR and canonical vocabulary.
2. Implement stable identity, bindings, capabilities, event storage, and
   deterministic projections in Phux.
3. Extend the Phux client and FFI with bounded work snapshots, cursors, typed
   commands, and explicit resync.
4. Add a read-only Cockpit work projection without changing topology or chrome.
5. Bind newly created durable Phux work to native terminal views.
6. Project Signals into the existing quiet attention path.
7. Add completed-run replay and Artifact inspection from coordinator evidence.
8. Persist `WorkSessionId` leaves only after end-to-end reattachment proof.

Each slice must improve the terminal now or reduce operator memory later. No
fleet dashboard, generic provider plugin system, or speculative UI precedes a
real coordinator capability.

## Proof Obligations

Phux owns conformance tests for identity, event order, store recovery, binding,
leases, completion, artifacts, and federation authority. Cockpit owns tests for:

- command targeting and stale-generation refusal through the client boundary;
- complete-snapshot-before-delta publication;
- frozen and read-only states routing no input;
- correct presentation after reconnect, reorder, and resync;
- topology remaining presentation-only;
- keyboard, pointer, accessibility, and progressive disclosure; and
- no added standing chrome for healthy single-session use.

Real process-survival tests run against Phux. A mocked Cockpit reducer cannot
prove execution durability.

## Explicit Non-Goals

- No Cockpit-owned authoritative work database.
- No Cockpit companion PTY runner.
- No duplicate Objective, Run, WorkSession, Artifact, Signal, or event codecs.
- No inference of durability from a saved layout or frozen canvas.
- No adoption of existing processes by PID or terminal title.
- No broad provider vtable that pretends every provider can detach, replay,
  produce artifacts, or preserve evidence.
