---
audience: consumers, contributors, agents
stability: stable
last-reviewed: 2026-09-03
---

# phux-workload/v1 — workload authentication and scoped authority

**TL;DR.** The reusable workload-authentication profile for Phux endpoints.
Mutually signed, nonce-fresh, incarnation- and channel-bound evidence identifies
a persistent server authority and a registered workload key. A canonical closed
scope set is intersected with the live registry and enforced before dispatch;
expiry and revocation terminate active connections.

---

<!-- impl-status: spec-only; probe: WorkloadChallenge,WorkloadResponse,TerminalScopeSet,TerminalEffectiveScopeSet,WORKLOAD_AUTH -->
> **Status: spec-only.** The profile, terminal frame mapping, registry, strict
> codec, classifier, and live-revocation path described here are not implemented.

## 1. Profile boundary

`phux-workload/v1` is an authentication profile reusable by independently
versioned Phux services. It is not a terminal tier, coordinator tier, transport
preamble, or shared frame namespace. Each endpoint SHALL define:

- its own HELLO and HELLO_OK carriers;
- the profile's fixed `WORKLOAD_RESPONSE = 0x04` and
  `WORKLOAD_CHALLENGE = 0x84` discriminants in that endpoint's independent
  frame catalog;
- one ASCII `service` value; and
- a total endpoint-specific operation classification table.

The terminal protocol mapping is in §7. Its service is the exact 13 ASCII bytes
`phux-terminal`. A separate coordinator endpoint uses the same two profile
discriminants in its own stream, its own HELLO/version/catalog, and the exact
service bytes `phux-coordinator`; a coordinator frame is never sent to an L1
parser ([ADR-0092](../../ADR/0092-durable-work-coordinator-authority.md)).

All hashes below are SHA-256. Signatures are Ed25519 over the exact §4 bytes,
with raw 32-byte public keys and 64-byte signatures. Verification SHALL be
strict RFC 8032: reject non-canonical encodings of public point `A` or signature
point `R`, small-order `A` or `R`, and scalar `S >= L`. The reference API is
`ed25519_dalek::VerifyingKey::from_bytes`, an explicit `is_weak()` rejection,
then `verify_strict`; a different vetted library is conforming only if it
enforces the same acceptance set. Phux SHALL NOT implement the primitives.

## 2. Identities and persisted material

Three identities remain separate:

| Identity | Width | Lifetime and use |
|---|---:|---|
| Authority public-key fingerprint | 32 bytes | SHA-256 of the raw authority public key; stable across restart, endpoint, and incarnation; the durable value a client pins |
| Server incarnation | 16 bytes | The endpoint's current opaque `server_id`; changes whenever reconnect-safety state is lost; replay fence, never durable authority |
| Workload key id | 32 bytes | SHA-256 of the raw workload public key; registry lookup and live-revocation handle |

The canonical human/CLI form for either 32-byte digest is `sha256:` followed by
exactly 64 lowercase hexadecimal digits. Parsers SHALL reject uppercase,
padding, omitted leading zeroes, and alternate encodings; protocol fields carry
the raw 32 bytes. Comparisons are constant-time.

A client may persist a provider binding only after verifying the authority
proof. That binding keys on the authority fingerprint; socket path, URL,
certificate instance, and server incarnation are deliberately absent. A changed
endpoint or incarnation does not change authority, while a changed fingerprint
invalidates cached projections and all mutating authority. This is the contract
Cockpit's `ProviderTrustBinding` mirrors.

The authority's private-key file SHALL persist at
`<state-dir>/workload-authority.ed25519` as exactly the 32-byte Ed25519 secret
seed from which the public key is derived. The containing state directory SHALL
be owner-only. The key file SHALL be a regular, owner-owned, no-follow-opened
file with mode `0600`; creation and replacement SHALL use an owner-controlled
lock, same-directory temporary file, file sync, atomic rename, and directory
sync. A missing key may be created only by an explicit initialization path.
Replacement changes the fingerprint and is authority rotation, not server
restart. A client with a prior binding SHALL surface `authority_changed` and
refuse; it SHALL NOT silently trust the replacement.

