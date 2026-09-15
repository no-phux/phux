---
audience: contributors
stability: stable
last-reviewed: 2026-09-15
---

# 0128 — Approvals are held actions

**TL;DR.** A workload grant may hold `SIGNAL` (`?signal`): each kill, signal,
or forced detach it sends is held by the server, not run, until a connection
holding un-held `SIGNAL` on the same subject approves or denies it. An
approval covers one action once, as the requester, and it expires. The
record of a held action is a server-owned L3 key. Separately, the kind
catalog marks which methods are dangerous, and every consumer confirmation
derives from that mark.

Status: Accepted
Date: 2026-09-15

## Context

The only approval gates were client-side: MCP's `confirm: true` on
`phux_kill`, `phux_detach`, and `phux_signal`, each written by hand. The
server never saw them. [ADR-0035](./0035-agent-asked-event.md) put a pending
*question* on the wire, but nothing on the server answers it or waits for the
answer. PHA-406 needs a separate gate for dangerous actions by delegated
agents, and PHA-336 §4.2 fixes its shape. An approval is a fact about one
action instance, with an expiry, never a standing grant, and a denial is a
normal, journaled outcome. PHA-333 left open where approval authority lives on
the wire: a coordinator scope bit or an L3 record convention. The scope grant
is enforced at dispatch ([ADR-0116](./0116-workload-auth-is-mtls.md)), so the
guard is the place to hold an action.

## Decision

1. **Danger is catalog metadata.** `MethodSpec.dangerous` marks
   `KILL_RESOURCE`, `KILL_RESOURCE_IF`, `KILL_RESOURCES`,
   `CLOSE_TAB_RESOURCES`, `SIGNAL_TERMINAL`,
   `DETACH_CLIENTS`, `SHUTDOWN`, `OPEN_LISTENER`, `UPGRADE`,
   `phux.config.reload/v1`, and the decide key. Two payload exceptions
   (`command_is_dangerous`, `frame_is_dangerous`): `freeze` and `resume`, the
   reversible brake, and a `deny` decision. Several things derive from this
   one fact: `phux resource methods` and `phux --capabilities` report it; MCP
   `destructiveHint` is "any dangerous method or `INPUT`"; MCP's three
   `confirm` checks become one table-driven check; and the CLI verbs `kill`,
   `signal`, `detach`, and `approve` take `--yes`. Without `--yes` they ask on
   a terminal, and when stdin is not one they exit 2 having sent nothing.
   `kill --server`, `upgrade`, and `config reload` do not ask: they are the
   owner's own lifecycle verbs. Nor does a verb whose own named effect is
   the teardown (`worktree rm`, `workspace archive`, MCP
   `phux_agent_session_close`, a spawn's rollback, a native client's
   explicit Close Tab): it is the consent.
2. **`?signal` holds.** The registry grammar spells a held verb `?signal`,
   and only `signal` may be held. The hold is server-local policy bound to
   the grant. It is never part of the canonical scope bytes, so a decoded
   image holds nothing, and a merge with an un-held grant of the same verb is
   un-held. The guard's third outcome, `Hold`, applies when a request needs
   `SIGNAL` on a subject that only a held clause covers. It is a separate step
   after `enforce`, which still admits held verbs.
3. **Only a command is held.** The server writes the record
   `phux.approval/v1/<id>` (`Global`, server-owned: requester, method,
   subjects, times) and journals `approval_requested = 0x0d { id }` with the
   requester as actor. It defers the `COMMAND_RESULT`. A held `SIGNAL` on any
   other frame is refused, because there is no result to defer.
4. **A decision is `SIGNAL` on the held subject, un-held.**
   `SET_METADATA { Global, "phux.approval.decide/v1/<id>" }` with `approve` or
   `deny`. The server intercepts it, stores nothing, and classifies it on the
   held command's subjects under the current topology. The requester cannot
   approve itself, and a connection that attached to a held subject as a
   `VIEWER` ([ADR-0127](./0127-attach-roles-are-lease-intent.md)) cannot
   decide it.
