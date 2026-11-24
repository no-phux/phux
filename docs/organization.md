---
group: internals
order: 2
---

# Organization and execution

The server owns session membership, arrangements, and the durable record of
what runs; clients project it into tabs and panes and never decide on their
own where a resource belongs. Vocabulary is in [concepts](./concepts.md);
commands are in [protocol](./protocol.md).

## Records, handles, and arrangements

Use the [identity terms](./concepts.md) consistently. On reconnect, a client
validates authority, record key and activation, rather than adopting whatever
object now has the same number.

- Every resource belongs to exactly one session. Moving it keeps its identity
  and process. Removing the last member of an Ephemeral session deletes the
  session in the same transition; the Intake session cannot be deleted.
- An arrangement is presentation, not membership. It names only members of
  its session, and moving a member prunes its old entries atomically.
- Splits are binary, with finite ratios strictly between zero and one.
- Focus, selection, scroll, zoom and the shown slot tab are client-local. A
  shared layout edit never takes over a person's focus.
- A locked group (`ARRANGEMENT_LOCK`, feature `TAB_LOCK`) refuses any close or
  `ARRANGEMENT_REPLACE` that would empty or drop it, with `INVALID_ARGUMENT`
  and no change. A replace carries locks over by group id, so a client that
  does not know the flag cannot clear it. `SESSION_KILL` overrides locks, and
  a natural exit is not a close.

The pure [core apply](../crates/phux-core/src/apply.rs) owns these invariants
and [arrangement validation](../crates/phux-core/src/arrangement.rs) owns tree
validity. The legacy layout codec translates into this model; it does not
replace it.

### Slot tabs

A leaf can hold several terminals as slot tabs: one shows, the rest keep
running at the slot's size, so switching never reflows. A slot tab holds
one terminal, never a subtree.

- The leaf's own entry is the default tab; the others are a trailing `tabs`
  list on the leaf, with `shown_at` placing the default in the strip. Older
  peers skip the trailing fields: an older client sees only the default, and
  an older server loads the record without the extra tabs, leaving their
  terminals running unarranged.
- A client writes tabs only to a server advertising `SLOT_TABS`, because an
  older server would drop them.
- A replace from a client that does not know tabs cannot drop them: the server
  carries them over by leaf entry id, then by the default's member key. An
  explicit new placement wins; a removed slot leaves its tabs unarranged, not
  closed.
- A member appears once per arrangement, tabs included. When the default tab
  leaves, the next tab takes the leaf; when the last leaves, the leaf goes and
  its split collapses.
- A slot is one unit for split, zoom, swap and move. `name:G.P` counts slots;
  `@N` names any tab.
- The stored default is what a fresh attach and an older client show. It
  moves only when tabs are added, removed or reordered, or by
  [`phux slot next|prev`](./reference/cli.md#phux-slot). An attached client's
  keys ([keys](./reference/keys.md)) change only what that client shows.

## Close intents

| Request | Target | Reply |
| --- | --- | --- |
| `KILL_RESOURCE` | live handle and bound descendants | close committed by the serving host |
| `KILL_RESOURCES` | host-local groups; naming every member can delete a persistent session | keyed replies per id |
| `CLOSE_TAB_RESOURCES` | host-local groups; keeps keep-empty sessions; needs its feature bit | forwards satellite groups without waiting |
| `RESOURCE_TERMINATE` | handle, instance and activation | `Accepted` confirms admission; lifecycle events report the effect |

`KILL_RESOURCE_IF` checks allocation instance for delayed cleanup.
`SESSION_KILL` ends the session and overrides locks. Every close is planned
first; admission and teardown use the same plan, and children close before
parents. A lost reply leaves the outcome unknown: reconcile from the serving
host's state and receipts ([retries](./protocol.md)).

## Commit before publication

Commit the transition, receipt and journal position before mirroring memory,
launching work or publishing a reply. The
[organization store](./organization-store.md#commit-before-publication)
owns durability, retry retention and stale-writer fencing.

## Activations and reconciliation

A hot handoff can preserve live PTYs; a cold restart cannot. Startup
[reconciliation](./organization-store.md#activations-and-reconciliation)
checks process identity and preserves uncertainty as **Lost**, never
implicitly relaunching a possibly live process. Executor persistence retries
record evidence, not execution.

## Work and missions

Organization records own execution, not scheduling. The
[work store](../crates/phux-work/src/lib.rs) has one keeper writer per store on
its host; missions use its client and never open SQLite. Operation ids
deduplicate mutations and expected revisions refuse stale edits. Mail and path
claims are advisory ([agents](./agents.md)).

[`phux mission start`](./reference/cli.md#phux-mission-start) creates a
worktree, brief, session and worker under an agent cap, pinning the queued
owner before any worker effect. The keeper binds each run to the exact
execution in the placed-create receipt (authority, resource key, activation,
instance); a pane id is only a hint. It rereads that state on startup, on wake
and after a journal gap.

| Observed fact | Task state |
| --- | --- |
| keeper launches a queued task; agent `working` | `working` |
| agent `blocked` | `needs_you` |
| agent `done`, or exit 0 after `done` | `done_unchecked` |
| non-zero exit, exit without `done`, confirmed launch failure | `failed` |
| execution missing, superseded, lost, or a pane-only binding | `needs_you` (`binding_uncertain`) |
| unresolved `credential_lost` | `needs_you` |

`done` is written only by merge cleanup. `needs_you` keeps the agent slot;
`done_unchecked` and `failed` free it and start the oldest queued task. On exit
the keeper rereads the agent record after a short grace, so a simultaneous
`done` is not recorded as `failed`. Failed setup leaves the worktree; a retry
reads the retained worker receipt and does not relaunch without it.

The projection is pure in [`mission/state.rs`](../crates/phux-work/src/mission/state.rs)
and applied by [`mission_project.rs`](../crates/phux/src/commands/mission_project.rs).

## Restore and executable authority

Importing dormant records is not permission to run them. Restore creates a new
execution; it never resurrects the old PTY or scrollback.

- **Terminal `agent_session` resume** stores a bounded native id and the
  owning integration. Resume argv comes from the currently enabled, trusted
  integration template; an ownership mismatch refuses, and option-shaped ids
  are rejected.
- **ACP `native_launch` restore** runs the archived `SpawnNativeAgent` recipe
  (argv, cwd, environment) without consulting current integrations. **Treat
  the archive as executable input.** The provider starts before the
  `loadSession` check, so a failed restore may still have run it
  ([ACP](./acp.md)).

Owned by [`runtime/organization.rs`](../crates/phux-server/src/runtime/organization.rs),
[`session/acp.rs`](../crates/phux-agent-host/src/session/acp.rs) and
[`commands/archive.rs`](../crates/phux/src/commands/archive.rs). Options are in
the [CLI reference](./reference/cli.md#phux-restore).

## Durable inspection

Activation records and creation receipts carry an optional launch diagnostic
(stage and OS errno only, never argv, path or environment) and a
`persistence_pending` flag. With `DURABLE_RESOURCE_INSPECTION`,
`GET_ORGANIZATION` can filter by a durable resource key and return that one
row. Field ids are in the [wire schema](./spec/wire-schema.json).
