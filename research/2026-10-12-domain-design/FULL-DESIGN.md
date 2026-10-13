---
audience: humans, contributors, agents
stability: scratch
last-reviewed: 2026-10-12
---

# Phux from first principles: resources, sessions, and workspaces

**TL;DR.** Phux organizes typed resources into sessions and links sessions into cross-machine workspaces. Execution ownership, presentation, and access are separate; every client consumes the same authoritative model. This is a complete proposed design for annotation, not approved architecture or shipped behavior.

For the one-picture overview, read [the visual summary](DESIGN.md). This document preserves the full technical proposal.

**Review draft 1 · 12 October 2026.** The design deliberately starts from the desired product, not from today's structs. All defaults below are proposals. Existing protocol guarantees remain authoritative until a reviewed migration changes them. Numbered sections and decision IDs are stable places to attach comments.

Read sections 01–02 for the picture; 05–08 for what happens to your work; 12 for the core architecture; 15 for the choices worth challenging. The HTML companion is generated from this Markdown, which is the editable source.

## 01. Product model

Phux is a place to organize, access, and operate work that can keep existing when you leave its interface. A terminal is one kind of work resource, not the definition of the product.

There are three primary product nouns:

```text
RESOURCE                  SESSION                  WORKSPACE
A thing to work with      A group of resources     Linked sessions
Terminal, browser, …      One owning authority     Across authorities/machines
```

Sessions are the highest level of resource organization on an owning authority. Workspaces organize the relationship between sessions, not another copy of their resources.

```text
Workspace: Ship the product                         persisted on Laptop
│
├── Session: Frontend ──────────────────────────── owned by Laptop / me
│   ├── Resource: Dev shell                         Terminal
│   ├── Resource: Preview                           Browser
│   └── Resource: Build output                      Terminal, completed
│
└── Session: Backend ───────────────────────────── owned by Mini / me
    ├── Resource: API shell                         Terminal
    └── Resource: Integration tests                 Terminal
```

Opening the workspace produces one navigable surface. It does not imply that both machines are online, that their processes migrated, or that the workspace owns those processes. Ownership and status remain visible without making the user administer a fleet dashboard.

The primary interface is a board of resource views. A session can be shown alone. A workspace can show linked sessions together, with small session/host labels. Tabs, stacks, splits, and full-screen presentation are ways to arrange views, not required steps in a containment hierarchy.

```text
┌─ Ship the product ──────────────────────────────────────────────┐
│ Frontend · Laptop                 Backend · Mini                │
│ ┌───────────────────────┐         ┌───────────────────────────┐ │
│ │ Preview · Browser     │         │ API shell · Terminal      │ │
│ │                       │         │                           │ │
│ └───────────────────────┘         └───────────────────────────┘ │
│ ┌───────────────────────┐         ┌───────────────────────────┐ │
│ │ Dev shell · Terminal  │         │ Tests · Completed          │ │
│ └───────────────────────┘         └───────────────────────────┘ │
└─────────────────────────────────────────────────────────────────┘
```

**Product promise:** the same resource and session mean the same thing in desktop, TUI, phone, web, CLI, and agent tools. Presentation can differ; identity, operations, and lifecycle cannot.

**Not part of this redesign:** migrating running processes between hosts, a distributed consensus system, arbitrary Objective/Run orchestration, an identical layout on every screen, or implementing every possible resource kind. The model must allow those concerns to integrate without pretending they already exist.

## 02. Vocabulary and relationships

### Domain glossary

| Term | Definition | Not this |
|---|---|---|
| Resource | An independently identified thing with a kind, lifecycle, and capabilities. | A rectangle, tab index, PID, or network connection. |
| Session | An authoritative named group of resources. Membership is independent of their arrangement. | A browser cookie session, agent conversation, or transport lifetime. |
| Workspace | An authoritative named set of links to sessions, potentially across machines. | A duplicate owner of those sessions or an OS window. |
| View | A presentation of one resource, with client-local interaction and viewport state. | The resource itself. |
| Arrangement | A saved organization of views: grid, split tree, stack, or tabbed presentation. | Resource membership or execution ownership. |
| Host | A machine reachable through one or more network routes. | An IP address or an authenticated principal. |
| Authority | The persistent identity of a per-user Phux owner responsible for a set of records and commands. | The machine name or the current server process. |
| Server instance | One running incarnation of an authority's Phux server. | A durable owner identity. |
| Connection | A temporary authenticated communication channel. | A session. |
| Attachment | A client's subscription/access relationship to a resource or session. | Exclusive ownership, permission to destroy work, or focus. |
| Activation | One concrete execution of a resource on its owner. | The persistent resource record. |

Views and arrangements are real implementation concepts, but they are not new levels the user must create before doing work. The user works with resources, sessions, and workspaces.

### Cardinality and containment

```text
Authority 1 ── owns ── * Session
Session   1 ── groups ─ * Resource
Authority 1 ── owns ── * Workspace
Workspace * ── links ── * Session
Resource  1 ── shown by * View
Client    1 ── owns ── * View
```

- Every published resource belongs to exactly one session on its owning authority. Zero views is valid. A nonvisual resource can still be a session member.
- Every session belongs to exactly one authority and may be linked from zero or many workspaces.
- A workspace may link any authorized sessions, including several on the same authority. It may be empty.
- Moving a resource between sessions on the same authority is a membership transaction, not an execution restart.
- Cross-authority movement is not a membership edit. To work elsewhere, explicitly create a new resource there. There is no pretend process migration.
- A native app window may show several sessions. A session may be shown in several native windows. Neither relationship changes ownership.
- A project, directory, host label, or repository is descriptive context, not another required organization level.