Authority and workload secret seeds, and both handshake nonces, SHALL be
generated directly from the OS CSPRNG. The reference call is
`getrandom::getrandom`; failure is fatal and has no deterministic fallback.
Generated public keys are validated by the same strict rules before persistence.

`<state-dir>/workload-keys` is the registry of workload public keys. Each record
contains the raw public key, derived key id, canonical scope ceiling, absolute
expiry, and optional revocation time. `add-key` and every registry load SHALL
parse the public point canonically and reject weak/small-order keys before the
snapshot becomes usable. One invalid record makes the loaded snapshot malformed.
The file obeys the same ownership, mode, no-follow, stable-read, lock, sync, and
atomic-replacement rules. Public keys and fingerprints are not secrets. A
malformed, replaced, or unstable read is an empty authority snapshot, never
permission to use a cached generation.

Registry generations are live (§8). The registry is local Phux authority; no UI,
peer ledger, or coordinator is queried during admission.

## 3. Handshake state machine

An endpoint maps these profile records into its own frames:

```text
Client                                              Server
  | HELLO { version, WorkloadOffer }                   |
  |--------------------------------------------------->|
  |                         compare major.minor first  |
  | WORKLOAD_CHALLENGE                                 |
  |<---------------------------------------------------|
  | verify service/version/nonces/channel/incarnation |
  | verify authority proof and pinned fingerprint     |
  | WORKLOAD_RESPONSE                                  |
  |--------------------------------------------------->|
  |                  consume challenge; verify proof  |
  |                  intersect current registry grant |
  | HELLO_OK { WorkloadGrant }                         |
  |<---------------------------------------------------|
  | ACTIVE                                             |
```

The records are:

```text
WorkloadOffer {
    profile: "phux-workload/v1",
    client_nonce: bytes32,
}

WorkloadChallenge {
    profile: "phux-workload/v1",
    service: ascii,
    negotiated_major: u16,
    negotiated_minor: u16,
    client_nonce: bytes32,
    server_nonce: bytes32,
    server_incarnation: bytes16,
    channel_binding_kind: u8,
    channel_binding: bytes32,
    authority_public_key: bytes32,
    authority_proof: bytes64,
}

WorkloadResponse {
    key_id: bytes32,
    requested_scope_bytes: bytes,
    requested_expiry: u64,
    workload_proof: bytes64,
}

WorkloadGrant {
    key_id: bytes32,
    effective_scope_bytes: bytes,
    expires_at: u64,
    authority_fingerprint: bytes32,
}
```

The scope fields are signed opaque bytes at the shared-profile layer and SHALL
be `1..=32768` bytes; a receiver checks that bound before allocating or parsing
them. Each endpoint owns closed canonical requested/effective scope schemas and
rejects any byte image outside the applicable schema before proof verification.
The terminal schemas are `TerminalScopeSet` and `TerminalEffectiveScopeSet`
(§6); the coordinator defines its own. An endpoint SHALL NOT translate names or
bits from another endpoint.

`client_nonce` and `server_nonce` SHALL come from an OS CSPRNG, SHALL not be all
zero, and SHALL be used for one handshake only. The client SHALL reject a
challenge that does not echo its offer exactly. The server SHALL bind one pending
challenge to one accepted connection, consume it on the first response attempt,
and retain consumed nonces until that connection closes. There is no retry of a
challenge after a malformed or failed response.

The server SHALL compare the endpoint protocol `major.minor` before semantically
parsing `WorkloadOffer`, looking up a key, signing a challenge, or disclosing any
registry result. A bounded TLV scan may locate the base HELLO version fields, but
no auth field is interpreted before equality succeeds. A mismatch follows the
endpoint's `VERSION_INCOMPATIBLE` path.

After a valid offer the server SHALL send one challenge and wait at most five
seconds for exactly one response. No PING, lifecycle frame, command, or other
endpoint frame may interleave. Any other frame, duplicate response, timeout, or
second HELLO is fatal. HELLO_OK is sent only after both proofs and a nonempty,
unexpired grant succeed.

A client configured to require paired authority SHALL treat HELLO_OK before a
valid challenge, absence of the endpoint's workload-auth capability, a missing
WorkloadGrant, or a different profile as downgrade and close without issuing a
stateful operation. An endpoint in paired policy SHALL reject a HELLO without the
exact offer. There is no fallback from `phux-workload/v1` to bearer-only or
ambient identity.

