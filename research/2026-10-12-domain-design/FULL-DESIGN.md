---
audience: humans, contributors, agents
stability: scratch
last-reviewed: 2026-10-12
---

# Phux domain model: resources, sessions, workspaces, actors

**TL;DR.** Resources are things to work with. Sessions group them on one owning server. Workspaces link sessions across machines. Actors, human or agent, act on all of it through one permission and input-control model. Execution, presentation, and access are separate axes. Proposed design, not shipped behavior.

**Review draft 2 · 12 October 2026.** Draft 1 was one 600-line document; it is now split by altitude. This file is the model. The rest is detail that conforms to it:

| File | Holds |
|---|---|
| [DESIGN.md](DESIGN.md) | The one-picture summary. |
| This file | Nouns, identity, actors, lifecycles, the decisions. |
| [CONTRACTS.md](CONTRACTS.md) | Identifiers, records, grants, commands, results, events, the store, the wire, module seams. |
| [PLAN.md](PLAN.md) | Seven staged PR series with gates and proofs. |
| [BROWSER-KIND.md](BROWSER-KIND.md) | The Browser resource kind contract. |
| [MIGRATION.md](MIGRATION.md) | From today's tree to this model, including TUI and tmux verbs. |

Draft 2 changes: actors are a core noun (§04); session lifetime is explicit and archive is cut (§06); a workspace owns its own arrangement and its ownership can transfer (§07); browser detail moved to its own kind contract; failure-handling detail moved to the contracts, which are now written against today's code.

## 01. The product in one paragraph

Phux is where a person and their agents organize, reach, and operate work that keeps existing when nobody is looking at it. A terminal is one kind of work, not the definition of the product. The same work means the same thing on desktop, TUI, phone, web, CLI, and agent tools; presentation differs, identity and lifecycle do not.

```text
Workspace: Ship the product                         owned by Laptop
│
├── Session: Frontend ──────────────────────────── on Laptop
│   ├── Dev shell                                   Terminal, running
│   ├── Preview                                     Browser
│   └── claude                                      Terminal + Agent conversation
│
└── Session: Backend ───────────────────────────── on Mini
    ├── API shell                                   Terminal, running
    └── Integration tests                           Terminal, completed
```

The primary surface is a board of views. A session can be shown alone; a workspace shows its linked sessions together with small host labels. Tabs, stacks, and splits are ways to arrange views, not containers the user must create before working.

```text
┌─ Ship the product ──────────────────────────────────────────────┐
│ Frontend · Laptop                 Backend · Mini                │
│ ┌───────────────────────┐         ┌───────────────────────────┐ │
│ │ Preview · Browser     │         │ API shell · Terminal      │ │
│ │                       │         │                           │ │
│ └───────────────────────┘         └───────────────────────────┘ │
│ ┌───────────────────────┐         ┌───────────────────────────┐ │
│ │ claude · agent working│         │ Tests · Completed         │ │
│ └───────────────────────┘         └───────────────────────────┘ │
└─────────────────────────────────────────────────────────────────┘
```

Out of scope: moving running processes between hosts, consensus between servers, offline edit merging, the Objective/Run work coordinator, identical layouts on every screen.

## 02. Nouns

| Term | Is | Is not |
|---|---|---|
| Resource | An identified thing with a kind, lifecycle, and capabilities. | A rectangle, tab index, PID, or connection. |
| Session | A named group of resources on one owning server. | A connection, a cookie, or an agent conversation. |
| Workspace | A named set of links to sessions, possibly on several machines, plus its arrangement. | An owner of those sessions or an OS window. |
| Actor | Who issued an action: a human on a device, an agent, or an automation. | A connection or a client app. |
| View | One client's presentation of one resource. | The resource. |
| Arrangement | A saved layout of views: grid, split tree, stack, tabs. | Membership or ownership. |
| Authority | The persistent identity of one user's phux server. | A machine name or a running process. |
| Server instance | One run of an authority's server. | A durable identity. |
| Activation | One concrete execution of a resource. | The resource record. |

Users work with resources, sessions, and workspaces. Views and arrangements are real, but nobody has to create one before doing work. Authority, instance, and activation are internal words; the UI says "Mini," not "authority."