**Proposal D01:** single session membership for each resource. If the product needs overlapping resource collections later, add saved filters or references; do not make destructive ownership ambiguous.

## 03. Identity and authority

Names are labels. All commands, arrangements, links, and saved references resolve through IDs.

```text
SessionRef  = (authority_id, session_id)
ResourceRef = (authority_id, resource_id)
WorkspaceRef = (authority_id, workspace_id)

LiveTarget = (ResourceRef, activation_id, server_instance_id)
```

Authority and domain IDs are persisted opaque identifiers, unique within their documented scopes. Activation and instance IDs identify live execution and are never inferred from names. The byte encoding and allocation scheme belong in the implementation specification, not in this glossary.

| Change | Identity outcome |
|---|---|
| Rename a session/resource | Same domain ID. |
| Change IP address or route | Same authority and domain IDs, after authenticated resolution. |
| Move a resource between sessions on its owner | Same resource and activation IDs. |
| Reopen or duplicate a view | Same resource; new local view identity. |
| Relaunch a completed/lost resource | Same resource record; new activation ID. |
| Server restart | Same persisted authority and records; new instance ID. |
| Restore/import a copy onto another authority | New identities, with explicit provenance to the source. |
| Reinstall without recovering owner identity | New authority; no silent adoption of old links. |

A fingerprint proves an authority/credential under the trust policy; it is not a resource name. An address is a route; it is not trust. A host label can be edited without rebinding anything.

**Command fencing:** inputs and execution mutations target the exact activation and instance the client observed. If that execution has been replaced, the command fails as stale. A stored view never types into a different process just because a numeric handle was reused.

Organizational edits target stable IDs and expected revisions. The owning authority serializes edits. Another client can observe and request changes, but cannot invent authoritative records in its own cache.

**Proposal D02:** one writer per authority, no multi-master editing. Cross-machine workspaces are session links plus federated observation, not a distributed database of process ownership.

Recovering an authority from backup is an exclusive owner-restoration operation: stop the old owner, revoke/replace its admission credentials as appropriate, and re-establish trust through an approved recovery channel. A concurrently usable copy gets a new authority ID. Copying a database or private key is not sufficient to establish exclusive ownership. A local process lock prevents accidental double-start on one machine; it cannot prevent an independently booted clone on another machine. If exclusive restoration cannot be established, clone rather than claim recovery. This design does not promise partition-safe failover or consensus between duplicated authorities.

## 04. Resource kinds and capabilities

### Common contract

Every resource has:

- stable identity, owning authority, session membership, name, kind, and revision;
- creation provenance and an optional explicit relationship to other resources;
- lifecycle state and activation history when the kind executes;
- a capability descriptor and supported presentation codecs;
- ordered events, bounded retained evidence, and a deletion policy;
- explicit operation results, including refusal, unsupported operation, and stale target.

A resource is not required to have a PTY, text grid, input stream, process ID, current directory, or visible surface. Those are kind-specific facts.

### Initial kinds

| Kind | Owned thing | Typical view | Example capabilities |
|---|---|---|---|
| Terminal | PTY execution plus terminal engine state. | Terminal grid. | Typed input, paste, resize, signals, text/history reads. |
| Browser | One owner-managed browser page with explicit lifecycle and access policy. | Page view, screenshot, or supported interactive stream. | Navigate, inspect, interact, screenshot; subject to grants. |
| Agent conversation | Harness conversation/event resource. | Transcript, activity, or compact status. | Read events, inspect provenance, supported harness actions. |
| Unknown kind | An identified opaque resource. | Name, status, supported fallback, or “renderer unavailable.” | Only negotiated common capabilities. |

Browser is a first-class design case, not a terminal workaround. One Browser resource represents one page; profiles and browser contexts are execution configuration, not additional organization levels. A permitted popup creates a related Browser resource in the same session, with recorded provenance; otherwise policy refuses it. Browser termination closes that resource's page, not other pages sharing its profile. Relaunch creates a new page activation using its explicit launch URL and profile policy, not an implied return to the previous document. Persistent profile reuse must be declared; temporary profiles do not imply cookie survival.

For the shared browser kind, execution happens on its owner; a client cannot silently substitute a local browser and claim to be showing the same state. Browser rendering codecs and supported interaction are negotiated. If only screenshots are supported, the client presents that limitation instead of fabricating an interactive surface. Each navigation/document generation has an identity; document-sensitive input carries that fence, so a delayed click cannot act on a replacement page after navigation. Authentication cookies and profiles remain on the browser owner unless an explicit authorized export says otherwise.

Browser grants separately govern observation, interaction/navigation, privileged inspection, file transfer, permission approval, and profile export. Observation of authenticated pages can itself expose secrets and requires an explicit grant. Ordinary inspection excludes profile credentials/cookies; authenticated page content remains subject to observation policy. Owner policy controls permitted destinations, including access to owner-local/private services, uploads/downloads, and device permissions. A page, popup, or fetched document cannot grant authority. Renderer clients never bypass those checks through an automation fallback.

