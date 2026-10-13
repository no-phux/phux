---
audience: humans, contributors, agents, consumers
stability: stable
last-reviewed: 2026-10-13
---

# How phux works

**TL;DR.** phux is a programmable terminal runtime that people and agents
share. A background server owns resources, such as terminals, and groups
each one into exactly one session. Clients show views of those resources.
Closing a view never ends work; killing a resource does. Sessions are
records with an explicit lifetime, and every change records who made it.

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
 │ Sessions and their arrangements are records     │
 │ the server keeps on disk.                       │
 └───────────────┬──────────────────▲──────────────┘
                 │                  │
     output goes │                  │ input comes back
     down as raw │                  │ up as structured
     VT bytes,   │                  │ key, mouse, and
     verbatim    ▼                  │ paste events
 ┌──────────────────────────────────┴──────────────┐
 │ attach: TUI, CLI, web, Cockpit, desktop, agents │
 │ clients show views and send input               │
 │ closing a client or a view leaves work running  │
 └─────────────────────────────────────────────────┘
```

## Sessions, resources, and views

| Term | What it means when you use phux |
|---|---|
| Server | The background process that owns your resources. Closing a client does not close it. |
| Resource | Something you work with: a terminal running a shell, editor, or agent, or an agent conversation. It belongs to exactly one session. |
| Session | A named group of resources on one server. You attach to it and return to it. |
| Inbox | The session that holds work you have not organized yet. Every server has one. |
| Arrangement | How a session's resources are laid out: groups, shown as tabs, each holding a split tree of views. |
| View | One client's presentation of one resource: a pane in the TUI, a tile in Cockpit. |
| Client | Your view and input path into the server: for example the TUI or Cockpit. |
| Attach / detach | Open / leave a view of existing work, rather than start / stop the work itself. |

A split adds a new resource to the session and a view of it to the current
group. A second client can show the same resource, and input from several
writers can interleave. Coordinate before typing into a terminal an agent
is driving, or use a viewer attach when you only want to watch.

### Close and kill

A verb named **close** removes presentation. Closing a view, a tab, or a
whole client leaves the resource running and listed in its session; you
can show it again. A verb named **kill** ends execution, explicitly, and
asks first when it would end more than one thing. Killing a session
terminates and removes each of its resources, then deletes the session; a
resource added while that runs survives, and the session stays.

Exiting a shell removes that terminal, as in tmux. A launched command or
agent stays listed after it exits, with its outcome, until you remove it.
Nothing restarts on its own.

### Session lifetime

Every session is either **persistent** or **ephemeral**, chosen when it is
created and shown everywhere it is listed. A named session (`phux new
NAME`) is persistent: it stays, empty if need be, until you delete it. The
session that bare `phux` starts is ephemeral: it goes away with its last
resource. Either can be switched later. A client's layout choices never
change a session's lifetime.

A session is a record, not proof that anything is running. Records survive
a server restart; running processes do not, unless a live upgrade hands
them over. After a crash, sessions come back with each terminal marked as
ended, and `phux restore` relaunches them. See
[workspace continuity](./operations.md#workspace-continuity-and-update-survival)
for detach, upgrade, crash, and restore boundaries.

## Choose your next step

- [Start a terminal](./QUICKSTART.md), then use the [TUI guide](./consumers/tui.md)
  for keys, copying, and navigation.
- [Run a coding agent](./consumers/getting-started.md).
- [Connect another machine](./remote-access.md).
- [Use Cockpit](./consumers/cockpit.md), the native macOS interface.

The remaining sections describe the model for automation and client
implementation.

## Resources

A resource is a server-owned record with:

- a kind, a permanent key, and a live id for the current server run;
- membership in exactly one session;
- an exit policy, `Remove` or `Retain`, and its current or most recent
  activation;
- an ordered, opaque output stream with a codec and a bootstrap loaded
  before live bytes;
- a kind-defined input channel;
- a tagged event stream;
- metadata, plus an optional parent fixed at spawn.

An **activation** is one execution of a resource. It is starting, running,
stopping, completed, failed, or lost, and when it ends it records the real
outcome: exit status, signal, or "unobserved" when the server was not
there to see it. *Lost* means the server lost track of a process that may
still be running; such a resource cannot be removed or relaunched until
the process is terminated or a later startup finds it gone. Relaunching a
finished resource starts a new activation of the same resource.

Terminal is a PTY child and a libghostty engine, with columns, rows, a title,
and a working directory. Typed input, resize, and screen reads are refused on
other kinds. A retained Terminal keeps its exit status and last grid
readable through `phux resource show` and `wait` after its process ends.

AgentSession is the second kind: an agent conversation, the structured event
stream from an agent harness, bound to the Terminal it runs in. The wire
name stays `AgentSession`; it is not a session in the sense above. Closing
the parent closes the child; closing the child never touches the parent.
While the stream is live it supplies agent lifecycle state; the pane
detector is the fallback for a harness that does not emit. Check
applicability under [Maturity](#maturity); producer procedures live in the
[harness author guide](./consumers/harness.md).

`phux agent show` is a different surface: it reads agent state from a pane,
not from an AgentSession resource.

Moving a resource to another session on the same server changes its
membership and nothing else; the process keeps running. There is no move
between servers: working elsewhere means creating a resource there.
Project, directory, repository, and host are attributes of a session, not
levels above it.

## Arrangements and the Inbox

A session may have one shared arrangement. The server owns it and changes
it by revision, so two clients editing the same tabs see a conflict instead
of overwriting each other. An arrangement entry points at a resource; it
never owns one, and a removed resource leaves a named placeholder. Each
client adapts the arrangement to its own screen. Focus, selection, scroll,
and window geometry stay local to the client and are never shared.

Resources created without a session land in the server's Inbox session.
Sessions brought over from an older server keep their members and tabs and
show up in the Inbox as intact groups until you mark them organized.
Organizing work, by drag, keyboard, or bulk selection, moves membership
only and can be undone while the state it changed is unchanged.

## Actors

Every accepted change records its **actor**: the authenticated principal,
the client it came through, and, for an agent acting for a person, that
person. The owner of a server can do anything on it. An agent acts only
through a grant the owner gave it, and a grant can be narrowed but never
widened. What an agent writes about itself in metadata is description, not
permission.

Each interactive resource has at most one input lease holder. Focus never
takes the lease, and an agent asking for attention can badge a view but
never moves another client's focus.

## The wire

The wire carries five things:

- **Identity.** A record has a 16-byte key that never changes. A live
  `ResourceId` is `Local { id }` or `Satellite { host, id }` and lasts one
  server run. A hub retags inventory with satellite ids; it does not merge
  remote sessions. Selectors render as `@42`, `prod-box-3/@42`, and
  `^ab12cd` for a key.
- **Lifecycle.** Spawn with a kind and an optional parent; close with a
  reason; parent cascade; atomic `KILL_RESOURCES`.
- **Bytes.** Opaque per-kind output (bootstrap and live). Structured input
  atoms for a Terminal; appended records for an AgentSession. Both ends run
  the engine for the kinds they show; the wire is not a second screen model.
- **Organization.** Typed commands that create, rename, move, and delete
  sessions and resources and replace arrangements. Each carries an
  operation id and the revision it expects, and answers with a result or a
  typed refusal.
- **Metadata.** Opaque key-value pairs the server stores without
  interpreting.

Wire details live in the [encoding reference](./spec/appendix-encoding.md),
[resource and terminal protocol](./spec/L1.md),
[organization](./spec/L2.md), and [metadata protocol](./spec/L3.md).

## Consumers are peers

The TUI, headless CLI, browser client, desktop, and Cockpit have no
protocol-level privileges. New capabilities require an ADR extending the
spec, not a client-specific hook
([ADR-0017](adr/0017-tui-not-protocol-privileged.md)). Their interfaces are
documented in the [consumer guides](./consumers/README.md).

A consumer carries the engine for each kind it renders. It can list other
kinds without rendering them, and shows an unknown kind as a named
placeholder rather than dropping it.

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

Typed organization needs a server that advertises `ORGANIZATION`. Against an
older server, a client treats organization as read-only, and older clients
keep working against a new server through a translation layer.

The [vision](./vision.md) describes the long-term direction; the table below
records current gaps.

## Status

The model arrives in stages. Resources, sessions with explicit lifetime,
arrangements, the owner-local Inbox, the organization store, crash
reconciliation, and typed organization commands land together
([ADR-0160](adr/0160-sessions-own-resources-and-window-is-retired.md),
[ADR-0161](adr/0161-organization-is-typed-commands-behind-a-feature-bit.md),
[ADR-0162](adr/0162-organization-records-persist-and-activations-reconcile.md)).
The rows below are what remains.

| Gap | Today | Owner | Tracked |
|---|---|---|---|
| Delegated agent credentials and human lease precedence (stage 5) | The `ORGANIZE` and `TERMINATE` verbs and the actor principal fields exist; agents still act with their connection's grant, are attributed by client name, and a deliberate takeover by any principal wins the lease. | [ADR-0163](adr/0163-actors-and-delegated-agent-grants.md) | phux-3q58 |
| Workspaces across machines (stage 6) | Designed, not built. Sessions link only within one server's view; the Inbox combines owners only as far as each client connects to them. | [ADR-0160](adr/0160-sessions-own-resources-and-window-is-retired.md) | phux-lxov |
| Browser resource kind (stage 7) | Designed, not built. Terminal and AgentSession are the only kinds. | [ADR-0160](adr/0160-sessions-own-resources-and-window-is-retired.md) | phux-3q58 |
| On-disk output journal and crash recovery | Decided: not built ([ADR-0130](adr/0130-on-disk-pty-journal-is-not-built.md)). Server death ends live processes and loses their terminal history; session and resource records survive and `phux restore` starts fresh PTYs, not the lost processes. The `EVENT` stream is a separate, memory-bounded journal ([ADR-0123](adr/0123-events-are-journaled.md)) that carries no PTY bytes. | [ADR-0003](adr/0003-server-process-model.md), [ADR-0092](adr/0092-durable-work-coordinator-authority.md), [ADR-0130](adr/0130-on-disk-pty-journal-is-not-built.md) | phux-p91i |
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
