---
audience: contributors, agents
stability: scratch
last-reviewed: 2026-10-12
---

# Domain contracts

**TL;DR.** The implementable surface of the [domain model](FULL-DESIGN.md): identifiers, records, grants, commands, results, events, the organization store, the wire capability, and which crate owns what. Written against today's tree; every "today" claim cites the code it replaces. Exact byte layouts land in `docs/spec/` with the wire PR, not here.

## 01. Identifiers

Two families: persistent **keys** name records forever; live **handles** name current execution and die with the server instance.

| Name | Type | Scope | Allocation |
|---|---|---|---|
| `AuthorityId` | 16 bytes | Global | Random, minted when the org store is first created; never changes. |
| `SessionKey` | 16 bytes | Per authority | Random at create. |
| `ResourceKey` | 16 bytes | Per authority | Random at create. |
| `WorkspaceKey` | 16 bytes | Per authority | Random at create. |
| `ArrangementKey` | 16 bytes | Per authority | Random at create. |
| `ActivationId` | u64 | Per resource | Monotonic from 1 per resource; persisted. |
| `LeaseEpoch` | u64 | Per resource | Monotonic; bumps on every input-lease change. |
| `OperationId` | 16 bytes | Per caller | Caller-generated. Same shape as ADR-0053's `APPLY_INPUT` id. |
| `Revision` | u64 | Per record | Bumps on every committed change to that record. |

Live handles stay as they are: wire `SessionId`/`ResourceId` are `u32` per server instance, never reused, minted by `IdBridge` (`crates/phux-protocol/src/ids.rs:34-50`, `crates/phux-server/src/id_bridge.rs:9-12`); `ServerInstance([u8;16])` scopes them (ADR-0109, `ids.rs:289-297`). A live target is therefore:

```rust
struct LiveTarget { resource: wire::ResourceId, instance: ServerInstance, activation: ActivationId }
```

Keys never appear in terminal I/O frames; handles never appear in the organization store. `GetOrganization` returns both so a client can go from one to the other.

Refs across machines prefix the authority: `SessionRef = (AuthorityId, SessionKey)`, and likewise for resources and workspaces. Text form, for CLI output and logs: lowercase base32 of the 16 bytes, shortened to the shortest unique prefix of at least 6 characters for display, always accepted in full.

## 02. Records

These live in `phux-core` and are IO-free. Field lists are complete for stage 1; new kinds add facets, never new top-level fields.

```rust
struct Session {
    key: SessionKey,
    name: String,              // unique among live sessions on this authority
    lifetime: SessionLifetime, // Persistent | Ephemeral
    awaiting_organization: bool, // imported group appears in Inbox until explicitly organized
    members: Vec<ResourceKey>, // order = creation order; presentation order lives in arrangements
    arrangement: Option<ArrangementKey>,
    attrs: Attrs,              // project, directory, host label: descriptive only
    created: Stamp,
    revision: Revision,
}

enum SessionLifetime {
    Persistent, // survives empty (the default for named sessions)
    Ephemeral,  // deleted in the same transaction that removes its last resource
}

struct Resource {
    key: ResourceKey,
    session: SessionKey,
    name: Option<String>,
    kind: ResourceKind,        // non_exhaustive: Terminal | AgentConversation | Browser | Unknown(String)
    provenance: Provenance,    // created_by: Actor, opener/parent: Option<ResourceKey>
    launch: Option<LaunchSpec>,// kind-specific; how a relaunch starts
    on_exit: ExitPolicy,       // Remove | Retain
    activation: Option<Activation>, // current or most recent
    revision: Revision,
}

struct Activation {
    id: ActivationId,
    instance: ServerInstance,  // which server run started it
    state: ExecState,          // Starting | Running | Stopping | Completed | Failed | Lost | Abandoned
    started: Stamp,
    ended: Option<Stamp>,
    outcome: Option<Outcome>,  // exit status, signal, kind result, or Unobserved
    host_proc: Option<ProcIdentity>, // Terminal: pid, pgid, process start time
}

struct Arrangement {
    key: ArrangementKey,
    owner: ArrangementOwner,   // Session(SessionKey) | Workspace(WorkspaceKey)
    groups: Vec<Group>,        // today's tabs; each holds one split tree
    revision: Revision,
}
struct Group { id: [u8; 16], name: Option<String>, tree: LayoutNode<EntryRef> }
struct EntryRef { entry: [u8; 16], resource: ResourceRef }

struct Workspace {
    key: WorkspaceKey,
    name: String,
    links: Vec<SessionRef>,
    arrangement: ArrangementKey,
    revision: Revision,
}

struct Actor {
    principal: PrincipalId,    // owner human, or an owner-registered credential
    client: Option<ClientName>,// from today's ActorRef.client_name
    on_behalf_of: Option<PrincipalId>,
}
```