Profile sharing is an explicit security relationship, not just launch configuration: cookies/storage can couple otherwise separate resources. Reuse requires owner authorization for the profile's authenticated state. Moving a page between sessions does not transfer profile rights. If destination access is incompatible, the move is refused or replaced by an explicitly authorized new resource with an isolated profile; it never silently copies cookies or changes the original identity. Two authorized pages sharing a profile explicitly share that state; they are not advertised as isolated.

An embedded local webpage can also be a client-only view. It becomes a shared Browser resource only when a Phux owner manages it under this contract. The UI must distinguish client-only content from shared work.

For AgentSession, the existing wire name/tag remains a compatibility spelling. Its domain meaning is a harness conversation/event resource, not the session that groups work. Harness-native conversation identity remains distinct. Existing parent/cascade contracts cannot be changed merely by renaming a type; an adapter must expose their supported semantics honestly. This proposal does not silently redesign agent persistence or import the separate work coordinator into session grouping.

### Capability-based presentation

```text
Resource kind + negotiated capabilities
                    │
           choose available renderer
                    │
          live view / read-only / fallback
```

Resource-specific commands have typed schemas and authorization checks. Arbitrary unvalidated payloads are not a universal escape hatch. A TUI can show a browser's status and offer a supported navigation command without having a graphical browser renderer. Unknown kinds remain listed; clients must not treat them as terminals or drop them from membership.

**Proposal D03:** no default “terminal” fields in the generic Resource interface. New kinds supply their own facts and operations through versioned kind contracts.

## 05. Session lifecycle

A session is a persistent organization record, not evidence that a program is running.

```text
Create session ──► Open, empty
                      │ add resources
                      ▼
                  Open, populated
                      │ last resource removed
                      ▼
                  Open, empty

Open ── explicit archive ──► Archived ── reopen ──► Open
Empty or Archived-empty ── explicit delete ──► Deleted tombstone
```

Empty/populated are derived membership facts, not competing persistence modes. Archived sessions remain addressable and visible in history. While archived, create, launch/relaunch, and moves into the session are refused. Observation, rename, terminate, remove, and moves out remain available under their grants. Reopening is explicit. Archiving never terminates existing resources; active work stays observable and is clearly marked. Archive is organization, not suspension.

| Action | Defined result |
|---|---|
| Create session | Creates an empty named record. Create-and-launch atomically creates the session, resource membership, and launch intent; actual execution starts asynchronously. Launch failure leaves the records with an explicit failed outcome. |
| Rename | Changes label, not identity, links, grants, or views. |
| Open session | Opens/subscribes to its records and supported views; does not launch replacement work. |
| Add resource | Creates a resource on this owner and binds membership in one transaction. |
| Move resource | Changes session membership on the same owner; execution survives. |
| Close last view | No membership or execution change. |
| Last execution ends | Resource records remain with their outcomes; session remains. |
| Remove last resource | Session becomes empty and stays. |
| Delete session | Requires no resource records. Nonempty deletion is refused with explicit alternatives. |

The UI can offer **Move resources and delete** or **Terminate resources, remove records, and delete** as separate confirmed operations. It must not hide destructive behavior inside a generic Delete button. Move-and-delete is an authority-local atomic membership transaction with source/destination revision checks and authorization on both sessions.

Terminate/remove/delete is an explicitly non-atomic execution workflow. Confirmation captures the ResourceRefs, activation fences, and session revision; retries never expand that target set. A membership change invalidates confirmation before admission, or leaves final deletion refused if it happens after admission. Newly added work is not implicitly terminated. Successful per-target outcomes remain committed and are reported even if the whole workflow cannot finish. Termination is not instantly complete; deletion waits for confirmed inactive execution and record removal. No shell is spawned just to make an empty session attachable.

Labels are unique among nondeleted sessions within an authority for convenient human lookup. References always retain IDs. Qualified selectors resolve duplicate labels across authorities; ambiguous short names refuse to guess.

**Proposal D04:** sessions persist by default, including empty sessions. The existing keep-empty flag is retired as a product distinction after compatibility migration. A disposable test/demo profile may have bounded cleanup, but a client's presentation preference cannot change session lifetime.

## 06. Workspace lifecycle and federation

A workspace owns an ordered list of **session links**, descriptive context, and shared arrangement references. It owns neither remote execution nor the referenced session's lifecycle.

```text
Workspace owner: Laptop
  Link A ──► Laptop / Frontend
  Link B ──► Mini   / Backend
  Link C ──► Linux  / Measurements

Each link: SessionRef + last observed name/revision + access/reachability status
```

Workspace records have one owning authority. Any authorized client can access that authority and see the same workspace. The owner's machine is not necessarily the machine owning the linked work.

| Action | Defined result |
|---|---|
| Create | Creates an empty persistent workspace record. |
| Link session | Adds a validated stable SessionRef; repeated links are idempotent. |
| Unlink session | Removes only the link and affected arrangement entries. |
| Delete workspace | Removes workspace record/arrangements; sessions and resources are untouched. |
| Open | Observes accessible linked sessions concurrently and restores views. |
| Owner offline | Cached workspace is readable with age/status; authoritative edits wait for owner. |
| Linked host offline | Other sessions work; disconnected views retain last-known identity and evidence. |
| Access revoked | Link becomes inaccessible; credentials are not shared by possession of the link. |
| Referenced session deleted | Shows a deleted-link state; does not bind to a new session with the same name. |

Workspace observation can be mediated by the owner or performed directly by authorized clients. The route is an implementation choice; remote owners remain authoritative, and all observations carry source identity and revision. The model does not depend on every resource owner having a reciprocal registration of every other host.