## 4. Exact signed transcripts

The following helpers are part of the profile:

```text
U16(n)       = n encoded as two unsigned big-endian bytes
U32(n)       = n encoded as four unsigned big-endian bytes
U64(n)       = n encoded as eight unsigned big-endian bytes
V16(bytes)   = U16(len(bytes)) || bytes
```

`service` is 1..=64 bytes from ASCII `[a-z0-9-/.]`. Its case and bytes are
significant. No Unicode normalization or NUL is permitted.

The server signs exactly:

```text
SERVER_TRANSCRIPT =
    "phux-workload/v1\0server-proof\0" ||
    V16(service) ||
    U16(negotiated_major) || U16(negotiated_minor) ||
    client_nonce[32] || server_nonce[32] ||
    server_incarnation[16] ||
    channel_binding_kind[1] || channel_binding[32] ||
    authority_public_key[32]

authority_proof = Ed25519.Sign(authority_private_key, SERVER_TRANSCRIPT)
```

The workload signs exactly:

```text
scope_bytes = requested_scope_bytes  // already canonical for this endpoint

CLIENT_TRANSCRIPT =
    "phux-workload/v1\0client-proof\0" ||
    V16(service) ||
    U16(negotiated_major) || U16(negotiated_minor) ||
    client_nonce[32] || server_nonce[32] ||
    server_incarnation[16] ||
    channel_binding_kind[1] || channel_binding[32] ||
    authority_public_key[32] || authority_proof[64] ||
    key_id[32] || U64(requested_expiry) ||
    U32(len(scope_bytes)) || scope_bytes

workload_proof = Ed25519.Sign(workload_private_key, CLIENT_TRANSCRIPT)
```

The server SHALL reconstruct `CLIENT_TRANSCRIPT` from the challenge retained on
the current connection and the decoded response. It SHALL compare the retained
incarnation and channel binding before signature verification, validate the
registry public key by §1, derive its SHA-256 key id, compare that id in constant
time, then call the strict verifier. It SHALL NOT accept transcript bytes
supplied by the client.

An unknown key, revoked key, failed proof, empty intersection, or invalid expiry
returns the same generic `PERMISSION_DENIED` handshake failure. Diagnostics and
timing SHALL not reveal which test failed.

## 5. Channel binding

`channel_binding_kind` is closed:

```text
TLS_EXPORTER = 0x00
UDS_PEER     = 0x01
```

For TLS 1.3 (including QUIC and WSS), both peers derive:

```text
context = SHA-256(
    "phux-workload/v1\0tls-context\0" ||
    V16(service) || U16(negotiated_major) || U16(negotiated_minor)
)
channel_binding = TLS-Exporter(
    label = "phux-workload/v1",
    context = context,
    length = 32
)
```

The value comes from the exact TLS session carrying the endpoint frames. A
certificate fingerprint, address, bearer token, or exporter from a proxied leg
is not equivalent.

For UDS, the accepting kernel SHALL provide the peer's effective uid, gid, and
pid, and the client SHALL compare all three with its own values. Each is encoded
as unsigned `U64`. Both peers derive:

```text
channel_binding = SHA-256(
    "phux-workload/v1\0uds-peer\0" ||
    server_nonce[32] || U64(uid) || U64(gid) || U64(pid)
)
```

A server SHALL reject paired UDS when the platform cannot authenticate all three
values. A client SHALL reject a challenge whose derivation does not match its
own credentials. Empty binding, address text, uid alone, and a server nonce
without peer credentials are forbidden.

An endpoint over a transport without either binding is unavailable in paired
mode unless a later profile version defines a cryptographic binding for it.
SSH authentication does not manufacture an exporter for a stdio stream.

## 6. TerminalScopeSet canonical encoding and intersection

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
new profile version; it is not ignored.

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
federation host key carried by `TerminalId::Satellite`, not an address resolved
from DNS. Unknown tags, subtype tags, zero-length hosts, and trailing selector
bytes are malformed.

`TerminalScopeSet` canonical bytes are:

```text
U16(grant_count) || repeated {
    U16(len(selector_bytes)) || selector_bytes || verbs[1]
}
```