```text
Authority 1 ── owns ── * Session ── groups ── * Resource ── shown by ── * View
Authority 1 ── owns ── * Workspace ── links ── * Session   (any authority)
Workspace 1 ── has ── 1 Arrangement      Session 1 ── has ── 0..1 shared Arrangement
Actor     * ── acts on ── *  (through grants; §04)
```

- Every resource belongs to exactly one session on its owning authority. Zero views is fine.
- Moving a resource between sessions on the same authority changes membership, never execution.
- There is no cross-authority move. Working elsewhere means creating a new resource there.
- Project, directory, repository, and host label are attributes, not organization levels.

**D01.** A resource belongs to exactly one session. Overlapping collections, if ever needed, are saved filters, never shared ownership.

## 03. Identity

Names are labels. Commands, links, arrangements, and saved references resolve through IDs.

```text
SessionRef   = (authority_id, session_id)
ResourceRef  = (authority_id, resource_id)
WorkspaceRef = (authority_id, workspace_id)
LiveTarget   = (ResourceRef, activation_id, server_instance_id)
```

| Change | Identity outcome |
|---|---|
| Rename | Same ID. |
| New IP or route | Same IDs, after authenticated resolution. |
| Move resource between sessions | Same resource and activation. |
| Reopen or duplicate a view | Same resource, new local view. |
| Relaunch a finished resource | Same resource, new activation. |
| Server restart | Same authority and records, new instance. |
| Copy onto another authority | New IDs, with provenance back to the source. |
| Reinstall without recovering identity | New authority; old links do not silently rebind. |

**Fencing.** Input and execution commands carry the LiveTarget the client observed. If that activation or instance is gone, the command fails as stale. A saved view never types into a different process because a handle number was reused.

**D02.** One writer per authority. Each authority serializes edits to its own records with expected revisions. Cross-machine anything is links plus observation, never a distributed database.

## 04. Actors

Phux exists so a human and agents can work in the same terminals. The model treats that as a first-class relationship, not a resource kind.

### Three things called "agent"

| Thing | What it is | Example |
|---|---|---|
| The Terminal it runs in | A resource. Holds the PTY and the process. | `claude` in pane 3. |
| Agent conversation | A resource. The harness's event and transcript record. | The session log Claude Code writes. |
| Agent actor | A principal that issues phux commands. | That agent calling `phux send` on another terminal. |

An agent running in a terminal and doing nothing to phux is just a process. It becomes an actor only when it calls phux, and then it is subject to the same grants as anyone else.

### Actor identity

Every accepted command and every event that records a change carries an actor:

```text
Actor = (principal, device_or_client, on_behalf_of?)
```

- **Principal** is the authenticated identity from the connection: the human user, or an agent or automation credential scoped by the owner ([ADR-0098](../../docs/adr/0098-workload-proof-and-closed-scope-authority.md) closed scope authority; [ADR-0106](../../docs/adr/0106-identity-is-the-serving-user.md) `whoami`).
- **on_behalf_of** names the human whose grant an agent is exercising, when it is delegated rather than independently credentialed.
- Agent-declared metadata (name, harness, state, as in [ADR-0040](../../docs/adr/0040-agent-identity-metadata.md)) describes an agent. It is never a credential. A process cannot become an actor by writing a record.

### Grants

