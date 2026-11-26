---
group: internals
order: 2.5
---

# Organization store and recovery

The durable side of [organization and execution](./organization.md): committing
records, refusing stale writers, and reconciling activations after a restart.

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

One server owns a store. If another connection commits, the old owner's
snapshot is stale: later organization reads and writes refuse rather than
resurrecting deleted sessions or members. This fence also detects writes by
older binaries, but cannot stop those binaries themselves from writing.
Keep one server per store; reopening it requires loading the committed records,
not retrying the old transition. An upgrade checks the stored records as well
as the handoff blob before replacing the running image. Inconsistent records
refuse the upgrade while the live processes remain owned by that image.

## Activations and reconciliation

Hot handoff can re-adopt inherited live PTYs. A cold restart cannot recreate
them. At startup the server reconciles recorded activations against the boot
identity, PID and process start time, or a vouched-for live handoff. It never
adopts an arbitrary matching PID or implicitly relaunches work. A surviving
recorded process without its owning engine becomes **Lost**; a process no
longer matching becomes Completed with an Unobserved outcome. A Starting
activation with no recorded process is also Lost: a crash can fall between
the process starting and its identity being recorded, so missing evidence
never proves the launch did not run. Only a launch whose own effect failed
while the server was stopping, so no process started, ends Failed with an
Unobserved outcome; its record carries `launch_interrupted`, so `phux restore`
relaunches a Terminal in that state but never one whose launch failed. Lost means
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
See [public durable inspection](./scripting.md#lost-replies-and-retry-scope).

[Reconciliation](../crates/phux-server/src/reconcile.rs),
[activation reports and policy tests](../crates/phux-server/src/state/org/lifecycle.rs)
and [core refusal rules](../crates/phux-core/src/apply.rs) own these decisions.