The set has at most 64 grants. Grants are strictly increasing by unsigned
lexicographic order of `selector_bytes`; every selector occurs once; `verbs` is
nonzero. Encoders merge equal selectors by OR-ing their verbs, remove zero
entries, sort, and emit the shortest form. Decoders SHALL reject an unsorted or
duplicate selector, unknown bit, non-minimal length, count/length mismatch,
truncation, or trailing byte. They SHALL not normalize an invalid image and then
verify it.

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

`requested_expiry` and every registry expiry are unsigned Unix seconds. They are
policy times, not freshness evidence. Both SHALL be greater than the server's
current time. `WorkloadGrant.expires_at` is their minimum. The grant is bound to
the authenticated connection, server incarnation, key id, registry generation,
and channel binding; it SHALL not be serialized as a reusable bearer credential.

## 7. Terminal endpoint mapping and total classification

The terminal protocol adds:

```text
HELLO field 6: workload_profile optional<str>
HELLO field 7: workload_client_nonce optional<bytes32>

WORKLOAD_RESPONSE  C -> S  type 0x04
WORKLOAD_CHALLENGE S -> C  type 0x84

HELLO_OK field 9: workload_grant optional<WorkloadGrant>
ServerFeature::WORKLOAD_AUTH = 0x00001000
```

The two HELLO fields form `WorkloadOffer` and SHALL be both absent or both
present. When present, field 6 contains the ordinary leaf-string image
`U32(16) || "phux-workload/v1"`, and field 7 contains exactly 32 nonce bytes.
The strict challenge fields are:

| Field id | Field value inside the TLV `BYTES` envelope |
|---:|---|
| 1 | `U32(16) || "phux-workload/v1"` |
| 2 | `U32(len(service)) || service` |
| 3 | `U16(negotiated_major)` |
| 4 | `U16(negotiated_minor)` |
| 5 | `client_nonce[32]` |
| 6 | `server_nonce[32]` |
| 7 | `server_incarnation[16]` |
| 8 | `channel_binding_kind[1]` |
| 9 | `channel_binding[32]` |
| 10 | `authority_public_key[32]` |
| 11 | `authority_proof[64]` |

The strict response fields are:

| Field id | Field value inside the TLV `BYTES` envelope |
|---:|---|
| 1 | `key_id[32]` |
| 2 | `U32(len(scope_bytes)) || scope_bytes` |
| 3 | `U64(requested_expiry)` |
| 4 | `workload_proof[64]` |

HELLO_OK `workload_grant` is the strict positional image:

```text
key_id[32] || authority_fingerprint[32] || U64(expires_at) ||
U32(len(effective_scope_bytes)) || effective_scope_bytes
```

For the terminal endpoint, `effective_scope_bytes` is exactly the
`TerminalEffectiveScopeSet` image from §6, retaining both selector provenances.

Authentication frames use the normal field-tagged TLV envelope, but every field
is required, appears once in ascending field-id order, uses top-level wire type
`BYTES`, and contains exactly its stated value. Unknown or duplicate fields,
missing fields, non-minimal varints, an incorrect fixed width, data after a
nested value, and body trailing bytes are `MALFORMED_MESSAGE` and fatal. This
strict rule intentionally overrides ordinary auth-unaware TLV skipping; profile
evolution uses a new profile string.

Before any routing, lookup that could disclose existence, queue insertion,
mutation, handler call, or satellite forwarding, the server maps each decoded
client frame to the following requirement. Every listed subject must match an
effective grant carrying the listed verb. A `Terminal` may match its exact
Terminal, current Group, owning Host, or Global selector according to §6.

