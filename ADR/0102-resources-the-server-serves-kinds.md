---
audience: contributors
stability: stable
last-reviewed: 2026-09-09
---

# 0102 - Resources: the server serves kinds; Terminal is the first

**TL;DR.** phux serves resources, not terminals. A resource is an id, an open
kind, an optional parent, a lifecycle, an ordered opaque output stream with a
per-stream negotiated codec, a bootstrap, a kind-defined input channel, a
tagged event stream, and an L3 scope. The PTY-plus-libghostty Terminal is kind
0 and AgentSession is kind 1. `TerminalId` becomes `ResourceId` with the same
bytes in protocol 0.9.0. Terminal-only operations fail on other kinds with
`WRONG_RESOURCE_KIND`.

Status: Accepted
Date: 2026-09-09

## Context

The server knows one served thing. Every frame, table, handle, and document
says Terminal, because [ADR-0016](./0016-terminal-id-as-wire-primary.md) made
`TerminalId` the wire primary and nothing since has needed a second one. The
agent program then grew one without a home: an agent's session is a real
server-side thing with a lifecycle, an ordered log, and observers, carried
today as an L3 record ([ADR-0040](./0040-agent-identity-metadata.md)) whose
`state` the server recovers by scraping the screen
([ADR-0046](./0046-server-side-agent-state-detection.md)). The reserved
`PROCESS_*` and `FORWARD_PORT` discriminants show where this was heading: one
frame family per new thing, each with its own spawn, close, and inventory.

[ADR-0030](./0030-engine-delegated-wire-and-projection-consumers.md) states
the wire's job as a closed list (identity, lifecycle, transport, opaque bytes,
metadata); [ADR-0070](./0070-native-engine-state-bootstrap.md) gave the bytes
a negotiated codec and a bootstrap generation. Neither requires a PTY.

## Decision

1. **A resource is the served unit.** It has a `ResourceId`, a `ResourceKind`,
   an optional `parent: ResourceId`
   ([ADR-0104](./0104-parent-bindings-are-l1-lifecycle.md)), a lifecycle
   (spawn, then closed with a reason), an ordered opaque output stream with a
   per-stream negotiated codec, a bootstrap in the ADR-0070 shape (a
   replaceable replica generation cut at a stream sequence), a kind-defined
   input channel, a tagged event stream, and an L3 metadata scope.
2. **`ResourceKind` is an open `u8`.** `Terminal = 0`, `AgentSession = 1`,
   `Unknown { tag }` on decode. A tag is never reused.
3. **`ResourceId` replaces `TerminalId`.** Same tagged union, `Local { id }`
   tag 0 and `Satellite { host, id }` tag 1. Location stays orthogonal to
   kind: a hub routes an `AgentSession` exactly as it routes a Terminal.
4. **The Terminal facet is cols, rows, title, cwd, and a PTY child.** Only
   Terminal-kind resources accept input atoms, `RESIZE_TERMINAL`,
   `GET_SCREEN`, `GET_TERMINAL_STATE`, `HISTORY_*`, `ACQUIRE_INPUT` and
   `RELEASE_INPUT`, `SIGNAL_TERMINAL`, `APPLY_INPUT`, `PUT_FILE`, and
   `TRANSCRIBE`. Sent to any other kind they fail with the new
   `ERROR { code: WRONG_RESOURCE_KIND }` or that code in the command's result.
5. **Protocol 0.9.0 renames the substrate once, keeping discriminants.**
   `TERMINAL_OUTPUT` becomes `RESOURCE_OUTPUT` (0x90), `SPAWN_TERMINAL`
   becomes `SPAWN_RESOURCE` (0x22), `TERMINAL_SPAWNED` becomes
   `RESOURCE_SPAWNED` (0xA2), `TERMINAL_CLOSED` becomes `RESOURCE_CLOSED`
   (0xA1), `TERMINAL_RESIZE` becomes `RESIZE_TERMINAL` (0x23, a facet frame).
   `MOVE_`, `ATTACH_`, `DETACH_`, `KILL_`, `KILL_*S`, `SUBSCRIBE_*_EVENTS`,
   `TerminalInfo`, `SessionSnapshot.panes`, `focused_pane`, `active_pane`,
   `Scope::Terminal`, `CommandValue::TerminalId`, `TerminalEventType`, and
   `AgentEvent::PaneSpawned/PaneClosed` take the resource spelling. Facet
   frames keep Terminal in their name (`INPUT_*`, `INPUT_TERMINAL_REPLY`,
   `GET_SCREEN`, `GET_TERMINAL_STATE`, `RESIZE_TERMINAL`, `SIGNAL_TERMINAL`).
