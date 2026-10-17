---
audience: consumers, contributors, agents
stability: stable
last-reviewed: 2026-10-17
---

# Workload authority over mTLS — authentication and scoped authority

**TL;DR.** Phux endpoints authenticate workloads with mutual TLS, not a
bespoke handshake. The server mints a CA on first routable listen;
`phux host add` enrolls a client certificate over ssh (§8.1); the TLS
handshake proves the
peer holds the private key on that channel, and the registry maps the
client identity to a closed scope ceiling enforced before dispatch.
Expiry and revocation terminate live connections. Owner UDS keeps
kernel-uid authority; the bearer token stays outer admission only.
(ADR-0116 retires the unshipped `phux-workload/v1` proof profile this
document previously specified; the authorization half stands.)

---

<!-- impl-status: shipped; probe: WorkloadRegistry,credential_id,ReloadingWorkloadRegistry,prepare_enrollment,WorkloadAction,ScopedPolicy,enforce,revoke_connection -->
> **Status: shipped.** The persisted workload CA, client enrollment,
> credential-id derivation, and the scope registry are implemented in
> `phux_server::workload`; the §5 scope types and registry grammar in
> `phux_protocol::scope`; the `paired` grant and the dispatch guard in
> `phux_server::policy` (`ScopedPolicy`, `enforce`); and live revocation
> (§7) in the server runtime (`revoke_connection`). The owner's socket is a
> Unix-socket peer whose kernel uid is the serving uid. §6's kind-bearing
> rows carry their own markers.

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
([ADR-0092](../adr/0092-durable-work-coordinator-authority.md)).
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

A client certificate the authority issues for a CSR names its credential
id twice: as the subject common name and as a URI subject alternative
name (the canonical `sha256:` spelling below is a valid URI). The name is
informative. Admission SHALL derive the credential id from the verified
certificate's public key and SHALL NOT read the subject or its
alternative names, so a certificate issued before issuance named the id
(the reference authority once wrote the constant `phux-workload-client`)
admits exactly as a new one does.

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

