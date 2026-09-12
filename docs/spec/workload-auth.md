---
audience: consumers, contributors, agents
stability: stable
last-reviewed: 2026-09-12
---

# Workload authority over mTLS — authentication and scoped authority

**TL;DR.** Phux endpoints authenticate workloads with mutual TLS, not a
bespoke handshake. The server mints a CA on first routable listen;
`phux pair` enrolls client certificates; the TLS handshake proves the
peer holds the private key on that channel, and the registry maps the
client identity to a closed scope ceiling enforced before dispatch.
Expiry and revocation terminate live connections. Owner UDS keeps
kernel-uid authority; the bearer token stays outer admission only.
(ADR-0116 retires the unshipped `phux-workload/v1` proof profile this
document previously specified; the authorization half stands.)

---

<!-- impl-status: foundation; probe: MtlsWorkloadIdentity,TerminalScopeSet,TerminalEffectiveScopeSet -->
> **Status: foundation landed.** The persisted workload CA, client enrollment,
> credential-id derivation, and validated scope registry are implemented in
> `phux_server::workload`. TLS handshake enforcement, scope classification,
> and live revocation remain follow-up work; the current `PolicyEngine` default
> is still permissive until those seams are wired.

## 1. Profile boundary

Authentication is the TLS handshake. There is no workload handshake
frame, no challenge/response exchange, no transcript, and no HELLO
workload field on any endpoint. Each endpoint SHALL define:

- its own HELLO and HELLO_OK carriers (unchanged);
- one closed canonical scope schema and its operation classification
  table (§5, §6); and
- nothing else: proofs are not endpoint-shaped.

The terminal endpoint's scope schemas are `TerminalScopeSet` and
`TerminalEffectiveScopeSet` (§5). A separate coordinator endpoint uses
its own schema; a coordinator frame is never sent to an L1 parser
([ADR-0092](../../ADR/0092-durable-work-coordinator-authority.md)).
Endpoints SHALL NOT translate names or bits from another endpoint. No
`service` string exists: the TLS session already binds identity to
channel, so there is nothing to name.

## 2. Identities and persisted material

Three identities remain separate:

| Identity | Form | Lifetime and use |
|---|---|---|
| CA fingerprint | SHA-256 of the DER CA certificate | stable across restart and endpoint; the durable value a client pins; rotation is explicit re-pairing |
| Server identity | server certificate issued by the CA | proves the endpoint to the client at handshake; rotation is transparent while the CA stands |
| Workload credential id | SHA-256 of the raw client public key | registry lookup and live-revocation handle |

The canonical human/CLI form for either digest is `sha256:` followed by
exactly 64 lowercase hexadecimal digits. Parsers SHALL reject uppercase,
padding, omitted leading zeroes, and alternate encodings; protocol state
carries raw bytes. Comparisons are constant-time.

A client may persist a server binding only after verifying the server
certificate chains to its pinned CA. That binding keys on the CA
fingerprint; socket path, URL, certificate instance, and server
incarnation are deliberately absent. A changed endpoint does not change
authority, while a changed CA fingerprint invalidates cached projections
and all mutating authority. A client with a prior binding SHALL surface
`authority_changed` and refuse; it SHALL NOT silently trust the
replacement.

The CA's key file SHALL persist at `<state-dir>/workload-ca.key` with
the CA certificate at `<state-dir>/workload-ca.pem`. The containing
state directory SHALL be owner-only. Key and registry files SHALL be
regular, owner-owned, no-follow-opened files with mode `0600`; creation
and replacement SHALL use an owner-controlled lock, same-directory
temporary file, file sync, atomic rename, and directory sync. A missing
CA may be created only by an explicit initialization path (first
routable listen, or `phux workload authority --init`).