6. **Kind rides additive fields where the body is TLV.** `SPAWN_RESOURCE`
   fields 1 to 10 are unchanged and are the Terminal backing; field 11 is
   `kind: u8` (absent means Terminal) and the decoder validates the rest per
   kind. `ResourceInfo` keeps its positional prefix (id, window_id, cols,
   rows, title, cwd) and gains trailing `kind`, `parent`, and a kind facet;
   a non-terminal entry carries `cols = rows = 0` and `window_id = 0` as a
   documented no-window sentinel. `ServerFeature::RESOURCE_KINDS = 0x4000`
   advertises that the server spawns kinds other than Terminal.
7. **The codec is negotiated per stream.** A Terminal stream negotiates as
   ADR-0070 specifies. Every other kind names its codec in
   `BOOTSTRAP_BEGIN.codec`; `BootstrapCodec` tags are never reused.
8. **The reserved `PROCESS_*` and `FORWARD_PORT` discriminants are subsumed
   by kinds** and stay unallocated.
9. **"Pane" stays a TUI and CLI word** for a Terminal-kind resource in a
   layout slot, off the wire except the `watch` gate names `pane_spawned` and
   `pane_closed`, frozen by [ADR-0071](./0071-what-phux-1-0-commits-to.md).
10. **Names not used.** `Workload` is an authenticated client
    ([ADR-0098](./0098-workload-proof-and-closed-scope-authority.md)),
    `WorkSession` is coordinator identity
    ([ADR-0092](./0092-durable-work-coordinator-authority.md)); `Block`,
    `Surface`, and `Target` already mean something in a consumer.

This supersedes ADR-0016 and scopes
[ADR-0014](./0014-server-terminal-pane-actor.md) (the actor is the Terminal
engine inside a kind-agnostic resource core),
[ADR-0015](./0015-protocol-layering.md) (L1 is the resource substrate),
[ADR-0022](./0022-tool-for-agents.md) (projections are per kind), and
ADR-0030 (the closed list reads identity, lifecycle, transport, opaque bytes
per kind under a negotiated codec, metadata).

## Why

**Vocabulary is the decision.** A second served thing could be bolted on as
`TerminalId` plus a kind byte. The documents, the CLI, the FFI header, and
every new contributor would then keep saying "terminal" for things that have
no PTY, and each new kind would re-open the argument about which frames apply
to it. Naming the unit once, and making the Terminal facet the exception list
rather than the default, is what stops the `PROCESS_*` pattern recurring.

**One lifecycle, N kinds.** Spawn, close with reason, attach, detach, kill,
atomic multi-kill, inventory, event subscription, and L3 scope are identical
for every kind. A per-kind frame family multiplies each, federation included.

**ADR-0030's argument generalizes instead of breaking.** That ADR refused
structured terminal state on the wire because both ends run the engine. An
agent session has no shared engine, but the same discipline holds: an opaque
stream under a named codec, projected by consumers. Kind selects the codec;
it is not a licence for typed frames.

**One minor bump, spent once.** Under
[ADR-0061](./0061-capabilities-add-versions-break.md) renamed type names
alone would not justify a break. The inventory does: a 0.8 client reads every
`SessionSnapshot` entry as a pane with a PTY, and hiding non-terminal entries
per client would make the fleet view lie. The program pays the break once for
ADR-0102, 0103, and 0104; everything optional in 0.9.0 sits behind the bit.

## Tradeoffs

- **The Cockpit projection lags the rename.** Cockpit still shows Terminal-only
  panes; wiring the AgentSession row is tracked as phux-am9y.25.
- **The rename touches every crate, the C ABI, Cockpit, and phux-web.**
  Golden snapshots survive where discriminants and prefixes are kept.
- **The no-window sentinel is a compromise.** `ResourceInfo` is positional
  at its head, so a non-terminal entry carries zeros a reader must know to
  ignore. A fresh frame would be cleaner and would cost a second inventory.
- **`Unknown { tag }` is a client obligation.** A 0.9.0 client skips kinds it
  does not know; the TUI never paints one as a pane. Facet dispatch suits a
  short closed list; a kind with two output streams needs another ADR.

## Alternatives

**Keep `TerminalId` and add a kind byte.** Smallest diff, rejected because
the vocabulary is the point: every document and consumer would still call a
non-terminal a terminal, and the facet exception list would never be written.

**Make an agent session a metadata convention.** ADR-0040's shape, extended.
Rejected: L3 has no ordered stream, no bootstrap generation, no observer
contract, and no cascade; each would be re-invented on last-writer-wins bytes.

**Model each kind as its own frame family**, as the reserved `PROCESS_*` and
`FORWARD_PORT` discriminants anticipated. Rejected: N parallel lifecycles,
N inventories, N federation arms, and no shared attach or kill semantics.