| Client-originated frame | Required verb | Subject selector |
|---|---|---|
| `HELLO` | handshake-exempt | valid only in `PRE_HELLO` |
| `WORKLOAD_RESPONSE` | handshake-exempt | valid only for this connection's `CHALLENGE_SENT` |
| `PING` | liveness-exempt | no state access; allowed before HELLO, forbidden during challenge |
| `DETACH` | cleanup-exempt | calling connection only |
| `ATTACH` existing/last target | `BIND` and `OBSERVE` | resolved Group; all returned Terminals are filtered by `OBSERVE` |
| `ATTACH` create-if-missing | `CREATE`, then `BIND` and `OBSERVE` | selected local Group; no creation occurs unless all requirements pass |
| `HISTORY_REQUEST` | `OBSERVE` | named Terminal |
| `FRAME_ACK` | `OBSERVE` | named Terminal and current stream/bootstrap generation |
| `COMMAND` | classified by nested command tag below | nested subjects; envelope alone grants nothing |
| `SUBSCRIBE` (unallocated) | default-deny | no selector contract exists |
| `INPUT_KEY`, `INPUT_PASTE`, `INPUT_MOUSE`, `INPUT_RAW`, `INPUT_FOCUS`, `INPUT_TERMINAL_REPLY` | `INPUT` | named Terminal |
| `VIEWPORT_RESIZE` | `BIND` | every currently attached Terminal; zero targets is a no-op |
| `SPAWN_TERMINAL { satellite: Some, owner_terminal: None }` | `CREATE` | named satellite Host |
| `SPAWN_TERMINAL { satellite: None, owner_terminal: None }` | `CREATE` | payload Group |
| `SPAWN_TERMINAL { satellite: None, owner_terminal: Some }` | `CREATE` and `BIND` | CREATE on the owner Terminal's side-effect-free resolved Group and BIND on that Terminal; payload Group MUST equal the resolved Group |
| `SPAWN_TERMINAL { satellite: Some, owner_terminal: Some }` | default-deny | invalid local/remote ownership combination |
| `TERMINAL_RESIZE` | `BIND` | named Terminal |
| `MOVE_TERMINAL` | `BIND` | both moved and destination-owner Terminals |
| `SUBSCRIBE_EVENTS { terminal: Some }` | `OBSERVE` | named Terminal |
| `SUBSCRIBE_EVENTS { terminal: None }` | `OBSERVE` | installs a filtered subscription over all observable Terminals; server-global events require Global |
| `GET_METADATA` | `OBSERVE` | encoded metadata Scope |
| `SET_METADATA { Global, "phux.session.create/v1" }` | `CREATE` and `BIND` | Global; BIND alone MUST NOT create a process |
| `SET_METADATA { Global, "phux.config.reload/v1" }` | `SIGNAL` | Global |
| `SET_METADATA` or `DELETE_METADATA` targeting `phux.session.created/v1` or its slash-prefixed results | default-deny | server-owned result namespace is non-writable |
| `SUBSCRIBE_METADATA` targeting that result namespace | default-deny | server-owned connection-private results are non-subscribable |
| `SET_METADATA` or `DELETE_METADATA` targeting `phux.pane-occupant/v1`, or `DELETE_METADATA` targeting `phux.config.reload/v1` | default-deny | server-owned keys are non-writable |
| Other `SET_METADATA`, `DELETE_METADATA` | `BIND` | encoded metadata Scope |
| `LIST_METADATA` | `INVENTORY` | encoded metadata Scope; server-owned result keys remain excluded |
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
| `SPAWN` (unallocated) | default-deny | dedicated `SPAWN_TERMINAL` owns create |
| `ATTACH_TERMINAL` | `BIND` and `OBSERVE` | named Terminal |
| `DETACH_TERMINAL` | cleanup-exempt | calling connection's binding only |
| `KILL_TERMINAL` | `SIGNAL` | named Terminal |
| `GET_SCREEN` | `OBSERVE` | named Terminal |
| `ROUTE_INPUT`, `APPLY_INPUT` | `INPUT` | named Terminal |
| `KILL_TERMINALS` | `SIGNAL` | every named Terminal; all-or-nothing |
| `RESIZE_TERMINAL` (unallocated) | default-deny | dedicated `TERMINAL_RESIZE` owns resize |
| `GET_STATE { SERVER }` | `INVENTORY` | requires at least one Inventory grant; returns only resources matched by those selectors, and server-global data only with Global |
| `RUN_HOOK` (unallocated) | default-deny | no wire contract exists |
| `GET_TERMINAL_STATE` | `INVENTORY` | named Terminal |
| `SUBSCRIBE_TERMINAL_EVENTS` | `OBSERVE` | named Terminal |
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
| Unknown, retired, or otherwise unclassified command tag | default-deny | none |