`LayoutNode` moves out of `crates/phux-core/src/window.rs:40-56` into `arrangement.rs` and becomes generic over its leaf. Today's `Window` (`window.rs:21-32`) and `Session.windows`/`active` (`session.rs:57-71`) are deleted, not renamed.

`ExitPolicy` defaults: an interactive shell started by `new`, `spawn`, or a split is `Remove`, so exiting a shell closes it, as in tmux. Anything started by `launch`, an agent, or with `--keep` is `Retain`, which absorbs ADR-0124's exited-pane retention. Respawning the session's last shell on exit (`crates/phux-server/src/state/sessions.rs:98-110`) is deleted: no implicit launch.

The authority record designates one persistent `inbox_session: SessionKey`. It is a Session, not a new entity kind or a name-based convention. It cannot be deleted or switched to Ephemeral while designated. `awaiting_organization` is a shared intake fact on imported sessions; clearing it removes the intake presentation, not membership or execution. Inbox is a permission-filtered projection over designated Inbox sessions and those marked imported groups, aggregated by the runtime across connected authorities.

## 03. Access

The five grant categories map onto the existing closed scope matrix (ADR-0098, `crates/phux-protocol/src/kinds.rs:55-68`). Two verbs are new.

| Category | Scope verbs | Covers |
|---|---|---|
| Observe | `Inventory`, `Observe` | Read records, mount read-only views, read evidence. |
| Interact | `Input`, `Signal` (non-terminating signals) | Typed input, paste, resize requests, kind actions. |
| Organize | **`Organize`** (new) | Rename, move membership, edit arrangements, link and unlink. |
| Create | `Create`, `Bind` | Create records and launch execution. |
| Terminate | **`Terminate`** (new) | Terminate, remove, delete, terminating signals. |

Selectors keep their shape (`scope.rs:303-316`). `Group(u32)` already means a session (`policy/enforce.rs:6-7`); it is redefined to resolve by current membership, so moving a resource out of a session drops access that came from the session in the same transaction (§07).

Principals:

- The owner principal is the serving user (ADR-0106). It holds every verb on its authority.
- Agent and automation principals are owner-registered credentials with a scope set, as workload clients are today. `phux agent start` and launches inside a session mint a delegated credential scoped to **Observe + Interact on its own session**, `on_behalf_of` the launching human. Delegation can only narrow.
- Agent metadata (`phux.agent/v1`, ADR-0040) is descriptive. It never grants anything.

Input lease (`crates/phux-server/src/state/lease_table.rs:135-164`) gains one rule: a lease held by a human principal cannot be taken by a non-human principal. `DELIBERATE` takeover by a human always succeeds and emits `LeaseChanged` with the new epoch.

## 04. Commands

All ride the existing `COMMAND` envelope (`crates/phux-protocol/src/wire/frame/command.rs:183-430`) behind the `Organization` capability (§09). Every command carries an `OperationId`; every command that edits a record carries `expected: Revision`.

