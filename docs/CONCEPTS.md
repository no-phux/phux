---
audience: humans, contributors, agents, consumers
stability: stable
last-reviewed: 2026-09-15
---

# How phux works

**TL;DR.** phux is a programmable terminal runtime. A background server owns
running terminals; clients organize them into sessions, windows, and panes.
Public control and event interfaces support interactive use, automation, and
remote access. Detaching closes a view, not the shell. Clients may replicate
rendering state, but they do not copy the running program.

---

```text
      your programs: zsh, vim, htop, an agent's shell
                          │
                          │  PTY
                          ▼
 ┌─────────────────────────────────────────────────┐
 │ phux server -- keeps running when you leave     │
 │                                                 │
 │ libghostty holds screen, scrollback, and modes. │
 │ This state survives detach and is available     │
 │ to each client.                                 │
 └───────────────┬──────────────────▲──────────────┘
                 │                  │
     output goes │                  │ input comes back
     down as raw │                  │ up as structured
     VT bytes,   │                  │ key, mouse, and
     verbatim    ▼                  │ paste events
 ┌──────────────────────────────────┴──────────────┐
 │ attach: TUI, CLI, web, Cockpit                  │
 │ clients render output and send input            │
 │ disconnecting a client leaves work running      │
 └─────────────────────────────────────────────────┘
```

## Sessions, windows, and panes

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

Persistence depends on the running server, not a disk checkpoint of your
programs. See [workspace continuity](./operations.md#workspace-continuity-and-update-survival)
for detach, upgrade, crash, and restore boundaries.

## Choose your next step

- [Start a terminal](./QUICKSTART.md), then use the [TUI guide](./consumers/tui.md)
  for keys, copying, and navigation.
- [Run a coding agent](./consumers/getting-started.md).
- [Connect another machine](./remote-access.md).
- [Use Cockpit](./consumers/cockpit.md), the native macOS interface.

The remaining sections describe the resource model for automation and client
implementation.

## Resources

A resource is a server-owned object with:

- a kind and a stable id;
- a lifecycle: spawned, running, optionally exited-but-retained, then closed
  with a reason (`Exited`, `Killed`, `ParentClosed`, `ServerShutdown`);
- an ordered, opaque output stream with a codec and a bootstrap loaded before live bytes;
- a kind-defined input channel;
- a tagged event stream;
- metadata, plus an optional parent fixed at spawn.

Terminal is a PTY child and a libghostty engine, with columns, rows, a title,
and a working directory. Typed input, resize, and screen reads are refused on
other kinds. With `retain_secs` (ADR-0124), its exit status and last grid remain
readable through `phux resource show`/`wait` after the process ends, until a
TTL, a count bound, or an explicit `kill` closes it.

AgentSession is the second kind: a structured event stream from an agent
harness, bound to the Terminal it runs in. Closing the parent closes the
child; closing the child never touches the parent. While the stream is live
it supplies agent lifecycle state; the pane detector is the fallback for a
harness that does not emit. Check applicability under [Maturity](#maturity);
producer procedures live in the [harness author guide](./consumers/harness.md).

`phux agent show` is a different surface: it reads agent state from a pane, not from an AgentSession resource.

Sessions, windows, panes, and splits are layouts built from metadata and client
logic, not a separate lifecycle tier. In the TUI and CLI, a "pane" is a
Terminal-kind resource in a layout slot.

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

The TUI, headless CLI, browser client, and Cockpit have no protocol-level
privileges. New capabilities require an ADR extending the spec, not a
client-specific hook ([ADR-0017](adr/0017-tui-not-protocol-privileged.md)).
Their interfaces are documented in the [consumer guides](./consumers/README.md).

A consumer carries the engine for each kind it renders. It can list other
kinds without rendering them.

## Maturity

The protocol version describes wire compatibility, not the installed product
version. Its authoritative definition is the [protocol specification](./spec/README.md).

For an installed release, check `phux status --json`: the server must advertise
`RESOURCE_KINDS` before you use AgentSession verbs. The source checkout
implements both resource kinds, but not every stable release includes them.
Without the capability, use ordinary Terminal operations and pane detection,
or [update](./INSTALL.md#updating) for AgentSession support. The
[agent CLI guide](./consumers/agents.md#this-tree-older-releases-two-agent-surfaces)
defines the exact refusals and distinguishes the two agent surfaces.

The [vision](./vision.md) describes the long-term direction; the table below
records current gaps.

## Status

Current gaps, with the decision that defines each target and its tracking issue:

| Gap | Today | Owner | Tracked |
|---|---|---|---|
| On-disk output journal and crash recovery | Decided: not built ([ADR-0130](adr/0130-on-disk-pty-journal-is-not-built.md)). Server death ends live sessions and loses their terminal history; a workspace archive recreates fresh PTYs, not the lost processes. The `EVENT` stream is a separate, memory-bounded journal ([ADR-0123](adr/0123-events-are-journaled.md)) that carries no PTY bytes. | [ADR-0003](adr/0003-server-process-model.md), [ADR-0092](adr/0092-durable-work-coordinator-authority.md), [ADR-0130](adr/0130-on-disk-pty-journal-is-not-built.md) | phux-p91i |
| Workload authentication enforcement | Paired mode requests a client certificate and enforces the scope matrix at dispatch; a revoked or expired credential now loses authority on the live connection, not just at the next HELLO. Unset mode beside a remote listener still admits every connection with the owner's full grant — a warned transitional posture, not the startup error the spec's target table asks for — and a configured CA or registry path with no mode is ignored rather than refused. | [ADR-0116](adr/0116-workload-auth-is-mtls.md) | phux-cockpit-p1q.11.2 |

Scopes, attach roles, and approval gates are shipped: scope enforcement at
dispatch ([ADR-0116](adr/0116-workload-auth-is-mtls.md)), `VIEWER`/`PRIMARY`
attach intent on the lease ([ADR-0127](adr/0127-attach-roles-are-lease-intent.md)),
and server-held approvals for dangerous actions
([ADR-0128](adr/0128-approvals-are-held-actions.md)). The remaining authentication
gap is the transitional posture above.

## Where to go next

| You want to | Read |
|---|---|
| Understand the wire bytes | [Protocol specification](./spec/README.md) |
| Understand how the server is built | [Architecture](./architecture/README.md) |
| See why a design was chosen | [Architecture decisions](adr/README.md) |
| Contribute | [Contributor guide](../CONTRIBUTING.md) |