Cross-owner actions are fan-out operations with one result per owner/target. There is no cross-host atomic kill, transaction, or total event order. A result can be partial or unknown and must say which part. A timeout is not proof that a mutation failed.

**Proposal D05:** one authority owns each workspace; offline edits are not silently queued or merged. Export or clone can move organization deliberately, with new identity and preserved source references. Offline mutation queues/CRDTs are excluded from this design.

## 07. Views, arrangements, and client behavior

### A view is not execution

```text
                         Resource: Dev shell
                         Owner: Laptop / me
                          /             \
                 Desktop view         Phone view
                 wide viewport        narrow viewport
                 local focus          local focus
```

Each view has a local view ID, a ResourceRef, kind-specific viewport state, and an attachment state. A resource may have no views or many views, even within the same client. Views carry no implicit kill authority. The resource owner arbitrates contested execution input and geometry; a second view is not a second PTY or browser context.

For a terminal, view viewport and authoritative PTY dimensions are separate. A client requests geometry under the owner's sizing/control policy. Observer views cannot resize someone else's job by mounting. Browser contexts follow an explicit negotiated viewport policy rather than pretending every client can independently change one shared page's execution geometry.

### Arrangements

An arrangement is a presentation document with stable entry IDs referring to resources, plus layout structure. It may be associated with a SessionRef or WorkspaceRef. Session arrangements refer only to members of that session; workspace arrangements refer to resources in linked sessions.

- Shared arrangements are stored by the owning session/workspace authority.
- Personal arrangements are stored in client preferences and cannot change membership.
- Shared edits use revisions; conflicting edits are refused/reloaded, not silently overwritten.
- Focus, selection, scroll, zoom, disclosure, device-specific adaptation, and native window geometry are client-local.
- Clients may adapt a shared arrangement to their device without publishing those adaptations back as edits.
- A resource removed from membership invalidates its arrangement references; stable IDs prevent index-based rebinding.
- Unknown renderers produce informative placeholders, not absent resources.

There is no authoritative execution-owning Window or Tab entity. Clients can expose tabs and splits. A saved shared “logical window” migrates to an arrangement group with stable identity; it is not permission to destroy its contents.

Session arrangement validity is enforced transactionally by its authority. Workspace arrangement validity is observational across owners: an entry can become unresolved when a remote resource moves or access changes. Views revalidate membership/access; stale entries become placeholders and never authorize attachment. The workspace owner reconciles invalid entries when it observes the source change. Membership movement alone does not close an independently mounted authorized view; access revocation does. No distributed arrangement transaction is implied.

### Every client follows this interaction contract

| Intent | Desktop / Cockpit | TUI | Mobile / web | CLI / agents |
|---|---|---|---|---|
| Inspect session | Board/list | List + supported views | Cards/list | Structured membership |
| Open resource | Mount view | Mount supported view or fallback | Mount supported view | Subscribe/inspect |
| Close view | Remove presentation | Leave presentation | Remove presentation | Detach subscription |
| Terminate resource | Explicit execution action | Explicit execution action | Explicit execution action | Typed terminate command |
| Resource unsupported | Named placeholder | Named fallback | Named placeholder | Kind + capabilities |
| Host unreachable | Last-known, disabled writes | Last-known, disabled writes | Last-known, disabled writes | Reachability + freshness |

An ephemeral client-local PTY may honestly have a different lifetime, but it must be marked **local-only / ends when closed**. It is not passed off as a shared resource. Publishing it requires an explicit owner-managed transition, not a fabricated reference.

**Proposal D06:** shared arrangement with device-local adaptation is the default for named sessions/workspaces; personal arrangements are an explicit option. Neither path shares focus or grants input.

## 08. Execution, retention, and recovery

Resource existence, execution, accessibility, and presentation are independent axes.

| Axis | Example states |
|---|---|
| Record | Present, deleted tombstone. |
| Execution | Dormant, starting, running, stopping, completed, failed, lost. |
| Reachability/access | Online, reconnecting, offline, unauthorized, unsupported. |
| Presentation | Visible, hidden, unmounted. |

Lifecycle transitions are kind-defined but exposed through this common vocabulary where applicable. A kind with no running execution reports that explicitly rather than filling invented process fields. Failed means an observed failure with evidence; lost means the owner cannot prove continued execution after an interruption. Offline is an observation failure, not an execution outcome.

```text
Dormant ── launch ──► Starting ── accepted execution ──► Running
                         │                                │
                         └─ failure ──► Failed            ├─ finishes ──► Completed
                                                          ├─ terminate ─► Stopping ─► Completed
                                                          └─ unrecovered owner loss ─► Lost

Completed / Failed / Lost ── proven inactive + explicit relaunch ──► new activation
```

Completion records carry the actual outcome: exit status, signal, supported kind-specific result, and evidence availability. Force termination is an outcome, not successful completion of the user's task. Starting has an operation identity so retries do not spawn duplicates. A resource may have at most one current activation unless its kind explicitly defines another execution contract.

**Lost is not stopped.** An orphaned process or browser may still be executing after its handle is lost. Ordinary removal and relaunch require executor-confirmed inactivity; if it cannot be proven, they are refused. An advanced, explicitly authorized abandonment retains an audited unresolved-activation record under its retention policy and releases management without claiming execution ended. The browser/terminal may continue externally. A deliberate replacement creates a separate ResourceRef and warns about possible duplicate execution. There is no silent relaunch over an unresolved activation.