5. **Once, as the requester.** The first ending removes the record, so a
   second decision is refused like an absent target. `approve` re-classifies
   the command under the requester's current grant, then runs it once with
   the requester's client id and `request_id`; the approver's grant is never
   used. Every ending journals `approval_decided = 0x0e { id, outcome }`:
   `approved` and `denied` carry the decider as actor, while `expired` (after
   `defaults.approval-ttl-secs`, 120) and `withdrawn` (the requester
   disconnected or was revoked, or a Terminal it names was reaped) carry
   none. A reaped Terminal's holds are withdrawn before its close is
   journaled, so nothing follows the close. Withdrawn has two cases: a reap
   withdrawal answers the requester `PERMISSION_DENIED "terminal gone"`, and
   a disconnect withdrawal answers no one, because the requester is gone. A
   keyed command (L1 §5.1.1) is never held twice: a key that already has an
   answer replays it without a hold, and an identical keyed repeat joins the
   pending hold, so one approval runs it once; a joined waiter counts
   against the per-connection bound, and runs the command again only if the
   first run failed, since a failure binds nothing (L20).
6. **Bounded and off by default.** A connection holds at most
   `defaults.approval-max-pending` (64) actions and the server at most
   `defaults.approval-max-pending-total` (1024); one more is
   `RESOURCE_EXHAUSTED`. Adding a hold to a live credential is a narrowing:
   its connections are revoked, and they reconnect with the hold. The owner's grant never holds, so `local` and
   transitional servers never hold. No grant holds anything unless its
   registry record spells `?signal`. `ServerFeature::APPROVALS = 0x40000000`.
7. **Consumer surface.** This amends ADR-0071 point 7c. It adds
   `phux approvals [--json]`, `phux approve ID [--yes]`, `phux deny ID`, and
   `--yes` on `kill`, `signal`, and `detach`. MCP gains `phux_approvals` and
   `phux_approve`. `phux watch` gains `approval_requested` and
   `approval_decided`, and whoami's grant entries gain `held`. It amends
   ADR-0035's scope: a question an agent asks is still `asked`, while an
   action a workload attempts is held and decided here.
8. **Not the coordinator's.** The record is live, local, bounded, expiring
   state: what L3 is for, and what ADR-0097 keeps out of a coordinator. A
   coordinator scope bit remains the federation-era successor. If a
   coordinator later routes approvals, its endpoint carries its own
   vocabulary, and this convention becomes the local projection of it.
   Nothing here reserves a coordinator bit.

## Why

Holding at the guard reuses the one place authority is already decided,
under the same snapshot, so a held action is judged by exactly the rules that
would have run it. Running the approved command as the requester keeps
attribution honest: the journal shows who asked and who approved, and the
effects are the requester's. One catalog fact behind every confirmation means
a new dangerous method cannot be confirmed by one surface and not another.

## Tradeoffs

A held command blocks its requester for up to the TTL; a client whose own
timeout is shorter gives up first. A requester whose grant changed since the
hold is refused after an approval, so the approver sees `approved` in the
journal and the requester sees `PERMISSION_DENIED`. A decider without
`OBSERVE` on `Global` cannot read the record, and learns ids from the events
instead. A hold on a satellite Terminal is not withdrawn when that Terminal closes,
because its close arrives relayed; it expires at its TTL instead, after the
relayed close (a known limit for federated Terminals, phux-x8k0). A decision
races an expiry; the first to take the id wins. A closing
connection withdraws its holds before anything else, but an approval that
landed just before the close is journaled `approved` even when the
connection is gone before the command can run. The CLI
now refuses an unconfirmed kill, signal, detach, or approve from a script;
scripts pass `--yes`. Approval fatigue is a policy choice, not a mechanism,
so nothing holds by default.

## Alternatives

A coordinator scope bit now: there is no coordinator endpoint in this repo,
and it would reserve wire space for a federation program that has not
decided its vocabulary.

Approval as scope widening (grant `SIGNAL` for a while): it is a standing
grant, which PHA-336 §4.2 rules out.

Client-side confirmation only: the server cannot tell an agent that asked a
human from one that did not.