`<state-dir>/workload-keys` is the registry of workload credentials.
Each record contains the client certificate (or raw public key),
derived credential id, canonical scope ceiling, absolute expiry, and
optional revocation time. One invalid record makes the loaded snapshot
malformed. The file obeys the same ownership, mode, no-follow,
stable-read, lock, sync, and atomic-replacement rules. Public keys and
fingerprints are not secrets. A malformed, replaced, or unstable read
is an empty authority snapshot, never permission to use a cached
generation.

Registry generations are live (§7). The registry is local Phux
authority; no UI, peer ledger, or coordinator is queried during
admission.

## 3. Authentication flow

```text
Client                                              Server
  | TLS ClientHello (SNI, ALPN)                        |
  |--------------------------------------------------->|
  |                         verify server chain to pinned CA
  | TLS handshake: server requests client certificate   |
  |<---------------------------------------------------|
  | client presents enrolled certificate (paired only)  |
  |--------------------------------------------------->|
  |                  verify chain to phux CA; map to registry
  | HELLO { version, caps } (ordinary frames resume)    |
  |--------------------------------------------------->|
  | HELLO_OK                                            |
  |<---------------------------------------------------|
  | ACTIVE                                              |
```

The server's TLS configuration requests — and in `paired` policy
requires — a client certificate verified against the phux CA
(`WebPkiClientVerifier` semantics: chain, expiry, and signature
validity; revocation is the registry's job, not the handshake's). The
verified identity (credential id plus transport evidence) is stamped
into `PeerIdentity` and handed to `PolicyEngine::authorize_hello`,
which intersects the registry ceiling into the connection's effective
grant before any stateful frame is processed.

- **No certificate, unknown certificate, or expired certificate** on a
  paired listener refuses the connection at the TLS layer — QUIC
  application close, WSS handshake rejection — before any phux frame
  is read. There is no phux-shaped error pre-HELLO because no phux
  frame has been exchanged.
- **A client configured to require paired authority** SHALL close
  without issuing a stateful operation when the server does not
  request a client certificate, or when the server certificate does
  not chain to the pinned CA. Receiving `HELLO_OK` over an
  unauthenticated channel is downgrade, not permission to continue.
- **TLS session resumption** preserves the authenticated identity: a
  resumed session carries the same verified peer as the session it
  resumes. 0-RTT application data is not used for phux frames.
