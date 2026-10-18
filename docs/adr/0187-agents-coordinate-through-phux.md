---
audience: contributors, agents
stability: stable
last-reviewed: 2026-10-18
---

# 0187 — Agents coordinate through phux

**TL;DR.** Mail between agents and advisory claims on paths live in the
host's work store, written by its one keeper. An agent's address is its pane
(`@749`, optionally `pi@749`). `phux mail send` stores the message, then rings
each recipient pane with one acknowledged prompt line. `phux claim` reserves
absolute paths with leases and fences. Blackbird is retired, and the ADR-0095
boundary goes with it.

Status: Accepted
Date: 2026-10-18

## Context

[ADR-0095](./0095-the-blackbird-boundary.md) split agent coordination in two.
phux owned what it executes: agent identity per pane (the `phux.agent/v1`
record, [ADR-0040](./0040-agent-identity-metadata.md)), acknowledged prompts
([ADR-0076](./0076-agent-prompt-and-lifecycle-wait.md)), lifecycle waits, and
the work store ([ADR-0173](./0173-mission-control-is-a-client-composition.md)).
Blackbird, a separate daemon, owned mail, threads, and path leases, under a
second identity: a project key, an agent name, and a registration token each
agent had to obtain and keep.

In use the split did not hold. Agents running in phux panes messaged each
other with `phux agent prompt`, because the pane is the address they could
see. That prompt arrives as if the human had typed it: no sender, no thread,
no record, no way to reply but typing back. The Blackbird half went unused for
the same exchange, because an agent that had never registered could not
reply, and nothing told a registered agent that mail had arrived. A
coordinating agent ended up asking peers to "avoid editing those checkouts"
in prose, while a Blackbird lease on the same paths was invisible to them.

ADR-0095 assumed agents would often coordinate from places phux is not:
IDEs, CI, and hosts without phux. On this fleet, every agent that coordinates
runs in a phux pane. Two identities for one agent, and two daemons for one
conversation, cost more than they bought.

## Decision

1. **The work store holds coordination.** Mail, per-recipient delivery state,
   claims, and the fence counter are tables in the store the work keeper
   already writes ([ADR-0173](./0173-mission-control-is-a-client-composition.md)).
   They are additive: they leave `user_version` at 1, so a binary that
   predates them still opens the store. One keeper per host is their only
   writer. The terminal server and public wire are unchanged. The keeper
   checks a semantic epoch before payload decoding; replacement preserves
   leases. A legacy parser gets one bounded, read-only probe. Only explicit
   version mismatch permits replacement, never PID or liveness inference.

2. **An agent's address is its pane.** `@749` is an address; `pi@749` is the
   same address with a display label, and both reach the same mailbox. A
   sender outside any pane (CI, a person at a shell) uses a bare name, which
   is its own key. A process speaks as `--as`, else `$PHUX_AGENT_ADDRESS`,
   else its pane (`$PHUX_TERMINAL_ID`), else `$USER`. No registration or secret
   token is needed. Addresses are advisory, not authenticated execution identity.
   Headless children share a pane address but use distinct acquisition fences.

3. **Mail is stored, then rung.** `phux mail send TO… -t THREAD -m TEXT`
   stores one message for every recipient, idempotent by operation id. It
   then sends each recipient pane one single-line acknowledged prompt
   (ADR-0076) naming the sender, the thread, a preview, and the commands to
   read and reply. A pane with no agent record is never rung, and a bell
   that fails never loses the mail. `phux mail read` returns unread mail and
   marks it read; `phux mail ack` tells the sender a message was handled.

4. **Claims are advisory leases on absolute paths.** Each acquisition is
   all of its paths or none and receives one monotonically increasing fence,
   shared by that path set. `take` creates a new acquisition; it never adopts
   or implicitly renews a live one, even at the same address. Exclusive
   acquisitions conflict on overlapping paths regardless of holder. Shared
   holders can coexist, but taking a path already held by this same principal
   is still a conflict rather than an implicit renewal.

   `renew FENCE` extends precisely that holder's live acquisition, preserving
   its path set, note, sharing mode and fence. Expired or superseded receipts
   fail without acquiring anew. `release FENCE` removes precisely that
   acquisition; repeating old cleanup returns zero and cannot delete its
   replacement. There is no name-only, path-only or release-all mutation.
   No additional owner registry, secret token or claim-group table is needed.

   Expiry (at most 24 hours) is authoritative. Process death, runner completion,
   a quiet pane or a reused pane number never authorize stealing or automatic
   cleanup. Handoff means stop writing, release the exact old receipt, then
   acquire a fresh fence; the recipient must still handle a competing claim.
   Never substitute a newly listed fence for a stale receipt automatically.
   Claims do not enforce filesystem access and are not an authentication
   boundary: they coordinate trusted processes on one host.


5. **MCP mirrors the CLI.** `phux_mail_send`, `phux_mail_read`,
   `phux_mail_ack`, `phux_claim_take`, `phux_claim_renew`, `phux_claim_release`, and
   `phux_claim_list` run the CLI and return its `phux.coord/v1` document, so
   the store has one client and one identity rule. The adapter runs in the
   agent's pane and speaks as that pane.

6. **Blackbird is retired.** phux does not read, write, or bridge Blackbird.
   The optional Blackbird correlation field ADR-0095 allowed in the
   `phux.agent/v1` record has no remaining writer. Retiring the Blackbird
   service and its client configuration is an operator step outside this
   repository.

## Why

The pane is the one identity every agent here already has, and the one phux
already attests and resolves. Mail keyed by it needs no registration, so
every agent can reply. A prompt is still how an idle agent is woken, but now
it carries a sender and a thread and points at a record, instead of being
the message.

The work store is the natural home: it is per host, durable, single-writer,
idempotent by operation id, and already started on demand by the phux
binary. Mail and claims add tables to it, not a daemon, a port, or an
install step.

## Tradeoffs

- **One host.** The store is per host, so mail and claims do not cross
  machines. An agent on a remote host is reached by running `phux mail` on
  that host. Cross-host coordination waits for the coordinator
  ([ADR-0097](./0097-durable-coordinator-is-a-separate-bounded-endpoint.md)).
- **A pane is reused.** Mail to `@749` is read by whatever agent occupies
  that pane later. Agents that restart in place keep their mail, which is
  usually wanted. A pane closed and its number reused gets the old pane's
  unread mail.
- **Agents outside phux** use a bare name with `--as`, and are never rung.
- **Advisory only.** A claim cannot stop a write. Fences make supersession
  detectable, not prevented.
- **The keeper is a single process.** Every mail and claim call is a request
  to it. The calls are small, and the keeper already serves every mission
  call.

## Alternatives

**Keep Blackbird and have integrations register.** This keeps two identities
for one agent and a second daemon to install, supervise, and keep in step.
It also leaves the doorbell to phux anyway, because only phux can reach the
pane.

**Coordinate through the server's L3 metadata.** Mail would then live and
die with the server process and need new keys on the wire. The work store
already survives server restarts and keeps the server unchanged.

**Use prompts alone.** This is what agents did in practice. A prompt has no
sender, no thread, no record, and no acknowledgement.
