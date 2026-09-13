---
audience: contributors
stability: stable
last-reviewed: 2026-09-02
---

# 0095 — Blackbird is a peer ledger, not a phux client

**TL;DR.** phux and Blackbird do not connect. Blackbird holds no phux workload
key, provisions no phux key registry, and reads no `phux agent probe` output;
phux writes nothing to Blackbird. The seam is one additive, optional field
inside the existing `phux.agent/v1` record ([ADR-0040](./0040-agent-identity-metadata.md)),
written by the agent-host integrations phux already ships, purely so the two
ledgers can be joined after the fact. This deletes the "required by Blackbird
ADR-0005" justification from `phux-pjc5` and `phux-ktzk`: that document was
never written and its architecture is archived.

Status: Accepted
Date: 2026-09-02

## Context

Four open issues in this repository are justified by a document in another one
that does not exist.

`phux-pjc5` (P0) opens "Implement the `phux-workload/v1` service pairing
required by **Blackbird ADR-0005**," and its design puts Blackbird in the
registry-provisioning path: "Provisioned by new `phux workload
add-key|list|revoke` CLI … and by Blackbird at `RuntimeEndpointRegistration`."
`phux-ktzk` opens "Expose the additive machine contract required by **Blackbird
ADR-0005**: `phux agent probe --json` returns `schema_version
phux.agent.probe/v1`, `binary_version`, `build_id`, `selected_protocol`, opaque
HELLO `server_id` …". `phux-pjc5.1` records the blocker directly: "ADR stays
Proposed until Blackbird ratifies its half (blackbird ADR-0005 does not exist
yet)."

It does not exist, and it is not late. The path
`docs/architecture/adr/0005-phux-runtime-binding.md` appears exactly once in
the Blackbird repository — in the reference list of its **archived**
effect-sidecar design, where Blackbird was to be a sidecar dialing a "runtime
provider / phux" and registering endpoints with it. Blackbird archived that
architecture. It also deleted the sealed identity and work plane that
`RuntimeEndpointRegistration` belonged to. The requirement outlived the design
that produced it, in a backlog in a different repository, and it has been
holding this repository's only open P0 outside the epics.

Meanwhile the two systems settled into a shape that needs no connection at all.
[ADR-0092](./0092-durable-work-coordinator-authority.md) claims durable
Objectives, Runs, WorkSessions, Artifacts, and Signals for a phux coordinator —
facts derived from execution phux owns, carrying source authority, incarnation,
sequence, and explicit gaps. Blackbird kept the facts nothing executes: agent
identity per project key, durable mail and conversations, and path reservations
with leases and fencing tokens. None of those require a PTY, a phux server, or
a terminal; agents assert them from IDEs, from CI, and from hosts where phux is
not installed.

Those are different authorities over different kinds of truth. phux cannot
arbitrate a lease between an agent it runs and one it does not. Blackbird
cannot attest that a process exited.

## Decision

1. **No connection in either direction.** Blackbird is not a `phux-workload/v1`
   peer and never appears in the admission path; phux does not dial Blackbird's
   HTTP or MCP transports. Neither daemon's availability affects the other's,
   and neither is a startup or handshake dependency of the other. This is the
   position [ADR-0092](./0092-durable-work-coordinator-authority.md) already
   implies by making the coordinator the sole writer for its own authority
   domain, stated explicitly for the one external system most likely to be
   confused for a peer.

2. **`phux-pjc5` and `phux-ktzk` lose their stated requester, not necessarily
   their value.** Scoped policy is worth building on the justification
   `phux-pjc5`'s own sequencing note gives — "scoped policy is what lets a
   human hand an agent a rented box without handing it their machine" — and
   that argument stands on its own. What must not survive is the claim that an
   external contract obliges it, or the design point provisioning phux's key
   registry from Blackbird. This ADR is the anchor, not the tracker: the pjc5
   chain and `phux-ktzk` currently survive only in the passive
   `.beads/issues.jsonl` export and are absent from the live database, so
   whichever text comes back must be reconciled against this decision rather
   than the other way round. The workload-auth ADR is unblocked either way,
   because there is no counterparty ratification left to wait for; it takes the
   next free number after this one.

3. **The seam is one optional metadata field.** Where an agent is both
   registered with Blackbird and running in a phux terminal, the agent-host
   integration writes the Blackbird correlation — the agent id and the project
   key it registered under, nothing more — into that terminal's
   `phux.agent/v1` record. [ADR-0040](./0040-agent-identity-metadata.md) made
   that record normative, `TerminalId`-scoped, and writable by "anything that
   can reach the socket," with an open field set; the integrations from
   [ADR-0067](./0067-cache-preserving-agent-fleet-context.md) already refresh
   agent projections each turn and are the natural writer. This adds no frame,
   no capability bit, no version bump, and no server interpretation — phux
   stores the bytes opaquely, exactly as it does today. Absent Blackbird the
   field is simply missing, which every consumer of the record already
   tolerates.

4. **The observation plane stays out of phux.** Token spend, model attribution,
   throughput, and cost belong to Blackbird.
   [ADR-0009](./0009-phux-vs-mux-positioning.md) put cost dashboards outside
   phux and [ADR-0092](./0092-durable-work-coordinator-authority.md) restated
   it while widening scope in every other direction; this ADR does not reopen
   either. [ADR-0082](./0082-retire-the-ci-metrics-store.md) supplies the
   operative half — a dashboard belongs to a consumer "fed by a source phux
   does not have to run" — and Blackbird is that source, since the token counts
   come from harness provider accounting rather than from terminal I/O.

## Authority

| Fact | Authority |
|---|---|
| Process, PTY, exit, geometry, output order, bootstrap generation | Terminal owner |
| Input lease, signal delivery, acknowledged input result | Terminal owner |
| Objective, Run, WorkSession, Artifact, Signal lineage | Work coordinator (ADR-0092) |
| Agent name, kind, lifecycle state, attention for a live terminal | `phux.agent/v1` record |
| Agent identity under a project key; mail; path leases and fences | Blackbird |
| Token spend, latency, model attribution | Blackbird |
| The correlation between the two ledgers | The integration that wrote it |

A concept appearing on both sides of that line is a defect in one of them.
phux does not mint agent coordination identity, path reservations, or fencing
tokens; Blackbird does not mint Run or WorkSession identity.

## Tradeoffs

- The join is best-effort and lossy. An agent that never ran in a phux terminal
  has no correlation at all, and a stale record can outlive its writer —
  [ADR-0040](./0040-agent-identity-metadata.md) already accepts that, and
  analysis must tolerate a missing key rather than assume one.
- Two durable stores exist once ADR-0092 ships. Accepted, because they hold
  disjoint facts; the non-duplication rule above is enforced by review rather
  than by a test, which is the weakest part of this decision.
- Deleting a requirement is not the same as deciding the feature. `phux-pjc5`
  now needs its own prioritization argument, and losing an external forcing
  function may mean it moves later.
- A correlation field carries an identifier from another system into a record
  that reaches model context under
  [ADR-0067](./0067-cache-preserving-agent-fleet-context.md). It stays inside
  that ADR's bounds, labelling, and control-character stripping; it is an
  opaque local id, not a credential, and nothing about it is authorization.

## Alternatives

**Wait for Blackbird ADR-0005.** Rejected: it would have to be written to
ratify an architecture Blackbird has archived, and waiting has already cost
this repository an indefinitely blocked P0.

**Make Blackbird a workload peer anyway.** Rejected: it buys neither system
anything. Blackbird would gain read access to terminal state it does not act
on, in exchange for a key registry, a challenge exchange, a revocation path,
and a dependency on a daemon most Blackbird deployments do not run.

**Carry the correlation on the wire as a new frame or capability.** Rejected:
it puts a foreign system's identity into the protocol's compatibility surface
under [ADR-0061](./0061-capabilities-add-versions-break.md)'s lockstep rule, to
express something an opaque L3 record already expresses at zero protocol cost —
which is the trade [ADR-0040](./0040-agent-identity-metadata.md) already made
for agent identity itself.

**Have the coordinator absorb Blackbird's coordination facts.** Rejected: the
coordinator's authority derives from owning the execution that produced a fact.
A lease between an agent in phux and an agent in CI has no such execution, so
the coordinator would be asserting agreements it cannot attest to — the same
objection ADR-0092 raises against making any single consumer authoritative.