| Command | Arguments | Grant | Effect |
|---|---|---|---|
| `GetOrganization` | session/workspace/intake filter, page size, cursor | Observe | Bounded page: authorized records, revisions, live handles, aggregate counts, snapshot revision and journal cursor. Cursors are source/filter/principal-scoped; expired snapshots require refresh. |
| `SessionCreate` | name, lifetime, attrs, optional `launch` | Create (+Create for launch) | Session, plus resource and launch intent when `launch` is set, in one transaction. `Ephemeral` requires `launch`; an ephemeral session is never empty. |
| `SessionRename` | session, expected, name | Organize | Label only. |
| `SessionSetLifetime` | session, expected, lifetime | Organize | Persistent and Ephemeral; Ephemeral on an empty session deletes it. |
| `SessionDelete` | session, expected | Terminate | Only when `members` is empty; else `NotEmpty`. |
| `ResourceCreate` | session, kind, launch, name, on_exit | Create | Record, membership, and launch intent in one transaction. |
| `ResourceMove` | resource, from (expected), to (expected) | Organize on both | Membership only. Replaces `MoveResource` 0x2A semantics; the frame stays as a compat alias. |
| `ResourceMoveMany` | fixed resource keys, source revisions, destination (expected) | Organize on every affected session | Bounded atomic same-owner membership move. Validate each resource/kind/access; no restart. A refused target refuses the whole owner batch. |
| `InboxResolve` | imported session, expected | Organize | Clears `awaiting_organization`; leaves the session/resources intact. Returns prior intake state in its organization receipt. |
| `OrganizationUndo` | original operation, expected resulting revisions | Original operation's current grants | Applies the receipt's inverse for supported membership/link/intake edits. Refuses expired receipts, changed state, or revoked authority; never reverses execution. |
| `ResourceRename` | resource, expected, name | Organize | Label only. |
| `ResourceTerminate` | live target, mode (graceful or force) | Terminate | Delegated to the executor; result is accepted, then events report the outcome. |
| `ResourceRemove` | resource, expected | Terminate | Only when execution is proven inactive; writes a tombstone. |
| `ResourceRelaunch` | resource, expected | Create | New activation from `launch`; refused while one is current or Lost. |
| `ResourceAbandon` | resource, expected, activation | Terminate (owner only) | Lost to Abandoned: releases management, records the unresolved activation. |
| `ArrangementGet` | arrangement | Observe | Document and revision. |
| `ArrangementReplace` | arrangement, expected, document | Organize | Validated: session arrangements may reference only members. |
| `WorkspaceCreate`, `Rename`, `Delete` | as for sessions | Organize, Create, Terminate | Stage 6. |
| `WorkspaceLink`, `Unlink` | workspace, expected, SessionRef | Organize | Idempotent. |
| `WorkspaceTransfer` | workspace, target authority | Organize on both | Stage 6. Export, import with provenance, forwarding tombstone. |

Kind operations keep their existing commands (`AcquireInput`, `SignalTerminal`, `ApplyInput`, and so on), now addressed by `LiveTarget`. A stale activation fails `Stale` before any effect.

"Kill a session" is a client workflow, not a command: snapshot members and their live targets, `ResourceTerminate` each, `ResourceRemove` each once inactive, then `SessionDelete` with the revision seen at the start. Work added in the meantime survives and makes the final delete fail `NotEmpty`. `phux kill <session>` keeps its meaning by running this workflow.

Drag/drop, keyboard move, bulk selection, and agent organization all submit these same typed operations. Human clients retain the organization receipt for visible undo; agents receive the same receipt. Selection never expands during retries. Workspace linking and remote intake resolution have separate owner results; failure of intake cleanup cannot undo a successfully committed link or make imported work disappear. Batches have advertised bounds; oversized selection is explicitly chunked with per-batch results, never falsely reported as one atomic operation.

## 05. Results

```rust
enum CommandResult<T> {
    Ok { value: T, revision: Option<Revision>, operation: OperationId },
    Accepted { operation: OperationId },  // execution effects follow as events
    Refused(Refusal),
}
enum Refusal {
    Unauthorized { verb: ScopeVerb },
    NotFound,
    Conflict { current: Revision },  // expected revision mismatch
    Stale { current: Option<ActivationId> },
    NotEmpty { members: u32 },
    Unsupported { capability: String },
    InvalidArgument { field: &'static str, reason: String },
    NameTaken,
    ExecutionUncertain,              // Lost activation blocks remove or relaunch
    LeaseHeld { holder: Actor },
    DuplicateOperation,              // same OperationId, different payload
}
```

Network-level timeouts are not results. A client that loses a reply retries with the same `OperationId` (§06), or reports **outcome unknown**.