### Persistence promises

| Event | Organization | Execution | Evidence |
|---|---|---|---|
| Close client | Survives. | Owner-managed execution continues. | Retained under owner policy. |
| Network disconnect | Survives. | May continue; reported last-known until revalidated. | Cached copies show age and gaps. |
| Graceful server upgrade | Survives. | Only survives if that owner's verified handover supports it. | Explicit continuity result. |
| Server crash/reboot | Persisted records survive. | Not promised to survive. New instance reconciles as recovered or lost. | Only committed durable evidence survives. |
| Resource relaunch | Same resource record. | New activation, explicit action. | Previous activation outcomes stay distinct. |
| Export/import | Organization copy survives. | Fresh execution only on explicit launch. | Exported evidence identified as a copy. |

On startup the authority reconciles recorded running activations with actual executor handles. It either proves recovery under the executor contract or marks them lost. It must not automatically rerun arbitrary commands or claim a new process is the old one.

Record retention and content retention are separate. Resource/session identity can remain while output is expired. A view says **history expired** rather than displaying an empty buffer as if nothing happened. Terminal byte history, browser captures, and conversation logs each have explicit storage limits and access policy. The model does not require retaining every byte forever.

**Proposal D07:** persistent records, bounded durable outcomes, no promise of crash-surviving processes or automatic relaunch. On-disk terminal journals are a separately specified evidence capability, not an implication of persistent sessions.

## 09. Commands, concurrency, and events

Core owns a small command vocabulary that all adapters expose. Commands are not metadata writes disguised as lifecycle operations.

```text
Sessions:    create · rename · archive · reopen · delete
Resources:   create · launch · inspect · move · terminate · remove
Workspaces:  create · rename · link · unlink · delete
Arrangement: read · replace(expected_revision)
Access:      attach · detach · acquire/release control
Kinds:       negotiated typed operations
```

Resource removal is allowed only when the executor confirms no current execution; otherwise refuse and offer termination or the explicit unresolved-activation policy in §08. Removal writes a tombstone and releases membership. History retention policy determines what evidence remains, not whether a deleted record can be silently revived. Relaunch and create are distinct operations.

### Command envelope

| Field | Purpose |
|---|---|
| operation_id | Stable caller-generated identity for retries. |
| target reference | Exact authority/domain ID, not a display name. |
| expected_revision | Optimistic concurrency for organization/configuration edits. |
| live target, when applicable | Activation/instance fence for execution mutations. |
| typed arguments | Validated by command and resource kind. |

The authority returns a typed accepted/refused/conflict/unsupported/stale result. Long-running operations return an operation handle and publish progress/final outcome. Retry deduplication and result lookup have a documented durable retention horizon. Beyond that horizon, callers reconcile current state instead of blindly repeating a mutation. Duplicate IDs with different command payloads are refused.

Within the advertised receipt horizon, an accepted retry returns the original admission/result without repeating effects. Current authorization is checked before sensitive receipts are disclosed, then deduplication precedes expected-revision/live-target checks for an already accepted matching command. Creation and activation records preserve originating operation identity for their retained lifetime. After all correlation evidence expires, the runtime reports **outcome unknown** and never automatically resubmits a non-idempotent command under a new identity. Human-authorized new work is a new operation, not an allegedly safe retry.

Organization changes commit atomically within one authority. Execution is external: launch/terminate is recorded as intent, delegated to the executor, then reconciled. Recording acceptance is not proof the process started or stopped. Executors need operation correlation and durable recovery logic; no magical database transaction spans SQLite and an OS process.

An ordered authority-local event sequence publishes accepted state changes: session created/renamed, membership changed, activation started/finished/lost, workspace link changed, and arrangement updated. Each event names source authority, object revision, operation correlation, and sequence. Replay cursors are authority-scoped. Gaps/expired cursors cause snapshot refresh. There is no fleet-wide ordering guarantee.

Output streams have their own kind-defined sequence and bootstrap contracts. Resource inventory events and terminal bytes are not forced into a single giant event log.

Batch organization commands are atomic only on their owner. Cross-owner commands carry explicit per-target results. Unknown results remain unknown until reconciliation.

## 10. Connections, access, and onboarding

Network setup is an operation on an authority connection, not session creation. The same connection machinery serves every client.

```text
Pick host
   │
   ▼
Resolve authority ─► verify trust ─► authenticate ─► load inventory ─► open views
   │                      │                │              │
named progress       explicit trust    scoped grant   actual IDs/state
```

Already-connected machines should reuse routes and grants. New authority setup can use existing SSH trust or a signed pairing handoff, but the UI must show what it is authorizing. Sharing a workspace link does not silently share credentials. Discovery returns candidates, not trusted identities. No unauthenticated resource inventory leaks through host discovery.

**Fast-path target:** on already-networked, provisioned, authorized hosts, selecting a saved host/session should reach a usable supported resource in five seconds. This is a measured product acceptance target, not a universal latency guarantee. Missing installation, network policy, approval, or unsupported capabilities get a named failure rather than a fake success checkmark.

Progress reports the actual stages and outcomes: finding host, checking trust, authenticating, loading work, mounting resource. A session record is not a successful terminal connection. Completion means the chosen view has a valid bootstrap and is ready for its supported interactions. Retry resumes from valid saved state without discarding credentials or inventing a new session.