Grants are the five permission categories in [CONTRACTS.md §03](CONTRACTS.md#03-access): observe, interact, organize, create/launch, terminate/remove. They scope to a session or a single resource.

- The owner's human principal holds everything on their authority.
- An agent principal holds only what it was granted. A sensible default for an agent started inside a session: observe and interact on its own session, nothing else.
- Effective permission is the intersection of the actor's grant and owner policy. Delegation can narrow a grant and never widen it.

### Input control

Grants decide who may type. Control decides who is typing right now.

- Each interactive resource has at most one input lease holder ([ADR-0033](../../docs/adr/0033-input-authority-and-process-signals.md), [ADR-0127](../../docs/adr/0127-attach-roles-are-lease-intent.md)). Focus never takes the lease.
- **A human's deliberate takeover always wins.** An agent can request the lease and gets it when free; it never takes it from a human holder.
- When a human takes over from an agent, the agent sees a lease-lost event, not a silent stream of refused writes.
- Attention, such as an agent asking a question, is advisory ([ADR-0049](../../docs/adr/0049-client-local-focus-and-advisory-attention.md)). It can badge a view; it never moves another client's focus.

### Attribution

Each lease change opens a **lease epoch** with its holder recorded. Evidence for a terminal can answer "who was driving between 14:02 and 14:09," without per-keystroke logging. Organization events (create, move, terminate, delete) record the actor directly.

**D09.** Actors are a core noun. Every mutation is attributed; agents act only through grants; a human can always take the keyboard back.

## 05. Resource kinds

Every resource has stable identity, owner, session, name, kind, revision, provenance, lifecycle state, a capability descriptor, and kind-specific operations with typed results. A resource is not required to have a PTY, grid, process ID, working directory, or a visible surface.

| Kind | Owns | Typical view | Example capabilities |
|---|---|---|---|
| Terminal | PTY execution plus engine state. | Terminal grid. | Typed input, paste, resize, signals, text and history reads. |
| Browser | One owner-managed page. See [BROWSER-KIND.md](BROWSER-KIND.md). | Page view or screenshot. | Navigate, inspect, interact, under grants. |
| Agent conversation | A harness's conversation record. | Transcript or status card. | Read events and provenance. |
| Unknown | An opaque resource from a newer peer. | Name, status, "renderer unavailable." | Common capabilities only. |

Clients choose a renderer from the kind and negotiated capabilities, falling back to read-only or a named placeholder. Unknown kinds stay listed; no client treats them as terminals or drops them.

The wire spelling `AgentSession` stays for compatibility; its domain meaning is "agent conversation." It is not a Session in this model.

**D03.** The generic Resource has no terminal fields. Kinds bring their own facts and operations through versioned kind contracts.

## 06. Session lifecycle

A session is a persistent organization record. It is not evidence that anything is running.

```text
create ──► open (empty) ◄──► open (populated) ──► delete (only when empty) ──► tombstone
```

Empty and populated are derived facts, not modes.

| Action | Result |
|---|---|
| Create | An empty named record. No shell is spawned. |
| Create and launch | Session, membership, and launch intent commit together; execution starts asynchronously and can fail visibly. |
| Rename | Label changes; identity, links, grants, and views do not. |
| Add resource | Creates the resource and its membership in one transaction. |
| Move resource | Changes membership on the same owner; execution continues. |
| Close the last view | Nothing changes. |
| Last execution ends | Records and outcomes remain; the session remains. |
| Delete | Only when the session has no resources. Otherwise refused, with choices. |

The UI offers **Move resources, then delete** and **Terminate, remove, then delete** as separate confirmed actions. Neither hides behind a generic Delete. The second is a multi-step workflow with a fixed target set; work added during it survives, and the final delete then refuses.

Session labels are unique per authority among live sessions. Short names that match on more than one authority are refused, never guessed.

Archive is cut from this design. If it returns, it is a filter on a session attribute, not a lifecycle state with its own refusal rules.

**Lifetime is an explicit attribute, set at create and visible everywhere.** Named sessions are **persistent**: they survive empty. A quick unnamed session, the bare `phux` tmux-style start, is **ephemeral**: it is deleted in the same transaction that removes its last resource. Either can be switched later. That replaces today's keep-empty flag, which inverted the default and was set by whichever client happened to create the session.

Resources have a matching exit policy. An interactive shell is removed when it exits, so typing `exit` closes it as in tmux. A launched command or agent is retained with its outcome. Nothing is ever respawned implicitly.

**D04.** Session lifetime is persistent or ephemeral, chosen explicitly at create; named sessions default to persistent. Creating a session launches nothing unless the same command says so. A client's presentation choices never change lifetime.

## 07. Workspaces

A workspace is a named, ordered list of session links plus one shared arrangement over the resources in those sessions. It owns neither the sessions nor their execution.

```text
Workspace "Ship the product"          owner: Laptop
  link ──► Laptop / Frontend
  link ──► Mini   / Backend
  link ──► Linux  / Measurements
  arrangement: board of views over resources in those sessions
```

| Action | Result |
|---|---|
| Create | An empty workspace on the authority you are connected to. |
| Link or unlink a session | Adds or removes a SessionRef. Repeats are idempotent. Unlink also removes that session's arrangement entries. |
| Delete | Removes the workspace and its arrangement. Sessions and resources are untouched. |
| Open | Observes reachable linked sessions concurrently and restores views. |
| Linked host offline | Its sessions show last-known state; the rest work normally. |
| Workspace owner offline | Cached read-only copy, labeled stale. Edits wait. |
| Linked session deleted | Shows a deleted link. Never rebinds to a new session with the same name. |
| Transfer ownership | Explicit, with both authorities reachable. New WorkspaceRef on the new owner; the old one becomes a forwarding tombstone. |

Linking a session grants nothing. Each viewer needs their own grant on each linked session; inaccessible links show a generic placeholder.

Actions across several owners return one result per owner. There is no cross-host atomic action and no fleet-wide event order. A timeout is reported as unknown, not as failure.

**Why a workspace has a server owner.** A workspace is shared across your devices and readable by agents, so it needs one authoritative copy with revisions. The cost: if the owner sleeps, edits wait. Mitigation: create it on an always-on machine, or transfer it there. The UI defaults to the current machine and keeps the choice out of the way.

**Staging.** Resources, sessions, and actors ship first. Workspaces ship once the client runtime holds sessions from several authorities at once. The model is fixed now so the first stage does not paint it into a corner.

**D05.** One authority owns each workspace. No queued offline edits. Ownership moves only by explicit transfer.

## 08. Views and arrangements

```text
                         Resource: Dev shell (Laptop)
                          /                    \
                 Desktop view                 Phone view
                 wide viewport                narrow viewport
                 local focus                  local focus
```

A view has a local ID, a ResourceRef, viewport state, and an attachment. Views carry no kill authority. A second view is never a second PTY or page. The owner arbitrates geometry: mounting an observer view does not resize somebody else's job.

An arrangement is a layout document with stable entry IDs pointing at resources. A session may have one shared arrangement; a workspace always has one.

- Shared arrangements live on the owning authority and change by revision. Conflicting edits are refused and reloaded.
- Personal arrangements live in client preferences and cannot change membership.
- Focus, selection, scroll, zoom, and native window geometry are always client-local.
- Clients adapt a shared arrangement to their screen without publishing the adaptation.
- Removed or inaccessible resources become named placeholders. Holding an entry never authorizes attachment.

There is no execution-owning Window or Tab entity. Clients offer tabs and splits as presentation. A shared "window" from today's model becomes an arrangement group.

| Intent | Desktop | TUI | Mobile and web | CLI and agents |
|---|---|---|---|---|
| Open resource | Mount view | Mount view or fallback | Mount view | Subscribe |
| Close view | Remove presentation | Leave presentation | Remove presentation | Detach |
| Terminate resource | Explicit action | Explicit action | Explicit action | Typed command |
| Unsupported kind | Named placeholder | Named fallback | Named placeholder | Kind and capabilities |
| Host unreachable | Last-known, writes disabled | Same | Same | Reachability and freshness |

A client-local PTY that ends with its window is allowed, but it is labeled **local only** and is not a shared resource until explicitly published to an owner.

**D06.** Shared arrangements with local adaptation are the default for sessions and workspaces. Personal arrangements are opt-in. Focus is never shared.

## 09. Execution and recovery

Existence, execution, reachability, and presentation are independent:

| Axis | States |
|---|---|
| Record | Present, tombstoned. |
| Execution | Dormant, starting, running, stopping, completed, failed, lost. |
| Reachability | Online, reconnecting, offline, unauthorized, unsupported. |
| Presentation | Visible, hidden, unmounted. |

```text
Dormant ─ launch ─► Starting ─► Running ─┬─ exits ─────────► Completed
                       │                  ├─ terminate ─► Stopping ─► Completed
                       └─ fails ─► Failed └─ owner loses track ─► Lost

Completed | Failed | (Lost, once proven inactive) ── relaunch ──► new activation
```

- Completion records the real outcome: exit status, signal, kind-specific result. Force-kill is an outcome, not success.
- A resource has at most one current activation unless its kind says otherwise.
- **Lost is not stopped.** The process may still be running after the owner loses its handle. Removal and relaunch are refused until inactivity is proven. An explicitly authorized abandon records the unresolved activation and releases management without claiming the process ended.
- Offline describes the observer, not the execution.

| Event | Records | Execution | Evidence |
|---|---|---|---|
| Close client | Survive | Continues | Kept per policy |
| Network drop | Survive | Continues, shown last-known | Cached copy shows age and gaps |
| Graceful upgrade | Survive | Survives only with verified handover | Explicit continuity result |
| Crash or reboot | Survive | Not promised; reconciled as recovered or lost | Only committed evidence |
| Relaunch | Same resource | New activation | Prior outcome stays separate |

On startup the authority reconciles recorded activations with real executor handles. It never reruns arbitrary commands or claims a new process is the old one.

Record retention and content retention are separate. When output has expired, the view says **history expired** instead of showing an empty buffer.

**D07.** Records and outcomes persist. Surviving a crash and restarting automatically are not promised; any kind that wants them must define an explicit policy with real recovery evidence.

## 10. Architecture

```text
Desktop   Cockpit   TUI   Web   Mobile   CLI / agents
   └─────────┴───────┴────┴──────┴──────────┘
                       │
   Shared client runtime + lossless projections (one connection stack)
                       │
   Versioned organization surface on the authenticated runtime
                       │
   Owner server: actor auth, grants, command admission, transactions
                       │
   CORE DOMAIN (no IO)              RESOURCE EXECUTORS
   identity, membership,            Terminal / Browser / Harness
   lifecycle, validation            execution, streams, evidence
                       │
   Persistent organization store + bounded event and receipt log
```

The core domain takes state and commands and returns validated transitions and required effects; it does no IO. The server commits transitions and dispatches effects to executors. Executors report facts back. One implementation of the rules serves every client. Module boundaries and the protocol shape are in [CONTRACTS.md](CONTRACTS.md).

The durable work coordinator ([ADR-0092](../../docs/adr/0092-durable-work-coordinator-authority.md)) stays separate. Its Objectives, Runs, and WorkSessions can reference ResourceRef and SessionRef without taking execution authority from owners.

**D08.** Typed organization operations replace session lifecycle hidden in metadata keys. Organization and resource streaming stay separate even on a shared connection.

## 11. Decisions

| ID | Proposed | Choosing differently means |
|---|---|---|
| D01 | One session per resource; workspaces link many sessions. | Shared membership needs non-owning links and much harder delete semantics. |
| D02 | One writer per authority; stable IDs plus live-target fencing. | Multi-master needs a consensus or conflict model this design excludes. |
| D03 | No terminal fields on the generic Resource. | Terminal-shaped generality blocks browser and nonvisual kinds. |
| D04 | Lifetime is explicit: named sessions persist when empty, quick sessions are ephemeral. Create launches nothing implicitly. | Lifetime depends on process and client behavior again. |
| D05 | One owner per workspace; explicit transfer; no offline edits. | Offline editing needs merge, conflict UI, and credential-aware replay. |
| D06 | Shared arrangements, local adaptation, local focus. | Personal-only loses continuity across devices; shared focus causes fights. |
| D07 | Records persist; processes are not promised to survive a crash. | Restartable execution needs per-kind policy and recovery evidence. |
| D08 | Typed organization operations; no magic metadata lifecycle. | The spec keeps claiming metadata is opaque while the server intercepts it. |
| D09 | Actors are core; every mutation is attributed; humans can always take the keyboard. | Agents act as anonymous clients and nobody can say who did what. |

### Settled in draft 2

| Question from draft 1 | Answer |
|---|---|
| Board or tabs as the primary surface? | Board. Tabs are an arrangement choice. |
| Workspace owner visible? | Defaults to the current machine; changeable, kept out of the way. |
| Persistent empty sessions for automation too? | Named ones, yes. Quick and scripted sessions can be ephemeral, explicitly. |
| Browser: one page or a context of pages? | One page per resource. Profiles are configuration. |
| Shared or personal arrangements by default? | Shared, adapted locally. |
| Archive in the first product? | Cut. |

### Still open

- Default agent grant when an agent is launched inside a session: own session observe and interact (proposed), or own terminal only?
- Should an agent holding the lease be able to hand it to a specific human, or only release it?
- Does the TUI keep tmux's "kill window kills its panes"? [MIGRATION.md](MIGRATION.md#03-tui-and-tmux-verbs) proposes a confirmed terminate of the group, never an implied one.

**Review format:** cite a section or decision, then state the change. "D09: agents get own-terminal-only by default." "§06: delete should offer move first."