## 06. Retries

- The server keeps a receipt per `OperationId` for 24 hours or 10,000 receipts, whichever comes first, in the org store.
- A retry with the same id and identical payload returns the original result without repeating effects. Dedup is checked before revision and staleness checks.
- Same id with a different payload: `DuplicateOperation`.
- Authorization is rechecked before a stored receipt is returned.
- Past the horizon, the runtime never resubmits on its own; it reconciles from `GetOrganization` and tells the caller the outcome is unknown.
- Records keep the `OperationId` that created them, so a late retry can still be matched against a resource that exists.

Supported organization receipts also record the inverse change and the resulting affected revisions. Undo is itself an idempotent operation with a new OperationId, checked against current state and permissions. Runtime exposes the actual available receipt/undo horizon rather than assuming low-volume usage keeps receipts alive for 24 hours. Execution operations never acquire an inverse merely because their receipt exists.

## 07. Events

Organization events join the existing journal (ADR-0123; `SUBSCRIBE_EVENTS` 0x41, `EVENT` 0xB3) as new `AgentEvent` variants, carrying today's `EventStamp{seq, ts_ms, actor, operation_id}` (`crates/phux-protocol/src/wire/frame/payload.rs:55-64`). `ActorRef` gains `principal` and `on_behalf_of`.

| Event | Payload |
|---|---|
| `SessionCreated`, `SessionRenamed`, `SessionDeleted` | key, name, revision |
| `SessionIntakeChanged` | session key, awaiting_organization, revision; emitted by InboxResolve and undo restoring intake state |
| `MembershipChanged` | resource, from, to, revisions |
| `ResourceCreated`, `ResourceRemoved` | key, session, kind |
| `ActivationChanged` | resource, activation, state, outcome |
| `ArrangementChanged` | key, revision |
| `LeaseChanged` | resource, epoch, holder |
| `WorkspaceChanged` | key, revision (stage 6) |

A gap or an expired cursor (`JournalGap`) means: refetch `GetOrganization`. Order is per authority only. Terminal bytes stay on their own streams and bootstrap (ADR-0013, ADR-0070).

Organization undo emits the same committed membership/link/intake events as the equivalent direct edit, correlated to its own OperationId. Every observing client therefore reconciles Inbox removal/restoration incrementally; intake state cannot be a private frontend cache.

Intercepted metadata broadcasts (`phux.session.created/v1`, and the rest) keep flowing for old clients until stage 4 retires them.

## 08. Organization store

**Format.** A versioned transactional SQLite organization store, `<state_dir>/org/v1.sqlite3` (`state_dir` is `$XDG_STATE_HOME/phux`), behind `OrgStore`. Use incremental row updates and indexes for owner/session membership, state, and intake queries; never rewrite all records for a single move. Keep one authority writer and bounded transactions, not distributed storage. Resource output stays in its kind-specific stream/evidence storage.

**Scale contract.** People and bot swarms use the same model. Design for thousands of resources and sustained automation mutation rates without assuming hundreds of records or human pacing. Inventory is paginated/filterable, counts are queryable without fetching every record, and subscriptions carry incremental changes. Large arrangements and batches are bounded; runtime exposes partial/paged results instead of silently omitting work. Storage/query/UI performance gates cover large intake, membership moves, receipt churn, and concurrent observation. This is not a promise of unlimited resources or executors; admission and backpressure remain explicit.

**Dependency justification.** Transactional persistence is warranted by atomic membership/bulk edits, durable receipts, and indexed inventory under automation load, not by speculative distribution. At implementation, reuse a suitable installed/workspace SQLite binding if available; adding a binding requires a scoped dependency/security review. JSON remains a portable archive format, not the live organization database.

**Contents.** `AuthorityId`, its designated Inbox session, sessions/intake facts, resources with activations, arrangements, workspaces, tombstones (kept 30 days), receipts (§06), migration receipts, and the last journal sequence number.

**Write discipline.** Commit the transition, associated receipts, and journal position in one durable owner transaction; then apply committed state in memory, acknowledge, and publish events. If commit fails, refuse and leave authoritative memory unchanged. Activation changes are persisted too. Recovery reconciles committed execution intents; a database transaction does not make OS launch/termination atomic. Schema migrations are versioned, backed up, and transactional; the owner refuses unsupported store versions rather than guessing.

