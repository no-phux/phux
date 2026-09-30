---
audience: humans, contributors, agents, consumers
stability: stable
last-reviewed: 2026-09-15
---

# How phux works

**TL;DR.** phux is a terminal multiplexer: shells run in a background
server while you organize them into sessions, windows, and panes.
Detaching closes your view, not your shell. A terminal client, Cockpit,
script, or coding agent can observe and control the same running terminal.
Clients may replicate rendering state; they do not create another copy of
your running program.

---

```text
      your programs: zsh, vim, htop, an agent's shell
                          │
                          │  PTY
                          ▼
 ┌─────────────────────────────────────────────────┐
 │ phux server -- keeps running when you leave     │
 │                                                 │
 │ libghostty terminal: the real one. Screen,      │
 │ scrollback, and modes live here, so they        │
 │ survive detach and feed every attach.           │
 └───────────────┬──────────────────▲──────────────┘
                 │                  │
     output goes │                  │ input comes back
     down as raw │                  │ up as structured
     VT bytes,   │                  │ key, mouse, and
     verbatim    ▼                  │ paste events
 ┌──────────────────────────────────┴──────────────┐
 │ attach: TUI, CLI, web, Cockpit                  │
 │ several clients share one live terminal;        │
 │ detach does not copy it                         │
 └─────────────────────────────────────────────────┘
```

The server holds the terminals. The TUI, CLI, web, and Cockpit attach to those same ones. Detach does not copy.

## Sessions, windows, and panes

For everyday use, start with these terms:

| Term | What it means when you use phux |
|---|---|
| Server | The background process that owns your terminals. Closing a client does not close it. |
| Session | A named workspace you can attach to and return to. |
| Window | One view within a session; switch windows to work on another set of panes. |
| Pane | A terminal in a window's layout: normally a shell, editor, or coding agent. |
| Client | Your view and input path into the server: for example the TUI or Cockpit. |
| Attach / detach | Open / leave a view of existing work, rather than start / stop the work itself. |

A split adds another pane. Exiting a shell or killing a pane is different
from detaching: it ends that terminal's work. A second client can observe the
same pane, and input from multiple writers can interleave. Coordinate before
typing into a terminal an agent is driving, or use a viewer attach when you
only want to watch.