For a metadata `Scope::Terminal`, `Scope::Group`, or `Scope::Global`, the subject
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

## 8. Denial, expiry, and live revocation

A structurally malformed auth frame receives fatal `MALFORMED_MESSAGE` followed
by `DETACHED { reason: PROTOCOL_ERROR }` and close. A well-formed but failed
authentication receives generic `PERMISSION_DENIED`,
`DETACHED { reason: AUTHENTICATION_FAILED }`, and close.

After authentication, an out-of-scope correlated frame or command receives its
ordinary correlated error carrying `PERMISSION_DENIED`; no effect occurs and the
connection stays active. An out-of-scope frame without a response channel is
dropped before effect and MAY receive a rate-limited uncorrelated
`PERMISSION_DENIED`; it does not close the connection. Repeated denials MAY be
rate-limited or cause an explicit policy close, but silence SHALL never turn the
denied operation into success.

The server SHALL retain key id, every requested/ceiling selector pair,
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

A client that does not recognize a detach reason already treats it as unstated,
so these values are additive.

## 9. Policy modes and secret handling

Policy is closed:

| Mode | Admitted stateful transports | Workload proof |
|---|---|---|
| `local` | owner-authenticated UDS only | not required; server mints all six verbs at Global for that connection |
| `paired` | any endpoint transport that also provides required confidentiality, server identity, and a §5 binding | required for every connection, including owner UDS |

These are the only modes. There is no bearer-only, SSH-auth-suffices, hybrid, or
unnamed compatibility state. WSS bearer authentication is an outer gate inside
`paired`, not a grant. SSH-stdio fits neither mode because it has no §5 binding
and is unavailable until a later profile defines a closed, independently
verifiable `SSH_SESSION` binding.

With no explicit mode, a server may start only with its owner-only UDS and no
configured workload registry; that is `local`. A configured registry, authority
path, or non-UDS listener with mode unset is a startup error. `local` with a
non-UDS listener is a startup error. `paired` with missing, malformed, or unsafe
authority or registry material is a startup error. Runtime corruption after a
successful start applies the empty-snapshot revocation rule in §8.

TLS, certificate verification, an SSH login, a WebSocket bearer token, or UDS
peer credentials remain necessary transport evidence where their transports
require them. None substitutes for workload proof in `paired`. Plaintext remote
transport is forbidden in every mode.

`phux workload authority --init` is the only CLI path that creates a missing
authority key and prints only its fingerprint. `phux workload add-key` accepts
only public keys from stdin or an explicitly opened file and writes the
registry. `list`, `revoke`, and `authority` display only key ids, public keys
where requested, scope ceilings, expiries, revocation state, and the authority
fingerprint. Their diagnostics SHALL not contain secret material.

A client private key may come from an inherited descriptor, an owner-only file
opened by the client, or an OS credential store. An environment variable may
name a descriptor or non-secret credential handle, but SHALL not contain key
bytes. A command-line option may name a file or descriptor, but SHALL not carry
key bytes. Private workload or authority bytes SHALL never enter argv,
environment values, the public registry, stdout, stderr, panic text, tracing,
metrics, or `Debug`; buffers are bounded, redacted, and zeroized when their
crypto API permits.

## 10. Conformance cases

A conforming implementation exercises at least these independent failures:
version mismatch before auth parsing; absent/downgraded profile; repeated nonce;
wrong service, authority, incarnation, connection, TLS exporter, UDS peer pid,
key id, expiry, or scope; non-canonical/small-order Ed25519 `A` or `R`; `S >= L`;
an invalid registry key; unknown verb/selector; duplicate, unsorted, truncated,
overlong, or trailing encoding; empty/over-ceiling grant; and revoked/expired
authority at admission and while live.

Authorization cases include every matrix row; BIND-only
`phux.session.create/v1`; mutation/subscription of server-owned metadata; a
Terminal moving out of and back into a ceiling Group; owner-addressed spawn
whose payload Group disagrees; every multi-target partial grant; satellite relay
bypass; SSH-stdio in both closed modes; remote `SIGNAL(Global)` attempting
SHUTDOWN; registry ceiling reduction; and absence of secret bytes from argv,
environment, stdout, stderr, diagnostics, and logs.