| Permission | Meaning |
|---|---|
| Observe | Read authorized inventory/evidence and mount supported read-only views. |
| Interact | Perform granted kind-specific input/actions. |
| Organize | Rename, move membership, edit links/arrangements within granted scope. |
| Create/launch | Create records and start authorized execution. |
| Terminate/remove | Stop execution or remove records under explicit grants. |

These are conceptual permission categories; the existing scope matrix must be mapped rather than silently replaced. Input control is separately leased/arbitrated by the resource owner. A focused pane does not automatically win control. The connection manager handles routes, reconnect/backoff, authentication, enrollment/renewal, and capability negotiation; UI adapters do not maintain their own bootstrap scripts.

Session-scoped grants follow current membership; an explicitly resource-scoped grant follows that ResourceRef. Effective permission is the intersection of the caller's grant and owner policy. Moving membership or editing grants re-evaluates existing subscriptions/control leases and refuses or cancels unauthorized pending mutations before effect dispatch. Resource moves across differently authorized sessions cannot accidentally retain old session access. Workspace and arrangement projections disclose only references/names authorized for that caller, with a generic inaccessible-link placeholder where appropriate. Holding a layout reference grants nothing. Membership-based authorization takes effect in the same owner transaction as the move.

Credentials remain in approved credential storage, separate from workspace/session documents. Remote access revocation invalidates live authority as required by the trust contract. Cached sensitive content follows an explicit purge/retention policy; denial is not a reason to keep issuing mutations.

## 11. Persistence and protocol

### Persisted organization

Each authority persists its identity, sessions, resource records/activations, workspace links, shared arrangements, object revisions, tombstones, bounded operation receipts, and ordered domain events. Client preferences hold personal arrangements and local presentation state. Credentials and resource output use their respective security/storage policies.

Use one transactional local store for organization; SQLite is the proposed native implementation, not a second distributed coordinator. Resource execution drivers remain separate from organization transactions. Server restart recovery reads committed state and reconciles outstanding operations before claiming execution is ready. Backups/export must exclude live handles and credentials by default.

Workspace/session membership is an explicit typed relation. Paths, project labels, and names are attributes. Terminal geometry and browser viewport policy are kind facts. A saved layout is not the only place a resource's membership can be discovered.

### Protocol shape

The domain model belongs in core independently of byte encoding. Clients receive lossless typed snapshots, command results, and events through shared runtime/projection interfaces. FFI encoders may change representation, not omit lifecycle or identity semantics.

The proposed organization interface has explicit Session, Workspace, membership, and arrangement operations. It is a versioned extension negotiated independently from resource presentation codecs. This is an intentional replacement for authoritative session lifecycle hidden behind magic metadata keys. It supersedes the current blanket claim that session semantics are only client convention; an ADR must state that change before implementation.

Resource synchronization retains its proven kind/output/input/bootstrap contracts. No existing frame tag is reinterpreted. The wire proposal must choose an explicit negotiated organization capability or separate versioned organization endpoint, with bounded typed encoding and authorization. This draft proposes the **capability-negotiated organization surface on the existing authenticated runtime**, not a new daemon or reuse of the separate work-coordinator vocabulary. Exact discriminants/encoding belong to the follow-up wire specification.

Older peers can continue supported terminal operations. They cannot correctly edit the new organization model without a compatibility adapter. A server must not advertise a capability whose semantics the handler cannot enforce. Missing organizational support is surfaced as read-only/unsupported, not emulated by client-local records pretending to be shared state.

**Proposal D08:** introduce honest typed organization semantics; stop pretending intercepted session commands are ordinary opaque metadata. Keep resource streaming and organization conceptually separate even when they share a connection.

## 12. Core architecture and client adapters

```text
Desktop   Cockpit   TUI   Web   Mobile   CLI/agents
   │         │       │    │      │          │
   └─────────┴───────┴────┴──────┴──────────┘
                     │
       Shared client runtime + lossless projections
       connection, operations, reconcile, subscriptions
                     │
         Versioned authenticated organization interface
                     │
       Owner server: command admission + transactions
                     │
       CORE DOMAIN MODEL             RESOURCE EXECUTORS
       identity / membership         Terminal / Browser / Harness
       lifecycle / validation        kind-specific lifecycle + streams
       command results               actual OS/harness evidence
                     │
       Persistent organization + bounded event/receipt store
```

### Modules and interfaces

| Module | Owns | Must not own |
|---|---|---|
| Core domain | Entity definitions, invariants, state transitions, command validation, typed outcomes. | Networking, native windows, PTY implementation, browser paint. |
| Owner server | Authentication, authorization, serialized mutation, persistence, executor coordination. | Client focus or device-specific arrangements. |
| Resource executors | Actual kind-specific execution, evidence, streams, activation recovery capability. | Workspace ownership or client-local view lifetime. |
| Shared client runtime | Resolve/connect, submit operations, correlate results, reconcile snapshots/events, subscribe, negotiate capabilities. | Inventing owner state or client-specific resource lifetime. |
| Shared projection/FFI | Lossless domain facts and resource references in usable language-native forms. | New lifecycle rules or omission of inconvenient states. |
| Client presentation adapters | Views, arrangement adaptation, controls, accessibility, local preferences. | Alternate session models, hidden kill-on-close, private pairing implementations. |

Core stays independent of IO. Its implementation accepts current state/commands and produces validated transitions and required effects; the server transaction layer commits those transitions. Executor completion comes back as authoritative facts, not optimistic UI guesses. One implementation of organization rules serves every adapter.