Persistence is tied to the running server, not a disk checkpoint of your
programs. For detach, upgrade, crash, and restore boundaries, see
[workspace continuity](./operations.md#workspace-continuity-and-update-survival).
Try the [quickstart](./QUICKSTART.md) before learning protocol terminology.

## Choose your next step

- [Use the terminal UI](./consumers/tui.md) for keys, copying, and navigation.
- [Run a coding agent](./consumers/getting-started.md) for a host-specific setup.
- [Connect another machine](./remote-access.md) for SSH enrollment and reconnect.
- [Use Cockpit](./consumers/cockpit.md) for the native macOS interface.

The sections below are for automation and client implementers. You do not
need resource kinds or wire details to use a session.

## Resources

A resource is a server-owned, addressable thing. Every resource has:

- a kind and a stable id;
- a lifecycle: spawned, then running, optionally exited-but-retained, then closed with a reason (`Exited`, `Killed`, `ParentClosed`, `ServerShutdown`). A Terminal spawned with `retain_secs` (ADR-0124) keeps its exit status and last grid readable after its process ends — `phux resource show`/`wait` reads it — until a TTL, a count bound, or an explicit `kill` purges it with the ordinary close;
- an ordered, opaque output stream with a codec, and a bootstrap a consumer loads before live bytes;
- a kind-defined input channel;
- a tagged event stream;
- metadata, and an optional parent set at spawn and immutable.

Terminal is the first kind: a PTY child and a libghostty engine, with columns, rows, a title, and a working directory. Operations that only make sense there — typed input, resize, screen reads — are refused on any other kind.

AgentSession is the second kind: a structured event stream from an agent
harness, bound to the Terminal it runs in. Closing the parent closes the
child; closing the child never touches the parent. While the stream is live
it supplies agent lifecycle state; the pane detector is the fallback for a
harness that does not emit. Check applicability under [Maturity](#maturity);
producer procedures live in the [harness author guide](./consumers/harness.md).

`phux agent show` is a different surface: it reads agent state from a pane, not from an AgentSession resource.

Sessions, windows, panes, and splits are not a lifecycle tier. "Pane" stays a TUI and CLI word for a Terminal-kind resource in a layout slot, expressed as metadata and client logic.

## The wire

The wire carries four things:

- **Identity.** A `ResourceId` is `Local { id }` or `Satellite { host, id }`. A hub retags inventory with satellite ids; it does not merge remote session or window models. Selectors render as `@42` and `prod-box-3/@42`.
- **Lifecycle.** Spawn with a kind and an optional parent; close with a reason; parent cascade; atomic `KILL_RESOURCES`.
- **Bytes.** Opaque per-kind output (bootstrap and live). Structured input atoms for a Terminal; appended records for an AgentSession. Both ends run the engine for the kinds they show; the wire is not a second screen model.
- **Metadata.** Opaque key-value pairs. The server stores them; it does not interpret them.

There is no L2 collection tier. Group membership is metadata plus client
logic; atomic teardown is a single L1 operation. See the
[collection-layer explanation](./spec/L2.md).

Wire details live in the [encoding reference](./spec/appendix-encoding.md),
[resource and terminal protocol](./spec/L1.md), and
[metadata protocol](./spec/L3.md).

## Consumers are peers

The reference TUI, the headless CLI, the browser client, and Cockpit are peers. None has protocol-level standing: if a consumer needs a capability the wire does not provide, the answer is an ADR that extends the spec, not a consumer-shaped hook ([ADR-0017](adr/0017-tui-not-protocol-privileged.md)).

- [Terminal UI](./consumers/tui.md)
- [Automation CLI](./consumers/agents.md)
- [Browser client development](./consumers/web.md)
- [Cockpit](./consumers/cockpit.md)

A consumer that wants structured state carries the engine for the kinds it shows. One that does not render a kind lists it and draws none of it.

## Maturity

The protocol version describes wire compatibility, not the installed product
version. Its authoritative definition is the [protocol specification](./spec/README.md).

**Capability-dependent behavior:** the source checkout implements both resource
kinds. For an installed release, check `phux status --json`: the running
server must advertise `RESOURCE_KINDS` before you use AgentSession verbs.
This is not a claim that every stable release includes them. If absent,
ordinary Terminal operations and pane detection remain the starting point;
update through your [install source](./INSTALL.md#updating) if you need
AgentSession support. The [agent CLI guide](./consumers/agents.md#this-tree-older-releases-two-agent-surfaces)
owns the exact refusals and distinction between those surfaces.

The [vision](./vision.md) describes the long-term direction. This page owns
the Status table below; other docs link here rather than restating the gaps.

## Status

Target-versus-shipped gaps as of the last review. Each row names the ADR that owns the target and the bead that tracks the work.

| Gap | Today | Owner | Tracked |
|---|---|---|---|
| On-disk output journal and crash recovery | Decided: not built ([ADR-0130](adr/0130-on-disk-pty-journal-is-not-built.md)). Server death ends live sessions and loses their terminal history; a workspace archive recreates fresh PTYs, not the lost processes. The `EVENT` stream is a separate, memory-bounded journal ([ADR-0123](adr/0123-events-are-journaled.md)) that carries no PTY bytes. | [ADR-0003](adr/0003-server-process-model.md), [ADR-0092](adr/0092-durable-work-coordinator-authority.md), [ADR-0130](adr/0130-on-disk-pty-journal-is-not-built.md) | phux-p91i |
| Workload authentication enforcement | Paired mode requests a client certificate and enforces the scope matrix at dispatch; a revoked or expired credential now loses authority on the live connection, not just at the next HELLO. Unset mode beside a remote listener still admits every connection with the owner's full grant — a warned transitional posture, not the startup error the spec's target table asks for — and a configured CA or registry path with no mode is ignored rather than refused. | [ADR-0116](adr/0116-workload-auth-is-mtls.md) | phux-cockpit-p1q.11.2 |

Scopes, attach roles, and approval gates are shipped: scope enforcement at
dispatch ([ADR-0116](adr/0116-workload-auth-is-mtls.md)), `VIEWER`/`PRIMARY`
attach intent on the lease ([ADR-0127](adr/0127-attach-roles-are-lease-intent.md)),
and server-held approvals for dangerous actions
([ADR-0128](adr/0128-approvals-are-held-actions.md)); nothing is open beyond
the transitional posture row above.

## Where to go next

| You want to | Read |
|---|---|
| Run it | [Quickstart](./QUICKSTART.md) |
| Understand the wire bytes | [Protocol specification](./spec/README.md) |
| Understand how the server is built | [Architecture](./architecture/README.md) |
| Drive it from an agent | [Coding-agent getting started](./consumers/getting-started.md) |
| Build a browser client | [Web client](./consumers/web.md) |
| Use Cockpit | [Cockpit guide](./consumers/cockpit.md) |
| Understand the TUI surface | [Terminal UI guide](./consumers/tui.md) |
| See why a design was chosen | [Architecture decisions](adr/README.md) |
| Read the long arc | [Vision](./vision.md) |
| Contribute | [Contributor guide](../CONTRIBUTING.md) |
