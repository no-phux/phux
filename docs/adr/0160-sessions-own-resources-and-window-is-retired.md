---
audience: contributors
stability: stable
last-reviewed: 2026-10-13
---

# 0160 — Sessions own resources; Window is retired

**TL;DR.** A resource belongs to exactly one session on its owning server.
A session is a persistent record with an explicit lifetime, persistent or
ephemeral, chosen at create. Window is deleted; tabs and splits are groups
in the session's arrangement. Each server designates one persistent Intake
session for unorganized work. Workspaces link sessions across servers and
arrive last. Closing a view never ends work.

Status: Accepted
Date: 2026-10-13

## Context

The server registry holds `Session { windows, active, keep_empty }` and
`Window { slots, layout, active }`, but only the first window of a session
is ever used. Real tabs live in the client-written `phux.tui.layout/v1`
metadata key. The spec says grouping is client convention
([ADR-0015](./0015-protocol-layering.md),
[ADR-0030](./0030-engine-delegated-wire-and-projection-consumers.md), and
`docs/spec/L2.md`), while the server intercepts session create, rename, and
keep-empty writes and reaps sessions itself.

Session lifetime follows processes and whichever client created the
session. [ADR-0105](./0105-sessions-can-outlive-their-last-window.md) made
keep-empty an opt-in, so a named session vanishes with its last shell unless
its creator asked otherwise, and Cockpit
([ADR-0114](./0114-cockpit-closes-terminals-and-detaches-windows.md)) and
the CLI disagree by default. Close and kill mean different things on
different clients. Four unrelated things are called "workspace". Agents and
people need one model they both operate, at the scale of thousands of
resources.

## Decision

1. **Nouns.** A *resource* is an identified thing with a kind, lifecycle,
   and capabilities. A *session* is a named group of resources on one
   owning server. A *workspace* is a named list of links to sessions,
   possibly on several servers, plus its arrangement. An *arrangement* is a
   saved layout of views; a *view* is one client's presentation of one
   resource. An *authority* is the persistent identity of one user's
   server; an *activation* is one execution of a resource.
2. **One session per resource.** Every resource is a member of exactly one
   session on its authority. Moving it between sessions on the same
   authority changes membership, never execution. There is no
   cross-authority move. Overlapping collections, if ever needed, are saved
   filters, not shared ownership.
3. **Keys and handles.** Records carry 16-byte random keys
   (`SessionKey`, `ResourceKey`, `ArrangementKey`, `WorkspaceKey`) that
   never change. The slotmap `SessionId` and `ResourceId` stay as live
   handles for one server instance. Only `WindowId` is deleted from core.
4. **Window is retired.** A session's members and its optional shared
   arrangement replace windows. The arrangement is a list of groups, each
   one split tree whose leaves name resources by key. Group and entry ids
   are random 16 bytes. Today's single window becomes one group.
5. **Lifetime is explicit.** `SessionLifetime` is `Persistent` or
   `Ephemeral`, set at create and visible on every surface. Named sessions
   default to persistent and survive empty. A quick unnamed session, as
   bare `phux` starts, is ephemeral and is deleted in the same transaction
   that removes its last resource. Either can be switched later. A
   client's presentation choices never change lifetime. Creating a session
   launches nothing unless the same command asks for it.
6. **Delete only when empty.** Deleting a session with members is refused.
   "Kill a session" is one server command, `SessionKill`, that ends and
   removes every member, work added meanwhile included, and deletes the
   session ([ADR-0161](./0161-organization-is-typed-commands-behind-a-feature-bit.md)
   amendment; it replaced a client workflow over a fixed target set).
7. **Close is not kill.** Closing a view, a tab, or a client removes
   presentation only. A verb named kill terminates execution, explicitly.
   A verb named close never does.
8. **Intake.** The authority record designates one persistent ordinary
   session as its Intake. Resources created without a session go there.
   It cannot be deleted or made ephemeral while designated. Imported
   sessions keep their membership and arrangement and carry an
   `awaiting_organization` mark until someone resolves it. The Intake view
   is a permission-filtered projection over designated Intake sessions and
   separate marked sessions across connected authorities. Mark session organized
   clears only the import mark. Attention Inbox is a different projection.
   The wire `INBOX_RESOLVE` and raw `inbox` field retain their spellings
   for compatibility; neither names attention Inbox.
9. **Shared arrangements, local focus.** A session may have one shared
   arrangement, owned by its server and changed by revision. Personal
   arrangements live in client preferences and cannot change membership.
   Focus, selection, scroll, and native window geometry stay client-local
   ([ADR-0049](./0049-client-local-focus-and-advisory-attention.md)).
10. **Workspaces come last.** One authority owns each workspace. Deleting a
    workspace leaves its sessions. Ownership moves only by explicit
    transfer, and there are no queued offline edits. Workspaces ship once
    the client runtime holds sessions from several authorities; the names
    "workspace" in today's CLI, client layout model, FFI, and plugin
    manifests are renamed first so the word has one meaning.

Operations, wire encoding, and persistence are
[ADR-0161](./0161-organization-is-typed-commands-behind-a-feature-bit.md),
[ADR-0162](./0162-organization-records-persist-and-activations-reconcile.md),
and [ADR-0163](./0163-actors-and-delegated-agent-grants.md). This
supersedes ADR-0105, the keep-empty opt-in of ADR-0114, and the claim of
ADR-0015, ADR-0030, and `docs/spec/L2.md` that grouping is client
convention.

## Why

The server already enforces membership and lifetime; calling it client
convention made the spec describe a system that does not exist. A typed
session record lets the CLI, TUI, Cockpit, desktop, web, and agents agree
on what exists without each re-deriving it from metadata. Making lifetime a
declared property, instead of a consequence of which process exited last
or which client created the session, removes the default inversion between
clients. Separating close from kill gives every client the same safe
gesture. One session per resource keeps delete and terminate semantics
simple; links give workspaces their cross-machine reach without shared
ownership.

## Tradeoffs

- A server can hold empty persistent sessions indefinitely; `ls` has to
  show them with counts, or users will not see why records accumulate.
- Old clients still read one window per session. The server synthesizes
  it from the arrangement until the translation layer is deleted.
- Killing a whole session is one `SessionKill` (§6), so it ends work that
  joined meanwhile too; a caller that wants only the members it saw kills
  those resources instead.
- Workspaces are designed now but not built, so stage-1 surfaces must not
  claim cross-machine grouping.

## Alternatives

**Keep Window as a server entity.** It would keep tabs in two places, the
server registry and client layout metadata, which is the drift this
replaces.

**Make every session persistent.** Quick tmux-style sessions would never
clean themselves up, and `phux` with no arguments would leave records
behind.

**Overlapping membership.** A resource in several sessions makes delete,
terminate, and grant evaluation ambiguous. Saved filters cover the use
case without shared ownership.

**Leave grouping as metadata.** Metadata has no revisions, no refusals,
and no attribution, so concurrent edits by people and agents could not be
detected or explained.
