# Substrate

The authority and work-store constraints below extend the
[architecture](./architecture.md) and [decisions](./decisions.md). They
describe trust boundaries, not a required organization or deployment layout.
The decision was adopted in [#1689](https://github.com/no-phux/phux/pull/1689).

## phux is the only layer

Hosts are phux servers. Clients and adapters use their existing grants;
displaying another host's sessions creates neither authority nor a
coordinator. Mail and path claims live in the phux work store. Blackbird is
retired from phux coordination, not a second store to bridge or restore.

## One home keeper

One keeper process is the only writer to a work store. Shared work has one
authoritative home store, not independently writable replicas. This avoids
conflicting task transitions and duplicate execution. Mission control is a
client composition over that store, not another work authority. This is the
accepted keeper constraint from [historical ADR-0173][keeper-design]; its archived Proposed
status is not the live decision.

The shipping [keeper exchange](../crates/phux-work/src/wire.rs) is a private
local-socket interface between equal builds, **not** a public cross-host
coordinator protocol. Joined satellites can already use their hub's mail
and claims through the [hub work-store route](./agents.md#mail-across-a-hub).
That route does not implement the proposed multi-host task coordinator.

The accepted forward-compatible design for a future coordinator uses a
separate `phux-coordinator/1` endpoint and a short-lived activation
certificate from an external fencing witness. Terminal streams never carry
coordinator frames. See [historical ADR-0097][coordinator-design]. Neither
that endpoint nor its fencing service ships today; ordinary QUIC terminal
attach is not a substitute.

## Three principal kinds

[`PrincipalKind`](../crates/phux-core/src/actor.rs) distinguishes human,
agent, and automation for input-lease precedence. Kind grants no verb.
The owner is the serving user; other principals act through credentials.
The organization store is not a cross-host human roster. Use a paired
`human` credential for another person rather than treating their host as
an extension of the owner's authority.

### Audience binding

A credential names one server; a local delegated agent credential also
names one session. A hop grants no authority on another server. A relay's
route admission is not consumer authorization: relayed consumers use
end-to-end TLS with the origin, which authenticates their workload
certificates and enforces their scopes.

### Narrow delegation that keeps the human

Committed changes record an [`Actor`](../crates/phux-core/src/actor.rs),
including `on_behalf_of` when a delegated principal exercises another
principal's grant. Delegation only narrows. The current local agent grant
carries Inventory, Observe, Input, and Bind on its own session, for 12 hours
by default and 24 at most. It is held in an
[in-memory ledger](../crates/phux-server/src/delegation.rs), not the
workload registry. See [agent safety](./agents.md#safety).

Minting or withdrawing a delegated credential requires Terminate on the
owner socket. Scoped grants cannot mint; there is no Mint capability.
A scoped Lead grant with Create and Organize, but neither Terminate nor
Signal, is not implemented. Running a Lead with owner authority requires a
person attending; it must not become
an unattended credential-minting workaround. A future worker-launch grant
must keep minting with the owner keeper (or a human pre-mint), restricted to
the task's owner-created sessions and on-behalf-of human, no broader than
the current local agent grant, with the Lead recorded as delegator and
credentials ending with the task.

A cold restart drops the ledger and ends every delegated credential. The
task then needs a person: **no unattended re-mint** without a durable grant
ledger and an explicit authority design. An in-place upgrade preserves the
live ledger. Cloud agents and bots have no implicit path to these local
credentials merely because an adapter can call phux.

### Origin checks

Each server [checks scope on its own dispatch](./remote.md#who-can-do-what).
A hub's admission is not authority on another server. **Reverse join is
different:** its streams are spliced into the satellite's owner socket,
giving the hub that server's owner authority
([uplink](../crates/phux-server/src/uplink.rs)). Use it only between one
person's own machines, never as scoped sharing between different humans.

Revocation closes live connections. Pairing expiry is capped at 30 days.
An overlay network address supplies transport, not authority; pairing and
origin checks still apply.

## Adapters, one way

External systems retain their own truth: for example, GitHub owns PR and CI
state, while the keeper owns its work records. Adapters write in one
direction; do not bidirectionally synchronize a pair of stores or let an
adapter's private run state become a competing task authority.

Keep secrets out of band and credentials in the files the issuing host
wrote. Give each caller its own key id so revoking one does not revoke
everyone. Host receipts are bounded to 24 hours and 10,000 entries; they
are not a durable cross-host audit trail.

## Grants across hosts

Host-scoped grants already exist through `phux workload add-key`: the
issuing owner chooses scope, expiry, and principal, and that server checks
and revokes the grant. A delegated per-agent grant preserving the human
principal across hosts does not ship. Reverse join's owner authority and
relay admission do not supply it.

The multi-host task coordinator, durable grant ledger, autonomous Lead,
cross-host per-agent delegation, and organization-to-organization trust
remain unshipped designs, not promises tied to a launch date.

[coordinator-design]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0097-durable-coordinator-is-a-separate-bounded-endpoint.md
[keeper-design]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0173-mission-control-is-a-client-composition.md