Placement/rendering remains an actual client seam: terminal renderers, browser viewers, and nonvisual cards differ. A small presentation interface selects a supported renderer for a ResourceRef and negotiated capability set. Unknown-kind fallback is mandatory. It does not need a universal renderer framework beyond what real kinds require.

The existing durable work coordinator remains separate. Objectives, Runs, Actors, Artifacts, and WorkSessions are not renamed into this model or required for a basic terminal session. Future coordination can reference ResourceRef and SessionRef without taking execution authority away from owners.

**Core-first means rules-first, not every feature in one crate.** Networking belongs in runtime, persistence in the owner, rendering in clients. Shared behavior crosses a small interface instead of duplicating state machines across those places.

## 13. Scenarios and acceptance contract

These are design acceptance cases, not claims about tests already passing.

| Scenario | Required behavior |
|---|---|
| Create an empty session in CLI; open in desktop | Same stable session appears, with no surprise shell. |
| Rename a session from mobile while desktop shows it | Label updates; views and workspace links keep their identity. |
| Close every desktop view of a running test | Test continues; session and resource remain discoverable. |
| Mount the same terminal twice | Two views, one activation, explicit control/geometry arbitration. |
| Open browser resource in TUI | Kind/status/fallback shown; no terminal commands sent to it. |
| Mini drops off the network | Mini work marked last-known; laptop work remains usable. |
| Terminate workspace-selected resources on two hosts; one times out | Per-target accepted/refused/unknown results; no claim of atomic success. |
| Server restarts and wire IDs are reused | Old inputs refused; saved views cannot retarget to unrelated work. |
| Server loses executor handle but the original program is still alive | Activation is uncertain; ordinary removal/relaunch refused until inactivity is proven. |
| Another client adds work during confirmed session termination | Original target set stays fixed; new work survives and final session deletion refuses. |
| Browser navigates before delayed input arrives | Old document-fenced interaction refused. |
| Browser client requests files or owner-local service access without grants | Owner refuses; no renderer or automation fallback bypasses policy. |
| Browser pages in differently authorized sessions request the same authenticated profile | Profile-level reuse is checked; incompatible move/reuse refused or explicitly replaced with isolated execution. |
| Resource moves out of a session granting the caller access | Membership-based input/subscriptions revoked; unauthorized contents no longer projected. |
| Receipt and all correlation evidence expire before reconciliation | Outcome unknown; runtime does not automatically create a fresh operation. |
| Relaunch completed tests | New activation; prior exit and history remain a separate outcome. |
| Remove session from workspace | Session and all resources continue unchanged. |
| Delete nonempty session | Refused; move/terminate/remove choices presented explicitly. |
| Two clients edit shared arrangement at once | One accepted revision, one conflict with reload/reapply; no silent overwrite. |
| Phone changes focus/scroll | Other clients' focus/scroll are unchanged. |
| Workspace owner offline | Cached read-only document labeled stale; no imaginary committed edits. |
| Resource output expires | Identity/outcome remain; view reports evidence expiry. |
| Connection loses reply after launching resource | Same operation ID reconciles outcome; no duplicate execution. |
| Remote session is deleted and recreated with the same name | Old link shows deletion; does not attach to the new session. |
| User has CLI and daemon on mini but no browser driver | Browser create returns unsupported; terminals still work. |
| User has trusted, provisioned mini connection | Measured fast path reaches a usable supported view within the five-second target. |
| A read-only grant opens shared workspace | Only authorized links/resources are visible; layout possession grants no input. |

The contract suite must test the core transitions, real owner persistence/restart, executor correlation, protocol dispatch, runtime reconciliation, and every shipped adapter's operations. Browser claims require a real supported browser execution/view path, not a terminal test with a different icon. UI assertions must exercise close versus terminate independently.

Performance checks measure time to actual interaction-ready views, bounded inventory loading, reconnect correctness, and mixed-host partial failure. They do not equate command acceptance or a rendered button with successful setup.

## 14. Migration and terminology removal

This is a coordinated domain migration, not a mass text replacement. The goal is the complete architecture above; staging exists to keep work and compatibility safe, not to perpetuate multiple models.

### Terminology map

| Existing use | Destination |
|---|---|
| Session as named server grouping | Session, with persistent ID and explicit membership. |
| Session as connection/runtime object | Connection or connection driver. |
| SessionKernel as engine synchronization state | Resource synchronization kernel, preserving its actual responsibility. |
| AgentSession as harness resource | Agent conversation domain meaning; compatible wire spelling retained until versioned change. |
| Core Window as execution-owning pane container | Session membership plus arrangement group; no destruction authority from layout. |
| Pane as PTY identity | ResourceRef of Terminal kind. |
| Pane as on-screen slot | View/placement. |
| Tab as shared entity | Arrangement group when shared; presentation control when local. |
| Native window | Native window, client-only. |
| Mobile workspace as host folder | Host group/filter, distinct from linked-session Workspace. |
| Mobile SessionCollection containing terminals | Saved resource filter; no fake session membership. |
| Workspace archive | Export/archive document, not a live workspace authority. |
| Future work-coordinator WorkSession | Remains distinct; never silently aliases a grouped Session. |

### Conversion rules

