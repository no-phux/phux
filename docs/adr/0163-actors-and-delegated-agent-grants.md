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
4. **Classification.** `GetOrganization` and `ArrangementGet` need
   `OBSERVE`; creates and relaunch need `CREATE`; rename, move, bulk move,
   Inbox resolution, and arrangement replace need `ORGANIZE` on every
   affected session; terminate, remove, abandon, and session delete need
   `TERMINATE`. Undo needs the original operation's grants as they stand
   now. A `group:` selector resolves by current membership and is
   re-evaluated in the same transaction as a move, so moving a resource
   out of a session drops authority that came from it.
5. **Delegated agents.** `phux agent start` and a launch inside a session
   mint a delegated credential holding `INVENTORY`, `OBSERVE`, and `INPUT`
   on its own session, `on_behalf_of` the launching human. It holds no
   `SIGNAL`, because legacy kill commands still require that verb. Delegation can narrow a
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