- **Owner UDS** carries no TLS and therefore no certificate. The
  kernel-authenticated uid is the authority (§8): full operator grant
  in `local`, and in `paired` the UDS keeps kernel-uid authority
  rather than a proof it cannot perform (a deliberate, documented
  narrowing of ADR-0098's paired-UDS rule).
- **The bearer token stays outer admission.** The pairing token
  (preamble / `Authorization` header) proves "may knock" at
  establishment; it never mints authority and its store is consulted
  before, and independently of, the certificate registry.
- **The relay is per-hop.** It terminates TLS on both legs for SNI
  routing, so a consumer certificate does not survive to the server:
  consumer↔relay and tunnel↔server authenticate separately, and
  authority across a relay is the tunnel's enrolled route authority
  ([ADR-0116](../../ADR/0116-workload-auth-is-mtls.md)).

There is deliberately no nonce, no transcript, and no exporter
derivation in this profile: replay of a captured handshake is
meaningless (possession of the private key is proven inside a live
session, not in bytes that can be replayed), and the channel binding
*is* the TLS session rather than a value derived from it.

## 4. Scope model

Authority is a grant pairing one nonempty verb bitset with one
selector, exactly as before — the proof mechanism changed, the
authorization model did not.

## 5. TerminalScopeSet canonical encoding and intersection

A scope grant pairs one nonempty verb bitset with one selector. The closed verbs
are:

```text
INVENTORY = 0x01   // enumerate identities and bounded non-content state
OBSERVE   = 0x02   // read content, history, events, metadata, or telemetry
CREATE    = 0x04   // create a Terminal or other endpoint resource
BIND      = 0x08   // attach, resize, move, lease, or mutate metadata/projection bindings
INPUT     = 0x10   // deliver user/terminal input or upload workload bytes
SIGNAL    = 0x20   // process/server lifecycle, hooks, forced detach, or signals
```

Bits `0xC0` are unknown in v1 and SHALL cause rejection. A later verb requires a
new scope-schema version; it is not ignored.

Selectors and their canonical bytes are:

```text
GLOBAL                         = 0x00
HOST_LOCAL                     = 0x01 || 0x00
HOST_SATELLITE(host)           = 0x01 || 0x01 || V16(host)
GROUP(group_id)                = 0x02 || U32(group_id)
TERMINAL_LOCAL(id)             = 0x03 || 0x00 || U32(id)
TERMINAL_SATELLITE(host, id)   = 0x03 || 0x01 || V16(host) || U32(id)
```

`host` is 1..=255 UTF-8 bytes, contains no NUL or Unicode control scalar, and is
compared byte-for-byte without case folding or normalization. It is the exact
federation host key carried by `ResourceId::Satellite`, not an address resolved
from DNS. Unknown tags, subtype tags, zero-length hosts, and trailing selector
bytes are malformed.

`TerminalScopeSet` canonical bytes are:

```text
U16(grant_count) || repeated {
    U16(len(selector_bytes)) || selector_bytes || verbs[1]
}
```

with `U16` / `U32` / `V16` as unsigned big-endian helpers (`V16(bytes) =
U16(len(bytes)) || bytes`). The set has at most 64 grants. Grants are strictly
increasing by unsigned lexicographic order of `selector_bytes`; every selector
occurs once; `verbs` is nonzero. Encoders merge equal selectors by OR-ing their
verbs, remove zero entries, sort, and emit the shortest form. Decoders SHALL
reject an unsorted or duplicate selector, unknown bit, non-minimal length,
count/length mismatch, truncation, or trailing byte. They SHALL not normalize an
invalid image and then verify it.

A selector denotes resource subjects at enforcement time:

- `GLOBAL` contains every subject;
- a Host contains that host, its groups, and its Terminals;
- a Group contains that group and its current Terminal members; and
- a Terminal contains only that exact Terminal.

The local server owns local Groups. A satellite host selector never contains a
Group in the hub's local L3 store.

The intersection retains provenance as conjunctive clauses; it SHALL NOT flatten
a dynamic Group or Host ceiling into a permanent bare Terminal grant. For every
requested/ceiling pair whose verb intersection is nonzero and whose selectors
overlap at admission, emit:

```text
TerminalEffectiveScopeSet =
    U16(clause_count) || repeated {
        U16(len(requested_selector)) || requested_selector ||
        U16(len(ceiling_selector)) || ceiling_selector ||
        intersected_verbs[1]
    }
```

Clauses are unique and strictly ordered by
`requested_selector || ceiling_selector`, with equal pairs merged by OR-ing
verbs. At most 64 clauses and 32,768 encoded bytes are allowed; a larger
intersection denies admission rather than truncating authority. All other
`TerminalScopeSet` canonical-refusal rules apply. A subject
is authorized only when both selectors in one clause contain it under the
current authoritative topology. This conjunction is re-evaluated on every
dispatch under the same state snapshot used for routing. A
`GROUP(G) ∩ TERMINAL(T)` clause therefore stops matching immediately when `T`
leaves `G` and is never laundered into unconditional Terminal authority.
Authorization results SHALL NOT be cached across a topology generation; any
cache is keyed by and invalidated atomically with that generation.

Registry expiries are unsigned Unix seconds — policy times, not freshness
evidence — and SHALL be greater than the server's current time. The effective
grant is bound to the authenticated connection, server incarnation, credential
id, and registry generation; it SHALL not be serialized as a reusable bearer
credential.

## 6. Terminal endpoint mapping and total classification

The terminal protocol adds no workload frame and no HELLO workload field:
HELLO field ids 7 and 8 stay reserved and unassigned, `WORKLOAD_RESPONSE
= 0x04` / `WORKLOAD_CHALLENGE = 0x84` are retired-unshipped back to the
reserved pool ([appendix-reserved.md](./appendix-reserved.md)), and there
is no `WORKLOAD_AUTH` feature bit. Authentication arrives with the
connection, via `PeerIdentity`, before HELLO is evaluated.

Before any routing, lookup that could disclose existence, queue insertion,
mutation, handler call, or satellite forwarding, the server maps each decoded
client frame to the following requirement. Every listed subject must match an
effective grant carrying the listed verb. A `Terminal` may match its exact
Terminal, current Group, owning Host, or Global selector according to §5.

| Client-originated frame | Required verb | Subject selector |
|---|---|---|
| `HELLO` | handshake-exempt | valid only in `PRE_HELLO` |
| `PING` | liveness-exempt | no state access; allowed before HELLO |
| `DETACH` | cleanup-exempt | calling connection only |
| `ATTACH` existing/last target | `BIND` and `OBSERVE` | resolved Group; all returned Terminals are filtered by `OBSERVE` |
| `ATTACH` create-if-missing | `CREATE`, then `BIND` and `OBSERVE` | selected local Group; no creation occurs unless all requirements pass |
| `HISTORY_REQUEST` | `OBSERVE` | named Terminal |
| `FRAME_ACK` | `OBSERVE` | named Terminal and current stream/bootstrap generation |
| `COMMAND` | classified by nested command tag below | nested subjects; envelope alone grants nothing |
| `SUBSCRIBE` (unallocated) | default-deny | no selector contract exists |
| `INPUT_KEY`, `INPUT_PASTE`, `INPUT_MOUSE`, `INPUT_RAW`, `INPUT_FOCUS`, `INPUT_TERMINAL_REPLY` | `INPUT` | named Terminal |
| `VIEWPORT_RESIZE` | `BIND` | every currently attached Terminal; zero targets is a no-op |
| `SPAWN_RESOURCE { satellite: Some, owner_terminal: None }` | `CREATE` | named satellite Host |
| `SPAWN_RESOURCE { satellite: None, owner_terminal: None }` | `CREATE` | payload Group |
| `SPAWN_RESOURCE { satellite: None, owner_terminal: Some }` | `CREATE` and `BIND` | CREATE on the owner Terminal's side-effect-free resolved Group and BIND on that Terminal; payload Group MUST equal the resolved Group |
| `SPAWN_RESOURCE { satellite: Some, owner_terminal: Some }` | default-deny | invalid local/remote ownership combination |
| `SPAWN_RESOURCE { kind: AGENT_SESSION, satellite: None, parent: Some }` | `CREATE` and `BIND` | CREATE on the parent Terminal's side-effect-free resolved Group and BIND on that parent Terminal; the parent MUST lie within the effective grant; payload Group MUST equal the resolved Group |
| `SPAWN_RESOURCE { kind: AGENT_SESSION, satellite: Some(H), parent: Some(Satellite { H, .. }) }` | `CREATE` and `BIND` | CREATE on the satellite Host H and BIND on the satellite-tagged parent Terminal; a `LOCAL` or different-host parent is default-deny |
| `SPAWN_RESOURCE { kind: not TERMINAL, parent: None }` or `{ kind: TERMINAL, parent: Some }` | default-deny | kind and binding disagree; the decoder's `SPAWN_FAILED` never runs |
| `RESIZE_TERMINAL` | `BIND` | named Terminal |
| `MOVE_RESOURCE` | `BIND` | both moved and destination-owner Terminals |
| `SUBSCRIBE_EVENTS { terminal: Some }` | `OBSERVE` | named Terminal |
| `SUBSCRIBE_EVENTS { terminal: None }` | `OBSERVE` | installs a filtered subscription over all observable Terminals; server-global events require Global |
| `GET_METADATA` | `OBSERVE` | encoded metadata Scope; `{ Global, "phux.whoami/v1" }` answers only the asking connection's own identity |
| `SET_METADATA { Global, "phux.session.create/v1" }` | `CREATE` and `BIND` | Global; BIND alone MUST NOT create a process |
| `SET_METADATA { Global, "phux.session.keep_empty/v1" }` with value `name\0true` | `CREATE` and `BIND` | Global; the mark keeps a session, and so the server, alive with zero processes (ADR-0105) |
| `SET_METADATA { Global, "phux.session.keep_empty/v1" }` with any other value | default-deny | malformed; the value is classified before the handler parses it |
| `SET_METADATA { Global, "phux.config.reload/v1" }` | `SIGNAL` | Global |
| `SET_METADATA` or `DELETE_METADATA` targeting `phux.session.created/v1` or its slash-prefixed results | default-deny | server-owned result namespace is non-writable |
| `SUBSCRIBE_METADATA` targeting that result namespace | default-deny | server-owned connection-private results are non-subscribable |
| `SET_METADATA` or `DELETE_METADATA` targeting `phux.pane-occupant/v1` or `phux.whoami/v1`, or `DELETE_METADATA` targeting `phux.config.reload/v1` or `phux.session.keep_empty/v1` | default-deny | server-owned keys are non-writable |
| Other `SET_METADATA`, `DELETE_METADATA` | `BIND` | encoded metadata Scope |
| `LIST_METADATA` | `INVENTORY` | encoded metadata Scope; server-owned result keys remain excluded |
| `LIST_DIRECTORY` | `INVENTORY` | Global; the serving host's filesystem is server-global data, so no Terminal, Group, or Host grant reaches it |
| Other `SUBSCRIBE_METADATA` | `OBSERVE` | encoded metadata Scope |
| Unknown, wrong-direction, retired, or otherwise unclassified frame | default-deny | none |

For owner-addressed spawn, the guard resolves the owner's actual Group under the
same authoritative state snapshot used for creation. It checks BIND on the
owner and CREATE on that resolved Group before exposing either existence or
membership. After authorization, a payload Group unequal to the resolved Group
returns `SPAWN_FAILED` and creates nothing; it is never used as the CREATE
subject and never silently ignored.

`COMMAND` is an envelope, not an authority. Its `request_id` may be decoded for
correlation, but the nested tag SHALL be classified at one common command choke
point before any handler or satellite branch:

| Command variant | Required verb | Subject selector |
|---|---|---|
| `SPAWN` (unallocated) | default-deny | dedicated `SPAWN_RESOURCE` owns create |
| `ATTACH_RESOURCE` | `BIND` and `OBSERVE` | named Terminal |
| `DETACH_RESOURCE` | cleanup-exempt | calling connection's binding only |
| `KILL_RESOURCE` | `SIGNAL` | named Terminal |
| `KILL_RESOURCE_IF` | `SIGNAL` | named Terminal |
| `GET_SCREEN` | `OBSERVE` | named Terminal |
| `ROUTE_INPUT`, `APPLY_INPUT` | `INPUT` | named Terminal |
| `KILL_RESOURCES` | `SIGNAL` | every named Terminal; all-or-nothing |
| `RESIZE_TERMINAL` (unallocated) | default-deny | dedicated `RESIZE_TERMINAL` owns resize |
| `GET_STATE { SERVER }` | `INVENTORY` | requires at least one Inventory grant; returns only resources matched by those selectors, and server-global data only with Global |
| `RUN_HOOK` (unallocated) | default-deny | no wire contract exists |
| `GET_TERMINAL_STATE` | `INVENTORY` | named Terminal |
| `SUBSCRIBE_RESOURCE_EVENTS` | `OBSERVE` | named Terminal |
| `UPGRADE` | `SIGNAL` | Global |
| `ACQUIRE_INPUT`, `RELEASE_INPUT` | `BIND` | named Terminal |
| `SIGNAL_TERMINAL` | `SIGNAL` | named Terminal |
| `REPORT_ASKED`, `REPORT_AGENT_STATE` | `BIND` | named Terminal |
| `PUT_FILE` | `INPUT` | named Terminal |
| `DETACH_CLIENTS { session: Some }` | `SIGNAL` | resolved Group |
| `DETACH_CLIENTS { session: None }` | `SIGNAL` | Global |
| `SHUTDOWN` | `SIGNAL` plus transport predicate | Global, and the authenticated transport MUST be the owner UDS; remote paired grants cannot stop the server |
| `GET_PERF { reset: false }` | `OBSERVE` | Global |
| `GET_PERF { reset: true }` | `OBSERVE` and `BIND` | Global |
| `APPEND_RESOURCE_OUTPUT` | `BIND` and `INPUT` | the named resource's parent Terminal; a grant naming only the child does not suffice, and a Terminal-kind target is refused after admission with `WRONG_RESOURCE_KIND` |
| Unknown, retired, or otherwise unclassified command tag | default-deny | none |

<!-- impl-status: partial; probe: ResourceKind,COMMAND_TAG_APPEND_RESOURCE_OUTPUT -->
> **Status: partial.** The kind-bearing spawn rows and the
> `APPEND_RESOURCE_OUTPUT` row classify frames the codec decodes but no
> server serves; they bind the classifier the day the `AGENT_SESSION` kind
> lands ([L1.md §1.1](./L1.md)).

A resource bound to a parent ([L1.md §1.2](./L1.md)) is admitted through
that parent: for every row above whose subject is "named Terminal", a child
resource named in the frame matches when its parent matches the selector.
Producing into the child is the one operation classified on the parent
alone, because the child's stream is that parent's agent account of itself
and a workload allowed to type into a pane is exactly the workload allowed
to narrate it.

For a metadata `Scope::Resource`, `Scope::Group`, or `Scope::Global`, the subject
is respectively that Terminal, Group, or Global. A result assembled from several
resources SHALL be filtered at the source as well as admission-checked; authority
to enumerate a container does not disclose members outside the effective set.

When deriving a selector requires server state (for example a named ATTACH or
forced-detach Group), the derivation is a side-effect-free part of the guard.
It returns the same denial for absent and unauthorized targets and SHALL not
leak existence, membership, or routing state before authorization succeeds.

The reference enforcement points are the terminal client frame loop immediately
after decode/state validation, and `handle_command` immediately after nested-tag
decode and before its satellite-relay branch. Per-handler checks may enforce
additional domain invariants but SHALL not replace either common check.

## 7. Denial, expiry, and live revocation

A TLS-layer refusal (no, unknown, or expired client certificate on a paired
listener) closes before any phux frame; there is no `DETACHED` because no
HELLO was exchanged. Diagnostics and timing SHALL not reveal which test
failed.

After authentication, an out-of-scope correlated frame or command receives its
ordinary correlated error carrying `PERMISSION_DENIED`; no effect occurs and the
connection stays active. An out-of-scope frame without a response channel is
dropped before effect and MAY receive a rate-limited uncorrelated
`PERMISSION_DENIED`; it does not close the connection. Repeated denials MAY be
rate-limited or cause an explicit policy close, but silence SHALL never turn the
denied operation into success.

The server SHALL retain credential id, every requested/ceiling selector pair,
intersected verbs, expiry, registry generation, and a per-connection
cancellation handle. It SHALL re-evaluate both selectors against the
authoritative topology on every dispatch, in the same critical section or
snapshot used by routing, and observe registry replacement and expiry while the
connection is active. Key removal, `revoked_at`, expiry, or a ceiling change
that no longer contains the minted conjunctive grant SHALL:

1. atomically block new dispatch for the connection;
2. release input leases, subscriptions, and pending authority owned by it;
3. best-effort flush one `PERMISSION_DENIED` and `DETACHED` with
   `AUTHORIZATION_REVOKED` or `AUTHORIZATION_EXPIRED`; and
4. close the transport without processing queued client frames.

A malformed reload applies the empty snapshot and therefore revokes every
workload session until a valid generation is loaded. It SHALL not preserve the
last known-good authority. Revocation does not kill the workload's Terminals or
processes unless a separately authorized operation requested that; it removes
this connection's authority to them.

The additional detach reasons are:

```text
AUTHENTICATION_FAILED  = 5
AUTHORIZATION_REVOKED  = 6
AUTHORIZATION_EXPIRED  = 7
```

`AUTHENTICATION_FAILED` covers post-HELLO authentication outcomes (a
pre-HELLO TLS refusal has no DETACHED to carry it). A client that does not
recognize a detach reason already treats it as unstated, so these values are
additive.

## 8. Policy modes and secret handling

Policy is closed:

| Mode | Admitted stateful transports | Workload authentication |
|---|---|---|
| `local` | owner-authenticated UDS only | not required; server mints all six verbs at Global for that connection |
| `paired` | owner UDS plus TLS transports (QUIC, WSS) | mTLS client certificate required on every TLS connection; owner UDS keeps kernel-uid authority |

These are the only modes. There is no bearer-only, SSH-auth-suffices, hybrid, or
unnamed compatibility state. The bearer token is an outer admission gate inside
`paired`, not a grant. SSH-stdio fits neither mode — it has no channel the
handshake can bind to — and is unavailable under `paired` until a later profile
defines a closed, independently verifiable binding for it.

With no explicit mode, a server may start only with its owner-only UDS and no
configured workload registry; that is `local`. A configured registry, CA path,
or non-UDS listener with mode unset is a startup error. `local` with a
non-UDS listener is a startup error. `paired` with missing, malformed, or unsafe
CA or registry material is a startup error. Runtime corruption after a
successful start applies the empty-snapshot revocation rule in §7.

TLS, certificate verification, an SSH login, a bearer token, or UDS
peer credentials remain necessary transport evidence where their transports
require them. None substitutes for the client certificate in `paired`.
Plaintext remote transport is forbidden in every mode.

`phux workload authority --init` is the only CLI path that creates a missing
CA and prints only its fingerprint. `phux workload add-key` accepts only a
client certificate or CSR from stdin or an explicitly opened file and writes
the registry. `list`, `revoke`, and `authority` display only credential ids,
public keys where requested, scope ceilings, expiries, revocation state, and
the CA fingerprint. Their diagnostics SHALL not contain secret material.

A client private key may come from an inherited descriptor, an owner-only file
opened by the client, or an OS credential store. An environment variable may
name a descriptor or non-secret credential handle, but SHALL not contain key
bytes. A command-line option may name a file or descriptor, but SHALL not carry
key bytes. Private client or CA bytes SHALL never enter argv,
environment values, the public registry, stdout, stderr, panic text, tracing,
metrics, or `Debug`; buffers are bounded, redacted, and zeroized when their
crypto API permits.

## 9. Conformance cases

A conforming implementation exercises at least these independent failures:
version mismatch before admission; no client certificate; unknown, expired,
or non-chaining certificate; downgrade (paired client against a listener
that does not request certificates); unknown credential id; empty or
over-ceiling grant; unknown verb/selector; duplicate, unsorted, truncated,
overlong, or trailing scope encoding; and revoked/expired authority at
admission and while live.

Authorization cases include every matrix row; BIND-only
`phux.session.create/v1`; mutation/subscription of server-owned metadata; a
Terminal moving out of and back into a ceiling Group; owner-addressed spawn
whose payload Group disagrees; every multi-target partial grant; satellite relay
bypass; SSH-stdio in both closed modes; remote `SIGNAL(Global)` attempting
SHUTDOWN; registry ceiling reduction; and absence of secret bytes from argv,
environment, stdout, stderr, diagnostics, and logs.