1. Inventory existing IDs, commands, metadata conventions, saved files, clients, and protocol guarantees. Establish recovery/export before changing ownership or lifetime.
2. Adopt the reviewed glossary and authority rules in core and an ADR. Publish one schema/behavior contract with generated binding projections.
3. Add authoritative persistent session/resource/workspace records and migration receipts. Existing session membership is imported from the owner registry; existing logical windows become arrangement groups. Names do not become identity keys.
4. A preexisting live resource gets a stable record bound to its current activation only through a verified owner migration. If adoption cannot be proven, require explicit restart; never hand-deploy into the installed server or promise continuity.
5. Old saved IDs are resolved only against their recorded owner/incarnation. Unqualified stale references become unresolved placeholders requiring user confirmation; they do not guess by name, position, or reused handle.
6. Legacy lifecycle commands are routed through the new domain operations where semantics are compatible. Destructive legacy close behaviors remain explicitly labeled until adapters change; wire tags are never repurposed to mean a safer but different operation silently.
7. Migrate all shipped adapters: CLI/TUI, desktop, Cockpit, web, iOS, Android, MCP/agent interfaces, FFI, and serializers. Client presentation differences remain; alternate authoritative models are removed.
8. Reconcile concepts, specs, architecture docs, help, generated references, and old ADR status. Replace authoritative lifecycle metadata tricks with compatibility adapters, not a second permanent control path.
9. Remove obsolete keep-empty bifurcation, execution-owning layout containers, identity-by-name joins, dropped FFI facts, terminal-only generic fields, and duplicate client connection/setup logic once compatibility coverage passes.

Every saved-state conversion is versioned, backed up, idempotent, and preserves provenance. Rollback uses the backup and documented compatible binaries; it does not load new state through an old decoder and hope. Resources keep execution on their owning hosts throughout any supported migration.

### Evidence from the current tree

These references identify what the design changes; they do not constrain the proposal to today's abstractions:

- [Current concepts](../../docs/CONCEPTS.md) and [L2](../../docs/spec/L2.md) describe grouping as client convention, while [L3](../../docs/spec/L3.md) describes intercepted creation/rename and server session lifecycle.
- [Core Session](../../crates/phux-core/src/session.rs) and [Window](../../crates/phux-core/src/window.rs) encode the current containment tree; [resource](../../crates/phux-core/src/resource.rs) associates Terminal membership with Window.
- [Runtime topology](../../crates/phux-client-runtime/src/control/topology.rs) carries more lifecycle facts than [FFI topology](../../crates/phux-client-ffi/src/projection/topology.rs) exposes.
- [Desktop placements](../../clients/desktop/src/workspace/layout.ts) distinguish resource presentation from execution; [web session](../../clients/phux-web/src/session.rs) still mixes connection and presentation vocabulary.
- [Keep-empty ADR](../../docs/adr/0105-sessions-can-outlive-their-last-window.md) records the current lifecycle split. [Focus ADR](../../docs/adr/0049-client-local-focus-and-advisory-attention.md) already preserves client-local focus.
- [Work coordinator ADR](../../docs/adr/0092-durable-work-coordinator-authority.md) describes a distinct work model; this proposal does not merge it into session organization.

The mobile repositories are separate consumers and require their own verified migration. Current iOS host groups and saved terminal collections must not be relabeled as cross-machine workspaces without converting their meaning.

## 15. Design choices for review

The document specifies the proposed product behavior, including failure and recovery defaults. Exact wire bytes, resource codec specifications, storage schema, and release gates are implementation specifications that follow approval; they are not silently claimed complete here. These are the highest-impact architectural choices to annotate, not piecemeal decisions delegated back to you.

| ID | Proposed choice | What choosing differently changes |
|---|---|---|
| D01 | Resource belongs to one session; workspace can link many sessions. | Multi-session resource membership requires explicit non-owning links and more deletion semantics. |
| D02 | One writer per authority; stable domain IDs plus live execution fencing. | Multi-master owners require a conflict/consensus model beyond this design. |
| D03 | Generic Resource has no terminal assumptions; kinds define capabilities. | Terminal-shaped generalization blocks genuine browser/nonvisual resources. |
| D04 | Sessions remain when empty; create does not implicitly launch. | Automatic disappearance makes organization depend on process/client behavior again. |
| D05 | Workspace is owner-persisted links; cached offline reads, no queued edits. | Offline editing needs reconciliation, conflict presentation, and credential-aware replay. |
| D06 | Shared arrangements adapt locally; personal arrangements optional; focus always local. | Private-only defaults reduce shared continuity; shared focus creates interference. |
| D07 | Records/outcomes persist; process crash survival and automatic restart are not promised. | Restartable execution requires kind-specific explicit policy and real recovery evidence. |
| D08 | Typed authoritative organization operations replace magic lifecycle metadata. | Keeping opaque-metadata claims while intercepting lifecycle leaves the conceptual contradiction. |

### Things to challenge in your notes

- Is the board of resources the right primary surface, or should sessions open into tabs by default?
- Should a workspace owner be visibly selectable, or default to this machine until advanced settings?
- Is persistent empty organization the right default for both human and automation workflows?
- Does the browser kind mean one owner-host page as proposed, or do you want a context containing several pages as one resource?
- Are shared arrangements the expected default, or should every client start with personal presentation?
- Should archive be visible in the first product, or remain an operation behind history/search?

**Review format:** reference a section or decision, then state the change. For example: “D06: personal layout by default” or “§05: deleting a session should offer moving resources first.” The Markdown is the source; the HTML copy is for reading and portable note export.