<!-- impl-status: shipped; probe: chain_authority,AuthorityChange,issue_server_identity,rotate_authority,known_authorities -->
> **Status: shipped (ADR-0153).** The reference server provisions its
> certificate from the CA and presents the chain, leaf then CA; a chain
> names its CA when the leaf verifies under the chain's last certificate.
> Clients pin the CA at pairing (`phux pair --json` `ca_fingerprint`, the
> connect link's `ca`) in `known-authorities` beside `config.toml`, keyed
> by the leaf pin. A changed CA is refused as `authority_changed`, naming
> both fingerprints, and never retried. A pin is made at pairing and never
> learned from a connection, and a CA pin accepts only leaves that CA
> issued (ADR-0179). A server whose certificate predates its CA keeps its
> self-signed leaf (it is never re-issued) and presents no CA, so clients
> reach it by its leaf pin alone and cannot enroll by ticket (§8.2) until
> its operator rotates. `phux workload authority --rotate` is the rotation:
> a new CA and a server certificate under it, with no cross-signing, so
> every client re-pairs.

The CA's key file SHALL persist at `<state-dir>/workload-ca.key` with
the CA certificate at `<state-dir>/workload-ca.pem`. The containing
state directory SHALL be owner-only. Key and registry files SHALL be
regular, owner-owned, no-follow-opened files with mode `0600`; creation
and replacement SHALL use an owner-controlled lock, same-directory
temporary file, file sync, atomic rename, and directory sync. A missing
CA may be created only by an explicit initialization path (first
routable listen, the `phux pair` that provisions the server certificate
before it, or `phux workload authority --init`).

Every directory above the state directory SHALL also be controlled by its
owner or root. The reference implementation checks only the immediate
parent and opens files by path, so a directory above it that another user
can write could substitute the whole state directory; operators who move
the material with `PHUX_WORKLOAD_CA`, `PHUX_WORKLOAD_CA_KEY`, or
`PHUX_WORKLOAD_KEYS` keep the same property along that path.

`<state-dir>/workload-keys` is the registry of workload credentials.
Each record contains the client certificate (or raw public key),
derived credential id, canonical scope ceiling, absolute expiry, and
optional revocation time, and optionally who the credential was
enrolled for: `principal`, `"human"` (a device a person paired) or
`"automation"`. The field decides input-lease precedence (ADR-0163 §6),
never scope. A record without it is `human` when its ceiling is exactly
`inventory,observe,create,bind,input,signal@global`, the ceiling pairing
grants (§8.1, §8.2), and `automation` otherwise. A ticket enrollment
(§8.2) SHALL record `human`. A writer SHALL omit the field when no one
named it, so an older reader keeps loading the file. A satellite
credential (§8.3) carries `satellite`, the name it enrolled as, and an
empty scope list in place of a ceiling; every other record omits
`satellite` and names at least one scope. A terminal endpoint SHALL treat a
satellite credential as unknown. One invalid record
makes the loaded snapshot malformed. The file obeys the same ownership, mode, no-follow,
stable-read, lock, sync, and atomic-replacement rules. Public keys and
fingerprints are not secrets. A malformed, replaced, or unstable read
is an empty authority snapshot, never permission to use a cached
generation.

Registry generations are live (§7). The registry is local Phux
authority; no UI, peer ledger, or coordinator is queried during
admission.

Each registry file carries a random instance id, minted by its first
write and kept by every later one; the generation counts writes within
that instance, and an admitted credential is stamped with both.
Consumers SHALL key anything derived from a snapshot on the pair
`(instance, generation)`, never on the generation alone. A credential
whose registry carries no instance id (a file not yet rewritten) matches
no such key. Restoring a registry backup can move the generation
backwards within one instance; a consumer SHALL treat any decrease, like
any change of instance, as a new epoch rather than as stale data.

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
  paired listener refuses the connection before any phux frame is read:
  a WSS listener rejects the handshake; a QUIC listener, which also
  serves the enrollment ALPN (§8.2) and so asks for a certificate without
  requiring one in the handshake, closes a terminal-ALPN connection that
  presented none, or one not active in the registry, with an application
  close before reading any stream (so no bearer preamble either). A
  certificate that is presented is still verified inside the handshake.
  There is no phux-shaped error pre-HELLO because no phux frame has been
  exchanged.
- **A client configured to require paired authority** SHALL close
  without issuing a stateful operation when the server does not
  request a client certificate, or when the server certificate does
  not chain to the pinned CA. Receiving `HELLO_OK` over an
  unauthenticated channel is downgrade, not permission to continue.
  (Informative: the reference dialer turns this on with
  `PHUX_WORKLOAD_REQUIRE_PAIRED`, off by default, and fails the TLS
  handshake itself when no `CertificateRequest` arrived, with session
  resumption disabled so every handshake can show one. It pins the CA when
  one is pinned and the leaf otherwise; see §2.)
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
- **Through a relay the certificate travels end to end.** A relay
  terminates the outer TLS on both legs for SNI routing, so a consumer runs
  a second TLS 1.3 session inside the relayed stream, terminated by the
  server: the server presents its own certificate (verified against the
  consumer's CA pin, §2) and asks for the client certificate exactly as a
  paired QUIC listener does; the bearer preamble then rides inside. The
  server tells the session from a plain bearer preamble by its first byte
  (`0x16` opens a TLS record; a preamble's big-endian length starts
  `0x00`). In every posture a bridged consumer is admitted only this way: a
  server MUST refuse a relayed stream whose first byte is not `0x16` before
  reading further, and a client MUST NOT dial a relay route without the
  server's CA pinned for it, nor write anything before the inner handshake
  verifies the server. The relay forwards ciphertext and holds no session
  key ([ADR-0154](../adr/0154-devices-enroll-with-a-ticket-over-their-own-alpn.md)
  item 5, [ADR-0180](../adr/0180-the-relay-is-blind.md)). The same session,
  with the same first-byte rule, rides a
  WebTransport stream (item 6); a WebTransport `CONNECT` still carries the
  bearer, so nothing follows the handshake there.

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
ORGANIZE  = 0x40   // rename, move membership, edit arrangements, resolve intake, undo (schema v2)
TERMINATE = 0x80   // terminate, remove, or delete an organization record (schema v2)
```

The scope schema is versioned. Schema v1 has the first six verbs; bits `0xC0`
are unknown in v1 and SHALL cause rejection of a v1 grant. Schema v2, the
current version, adds `ORGANIZE` and `TERMINATE` and knows all eight bits. A
later verb requires a new scope-schema version; it is not ignored. The
canonical image below carries no version: the store that persists an image or
its registry strings records the schema it was written under, and a reader
that does not know that schema refuses the record rather than guessing.

A reader of a schema-v1 grant (canonical bytes or registry strings) SHALL read
it as v2 with two implications: `SIGNAL` implies `TERMINATE`, and `CREATE`
implies `ORGANIZE`. A held `?signal` (§6.1) implies nothing, so a hold never
becomes an un-held terminating authority. The new verbs gate only the
organization commands ([L1.md](./L1.md) §5.7); every other row of §6 keeps
exactly the verbs it required under v1, so the implication reaches only the
organization equivalents of what a v1 grant could already do, and no stored
grant loses an operation.

Selectors and their canonical bytes are:

```text
GLOBAL                         = 0x00
HOST_LOCAL                     = 0x01 || 0x00
HOST_SATELLITE(host)           = 0x01 || 0x01 || V16(host)
GROUP(group_id)                = 0x02 || U32(group_id)
TERMINAL_LOCAL(id)             = 0x03 || 0x00 || U32(id)
TERMINAL_SATELLITE(host, id)   = 0x03 || 0x01 || V16(host) || U32(id)
SESSION(key)                   = 0x04 || key[16]
RESOURCE(key)                  = 0x05 || key[16]
```

`host` is 1..=255 UTF-8 bytes, contains no NUL or Unicode control scalar, and is
compared byte-for-byte without case folding or normalization. It is the exact
federation host key carried by `ResourceId::Satellite`, not an address resolved
from DNS. `key` is a local organization record's persistent `SessionKey` or
`ResourceKey` ([L1.md](./L1.md) §5.7.1): 16 bytes, never all zero, random at
create and never reused. Unknown tags, subtype tags, zero-length hosts, an
all-zero key, and trailing selector bytes are malformed.

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
- a Group contains that group and its current Terminal members;
- a Session contains the local session whose record carries that key, and
  its current Terminal members: the Group of that session, named by key;
- a Terminal contains only that exact Terminal; and
- a Resource contains only the local resource whose record carries that key,
  under whichever activation is current.

A key that no live record carries contains nothing. A key and a wire id may
name the same session or Terminal, so for overlap and §7 containment a
keyed selector is treated like the id selector of its kind, never as equal
to it.

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

### 5.1 Registry string grammar (informative)

The registry file and `phux workload add-key --scope` spell one grant as
`<verb>[,<verb>...]@<selector>`. A verb is `inventory`, `observe`,
`create`, `bind`, `input`, `signal`, `organize`, or `terminate`, each at most
once, or the single wildcard `*` (every verb of the schema, never combined
with a name). A schema-v1 record may name only the first six, its `*` is
those six, and it is read with the §5 implications. `signal` may instead be
spelled `?signal`: the grant carries `SIGNAL`, but it is *held*, and every
`SIGNAL` command it admits waits for a decision (§6.1, ADR-0128). `?` on any
other verb is refused, and `signal,?signal` repeats a verb. A selector is `global`,
`host` (the serving host), `host:<name>`, `group:<id>`, `terminal:<id>`, or
`terminal:<host>/<id>`, `session:<key>`, or `resource:<key>`. An `<id>` is a
canonical unsigned 32-bit decimal (no sign, no leading zero), a `<host>` obeys
the rules above, and the last `/` splits a satellite Terminal's host from its
id. A `<key>` is the key's full text form ([L1.md](./L1.md) §5.7.1), written
lowercase: 26 base32 digits, never a shortened prefix and never uppercase, so
each key has one spelling.

In the reference server a Group is a session and `group:<id>` names its wire
session id, the one `ATTACHED` reports. `GroupId`, the `SPAWN_RESOURCE`
payload group and the `Scope::Group` metadata key, is an opaque key only the
local host as a whole contains.

One record's strings form one `TerminalScopeSet`: equal selectors merge, and
one string that does not parse makes the record, and so the registry
snapshot, malformed. A verb one merged string holds and another grants
un-held is un-held: a merge is a union of authority, and a hold only ever
restricts. A hold is server-local policy bound to the grant; the canonical
bytes above carry the verbs only, so a decoded image holds nothing, and the
effective grant is never serialized. No partial grant is minted. The grammar is a spelling
of the canonical bytes above, not a second authority.

A persisted registry record SHALL NOT name a `group:` or `terminal:`
selector. The reference server's session and Terminal ids restart at 1
with every server process, so an id written into the registry would come
to name a different session or Terminal after a restart. `phux workload
add-key` refuses them, and a registry file holding one is malformed (the
empty snapshot of §7). The selectors stay valid in the canonical encoding
above, for a requested attenuation that lives only as long as its
connection. A persisted ceiling narrower than a host names the record
instead: `session:<key>` or `resource:<key>`. A record key survives a
restart, an upgrade, and a relaunch, and is never reused, so the grant
reaches the same session or resource for as long as the record exists and
nothing once it is deleted.

HELLO carries no scope request, so the requested set is the ceiling. With
no requested attenuation the server mints one clause `(g, g, verbs)` per
ceiling grant. That grants exactly what the full self-intersection grants:
an off-diagonal clause covers only subjects both its selectors contain,
which the diagonal clause for either selector already covers with at least
the same verbs.

## 6. Terminal endpoint mapping and total classification

<!-- impl-status: shipped; probe: classify_command,ScopedPolicy,enforce,filter_snapshot,observes_event -->
> **Status: shipped.** Both tables below are mirrored row for row by
> `phux_protocol::kinds` (ADR-0125), the table `classify_frame` and
> `classify_command` read; a test pins it to these rows in both directions,
> and a table-driven test checks every reachable row's verbs on its own
> subject selector against the near misses. The reference server enforces
> it for `paired` workload grants at three points that share one `enforce`:
> the client frame loop after decode, the top of command dispatch (above
> the input lane and every satellite relay), and QUIC `STREAM_BIND`
> (`OBSERVE` on the bound Terminal). The rows that allow a filtered result
> filter at the source (`phux_server::policy::filter`):
> `SUBSCRIBE_EVENTS { terminal: None }` is admitted with `OBSERVE` on any
> selector and delivers, and replays, only events on Terminals the grant
> observes (a child through its parent; an event naming no Terminal only
> with Global); `GET_STATE { SERVER }` is admitted with `INVENTORY` on any
> selector and returns only the sessions, windows, Terminals, and satellite
> rows it inventories, with the listener report only at Global; `ATTACH`'s
> `ATTACHED` snapshot keeps only what the grant observes. An event is
> placed in its Group when it is journaled. An unowned local
> `SPAWN_RESOURCE` needs `CREATE` on the local host, because its payload
> `GroupId` is the opaque key only the local host contains (§5.1). The
> owner's grant (the owner socket, in either mode) admits every frame and
> filters nothing, leaving the handlers' own checks as its only limit.

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
| `GET_METADATA { Global, "phux.whoami/v1" }` | self-exempt | calling connection only: the record describes the asking connection's own identity and grant, never another's |
| Other `GET_METADATA` | `OBSERVE` | encoded metadata Scope |
| `SET_METADATA { Global, "phux.session.create/v1" }` | `CREATE` and `BIND` | Global; BIND alone MUST NOT create a process |
| `SET_METADATA { Global, "phux.session.keep_empty/v1" }` with value `name\0true` | `CREATE` and `BIND` | Global; the mark keeps a session, and so the server, alive with zero processes (ADR-0105) |
| `SET_METADATA { Global, "phux.session.keep_empty/v1" }` with value `name\0false` | `SIGNAL` | named session, resolved side-effect-free; an absent name resolves to no Group a narrower grant holds. Clearing the mark of a windowless session removes it, so this is `SIGNAL` on that session, like `DETACH_CLIENTS { session: Some }` |
| `SET_METADATA { Global, "phux.session.keep_empty/v1" }` with any other value | default-deny | malformed; the value is classified before the handler parses it |
| `SET_METADATA { Global, "phux.config.reload/v1" }` | `SIGNAL` | Global |
| `SET_METADATA { Global, "phux.approval.decide/v1/<id>" }` with value `approve` or `deny` | `SIGNAL` | the held action's subject, resolved side-effect-free from the pending approval `<id>` names; an absent or already-decided id resolves to no subject, and only an un-held `SIGNAL` grant satisfies it (§6.1) |
| `SET_METADATA { Global, "phux.approval.decide/v1/<id>" }` with a malformed id or any other value | default-deny | malformed; the key and value are classified before the handler parses them |
| `SET_METADATA` or `DELETE_METADATA` targeting `phux.session.created/v1` or its slash-prefixed results | default-deny | server-owned result namespace is non-writable |
| `SUBSCRIBE_METADATA` targeting that result namespace | default-deny | server-owned connection-private results are non-subscribable |
| `SET_METADATA` or `DELETE_METADATA` targeting `phux.pane-occupant/v1`, `phux.whoami/v1`, or a `phux.approval/v1/<id>` record, or `DELETE_METADATA` targeting `phux.config.reload/v1`, `phux.session.keep_empty/v1`, or a `phux.approval.decide/v1/<id>` key | default-deny | server-owned keys are non-writable |
| Other `SET_METADATA`, `DELETE_METADATA` | `BIND` | encoded metadata Scope |
| `LIST_METADATA` | `INVENTORY` | encoded metadata Scope; server-owned result keys remain excluded |
| `LIST_DIRECTORY` | `INVENTORY` | Global; the serving host's filesystem is server-global data, so no Terminal, Group, or Host grant reaches it |
| `PATH_QUERY` | `INVENTORY` | Global; the selected host's filesystem is server-global data, matching `LIST_DIRECTORY` |
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
| `CLOSE_TAB_RESOURCES` | `SIGNAL` | every named Terminal; all-or-nothing |
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
| `TRANSCRIBE` | `INPUT` | named Terminal (`terminal_id`) |
| `DETACH_CLIENTS { session: Some }` | `SIGNAL` | resolved Group |
| `DETACH_CLIENTS { session: None }` | `SIGNAL` | Global |
| `SHUTDOWN` | `SIGNAL` plus transport predicate | Global, and the authenticated transport MUST be the owner UDS; remote paired grants cannot stop the server |
| `OPEN_LISTENER` | `SIGNAL` plus transport predicate | Global, and the authenticated transport MUST be the owner UDS; remote paired grants cannot open a listener |
| `GET_PERF { reset: false }` | `OBSERVE` | Global |
| `GET_PERF { reset: true }` | `OBSERVE` and `BIND` | Global |
| `APPEND_RESOURCE_OUTPUT` | `BIND` and `INPUT` | the named resource's parent Terminal; a grant naming only the child does not suffice, and a Terminal-kind target is refused after admission with `WRONG_RESOURCE_KIND` |
| `NATIVE_AGENT { type: spawn }` | `CREATE` and `BIND` | the session the command's key names, resolved from `spec.session` (the exact organization session name) to its Group |
| `NATIVE_AGENT { type: command }` | `INPUT` | named Terminal subject: the exact AgentSession resource ID, never an inferred latest child; prompt/interrupt also obey the shared parent input lease, and every answer intent additionally requires human authority |
| `NATIVE_AGENT { type: register/receive/complete/publish/close }` | `BIND` | the named resource's parent Terminal; additionally requires the reserved exclusive controller binding proof, activation, authenticated host identity, and current connection generation |
| `GET_ORGANIZATION` | `INVENTORY` | filtered to the organization records the grant inventories: a session through its Group, a resource through its session; authority totals and the Intake designation (raw `inbox` field) only with Global |
| `SESSION_CREATE` | `CREATE` | Global; a launch in the same command needs nothing more |
| `SESSION_RENAME`, `SESSION_SET_LIFETIME`, `INBOX_RESOLVE` | `ORGANIZE` | the session the command's key names, resolved side-effect-free to its Group; an absent key resolves to no subject |
| `SESSION_DELETE`, `SESSION_KILL` | `TERMINATE` | the session the command's key names, resolved side-effect-free to its Group |
| `RESOURCE_CREATE` | `CREATE` | the session the command's key names, resolved side-effect-free to its Group |
| `RESOURCE_MOVE`, `RESOURCE_MOVE_MANY` | `ORGANIZE` | every session a moved resource leaves and the destination session; all-or-nothing |
| `RESOURCE_RENAME` | `ORGANIZE` | the resource the command's key names, through its current session's Group |
| `RESOURCE_TERMINATE` | `TERMINATE` | named Terminal: the live target's `resource` |
| `RESOURCE_REMOVE` | `TERMINATE` | the resource the command's key names, through its current session's Group |
| `RESOURCE_RELAUNCH` | `CREATE` | the resource the command's key names, through its current session's Group |
| `ORGANIZATION_UNDO` | `ORGANIZE` | every session and resource the undone operation's recorded inverse touches, under current membership |
| `ARRANGEMENT_GET` | `INVENTORY` | the session that owns the arrangement |
| `ARRANGEMENT_REPLACE` | `ORGANIZE` | the session that owns the arrangement |
| `DELEGATE_AGENT`, `REVOKE_DELEGATION` | `TERMINATE` plus transport predicate | Global, and the authenticated transport MUST be the owner UDS; only the owner mints or withdraws a delegated agent credential |
| Unknown, retired, or otherwise unclassified command tag | default-deny | none |

<!-- impl-status: shipped; probe: ResourceKind,COMMAND_TAG_APPEND_RESOURCE_OUTPUT -->
> **Status: shipped.** The `AGENT_SESSION` kind is served
> ([L1.md §1.1](./L1.md)), and the kind-bearing spawn rows and the
> `APPEND_RESOURCE_OUTPUT` row are enforced as written: a local agent
> session needs `CREATE` on its parent's Group and `BIND` on the parent; a
> remote one `CREATE` on its host and `BIND` on the same-host parent, and a
> different-host parent is refused; a kind that contradicts its binding is
> refused whatever is granted; and output is appended only through the
> parent, never through a grant naming the child alone.

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
to enumerate a container does not disclose members outside the effective set. A
filtered result is the grant's view, not a record of what exists: a Terminal
it omits may be alive, and a consumer reads its absence as
[L1.md](./L1.md) §7.3 says.

When deriving a selector requires server state (for example a named ATTACH or
forced-detach Group), the derivation is a side-effect-free part of the guard.
It returns the same denial for absent and unauthorized targets and SHALL not
leak existence, membership, or routing state before authorization succeeds.

The reference enforcement points are the terminal client frame loop immediately
after decode/state validation, and `handle_command` immediately after nested-tag
decode and before its satellite-relay branch. Per-handler checks may enforce
additional domain invariants but SHALL not replace either common check.

### 6.1 Held actions: the Hold outcome

<!-- impl-status: shipped; probe: Admission,hold_command,APPROVAL_DECIDE_KEY_PREFIX -->
> **Status: shipped.** ADR-0128. Advertised by `ServerFeature::APPROVALS =
> 0x40000000` ([proto.md](./proto.md) §6.2).

The guard has a third outcome beside admit and deny. A request the effective
grant admits only through a clause that holds `SIGNAL` (§5.1 `?signal`) is
*held*: every verb it needs on every subject must be covered, and at least
one `SIGNAL` need is covered only by a held clause.

- Only a nested `COMMAND` can be held. A held `SIGNAL` on any other frame (a
  keep-empty clear, a config reload, a decision) is refused like any other
  denial, because there is no result to defer.
- A held command is not run. The server writes the server-owned record
  `phux.approval/v1/<id>` ([L3.md](./L3.md) §3.10), journals
  `approval_requested { id }` with the requester as actor
  ([L1.md](./L1.md) §7.1), and defers the command's `COMMAND_RESULT`.
  A connection may hold at most `defaults.approval-max-pending` (64)
  commands, and the server at most `defaults.approval-max-pending-total`
  (1024); one more is answered `RESOURCE_EXHAUSTED` at once. A batch
  (`KILL_RESOURCES`, `CLOSE_TAB_RESOURCES`) names each of its Terminals.
- A decision is `SET_METADATA { Global, "phux.approval.decide/v1/<id>" }`
  with value `approve` or `deny`, classified `SIGNAL` on the held command's
  subjects under the current topology. It needs that `SIGNAL` un-held, so
  the requester cannot approve itself, and a connection subscribed to a held
  subject as a `VIEWER` ([L1.md](./L1.md) §8.1) cannot decide it. The server
  stores nothing under the decide key.
- Each approval is decided once. The first decision removes the record, so a
  second finds no pending approval and is refused like an absent target.
- On `approve` the held command is classified again under the requester's
  current grant and topology. If that still admits it, the command runs once,
  as the requester under the requester's original `request_id`; the approver's
  grant is never used. Otherwise the requester gets `PERMISSION_DENIED`.
  On `deny` the requester gets `PERMISSION_DENIED { "approval denied" }`.
- Nobody deciding within `defaults.approval-ttl-secs` (120) expires the
  approval: the record is removed and the requester gets
  `PERMISSION_DENIED { "approval expired" }`.
- A requester that disconnects or has its authority revoked withdraws its
  holds. Nothing runs and no result is owed. A Terminal reaped while a hold
  names it withdraws that hold before its `pane_closed` is journaled, and
  the requester gets `PERMISSION_DENIED { "terminal gone" }`.
- A hold only restricts, so for §7's containment a ceiling that holds a
  verb a live grant minted un-held no longer contains it: adding `?signal`
  to a live credential revokes its connections.
- Every ending journals `approval_decided { id, outcome }`, with the
  decider as actor for `approved` and `denied`, and no actor for `expired`
  and `withdrawn`.

The owner's grant never holds, so a `local` server never holds anything.
No grant holds anything unless its registry record spells
`?signal`.

## 7. Denial, expiry, and live revocation

<!-- impl-status: shipped; probe: revoke_connection,spawn_revocation_watcher,AuthorizationRevoked -->
> **Status: shipped.** The reference server watches the workload registry,
> each live grant's expiry, and the pairing-token store behind every
> bearer-admitted connection: the bearer stays outer admission, but its
> revocation, removal, or expiry ends the connection it admitted, because
> ADR-0116 supersedes ADR-0031's survive-until-drop. While such a
> connection is live it polls every 250 ms, each poll one `stat` per store,
> and it wakes at the earliest expiry. One critical section of the state
> lock performs steps 1 and 2, signals the connection's writer, and cancels
> the connection token. The writer drops everything queued, writes the two
> frames of step 3 (bounded, so a peer that stopped reading cannot hold it
> open), and closes. The cancelled token aborts the connection's queued bulk
> `PUT_FILE` and `TRANSCRIBE` and its pending input-lane receipts, and input
> it queued on the input lane is not delivered after the revocation. A
> connection revoked before its HELLO completed closes with no frame. A
> ceiling change is judged clause by clause: a grant stays while the new
> ceiling carries each minted clause's verbs on a selector containing either
> of the clause's selectors, and it then adopts the new generation and
> expiry. A registry read that fails for a reason that may clear (an I/O
> error, a file that kept changing) is no verdict and is retried at the next
> poll. A missing, insecure, or malformed registry applies the empty
> snapshot only once the same broken state has persisted for five seconds,
> so a write caught mid-way revokes nothing. The pairing-token store is
> judged more narrowly: only a store that loaded cleanly and holds
> credentials gives a verdict (the admitting generation revoked, expired, or
> absent); a missing, empty, insecure, or unparseable one refuses new
> connections but ends no live one. A bearer revoked since its upgrade is
> refused at HELLO. A ceiling that splits one minted clause across several
> grants is judged as not containing it, the safe direction. The owner
> socket's grant is never watched.

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
pre-HELLO TLS refusal has no DETACHED to carry it): the policy engine refusing
the peer the transport authenticated, when HELLO is evaluated. That includes
a TLS peer the `local` mode does not admit, a credential the registry no
longer holds or has revoked, and a bearer revoked or expired since its
upgrade. The refused HELLO is answered `ERROR { PERMISSION_DENIED }`, then
`DETACHED { AUTHENTICATION_FAILED }`, and the transport closes; no grant is
minted, so no `HELLO_OK` is sent. A client that does not recognize a detach
reason already treats it as unstated, so these values are additive.

## 8. Policy modes and secret handling

Policy is closed:

| Mode | Admitted stateful transports | Workload authentication |
|---|---|---|
| `local` | owner-authenticated UDS only | not required; server mints all six verbs at Global for that connection |
| `paired` | owner UDS plus TLS transports (QUIC, WSS) | mTLS client certificate required on every TLS connection; owner UDS keeps kernel-uid authority |

These are the only modes. There is no bearer-only, SSH-auth-suffices, hybrid, or
unnamed compatibility state. The bearer token is an outer admission gate inside
`paired`, not a grant. `phux stdio-bridge` is an owner-UDS peer, so a bridged
connection keeps owner authority under `paired` as well: an SSH peer that can
run the bridge already owns the host.

With no explicit mode, a server may start only with its owner-only UDS and no
configured workload registry; that is `local`. A configured registry, CA path,
or non-UDS listener with mode unset is a startup error. `local` with a
non-UDS listener is a startup error. `paired` with missing, malformed, or unsafe
CA or registry material is a startup error. Runtime corruption after a
successful start applies the empty-snapshot revocation rule in §7.

A server resumed across a graceful upgrade SHALL NOT exit for a mode error:
the previous process is gone, and exiting would end every terminal it handed
over. It runs `local` with no non-UDS listener or relay connector bound, and
reports each listener it left unbound with `disabled_reason:
"policy_mode_refused"` ([L1.md](./L1.md) §9.1 remote-listeners report). A
retired-setting error alone is ignored when the rest of the configuration
resolves.

<!-- impl-status: shipped; probe: PolicyPosture,UnsetWithRemoteListener,UnsetWithWorkloadAuthority,RetiredWorkloadMtlsEnv -->
> **Status: shipped (ADR-0179).** The reference server reads the mode from
> `[policy] mode` and enforces `local`, `paired`, and every startup error
> above. With no mode it is `local`: a remote listener (`--listen`,
> `--quic`, `--webtransport`, or their environment variables) or a
> `[[connector]]` refuses to start, as does a CA or registry location named
> by `PHUX_WORKLOAD_CA`, `PHUX_WORKLOAD_CA_KEY`, or `PHUX_WORKLOAD_KEYS`. A
> registry at the default location is not "configured" in this sense:
> `phux host add` (§8.1) enrolls into it over ssh before the host opts in to
> `paired`, so its presence alone refuses nothing. The retired opt-in
> `PHUX_WORKLOAD_MTLS` refuses to start and names `[policy] mode`. The
> overlay listener is auto-bound only under `paired`, and `local` refuses
> `OPEN_LISTENER`. An embedder that injects its own engine through
> `ServerConfig::policy_engine` (ADR-0072) decides every HELLO itself; `phux
> server` never does, so no configuration reaches that. A `phux upgrade`
> into a refused configuration keeps its panes as above and logs the
> refusal; setting the mode and upgrading again binds the listeners.

TLS, certificate verification, an SSH login, a bearer token, or UDS
peer credentials remain necessary transport evidence where their transports
require them. None substitutes for the client certificate in `paired`.
Plaintext remote transport is forbidden in every mode.

`phux workload authority --init` is the only CLI path that creates a missing
CA and prints only its fingerprint. `phux workload authority --rotate`
replaces an existing CA (§2) and prints only the old and new fingerprints. `phux workload add-key` accepts only a
client certificate or CSR from stdin or an explicitly opened file and writes
the registry. For a CSR it writes the issued chain (leaf, then CA) to a new
`--cert-out` file or, with `--cert-stdout`, to stdout (as the `--json`
document's `certificate_chain`); the chain is public. `--replace <id>`
revokes that credential in the same registry write that enrolls the new
one. `list`, `revoke`, and `authority` display only credential ids,
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

### 8.1 Client enrollment over ssh

<!-- impl-status: shipped; probe: ClientRequest,IssuedIdentity,settle_identity,RENEW_WITHIN_SECONDS,HOST_ADD_WORKLOAD_SCOPE -->
> **Status: shipped.** `phux host add` (and the attach repair rung that
> re-pairs over ssh) enrolls a CLI or desktop client. A device with no ssh
> channel enrolls with a ticket instead (§8.2).

A client that already holds ssh access to the serving host enrolls over
that channel; enrollment grants no authority ssh did not already grant.

1. The client generates a fresh private key and a CSR for it. The key
   never leaves the client.
2. Over ssh, the client runs `phux workload authority --init` (idempotent)
   and then `phux workload add-key --json --cert-stdout --scope <ceiling>`,
   passing the CSR on stdin. Neither the CSR nor any key material appears
   in the remote command line. The reference client asks for every verb at
   `global`, the authority an ssh login already has.
3. The client SHALL treat the reply as untrusted and refuse it, storing
   nothing, unless: the reply names a canonical credential id equal to
   `sha256:` of its own key; the chain is exactly two PEM certificates, the
   issued leaf then the CA, and carries no private key; the leaf carries the
   client's own public key, is not a CA, and verifies against that CA as a
   client-authentication certificate valid now.
4. The client stores the key and chain as owner-only (`0600`) files under
   new names in its state directory, obeying the same no-follow,
   owner-controlled-directory, and atomic-write rules as §2, and records
   their paths (never their bytes) in its `[[remote]]` entry as
   `client-cert` and `client-key`. Every TLS dial to that remote presents
   them; an entry with neither presents no certificate, and an entry with
   one but not the other is refused rather than dialed without it.
5. Re-enrollment is two-phase. The client enrolls the new credential
   (step 2, without `--replace`), validates and stores it (steps 3 and 4),
   and rewrites its entry to the new paths in one write. Only then does it
   run `phux workload revoke <previous id>` over ssh and remove the
   previous files it enrolled (never files the operator named in the
   entry themselves). The previous id is read from the certificate the
   entry names or, for an entry that names only its key, from the enrolled
   certificate beside that key; with neither, the client warns that the
   previous credential cannot be named and revokes nothing. A failure
   before the entry is rewritten leaves the previous entry in place and
   still admitted; a credential the far host enrolled that the client then
   refused or could not record is revoked by the client's own request id,
   never by an id the reply named. A failed revocation of the previous
   credential is reported with the command that completes it; that
   credential stays admitted until it expires. A host that holds no
   credential with the previous id (the entry used to reach another host)
   is reported as such, never as a revocation. Between the two ssh calls
   both credentials of this one client are admitted; no failure strands the
   entry on a revoked credential.
6. Renewal re-runs steps 2 to 5 for the same entry. `phux host renew
   NAME` does so on demand, over the entry's ssh destination, and changes
   nothing else in the entry; it refuses, leaving the entry unchanged, a
   certificate issued by another CA than the held chain's, and an entry
   with neither a recorded ssh destination nor a held CA to compare. `phux host add` does so instead of reusing a
   certificate whose admission ends within 14 days, or that is expired or
   unreadable; a client dialing such an entry warns once per process, and
   `phux doctor` reports it. Admission is taken to end one day before the
   leaf's `notAfter`, since issuance rounds the registry expiry up to the
   next midnight after a day of slack. Nothing renews in the background.

A failed enrollment does not fail pairing: the host is registered, and its
dials present the previous identity, or none; `phux host add --json`
reports the outcome in its `enrollment` object. A private key SHALL NOT
appear in a QR code, connect link, `phux pair --json`, a log, or an ssh
argument. `phux pair revoke` and `phux workload revoke` both accept an
enrolled certificate's `sha256:` id; revocation follows §7.

### 8.2 Client enrollment with a ticket

<!-- impl-status: shipped; probe: ENROLL_ALPN,mint_ticket,redeem_ticket,enroll_with_ticket,enroll_target,check_issued_chain -->
> **Status: shipped (ADR-0154).** `phux pair --enroll` mints the ticket;
> the reference server offers the ALPN on its configured QUIC listener in
> every policy mode; `phux --remote NAME --code LINK` and the phone binding
> (`RemoteClient.enroll_device`, a keystore-held key) enroll with it. A
> ticket rides QUIC: the configured listener's ALPN, or the end-to-end
> session a relayed device runs (§3), which negotiates the same ALPN inside.
> The WebTransport listener does not serve it.

A client with no ssh channel to the serving host (a phone) enrolls with a
single-use **enrollment ticket**: 32 random bytes the server mints into a
pairing link (`enroll=`, lowercase hex), keeping only their SHA-256, the
scope ceiling and lifetime the credential will get, and the ticket's own
expiry, in `<state-dir>/enrollment-tickets` beside the registry under the
same file rules (§2). The reference server mints a ticket for every verb at
`global` (the ceiling `phux host add` uses), a one-year credential, and a
ten-minute ticket. A ticket authorizes exactly one enrollment: the first
redemption consumes it under the store lock, before anything is issued, and
an unknown, expired, and consumed ticket are refused alike.

The exchange runs on its own QUIC ALPN, `phux-enroll/1`, which a listener
offering it negotiates beside `phux-quic/1`. It carries no bearer token, no
phux frame, and grants nothing. On one bidirectional stream the client sends
the whole request and finishes:

```text
version:u8 (= 1) | ticket_len:u8 (1..=64) | ticket | csr_len:u32 BE (1..=16384) | csr (PKCS#10 DER)
```

and the server answers once, finishes, and closes the connection:

```text
status:u8 (0 issued, 1 refused) | len:u32 BE | body
```

An issued body is the chain the CA issued for the CSR's key, PEM, leaf then
CA, with the fields §8.1 gives an ssh enrollment; a refused body is the word
`refused`, whatever failed. The server checks the CSR's self-signature before
it redeems the ticket, so a malformed request spends nothing. Decoders refuse
a wrong version, an out-of-bounds length, truncation, and trailing bytes.

The client generates its key and keeps it: on a phone in the platform
keystore, signing through the binding for the CSR and every later handshake.
The link SHALL carry the server's CA (`ca`, ADR-0153), and a client SHALL
refuse to enroll without it rather than learn the CA from the reply
(ADR-0179); a server whose certificate no CA issued mints no ticket. The
client dials trusting the server by that pin, treats the reply as §8.1 step 3
does, and additionally refuses a CA other than the pinned one. The private key never appears in the link, the request,
or any log; the ticket is a secret like the bearer token and is shown once.

An unspent ticket may be revoked by its handle (`t-` and eight lowercase
hex digits; the reference CLI is `phux workload revoke t-…`). From then on it
is refused like a consumed one.

### 8.3 Satellites that dial out

<!-- impl-status: shipped; probe: SATELLITE_ALPN,mint_satellite_ticket,register_satellite,SatelliteAdopter,JoinCode -->
> **Status: shipped (ADR-0181).** `phux host ticket NAME` mints the
> ticket and registers `[[satellites]] NAME` as `inbound://`. `phux host
> join CODE` enrolls and records `[[uplink]]`. The configured QUIC listener
> of a `paired` server serves the ALPN, and the server's `[[uplink]]`
> supervisors dial it.

A server the hub cannot reach (a sandbox behind NAT) becomes the hub's
satellite by dialing the hub. Its enrollment is §8.2's exchange with a
**satellite ticket**: a ticket whose store record names the satellite,
`NAME`. The ticket, never the request, decides the role. The credential it
enrolls is recorded with `satellite = NAME` and no scope ceiling (§2). In
the same locked write, the hub revokes every other active credential of
`NAME`, so one name has one live credential. The reference hub mints a
satellite ticket for ten minutes and its credential for one year.

The hub's join code is
`phux://satellite?hub=HOST:PORT[&sni=NAME]&ca=sha256:…&name=NAME&enroll=HEX`
(`hub`, `sni`, and `name` percent-encoded). It carries no bearer token. The
`ca` is required: the joining server pins the hub's CA, never learns it,
and refuses an issued chain whose CA differs.

The satellite then dials the hub's QUIC listener under the ALPN
`phux-satellite/1`, trusting the hub by that CA and presenting its
satellite certificate. On that ALPN the hub:

- offers the ALPN only on a listener that verifies workload certificates
  (`paired`);
- admits the connection only when the certificate maps to an active
  satellite credential whose `NAME` is an enabled `inbound://` satellite in
  its table, and otherwise closes it with `AUTH_FAILED` (`0x01`) before any
  stream;
- is the phux client: it opens bidirectional streams, and each one is one
  ordinary phux connection (§5 of [proto.md](./proto.md)) beginning with
  the hub's `HELLO`;
- runs that connection as `NAME`'s satellite link, exactly as a link it
  dialed, and closes an older connection of `NAME` when a newer one is
  admitted;
- closes the connection with `AUTH_FAILED` once the credential is revoked,
  expired, or no longer in the registry, re-checking at least every second.

The satellite opens no stream. It serves each stream the hub opens as a
connection to its own owner socket, so the hub holds the satellite owner's
authority, as a hub dialing `ssh://` does through `phux stdio-bridge`. A
satellite therefore needs no listener and may run `local`. It redials a lost
or refused connection with capped exponential backoff.

## 9. Conformance cases

A conforming implementation exercises at least these independent failures:
version mismatch before admission; no client certificate; unknown, expired,
or non-chaining certificate; downgrade (paired client against a listener
that does not request certificates); unknown credential id; empty or
over-ceiling grant; unknown verb/selector; duplicate, unsorted, truncated,
overlong, or trailing scope encoding; an all-zero `SESSION` or `RESOURCE`
key; an uppercase or shortened-prefix key spelling; a persisted registry
record accepting `session:<key>` and `resource:<key>` but refusing `group:`
and `terminal:`; a key no live record carries matching nothing; and
revoked/expired authority at admission and while live. Enrollment cases (§8.2): a consumed, expired, or
unknown ticket; one ticket raced by two keys; a malformed request leaving the
ticket unspent; an issued chain presented with another key; and a
certificate-less terminal connection on a listener that serves enrollment.
Satellite cases (§8.3): a revoked ticket; a satellite credential presented
on the terminal ALPN; a satellite credential whose name the hub has no
enabled `inbound://` entry for; a re-enrollment revoking the name's previous
credential; and a credential revoked while its link is live.

Authorization cases include every matrix row; BIND-only
`phux.session.create/v1`; mutation/subscription of server-owned metadata; a
Terminal moving out of and back into a ceiling Group; owner-addressed spawn
whose payload Group disagrees; every multi-target partial grant; satellite relay
bypass; SSH-stdio in both closed modes; remote `SIGNAL(Global)` attempting
SHUTDOWN; registry ceiling reduction; and absence of secret bytes from argv,
environment, stdout, stderr, diagnostics, and logs.

Scope-schema cases (§5): bits `0xC0` refused in a v1 image and accepted in a
v2 one; a v1 `SIGNAL` grant reaching `RESOURCE_TERMINATE` and
`SESSION_DELETE` and a v1 `CREATE` grant reaching `RESOURCE_MOVE`; a v1 held
`?signal` reaching no `TERMINATE` row; an `organize` or `terminate` name
refused in a v1 record; and every legacy row admitting exactly the grants it
admitted under v1. Organization cases: a resource moving out of a ceiling
Group by `RESOURCE_MOVE` losing the access it had through that Group in the
same transaction; and a `RESOURCE_MOVE_MANY` with one unauthorized source
session refused whole.
