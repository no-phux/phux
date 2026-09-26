---
audience: humans, contributors, agents
stability: evolving
last-reviewed: 2026-09-16
nav-order: 2
---

# phux and herdr: two system shapes

**TL;DR.** herdr is an agent-workspace application whose server projects panes,
workspaces, and agent state to its clients. phux is a resource control plane
whose server exposes terminals and agent sessions over one wire; its TUI is one
consumer of that substrate. Both keep real PTYs alive. They put the durable
boundary in different places.

---

This is not a feature scorecard. Features move too quickly, and both projects
are active. The useful comparison is the boundary each system asks the rest of
its architecture to preserve.

For a short product decision, start with [When to use phux](../when-to-use.md#compared-with-herdr).
For measured latency, throughput, and memory from one controlled host, see
[Performance](../performance.md).

## herdr: the workspace is the product boundary

```text
       shell or agent
              │ PTY
              ▼
┌──────────────────────────────┐
│ herdr server                 │
│ terminals + workspace model │
│ pane rendering              │
└──────────┬───────────┬───────┘
           │           │
  binary endpoint   JSON API
  snapshots/surfaces   │
           │           ▼
           │      CLI/hooks/tools
           ▼
    ┌──────────────┐
    │ herdr client │
    │ presentation │
    └──────────────┘
```

The herdr server owns the PTYs, terminal state, and the application model around
them. Its attached client receives workspace snapshots and rendered pane
surfaces or patches, then owns the outer terminal and local presentation.
Automation reaches the same application through a separate newline-delimited
JSON API.

Agents are processes in panes. herdr recognizes them from foreground-process
evidence, terminal content, and optional native integrations, then projects
their state into the workspace model. For multiple machines, one client
connects to independent herdr servers through SSH-backed endpoints and composes
their workspace and agent projections into one interface.

The durable idea is **a server-owned agent workspace with client-owned
presentation**. The stable endpoint contract can evolve independently of the
private same-install protocol because the endpoint projection is the boundary.

## phux: the wire is the product boundary

```text
 shell/agent       harness
      │ PTY bytes     │ records
      ▼               ▼
┌──────────────────────────────┐
│ phux server                  │
│ Terminal     AgentSession    │
│ resources    resources       │
└──────────────┬───────────────┘
               │
     one resource protocol
               │
       ┌───────┼────────┐
       ▼       ▼        ▼
      TUI  Cockpit/web CLI/SDK/MCP
  replica   projection  headless
```

The phux server also owns the PTYs and canonical terminal state, but it does not
export a pane-surface model. Terminal output remains VT bytes on the wire, and a
rendering consumer maintains its own libghostty replica. Structured key, mouse,
focus, and paste events travel back to the server. Lifecycle, output, events,
and metadata use the same framed protocol on every transport.

Sessions, windows, panes, splits, and focus are a TUI convention expressed with
metadata and client logic, not protocol-privileged server entities. An
AgentSession is a second resource kind bound to its Terminal parent. A harness
can append lifecycle records directly; process and screen detection remain a
compatibility path when a harness does not emit.

A remote client can dial another per-user server directly. A federation hub can
also relay the same frames while qualifying resource ids by host; it does not
merge remote workspace models. The durable idea is **server-owned resources
with peer consumers over one wire**.

## Where the architectures actually diverge

**Rendering.** herdr sends an application-level pane projection: snapshots,
surfaces, and cell patches. phux sends a resource stream: terminal bytes plus a
bootstrap, which a rendering consumer applies to its own terminal engine.

**Product vocabulary.** herdr's workspace, tab, pane, and agent model is part of
the server/client application contract. phux's protocol vocabulary stops at
resources, kinds, parents, lifecycle, streams, events, and metadata; a pane is
one consumer's view of a Terminal resource.

**Automation.** herdr exposes a dedicated JSON control API beside its binary
client protocol. phux's headless CLI, SDK, and MCP adapter are projections over
the same resource protocol used by visual clients.

**Agent truth.** herdr associates recognized agent processes and integration
signals with panes in its workspace model. phux can represent an agent run as
an addressable, producer-fed AgentSession resource; terminal-scoped detection
is the fallback rather than the resource model.

**Machines.** herdr's client composes several independent Local or SSH server
endpoints. phux either addresses a server directly or routes host-qualified
resource ids through a hub that relays the same frames.

These differences do not establish which interface is better, faster, or more
complete. They explain what extension pressure each architecture absorbs.
herdr can evolve its workspace projection as one product. phux pays the cost of
a public substrate so independently shaped consumers can remain peers.

## Read the boundaries, not this summary

For phux, the authoritative detail lives in the [system shape
diagram](./DIAGRAM.md), [data model](./data-model.md), [transport
boundary](./transport.md), and [wire specification](../spec/README.md). For
herdr's current behavior, read its [concepts](https://herdr.dev/docs/concepts/),
[socket API](https://herdr.dev/docs/socket-api/), and [multi-machine
model](https://herdr.dev/docs/connecting-machines/). If either project changes
an implementation detail without moving the boundary above, this page should
not need to change.
