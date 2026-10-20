# Substrate

phux is the substrate. Humans, hosts, and agents are phux principals. Work
for the org lives in one home keeper. Hosts are phux servers. Linear,
GitHub, beads, the ops Workers, and Grok Bot attach as clients or one-way
adapters. phux is the only layer, and Blackbird stays frozen.

This page is the live decision for [PHUX-360][phux-360], recorded in
[#1689][pr-1689]. Numbered ADRs stay
in the [historical snapshot](./architecture.md#historical-decisions) at
`d21db2e3`, unchanged. Code ranges are pinned to main `792a9fe9`. Nothing
here changes the wire.

## phux is the only layer

Federation is phux itself: phux hosts, phux principals and grants, and one
home keeper. The desktop catalog of sessions on registered hosts
([#1470][pr-1470]) is a view of those sessions. It adds no grant and no
coordinator.

Blackbird stays frozen. [ADR-0095][adr-0095-boundary] closed a connection
in either direction. [ADR-0187][adr-0187-retired] (Accepted) then retired
Blackbird: mail and path claims live in the phux work store, and the
ADR-0095 boundary retires with Blackbird. The retirement stands.

## One home keeper

[ADR-0173][adr-0173-keeper] lines 62–64: one keeper process per store is
the only writer, and another host's tasks are reached through that host's
keeper. S-D10 is the policy that names one home keeper for the org, on the
Mac mini. Other hosts are clients of it.

The archive copy of ADR-0173 is still [Proposed][adr-0173-status]. S-D7
accepts it on this page. The snapshot is left as it was.

The shipping keeper speaks the local-socket protocol in
[phux-work](../crates/phux-work/src/wire.rs)
([private exchange][code-keeper]): a private interface between equal
builds, distinct from `phux-coordinator/1`. Before launch, other hosts are
not yet dialing that keeper. The cross-host view until then is
[`phux ls`](./reference/cli.md#phux-ls) across hosts, over credentials that
already exist.

The coordinator wire is already decided. [ADR-0097][adr-0097-status] is
Accepted (forward-compat). The coordinator is a separate
`phux-coordinator/1` endpoint. Terminal streams never carry its frames
([endpoint][adr-0097-endpoint]). Each activation takes a short-lived
certificate from an external fencing witness
([witness][adr-0097-witness]). Once the coordinator ships (MC-5.1),
another host reaches the home keeper on that endpoint. The existing
QUIC attach stays the terminal stream, and this decision adds no wire
ADR. [ADR-0092][adr-0092-authority] (still
[Proposed][adr-0092-status]) is the authority that endpoint serves.
[ADR-0132][adr-0132-clients] (still [Proposed][adr-0132-status]) makes
swarm members coordinator clients: they authenticate to
`phux-coordinator/1` as peers of the other projections.

Mission control is the human surface of the keeper. Capture that routes
once to a tracker, and keeps no work of its own, is intake in front of
that surface.

The multi-host coordinator itself is after launch (S-D2).

## Three principal kinds

[ADR-0163][adr-0163-kinds] names three kinds: human, agent, and
automation. Kind decides input-lease precedence and grants no verb. On
main, [`PrincipalKind`](../crates/phux-core/src/actor.rs) is those three
([definition][code-kind]), and [`PrincipalId`][code-principal-id] is the
Owner, a named credential, or System.

The [organization store](../crates/phux-server/src/org_store.rs) holds
sessions, resources, arrangements, and receipts
([ADR-0162][adr-0162-store], [schema][code-org-schema]). It has no member
table. A human is the server Owner — identity is the serving user
([ADR-0106][adr-0106-user]) — or a paired-device credential of kind
`human` on someone else's host. A roster that names the same human on
every host is later work. Until it lands, a member who is not the Owner
is a paired `human` credential, and that person's own host is not joined
(S-D1).

### Audience binding

A credential names one server. An agent credential also names one session.
A hop does not forward it. Across a relay, authority is the tunnel's route
authority; per-client authority across a relay is future work
([ADR-0116][adr-0116-relay]).

### Narrow delegation that keeps the human

Every committed change records an actor with `on_behalf_of` the human
whose grant the agent exercises ([`Actor`][code-actor]). Delegation only
narrows. The local grant is Inventory, Observe, Input, and Bind on the
agent's own session, 12 hours by default and 24 at most, held in memory
([ADR-0163][adr-0163-delegate] §5,
[ledger](../crates/phux-server/src/delegation.rs),
[verbs and lifetime][code-delegation]). The live rules are
[delegated execution](./agents.md#delegated-execution-identity).

Minting or withdrawing that credential requires Terminate on the owner
socket ([kind rule][code-mint]). A scoped grant never mints
([enforcement][code-enforce], [test][code-mint-test]). The launcher mints
when the launcher is the owner.

A scoped Lead grant (Create and Organize, and neither Terminate nor
Signal) is later work. It adds no Mint capability; a Mint verb would be a
protocol change. Under S-D5 the owner keeper mints the worker credential
and records Lead as delegator. Until that grant exists, Lead runs with
the owner's authority only while a person is attending.

Grok Bots and cloud agents have no path to this credential: the ledger is
a local socket. At launch they stay clients (S-D8).

### Origin checks

Each server checks scope on its own dispatch
([connection authority](./remote.md#connection-authority)). A hub's
admission is not authority on another server. The exception is a reverse
join: each hub stream is spliced to the satellite's owner socket, so the
hub holds that server's owner authority
([uplink](../crates/phux-server/src/uplink.rs), [splice][code-uplink],
[linking](./remote.md#linking-machines)). Before launch that join stays
between one person's own machines (S-D3).

Revocation closes the live connection
([watcher](../crates/phux-server/src/runtime/revocation.rs)).
[`phux pair`](./reference/cli.md#phux-pair) `--expires` is at most 30 days
([cap][code-pair]). An overlay address is transport for pairing and listen
([environment](./reference/environment.md)). Pairing still decides
authority.

A cold restart drops the in-memory ledger, so every delegated credential
ends. The task then needs a person. Unattended re-mint waits on the
durable grant ledger, which is after launch.

## Adapters, one way

GitHub owns pull-request and CI truth. The keeper owns task state. Linear
and beads are trackers. The ops Workers and Grok Bot attach as clients or
adapters. An adapter that still holds its own run state does not own the
org's tasks. Each adapter writes in one direction. No pair of stores syncs
both ways before launch.

Worker secrets stay out of band. One key id per caller, so revoking one
caller leaves the others in place (S-D4). phux credentials stay in the
files the issuing host wrote.

Receipts on a host are kept for 24 hours and at most 10,000
([caps][code-receipts]). A durable cross-host audit trail is after launch.

## Grants across hosts

A human or a host can already hold a host-scoped grant. The owner of the
issuing server enrolls it with
[`phux workload add-key`](./reference/cli.md#phux-workload): a scope, an
expiry, and a principal ([scope and expiry][code-workload-scope],
[principal][code-workload-principal]). That server checks the grant at the
origin and revokes it live. This is the launch path for a paired human and
for an observe-only host credential.

A per-agent grant that keeps the human principal across hosts is not in
the tree. A reverse join is owner authority
([uplink](../crates/phux-server/src/uplink.rs), [splice][code-uplink]).
A relay is route authority ([ADR-0116][adr-0116-relay]), and a delegated
credential is a local socket. That grant is after launch (S-D2).

## Launch cut

Before 5 November 2026, Phase 0 is this decision and the policy it names:
one home keeper on the Mac mini, one-way adapters, reverse join only
between one person's own machines, and one key id per caller. Per-agent
cross-host grants, the multi-host coordinator on `phux-coordinator/1`,
the durable grant ledger, and org-to-org trust are after launch.

Ops hub runners gain no new session-start features now. They fold into
[`phux mission start`](./reference/cli.md#phux-mission-start) after launch
(S-D6).

## Decisions

Accepted defaults. On 2026-10-20 at 10:52am PT, Patrick replied "defaults"
to Chief of Staff, accepting S-D1 through S-D10 as written in the plan.

- **S-D1. Membership.** jamesponwith is a member under the one home keeper,
  in this org rather than a separate federating org. Until a roster names
  humans across hosts, he is a paired `human` credential on this org's
  hosts, and his own host is not joined.
- **S-D2. Launch cut.** Phase 0 only before 5 November 2026. Per-agent
  cross-host grants, the multi-host coordinator, the durable grant ledger,
  and org-to-org trust wait until after that launch.
- **S-D3. Reverse join.** A satellite join, which gives the hub owner
  authority, is allowed only between one person's own machines. It stays
  off between different humans.
- **S-D4. Caller keys.** One key id per bot or person replaces a shared
  operator key. Revoking one caller leaves the others in place. Secrets
  stay out of band.
- **S-D5. Lead's workers.** The owner keeper mints only Inventory, Observe,
  Input, and Bind, only on sessions it created for a task Lead started,
  only for that task's on-behalf-of human. It records Lead as delegator.
  The credential expires with the task. Fallback: the human pre-mints.
  There is no Mint capability.
- **S-D6. Runners.** Ops hub runners gain no new session-start features
  now. They fold into `phux mission start` after launch.
- **S-D7. ADR-0173.** Accepted. The interim store already shipped. The
  archive copy stays as it was; this page is the acceptance.
- **S-D8. Bots.** Grok Bots and cloud agents are not phux principals at
  launch. They stay clients. Revisit after launch.
- **S-D9. Access inputs.** Add the operator's sign-in. Argo stays on the
  PHA team until told otherwise. A Linear API credential is supplied out
  of band. Mail routing for the phux domain is enabled.
- **S-D10. Home keeper.** The Mac mini runs the org's one keeper. Other
  machines are clients.

[phux-360]: https://linear.app/phall/issue/PHUX-360/s-1-adr-the-substrate-is-phux-principals-hosts-one-home-keeper
[pr-1689]: https://github.com/no-phux/phux/pull/1689
[pr-1470]: https://github.com/no-phux/phux/pull/1470
[adr-0092-status]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0092-durable-work-coordinator-authority.md#L15
[adr-0092-authority]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0092-durable-work-coordinator-authority.md#L64-L73
[adr-0095-boundary]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0095-the-blackbird-boundary.md#L64-L71
[adr-0097-status]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0097-durable-coordinator-is-a-separate-bounded-endpoint.md#L16
[adr-0097-endpoint]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0097-durable-coordinator-is-a-separate-bounded-endpoint.md#L40-L46
[adr-0097-witness]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0097-durable-coordinator-is-a-separate-bounded-endpoint.md#L51-L58
[adr-0106-user]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0106-identity-is-the-serving-user.md#L38-L40
[adr-0116-relay]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0116-workload-auth-is-mtls.md#L74-L78
[adr-0132-status]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0132-swarm-members-are-coordinator-clients.md#L14
[adr-0132-clients]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0132-swarm-members-are-coordinator-clients.md#L47-L51
[adr-0162-store]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0162-organization-records-persist-and-activations-reconcile.md#L9-L11
[adr-0163-delegate]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0163-actors-and-delegated-agent-grants.md#L62-L69
[adr-0163-kinds]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0163-actors-and-delegated-agent-grants.md#L123-L132
[adr-0173-status]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0173-mission-control-is-a-client-composition.md#L15
[adr-0173-keeper]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0173-mission-control-is-a-client-composition.md#L62-L64
[adr-0187-retired]: https://github.com/no-phux/phux/blob/d21db2e345aeaff43a681b3a04dfd43f2cc082ca/docs/adr/0187-agents-coordinate-through-phux.md#L9-L16
[code-keeper]: https://github.com/no-phux/phux/blob/792a9fe90fd02cdb281f5e550e48f052aca71134/crates/phux-work/src/wire.rs#L1-L4
[code-kind]: https://github.com/no-phux/phux/blob/792a9fe90fd02cdb281f5e550e48f052aca71134/crates/phux-core/src/actor.rs#L22-L28
[code-principal-id]: https://github.com/no-phux/phux/blob/792a9fe90fd02cdb281f5e550e48f052aca71134/crates/phux-core/src/actor.rs#L8-L14
[code-actor]: https://github.com/no-phux/phux/blob/792a9fe90fd02cdb281f5e550e48f052aca71134/crates/phux-core/src/actor.rs#L39-L50
[code-org-schema]: https://github.com/no-phux/phux/blob/792a9fe90fd02cdb281f5e550e48f052aca71134/crates/phux-server/src/org_store.rs#L245-L289
[code-receipts]: https://github.com/no-phux/phux/blob/792a9fe90fd02cdb281f5e550e48f052aca71134/crates/phux-server/src/org_store.rs#L26-L30
[code-delegation]: https://github.com/no-phux/phux/blob/792a9fe90fd02cdb281f5e550e48f052aca71134/crates/phux-server/src/delegation.rs#L35-L42
[code-mint]: https://github.com/no-phux/phux/blob/792a9fe90fd02cdb281f5e550e48f052aca71134/crates/phux-protocol/src/kinds.rs#L838-L842
[code-enforce]: https://github.com/no-phux/phux/blob/792a9fe90fd02cdb281f5e550e48f052aca71134/crates/phux-server/src/policy/enforce.rs#L304-L309
[code-mint-test]: https://github.com/no-phux/phux/blob/792a9fe90fd02cdb281f5e550e48f052aca71134/crates/phux-server/src/policy/tests.rs#L1378-L1380
[code-uplink]: https://github.com/no-phux/phux/blob/792a9fe90fd02cdb281f5e550e48f052aca71134/crates/phux-server/src/uplink.rs#L10-L13
[code-pair]: https://github.com/no-phux/phux/blob/792a9fe90fd02cdb281f5e550e48f052aca71134/crates/phux/src/commands/pair.rs#L2013-L2018
[code-workload-scope]: https://github.com/no-phux/phux/blob/792a9fe90fd02cdb281f5e550e48f052aca71134/crates/phux/src/commands/workload.rs#L83-L100
[code-workload-principal]: https://github.com/no-phux/phux/blob/792a9fe90fd02cdb281f5e550e48f052aca71134/crates/phux/src/commands/workload.rs#L120-L126