**Startup reconciliation.** For each activation recorded `Starting`, `Running`, or `Stopping` under a previous `ServerInstance`:

1. If a live-upgrade handover (ADR-0032 `StateBlob`) vouches for it, rebind it: same activation, new handle.
2. Otherwise, check `ProcIdentity`. If the pid exists **and** its start time matches, the process outlived us: mark `Lost`. The owner can terminate the orphan by pgid after rechecking start time, or abandon it.
3. If the pid is gone or its start time differs, mark `Completed` with outcome `Unobserved`.
4. Never relaunch anything automatically. `ExitPolicy::Remove` resources ended by the crash are kept as records so the user sees what was there, flagged ended-by-restart; one `phux restore` relaunches them ([MIGRATION.md §04](MIGRATION.md#04-cli)).

Native agent restore (ADR-0068, ADR-0151) becomes part of the Terminal `LaunchSpec`: relaunching a terminal that hosted an agent resumes that agent's conversation.

**Server lifetime.** The server stays alive while any activation is live or any client is connected; otherwise it may exit. Records survive in the store either way. This replaces the "no sessions left" exit (`crates/phux-server/src/state/bindings.rs:192-196`).

**Export.** `phux archive export` writes a portable copy without live handles, credentials, or receipts. Import onto another authority mints new keys and records provenance.

## 09. Wire surface

- New capability bit `ServerFeatureExt::Organization` (`crates/phux-protocol/src/caps.rs:1058-1068`). A server sets it only when it enforces everything in this document for stage 3.
- New commands are `Command` variants under the existing envelope. No frame tag is reused or reinterpreted.
- Keys on the wire are 16 raw bytes. Names are UTF-8, at most 256 bytes. Arrangement documents are bounded at 64 KiB.
- Compatibility: while old clients exist, the server keeps translating the intercepted keys (`phux.session.create/v1`, `phux.session.name/v1`, `phux.session.keep_empty/v1`; `crates/phux-server/src/runtime/client.rs:4848-4859`) into the new commands, attributed to the sending client. `keep_empty` maps to `lifetime`. The TUI layout key `phux.tui.layout/v1/<session-id>` is translated to and from the session's arrangement.
- A client that sees no `Organization` bit treats organization as read-only and says so.

## 10. Module seams

| Crate | Owns after the change | Never owns |
|---|---|---|
| `phux-core` | Records, invariants, `apply(state, command, actor) -> Result<(Transition, Vec<Effect>), Refusal>`. Pure; the clock is passed in. | IO, wire bytes, PTYs. |
| `phux-protocol` | Command, result, and event encodings; the capability bit; scope verbs. | Domain rules. |
| `phux-server` | Authentication, grants, admission, `OrgStore`, committing transitions, dispatching effects to executors, reconciliation. | Client focus, presentation. |
| Resource executors (`phux-server/src/resource/*`) | Execution, `ProcIdentity`, outcomes, streams. | Membership. |
| `phux-client-runtime` | Paged snapshot/journal reconciliation, permission-filtered multi-owner Inbox, operation/undo correlation, retries, one connection stack. | Its own idea of what exists, unbounded inventory/attachment fan-out. |
| `phux-client-ffi` | Lossless projection of every record and state above. | Dropping fields. Today it drops `keep_empty`, windows, agent sessions, and dims (`crates/phux-client-ffi/src/projection/topology.rs:14-89`). |
| Clients (`phux-tui`, desktop, Cockpit, web, mobile) | Views, local arrangement adaptation, focus, controls. | Lifecycle rules, kill-on-close. |
| `phux-client-core` | Layout tree editing for arrangements, without tokio or ratatui. | Session membership. |

## 11. Connection and onboarding

Unchanged by this design, except: connection setup produces an authenticated principal and grant before any inventory is shown; discovery returns candidates, never inventory; progress names the real stage (finding host, checking trust, authenticating, loading work, mounting view). Target: on an already trusted, provisioned host, picking a saved session reaches an interactive view in under five seconds, measured, not asserted.
