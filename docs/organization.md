# Organization and execution

The server owns session membership and shared arrangements. Clients project
that organization into tabs and panes; they do not independently decide where
a resource belongs. Start with [concepts](./concepts.md) for user vocabulary,
[protocol](./protocol.md) for commands, and [troubleshooting](./troubleshooting.md#cold-restart-and-downgrade)
for recovery operations.

## Records, handles, and arrangements

Use the [identity terms](./concepts.md#identity-and-execution) consistently.
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

The pure [core apply function](../crates/phux-core/src/apply.rs) owns these
invariants; [arrangement validation](../crates/phux-core/src/arrangement.rs)
owns tree validity. Expected revisions, operation ids and live activation
fences are concurrency/authority checks, not optional UI bookkeeping. The
legacy TUI layout metadata adapter translates into this model; it does not
supplant it.

## Commit before publication

The dataflow is **command + actor → pure apply → durable commit → mirror →
effects/events/reply**. A refusal changes nothing. Commit the transition,
operation receipt and journal position in one store transaction before
mirroring memory, acknowledging success or performing its effects. Replaying
a retained identical operation returns its receipt; changing the payload
under that id is refused. Do not copy the transient protocol dedupe cache's
lifetime into this durable receipt model; each store owns its retention.

The [server commit seam](../crates/phux-server/src/state/org.rs) separates
`persist_command_from` from `mirror_persisted`; its
`a_failed_store_write_refuses_and_leaves_memory_unchanged` test is the
regression boundary. The per-socket [SQLite organization store](../crates/phux-server/src/org_store.rs)
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
[the recovery limitation and escalation path](./troubleshooting.md#cold-restart-and-downgrade),
not an assumed PID-kill or relaunch sequence.

`ExitPolicy::Remove` removes a normally exited resource (and an empty
Ephemeral session). `Retain` keeps its outcome inspectable until purge or
retention expiry. Exit is not close: purging a Terminal closes its bound
AgentSession children; a retained parent can outlive its process.
[Reconciliation](../crates/phux-server/src/reconcile.rs),
[activation reports and policy tests](../crates/phux-server/src/state/org.rs)
and [core refusal rules](../crates/phux-core/src/apply.rs) own these decisions.

## Work and missions

Organization records own execution, not task scheduling. The separate
[work store](../crates/phux-work/src/lib.rs) has one keeper writer per host;
mission documents compose over its client and never open SQLite directly.
Operation ids deduplicate work/mail mutations and expected revisions refuse
stale edits. Mail/path claims are advisory host-local coordination, not
filesystem enforcement or an authenticated execution identity; see
[claims and acquisition fences](./agents.md#mail-and-path-claims-schema-phuxcoordv1).

The shipping [`mission start`](./reference/cli.md#phux-mission-start) creates
a task worktree, brief, session and configured harness worker, with a bounded
agent cap and a revision-checked keeper commit. Queued is not launched. Its
[CLI implementation](../crates/phux/src/commands/mission_start.rs),
[pure launch planning](../crates/phux-mission/src/start.rs) and
[keeper start transaction](../crates/phux-work/src/start.rs) own the sequence.
This is not the proposed cross-host coordinator, autonomous Lead, or durable
Capture dispatch service; do not infer those from historical design names.

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
that current limitation; [ACP usage](./agents.md#acp-owned-sessions) covers
readiness and operation receipts.

The [archive resolver](../crates/phux/src/commands/archive.rs) and
[live-session bridge tests](../crates/phux/src/commands/archive/bridge.rs)
own this trust seam; [CLI reference](./reference/cli.md#phux-restore) owns the
current restore/import options.
