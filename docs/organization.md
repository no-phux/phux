# Organization and execution

The server owns session membership and shared arrangements. Clients project
that organization into tabs and panes; they do not independently decide where
a resource belongs. Start with [concepts](./concepts.md) for user vocabulary,
[protocol](./protocol.md) for commands, and [troubleshooting](./troubleshooting.md#downgrading)
for recovery operations.

## Records, handles, and arrangements

Use the [identity terms](./concepts.md#names-ids-and-keys) consistently.
Clients must validate authority, record key and activation/incarnation when
reconnecting, rather than adopting whichever object now has the same number.

Every resource belongs to exactly one server-owned session. Moving it
preserves its identity and execution; removing the last member of an
Ephemeral session deletes that session in the same transition. The designated
Intake is persistent and cannot be deleted. An arrangement is presentation,
not membership: it may name only members of its session, and moving a member
prunes its old arrangement entries atomically. Splits are binary, with finite
ratios strictly between zero and one. Focus, selection, scroll and zoom are
client-local; a shared layout edit must not take over human focus.

A group can be **locked** (`ARRANGEMENT_LOCK`, behind `features_ext.TAB_LOCK`).
While it is, terminating its last member, a legacy kill that would empty it,
and an `ARRANGEMENT_REPLACE` that drops it are all refused with
`INVALID_ARGUMENT` naming the tab, and nothing changes. A replacement
never locks or unlocks: the server carries each lock over by group id, so a
client that does not know the flag cannot clear it. `SESSION_KILL` overrides
locks, because a locked tab guards against closing that tab, not against
ending the session. A natural exit is not a close, so it isn't refused. The
flag is stored in the arrangement record, so it survives a hot upgrade, and on
the wire it is a trailing byte of the group record that older peers skip.

### Slot tabs

A layout slot (a leaf) can hold several terminals as **slot tabs**: one shows,
and the rest keep running at the slot's size, so switching never reflows.
"Stacked" already means a vertical split, so the feature isn't called that.

- **Shape.** The leaf's own entry is the slot's default tab. Its other tabs
  ride on the leaf as `tabs`, an ordered list of entries, with `shown_at` giving
  the default's place in the strip. On the wire they are trailing fields
  of the leaf record, which older peers skip, the same way they skip the
  lock byte. In the stored JSON (the org store and upgrade blob) they are
  default-empty fields an older server ignores. A tab holds one terminal,
  never a subtree.
- **Older peers.** An older client sees and edits the default tab only; the
  others are session members with no place in its layout. An older server
  loads the record without them, so a rollback starts and their terminals
  keep running, unarranged. A new node kind would instead make every older
  client's `ARRANGEMENT_GET` unreadable, block the TUI's layout writes, and
  stop an older server from loading its store, so there is none.
- **Carry-over.** An `ARRANGEMENT_REPLACE` from a client that doesn't know
  tabs cannot drop them: the server carries each slot's tabs over by the
  leaf's entry id, then by its default's member key (the legacy layout
  adapter mints fresh ids). A tab the replacement places somewhere else
  leaves the slot, because an explicit placement wins. If the replacement
  removed the slot, its tabs become unarranged members; nothing is closed.
- **Invariants.** A member appears at most once in an arrangement, tabs
  included, and every tab is a member of the session. Moving a member out
  (`phux mv`) or ending it removes it from its slot. When it was the default,
  the next tab in strip order takes the leaf. The last tab leaving
  removes the leaf, and its split collapses as today. Undo restores whole
  groups, so it restores tabs with them.
- **What shows is client-local.** Which tab a client shows is client-local,
  like the active group, so switching on a phone never flips a desktop. The
  stored default is what a fresh attach and an older client show; it moves
  only when tabs are added, removed or reordered.
- **Layout verbs.** A slot is one unit for split, zoom, swap and move.
  `name:G.P` counts slots, not tabs, and `@N` names any tab. Clients keep
  their split tree as it is and hold each group's slot tabs beside it, keyed
  by the shown leaf, so no tree walker changes.
- **Advertised.** The server advertises `features_ext.SLOT_TABS`. A client
  writes tabs only to a server that advertises it, because an older server
  would drop the trailing fields and the tabs with them.
- **Surface.** `phux slot add @N --to @M` makes terminal `@N` a tab of the
  slot showing `@M`. `phux slot out @N` gives a tab a slot of its own, split
  beside the one it left. `phux slot next|prev [@M]` moves the slot's
  default, since a headless verb has no view of its own. In an attached
  client the keys switch only what that client shows: `C-a }` and `C-a {`
  in the TUI. In Rune they are
  Cmd+Shift+→ and Cmd+Shift+← on macOS (beside Cmd+← and Cmd+→, which walk
  terminals) and Ctrl+Shift+→ and Ctrl+Shift+← elsewhere. In Rune,
  dragging a terminal's sidebar row onto a pane of the same session makes
  it that pane's slot's last tab. In the TUI, dragging a pane by its title
  onto another pane does the same. A slot with two
  or more tabs shows a one-row strip of their titles and agent states.

The pure [core apply function](../crates/phux-core/src/apply.rs) owns these
invariants; [arrangement validation](../crates/phux-core/src/arrangement.rs)
owns tree validity. Expected revisions, operation ids and live activation
fences are concurrency/authority checks, not optional UI bookkeeping. The
legacy TUI layout metadata adapter translates into this model; it does not
supplant it.

## Complete creation intentions

The [public terminal workflow](./public-api.md) commits membership, a stable
arrangement entry and launch intent together. Invalid or stale placement
refuses before any process starts; callers do not spawn then publish a layout
or compensate with a kill. OS launch follows the durable commit and can fail.
Its receipt distinguishes that failure from refusing the organization edit.

## Commit before publication

The dataflow is **command + actor → pure apply → durable commit → mirror →
effects/events/reply**. A refusal changes nothing. Commit the transition,
operation receipt and journal position in one store transaction before
mirroring memory, acknowledging success or performing its effects. Replaying
a retained identical operation returns its receipt; changing the payload
under that id is refused. Do not copy the transient protocol dedupe cache's
lifetime into this durable receipt model; each store owns its retention.
A receipt row this binary cannot decode, as after a rollback past a new
operation kind, does not stop the server. The row stays stored and its id is
refused as a duplicate until it expires, because a missing receipt would let
a retry run the operation twice.

A replay of an activation-fenced terminate still needs its original live
handle and server incarnation to resolve the target. After retirement or
a cold restart, reconcile from the durable resource record if retained
and from current inventory. Reusing an operation against a replacement
activation is a different intent.

The [server commit seam](../crates/phux-server/src/state/org.rs) separates
`persist_command_from` from `mirror_persisted`; its
`a_failed_store_write_refuses_and_leaves_memory_unchanged` test is the
regression boundary. The per-socket [SQLite organization store](../crates/phux-server/src/org_store/mod.rs)
uses WAL and full synchronization. It stores organization/launch intent,
receipts and journal position, not PTY output, scrollback or a recoverable
terminal process. Event payloads remain a bounded in-memory journal: a gap
requires a level-state reread, not invented continuity.

## Activations and reconciliation

Hot handoff can re-adopt inherited live PTYs. A cold restart cannot recreate
them. At startup the server reconciles recorded activations against the boot
identity, PID and process start time, or a vouched-for live handoff. It never
adopts an arbitrary matching PID or implicitly relaunches work. A surviving
recorded process without its owning engine becomes **Lost**; a process no
longer matching becomes Completed with an Unobserved outcome. Lost means
uncertainty, not an inactive pane: removal/relaunch is refused while it may
still run. The current CLI cannot resolve every Lost activation; follow
[the recovery limitation and escalation path](./troubleshooting.md#downgrading),
not an assumed PID-kill or relaunch sequence.

`ExitPolicy::Remove` removes a normally exited resource (and an empty
Ephemeral session). `Retain` keeps its outcome inspectable until purge or
retention expiry. Exit is not close: purging a Terminal closes its bound
AgentSession children; a retained parent can outlive its process.
Executor lifecycle reports that encounter a refused store write stay pending
in memory, ordered start-before-end and fenced by resource key, activation and
instance. A bounded server-owned retry pass persists evidence, **never reruns
the process**. Observations flag `persistence_pending` while displaying the last
committed state. Removal/replacement retires stale reports. Hot handoff must
flush them or refuse. Orderly shutdown makes one final attempt but still permits
exit if the store is unavailable; any exit with undurable evidence can lose it.
Restart can only reconcile the committed process identity, not recover a lost
diagnostic.
See [public durable inspection](./public-api.md#lost-replies-and-retry-scope).

[Reconciliation](../crates/phux-server/src/reconcile.rs),
[activation reports and policy tests](../crates/phux-server/src/state/org/lifecycle.rs)
and [core refusal rules](../crates/phux-core/src/apply.rs) own these decisions.

## Work and missions

Organization records own execution, not task scheduling. The separate
[work store](../crates/phux-work/src/lib.rs) has one keeper writer per store,
on the host that runs it. Mission documents compose over its client and
never open SQLite directly.
Operation ids deduplicate work/mail mutations and expected revisions refuse
stale edits. Mail/path claims are advisory host-local coordination, not
filesystem enforcement or an authenticated execution identity; see
[claims and acquisition fences](./agents.md#mail-and-path-claims).

The shipping [`mission start`](./reference/cli.md#phux-mission-start) creates
a task worktree, brief, session and configured harness worker, with a bounded
agent cap and a revision-checked keeper commit. Queued work pins its owner
before any worker effect; an unrelated authority cannot promote it. Its
[CLI implementation](../crates/phux/src/commands/mission_start.rs),
[pure launch planning](../crates/phux-work/src/mission/start.rs) and
[keeper start transaction](../crates/phux-work/src/start.rs) own the sequence.

The keeper binds each started run to the execution in the host's placed-create
receipt: owning authority, resource key, activation, and instance. A pane ID
is an optional presentation hint and is never used to settle the run. The
keeper reads the exact durable state on startup, on wakeups, and after a
journal gap. Replaying the same derived observation ID writes nothing.

| Observed fact | Task state |
| --- | --- |
| the keeper launches a queued task | `working` |
| agent `working` | `working` |
| agent `blocked` | `needs_you` |
| agent `done` | `done_unchecked` |
| process exit 0 after `done` | `done_unchecked` |
| process exit non-zero, or exit with no `done` | `failed` |
| confirmed worker launch failure | `failed` |
| exact execution missing, superseded, lost, or legacy pane-only binding | `needs_you` (`binding_uncertain`) |
| unresolved `credential_lost` | `needs_you` |

`done` is reserved for merge cleanup and is not written from agent state.
`needs_you` keeps the agent slot. `done_unchecked` and `failed` free it, and
the keeper then starts the oldest queued task through the same worktree and
brief path as `mission start`. The keeper stays up while any task is
`working` or `queued`. On exit it waits a short bounded grace and re-reads
that terminal's agent record before recording `failed`, so a `done` that
lands in the same moment becomes `done_unchecked`.

The work store migrates to `user_version` 3 to preserve exact execution
bindings and queued owners. Legacy work, receipts, and pane hints remain
readable; no execution or owner is inferred from an old pane ID. Legacy
unpinned queued runs are not automatically launched. Older keepers refuse the migrated
store rather than observing it with obsolete identity rules. Failed or
uncertain worker setup leaves the worktree in place. A retry for that same
worktree checks the authorized session and reads the retained worker receipt;
it does not launch again if the receipt is missing.

The transition is pure in
[state.rs](../crates/phux-work/src/mission/state.rs) and applied by
[the keeper projector](../crates/phux/src/commands/mission_project.rs).
Journal wake-ups follow
[ADR-0123](https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0123-events-are-journaled.md)
and
[ADR-0130](https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0130-on-disk-pty-journal-is-not-built.md).
The single keeper writer is
[ADR-0092](https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0092-durable-work-coordinator-authority.md).
Agent records are
[ADR-0040](https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0040-agent-identity-metadata.md)
and
[ADR-0046](https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0046-server-side-agent-state-detection.md).
Delegated grants are
[ADR-0163](https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0163-actors-and-delegated-agent-grants.md).
Agents coordinate through phux per
[ADR-0187](https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0187-agents-coordinate-through-phux.md).
Single-writer work authority and delegation limits are the
[substrate decision](./substrate.md). A multi-host
coordinator, an autonomous Lead, and a durable grant ledger are not
shipping contracts. Do not infer them from historical design names.

## Restore and executable authority

Importing dormant organization records is not permission to execute them.
Explicit restore/relaunch creates a new execution; it never resurrects the
old PTY or its scrollback. There are two different agent restore lanes:

- **Terminal `agent_session` resume:** stores bounded opaque native identity
  and owning plugin/integration ids. Resume argv is resolved from the
  **currently enabled, trusted** integration template. Ownership mismatch
  refuses before creation; other unresolved terminal integrations may warn
  and fall back to a plain shell. Option-shaped native ids are rejected.
- **ACP `native_launch` restore:** archives a complete `SpawnNativeAgent`
  executable recipe, including command argv, cwd and environment. Explicit
  restore/relaunch currently executes that saved recipe without resolving
  current integration ownership/templates. **Inspect and trust the archive
  as executable input before restoring it**; disabling/changing its plugin
  does not invalidate the saved command. The provider process starts before
  the `loadSession` capability check, so a later restore failure does not
  mean no executable ran. `loadSession` is still required to resume the
  provider conversation, not an executable-trust check.

The [saved recipe path](../crates/phux-server/src/runtime/organization.rs)
and [ACP process startup](../crates/phux-agent-host/src/session/acp.rs) define
that current limitation; [ACP usage](./acp.md) covers
readiness and operation receipts.

The [archive resolver](../crates/phux/src/commands/archive.rs) and
[live-session bridge tests](../crates/phux/src/commands/archive/bridge.rs)
own this trust seam; [CLI reference](./reference/cli.md#phux-restore) owns the
current restore/import options.

## Execution evidence and durable inspection

Organization activation records and creation receipts add optional TLVs:
`LAUNCH_DIAGNOSTIC` (`47`, at most 128 bytes of JSON `{stage, errno}`) and
`PERSISTENCE_PENDING` (`48`, boolean, absent means false). Diagnostic stages
are `pty`, `spawn`, `cwd`, `exec`, `handshake`, `engine`; errno is an optional
platform OS error, never the program's exit status. There is no command line,
path or environment payload. Older clients skip these fields; old stored
activations without a diagnostic remain readable. Unknown exit evidence
projects as `Unobserved`, never fabricated exit zero.

`features_ext.DURABLE_RESOURCE_INSPECTION` (`0x100000`) gates filter discriminant
`4` on the existing `GET_ORGANIZATION`: the discriminant is followed by the
16-byte durable resource key. It returns at most that one resource row, no
session rows and no continuation cursor. The existing per-resource inventory
filter uses current membership and authority even without an engine; hidden
and missing records both yield an empty resource page. New clients check the
bit before sending; older requests/filters are unchanged. CLI/MCP expose this
through the existing resource inspection noun; its durable-key JSON document
is schema 2, while live-engine inspection remains schema 1.

[File transfer and checkout methods](./protocol-files.md) have their own page.

### Where the code lives

Terminal workflow code is split at ownership seams: `runtime/commands/`
separates dispatch, pane construction, attachment generations, inventory,
input and satellite forwarding; `runtime/organization/` separates pure command
translation, execution effects, receipt projection and filtered record reads.
`state/org/lifecycle.rs` owns bounded retries of executor evidence, not effects.
The core apply module separates transaction assembly, session edits, placement
and execution transitions. Public entry points and wire identities are unchanged.
