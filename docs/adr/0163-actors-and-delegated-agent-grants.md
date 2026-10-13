---
audience: contributors
stability: stable
last-reviewed: 2026-10-13
---

# 0163 — Actors and delegated agent grants

**TL;DR.** Every accepted command and recorded change names an actor: a
principal, its client, and the human it acts for. Two scope verbs are
added, `ORGANIZE` and `TERMINATE`, under a new scope-schema version that
maps old grants onto them without losing authority. Agents act only
through owner-scoped delegated credentials. A human's deliberate takeover
of the input lease always wins, and each lease change opens an epoch.

Status: Accepted
Date: 2026-10-13

## Context

Events carry an `ActorRef` with a client name, which says which app acted
but not who. Agent identity is descriptive metadata
([ADR-0040](./0040-agent-identity-metadata.md)) and grants nothing.
Workload grants use six closed verbs
([ADR-0098](./0098-workload-proof-and-closed-scope-authority.md),
`docs/spec/workload-auth.md` §5); renaming and moving fall under `BIND`,
and killing under `SIGNAL`, so an agent allowed to send an interrupt can
also delete work. Leases
([ADR-0033](./0033-input-authority-and-process-signals.md),
[ADR-0127](./0127-attach-roles-are-lease-intent.md)) let any deliberate
takeover seize input, including an agent taking the keyboard from a
person. [ADR-0161](./0161-organization-is-typed-commands-behind-a-feature-bit.md)
adds organization commands that each need a grant.

## Decision

1. **Actor.** `Actor = (principal, client, on_behalf_of)`. The principal
   is the authenticated identity of the connection: the owner, who is the
   serving user ([ADR-0106](./0106-identity-is-the-serving-user.md)), or an
   owner-registered agent or automation credential. `on_behalf_of` names
   the human whose grant a delegated agent exercises. `ActorRef` gains
   `principal` and `on_behalf_of` as optional trailing fields that encode
   to zero bytes when absent, so existing encodings stay byte-identical.
2. **Two verbs.** `ORGANIZE = 0x40` covers rename, membership moves,
   arrangement edits, Inbox resolution, and workspace links. `TERMINATE =
   0x80` covers terminate, remove, delete, abandon, and terminating
   signals. The scope schema version bumps to 2, as workload-auth §5
   requires for a new verb. `*` covers all eight verbs.
3. **No grant changes meaning.** Every existing command keeps exactly its
   current verb requirement. A schema-1 grant decodes with `SIGNAL`
   implying `TERMINATE` and `CREATE` implying `ORGANIZE`, so an old
   registry reaches the typed equivalent of what it could already do and
   nothing more.
4. **Classification.** `GetOrganization` and `ArrangementGet` read
   records, not content, and need `INVENTORY`; creates and relaunch need `CREATE`; rename, move, bulk move,
   Inbox resolution, and arrangement replace need `ORGANIZE` on every
   affected session; terminate, remove, abandon, and session delete need
   `TERMINATE`. Undo needs the original operation's grants as they stand
   now. A `group:` selector resolves by current membership and is
   re-evaluated in the same transaction as a move, so moving a resource
   out of a session drops authority that came from it.
5. **Delegated agents.** `phux agent start` and a launch inside a session
   mint a delegated credential holding `INVENTORY`, `OBSERVE`, `INPUT`, and
   `BIND` on its own session, `on_behalf_of` the launching human. `BIND`
   lets the agent attach, hold the input lease when it is free (§6 keeps a
   human's lease out of its reach), and report its own agent state. It
   holds no `SIGNAL`, because legacy kill commands still require that verb,
   and no `TERMINATE`, `ORGANIZE`, or `CREATE`. Delegation can narrow a
   grant and never widen it. Agent metadata never grants anything.
6. **Lease precedence.** A lease held by a human principal cannot be taken
   by a non-human principal. A human's `DELIBERATE` takeover always
   succeeds. An agent can request the lease and gets it when free, and can
   release it, but cannot hand it to a chosen holder. When a human takes
   over, the agent receives a lease-lost event rather than a stream of
   refused writes.
7. **Lease epochs.** Each lease change bumps a per-resource `LeaseEpoch`
   and emits `LeaseChanged` with the holder. Evidence can answer who was
   driving a terminal over an interval without logging keystrokes.
8. **Attribution.** Every journaled organization event records its actor.
   Clients show it: "killed by claude (on behalf of you)". Attention from
   an agent stays advisory
   ([ADR-0049](./0049-client-local-focus-and-advisory-attention.md)).

This supersedes in part ADR-0033: a supervising agent can no longer seize
input a human holds.

## Why

Attribution is what lets a person trust agents in the same environment:
every change can be traced to a principal and the human behind it. Two new
verbs separate "may reorganize" and "may destroy" from "may attach" and
"may interrupt", which a fleet of agents needs before it is given
organization access. Mapping schema-1 grants by implication, instead of
moving existing commands to the new verbs, keeps every installed registry
working with exactly its current authority. Human precedence on the lease
keeps the keyboard recoverable without making focus or attention a control
channel.

## Tradeoffs

- Two scope schemas exist while older registries are in use, and the
  decoder carries the implication table.
- A principal registry and delegated credentials are new state the owner
  manages; until they ship, agents act with the connection's grant and are
  attributed by client name only.
- An agent cannot pass the lease directly to a specific person; a human
  takes it.

## Alternatives

**Reuse `BIND` and `SIGNAL`.** No new schema, but organization and
destruction stay bundled with attach and interrupt, which is the
over-grant this fixes.

**Move existing kill commands to `TERMINATE`.** Cleaner table, but every
existing `SIGNAL` grant would silently lose kill authority.

**Agents as anonymous clients.** Simpler, and nobody can answer who did
what.

## Amendment — 2026-10-13: paired devices are human; delegations end with their session

**Principal kind.** Every principal is human, agent, or automation. The
owner and a person's paired device are human, a delegated credential is an
agent, and any other registry credential is an automation, so no agent takes
a paired phone's lease and the phone's deliberate takeover beats an agent's,
as the owner's does. A registry record states who it was enrolled for
(`principal`, [workload-auth.md](../spec/workload-auth.md) §2): a `phux pair
--enroll` ticket records `human`, and `phux workload add-key --principal`
records either. A record without the field is human exactly when its ceiling
is the one pairing grants, which keeps installed registries and older far
hosts working. Kind decides precedence only and grants no verb.

**Session deletion.** Deleting a session revokes every delegated credential
scoped to it at once: its socket closes, and each live connection acting as
it gets `AUTHORIZATION_REVOKED` (workload-auth §7).

**Restart.** A credential names its session by wire id. A hot upgrade keeps
wire ids, so it carries each live credential and its bound socket in the
state blob, and the agent's `PHUX_SOCKET` keeps answering. The new image
reinstates a carried credential only while it is unexpired, its session
survived, and the human it acts for and the principal that minted it still
hold credentials; otherwise it revokes it and removes the socket. A cold
restart renumbers sessions, so it revokes every credential: the new server
removes delegated socket files nothing serves, the agent's connection is
refused, and `phux agent start` mints a new one. Credentials are never
written to disk, where a session id from one run could name another.
