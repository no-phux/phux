---
audience: consumers, contributors, agents
stability: stable
last-reviewed: 2026-09-12
---

# Coordinator — durable work endpoint

**TL;DR.** Normative `phux-coordinator/1` wire and state contract. It defines an
independently versioned, workload-authenticated endpoint for durable work:
opaque IDs, fenced authority epochs, closed scopes, operation idempotence,
complete paged snapshots, cursor-bound replay, credit-controlled events,
explicit gaps, and immutable references to terminal resources. It never carries
terminal input, output, bootstrap, or history.

---

<!-- impl-status: spec-only; probe: COORD_HELLO,CoordHello -->
> **Status: spec-only.** No codec, server, or client in this tree implements
> the coordinator endpoint. The terminal protocol is independent of it.

## 1. Scope and status

This document specifies coordinator protocol `0.1.0`. The endpoint is
spec-only: no codec or server exists in this tree. The key words `MUST`, `MUST NOT`,
`REQUIRED`, `SHALL`, `SHALL NOT`, `SHOULD`, `SHOULD NOT`, `MAY`, and `OPTIONAL`
are interpreted as RFC 2119 requirements.

The coordinator owns durable Objective, Run, WorkSession, Artifact, Signal,
binding, operation-result, ordered-event, and evidence state under
[ADR-0092](../adr/0092-durable-work-coordinator-authority.md). Terminal
owners remain authoritative for processes, PTYs, terminal output order,
bootstrap generations, input leases, input results, and process signals.

This is not L1, L2, L3, or an extension of the terminal frame namespace.
[ADR-0030](../adr/0030-engine-delegated-wire-and-projection-consumers.md)'s
closed list, as generalized by
[ADR-0102](../adr/0102-resources-the-server-serves-kinds.md), remains normative for terminal synchronization. In particular:

- no frame in this document is legal on a terminal connection;
- terminal bytes, cells, input atoms, history, and bootstrap never appear here;
- no coordinator record may be encoded into L3 as an alternative authority; and
- an implementation may expose the terminal endpoint without exposing this one.

A client that requires durable work MUST refuse when this endpoint or a required
capability is absent. It MUST NOT degrade to client-local authoritative state,
L3 metadata, title/cwd/PID matching, or a newly minted Run.

## 2. Endpoint binding and transport

The application label is exact ASCII `phux-coordinator/1`; the workload service
is exact ASCII `phux-coordinator`. v0.1 is **paired-only**, including local
access. The server refuses startup in workload-auth local/proofless mode.

The endpoint is selected before decoding:

| Transport | v0.1 binding |
|---|---|
| Local | Dedicated owner-only Unix socket, distinct from the terminal socket, plus kernel-uid authority |
| QUIC | ALPN `phux-coordinator/1`, plus mTLS client certificate |
| SSH/stdin bridge | Forbidden: no independently verifiable channel binding |
| Any other transport | Forbidden until a capability and channel binding are specified |

Terminal and coordinator endpoints MAY share a process and implementation
components. They MUST NOT share a stream, COORD_HELLO state, version, capability
namespace, or frame namespace. Remote transport always supplies confidentiality
and integrity; the mTLS client certificate is authentication, not encryption.

No private key or bearer secret may be passed through argv, environment
variables, frame diagnostics, or logs.

## 3. Encoding and hard bounds

### 3.1 Envelope and allocation order

Every message is:

```text
u32_be length || u8 type || payload
```

`length` counts type plus payload and is in `1..=1_048_576`. A receiver reads
the four-byte length and one-byte type into fixed storage, derives the applicable
cap from connection state and type, and only then reserves `length - 1` payload
bytes. During handshake, every allowed type is capped at 16 KiB. After
COORD_HELLO_OK, the selected frame cap applies; page and typed-payload frames
also apply their narrower selected caps. Invalid length/type/state is rejected
before payload allocation. On a message transport, declared and actual lengths
match exactly.

Message bodies use [appendix-encoding.md](./appendix-encoding.md) TLV. Encoders
emit each field once in increasing ID order with minimal varints. Decoders skip
unknown fields by length but reject duplicate IDs, wrong known wire types,
non-minimal varints, invalid UTF-8/bools, truncated nested values, and trailing
nested bytes. Every top-level field uses wire type `BYTES`; logical scalar
widths below describe that field's bounded positional value.

### 3.2 Limits and quotas

COORD_HELLO selects each offered limit as `min(client, server)`. Zero or an
offer above its hard cap is malformed. Selected page bytes and typed payload
bytes MUST be at most `selected_frame_bytes - 512`; otherwise COORD_HELLO is refused.
Selected event credit bytes MUST encode at least one maximum-sized COORD_EVENT.

| Item | Hard cap | Reference offer |
|---|---:|---:|
| Outer frame | 1 MiB | 512 KiB |
| Handshake frame | 16 KiB | 16 KiB |
| Snapshot/replay page payload | 512 KiB | 256 KiB |
| Records/events per page (`u16` count) | 1,024 | 512 |
| Command/event typed payload | 256 KiB | 128 KiB |
| Opaque cursor | 4 KiB | 4 KiB |
| Name | 128 UTF-8 bytes | 128 bytes |
| Diagnostic | 1,024 UTF-8 bytes | 1,024 bytes |
| Outstanding requests per connection | 256 | 128 |
| Subscriptions per connection | 32 | 16 |
| Credit balance | 4,096 events / 8 MiB | 1,024 / 4 MiB |
| Unsent queue per subscription | 1,024 frames / 4 MiB | same |
| Reserved control lane per connection | 32 frames / 64 KiB | same |
| Snapshot/replay leases per connection | 8 | 4 |
| Preconditions per command (`u16` count) | 64 | 32 |
| Snapshot/replay lease duration | 60 seconds | 60 seconds |

Connections do not reset aggregate quotas:

| Resource | Per authenticated principal | Server-wide |
|---|---:|---:|
| Connections | 16 | 256 |
| Outstanding requests | 512 | 8,192 |
| Snapshot/replay leases | 16 | 256 |
| Bytes pinned by immutable cuts | 32 MiB | 512 MiB |
| Subscriptions | 64 | 2,048 |
| Unsent subscription queues | 4,096 frames / 16 MiB | 65,536 / 256 MiB |
| Pending operations/effect attempts | 10,000 | 100,000 |

The server charges principal and server quotas before creating a cut, pinning a
version, admitting an operation, or enqueueing a frame. Exhaustion refuses the
new action without evicting old state. Leases and queues release on completion,
disconnect, expiry, or revocation. A record/list/manifest/cache is additionally
bounded by its enclosing cap. Bulk content goes to an Artifact; a peer never
raises a local allocation limit after receipt.

### 3.3 Canonical list and union encoding

All nested lists begin with a big-endian `u16` count followed by exactly that
many elements; per-type caps still apply. Tagged unions begin with `u8 tag`.
Nested records contain their fields positionally in the order shown and consume
their entire enclosing TLV field. `optional<T>` is `u8 present` (`0` or `1`)
then `T` iff present. `bytes16`, `bytes32`, and `bytes64` are exact-width with
no inner length; variable `bytes`/`str` use a big-endian `u32` length.

## 4. Identity, cuts, and authority guards

Each nominal ID is exactly 16 nonzero opaque CSPRNG bytes:

```text
CoordinatorId  IncarnationId  ObjectiveId  RunId  WorkSessionId
BindingId      PendingBindingId ArtifactId SignalId EventId OperationId
EffectAttemptId SnapshotId      ReplayId   SubscriptionId
ActivationWitnessId
```

Consumers compare bytes only; IDs encode no time, type, host, route, PID, or
row number. Sorting is allowed only where a canonical byte image requires it.

`AuthorityEpoch`, `ObjectRevision`, `EventSequence`, `DeliverySequence`,
`SourceSequence`, and `ActivationSequence` are distinct nonzero checked `u64`
types. `EventCut` and `DeliveryCut` are `u64`: zero means no event/delivery yet,
otherwise they contain the corresponding sequence. Wrap fences the writer.

`CoordinatorId` survives restart. `IncarnationId` changes every process start.
Epoch and activation sequence advance before bind. Every state-rooting command,
lookup, snapshot, subscription, or replay request carries:
```text
AuthorityGuard = (
  coordinator_id: bytes16,
  authority_epoch: u64,
  activation_sequence: u64,
  fencing_token_sha256: bytes32,
)
```

The guard MUST match the unexpired witness certificate from COORD_HELLO_OK.
Mismatch, expiry, or witness revocation is fatal `STALE_AUTHORITY` before
allocation, mutation, or lookup. Continuation frames (NEXT, APPLIED, CREDIT)
inherit the guard bound to their unpredictable server-issued ID/cursor and the
authenticated connection, and recheck it before acting. Request correlation and
OperationId are not part of the guard.

## 5. Authentication, COORD_HELLO, and capabilities

The only handshake is:

```text
TLS handshake (mTLS client certificate) -> COORD_HELLO -> COORD_HELLO_OK
```

COORD_PING is the only permitted pre-COORD_HELLO interleaving.
Authentication is the TLS handshake per [workload-auth.md](./workload-auth.md)
as amended by [ADR-0116](../adr/0116-workload-auth-is-mtls.md): the server
verifies an mTLS client certificate against the phux CA and authorizes the
credential id against the registry before COORD_HELLO is evaluated. There
are no `WORKLOAD_CHALLENGE` / `WORKLOAD_RESPONSE` frames and no
`WorkloadOffer` / `WorkloadGrant` records; the endpoint owns its closed
scope schema but not a proof profile. COORD_HELLO carries no workload
field; admission beyond the TLS layer is version check, then scope check.

The server reads only bounded version fields first. Major/minor mismatch is
fatal `VERSION_INCOMPATIBLE` before authority material is parsed. Missing,
legacy, local/proofless, expired, replayed, wrongly scoped, or transcript-
mismatched proof is fatal `AUTHENTICATION_FAILED`.

All fields below are required TLV `BYTES`, with IDs shown:

```text
COORD_HELLO {                                // 0x01, C -> S
  1 protocol_major: u16
  2 protocol_minor: u16
  3 protocol_patch: u16
  4 client_name: str
  5 offered_capabilities: u64
  6 required_capabilities: u64
  7 max_frame_bytes: u32
  8 max_page_bytes: u32
  9 max_page_records: u16
 10 max_typed_payload_bytes: u32
 11 max_event_credit_count: u32
 12 max_event_credit_bytes: u32
 // field 13 is retired-unshipped: the WorkloadOffer record belonged to the
 //   retired phux-workload/v1 proof profile (ADR-0116). No sender emits it.
}

COORD_HELLO_OK {                             // 0x80, S -> C
  1 protocol_major: u16
  2 protocol_minor: u16
  3 protocol_patch: u16
  4 negotiated_capabilities: u64
  5 coordinator_id: bytes16
  6 authority_epoch: u64
  7 incarnation_id: bytes16
  8 max_frame_bytes: u32
  9 max_page_bytes: u32
 10 max_page_records: u16
 11 max_typed_payload_bytes: u32
 12 max_event_credit_count: u32
 13 max_event_credit_bytes: u32
 // field 14 is retired-unshipped: the WorkloadGrant record belonged to the
 //   retired phux-workload/v1 proof profile (ADR-0116). No sender emits it.
 15 operation_result_retention_secs: u64     // >= 2_592_000
 16 activation_certificate: ActivationCertificate
}
```

Challenge incarnation and COORD_HELLO_OK incarnation match exactly. Authority
fingerprint, CoordinatorId, witness identity, and fencing token are independent
pins. Results are retained at least the advertised 30-day floor; storage
pressure refuses new mutations before violating it.

Unknown capability bits are ignored during intersection and echoed only when
both peers offer them. Required bits are a subset of the client offer and the
intersection or COORD_HELLO fails.

```text
EVENT_REPLAY       = 0x0000_0000_0000_0001
TERMINAL_BINDINGS  = 0x0000_0000_0000_0002
ARTIFACT_MANIFESTS = 0x0000_0000_0000_0004
SIGNALS            = 0x0000_0000_0000_0008
ADMINISTRATION     = 0x0000_0000_0000_0010
```

Authenticated scopes, idempotent commands/results, whole-authority snapshots,
live events, credit, GAP, and fresh-snapshot replacement are baseline.
Capability-gated records/commands are refused when their bit is absent.
Allocations are permanent; additive growth uses a bit or unknown-skippable
field under [ADR-0061](../adr/0061-capabilities-add-versions-break.md).

## 6. Closed scopes and total read visibility

Coordinator `requested_scope_bytes` and `effective_scope_bytes` are exactly one
big-endian `u64`. Any other length, noncanonical image, or unknown bit is fatal
before proof verification.

```text
WORK_READ         = 0x0000_0000_0000_0001
WORK_COMMAND      = 0x0000_0000_0000_0002
TERMINAL_CONTROL  = 0x0000_0000_0000_0004
ARTIFACT_READ     = 0x0000_0000_0000_0008
SIGNAL_ACK        = 0x0000_0000_0000_0010
ADMIN             = 0x0000_0000_0000_0020
```

Scopes are independent; ADMIN implies none. Effective scope is the intersection
of requested bits and one authenticated, unexpired policy grant. Empty is
refused. Expiry/revocation is rechecked immediately before every transaction or
delegated effect and before serializing each page/event/result.

One visibility predicate applies identically to snapshot, subscription, replay,
direct lookup, and operation-result payloads:

| Record/action | Visibility or mutation requirement |
|---|---|
| Objective, Run, WorkSession | `WORK_READ`; mutation `WORK_COMMAND` |
| Binding and admitted terminal-source fact | `WORK_READ` plus negotiated `TERMINAL_BINDINGS`; lifecycle delegation also `WORK_COMMAND + TERMINAL_CONTROL` |
| Artifact identity/revision stub | `WORK_READ` |
| Artifact manifest/content/evidence payload | `ARTIFACT_READ` plus negotiated `ARTIFACT_MANIFESTS` |
| Signal fact/history | `WORK_READ` plus negotiated `SIGNALS`; acknowledge/suppress also `SIGNAL_ACK` |
| Operation metadata/result | Its authenticated originating principal with `WORK_READ` or `WORK_COMMAND`; any principal with `ADMIN` |
| Activation, retention, credential, or administrative record | `ADMIN` plus negotiated `ADMINISTRATION` |

Unauthorized families are omitted deterministically from all four streaming/read
surfaces and direct lookup returns `NOT_FOUND`, not a revealing denial.
Reserved snapshot subscriptions use the exact predicate fixed at the cut.
Grant expiry or revocation emits GAP where possible and closes the connection;
it never leaves a reduced, plausibly complete stream.

## 7. Frame and nested-type allocation

The IDs are coordinator-local. The `0x04` / `0x84` slots once pencilled
for imported `WORKLOAD_RESPONSE` / `WORKLOAD_CHALLENGE` are
retired-unshipped with the proof profile (ADR-0116) and stay unallocated.

| ID | Direction | Frame |
|---:|---|---|
| `0x01` | C -> S | `COORD_HELLO` |
| `0x10` | C -> S | `COORD_COMMAND` |
| `0x11` | C -> S | `GET_OPERATION_RESULT` |
| `0x20` | C -> S | `OPEN_SNAPSHOT` |
| `0x21` | C -> S | `SNAPSHOT_NEXT` |
| `0x22` | C -> S | `SNAPSHOT_APPLIED` |
| `0x23` | C -> S | `SUBSCRIBE` |
| `0x24` | C -> S | `CREDIT` |
| `0x25` | C -> S | `OPEN_REPLAY` |
| `0x26` | C -> S | `REPLAY_NEXT` |
| `0x27` | C -> S | `REPLAY_APPLIED` |
| `0x7f` | either | `COORD_PING` |
| `0x80` | S -> C | `COORD_HELLO_OK` |
| `0x82` | S -> C | `ACK` |
| `0x83` | S -> C | `COORD_AUTHORITY_RENEWED` |
| `0x84` | — | retired-unshipped (was `WORKLOAD_CHALLENGE`; ADR-0116) |
| `0x90` | S -> C | `COORD_COMMAND_RESULT` |
| `0x91` | S -> C | `OPERATION_RESULT` |
| `0xa0` | S -> C | `SNAPSHOT_BEGIN` |
| `0xa1` | S -> C | `SNAPSHOT_PAGE` |
| `0xa2` | S -> C | `SNAPSHOT_END` |
| `0xa3` | S -> C | `SUBSCRIBED` |
| `0xa4` | S -> C | `REPLAY_BEGIN` |
| `0xa5` | S -> C | `REPLAY_PAGE` |
| `0xa6` | S -> C | `REPLAY_END` |
| `0xb0` | S -> C | `COORD_EVENT` |
| `0xb1` | S -> C | `GAP` |
| `0xc0` | either | `COORD_ERROR` |
| `0xff` | either | `COORD_PONG` |

All are spec-only. Wrong state/direction or unknown type is fatal. Frame schemas
in this document assign TLV field IDs explicitly. Imported frame bodies use
workload-auth.md. `ACK { 1 request_id:u32 }`, `COORD_PING { 1 nonce:u64 }`,
`COORD_PONG { 1 nonce:u64 }`, and
`COORD_AUTHORITY_RENEWED { 1 activation_certificate:ActivationCertificate }`
are complete schemas. COORD_PING proves liveness only.

Connection-local nonzero request IDs are occupied through the terminal reply.
Reuse while outstanding is malformed. CREDIT, COORD_PING/COORD_PONG, GAP, COORD_EVENT,
and COORD_AUTHORITY_RENEWED have no request ID.

```text
ObjectKind = u16 {
  OBJECTIVE=1, RUN=2, WORK_SESSION=3, BINDING=4, ARTIFACT=5,
  SIGNAL=6, OPERATION=7, EVIDENCE=8, ADMIN=9, ARTIFACT_MANIFEST=10
}
ObjectRef = ObjectKind || bytes16
Precondition = ObjectRef || u64 expected_revision
WholeAuthoritySelector = u8 tag(0)              // no trailing bytes

SnapshotRecord =
  ObjectKind || bytes16 id || u64 revision || u16 schema_version ||
  u32 payload_len || payload

CommandKind = u16 {
  CREATE_OBJECTIVE=1, UPDATE_OBJECTIVE=2, START_RUN=3, CANCEL_RUN=4,
  PAUSE_RUN=5, RESUME_RUN=6, ACK_SIGNAL=7, SUPPRESS_SIGNAL=8,
  REGISTER_ARTIFACT_MANIFEST=9, END_BINDING=10, CHANGE_RETENTION=11
}
ResultKind = u16 { NONE=0, OBJECT_REF=1, STARTED=2, DOCUMENT=3 }
EventKind = u16 {
  OBJECTIVE_CHANGED=1, RUN_CHANGED=2, WORK_SESSION_CHANGED=3,
  BINDING_OPENED=4, BINDING_ENDED=5, ARTIFACT_REVISION_ADDED=6,
  SIGNAL_RAISED=7, SIGNAL_ACKNOWLEDGED=8, SIGNAL_SUPPRESSED=9,
  SIGNAL_RESOLVED=10, OPERATION_CHANGED=11, SOURCE_FACT_ADMITTED=12,
  SOURCE_GAP=13, EVIDENCE_SEALED=14
}
```

Every command payload is canonical TLV with required fields below; optional is
stated explicitly. Payload IDs are never inferred from OperationId:

| Kind | Payload fields by TLV field ID |
|---|---|
| CREATE_OBJECTIVE | `1 objective_id:bytes16, 2 schema:u16, 3 document:bytes` |
| UPDATE_OBJECTIVE | `1 objective_id:bytes16, 2 schema:u16, 3 document:bytes` |
| START_RUN | `1 objective_id:bytes16, 2 run_id:bytes16, 3 work_session_id:bytes16, 4 executor_schema:u16, 5 executor_document:bytes, 6 pending_binding_id:bytes16, 7 owner_authority_fingerprint:bytes32, 8 owner_incarnation:bytes16, 9 terminal_group:bytes` |
| CANCEL_RUN / PAUSE_RUN / RESUME_RUN | `1 run_id:bytes16` |
| ACK_SIGNAL / SUPPRESS_SIGNAL | `1 signal_id:bytes16, 2 note:optional<str>` |
| REGISTER_ARTIFACT_MANIFEST | `1 artifact_id:bytes16, 2 revision:u64, 3 schema:u16, 4 manifest:bytes` |
| END_BINDING | `1 binding_id:bytes16, 2 reason:u8` (`Completed=0, OwnerLost=1, Cancelled=2, Failed=3`) |
| CHANGE_RETENTION | `1 operation_result_secs:u64, 2 event_bytes:u64, 3 artifact_bytes:u64` |

Result payloads are positional and exact:

```text
NONE = empty
OBJECT_REF = ObjectKind:u16 || id:bytes16
STARTED = run_id:bytes16 || work_session_id:bytes16 ||
          optional<binding_id:bytes16>
DOCUMENT = schema:u16 || payload_len:u32 || canonical_payload
```

SnapshotRecord `schema_version=1` payloads are canonical TLV:

| ObjectKind | Payload fields by TLV field ID |
|---|---|
| OBJECTIVE | `1 document_schema:u16, 2 document:bytes, 3 state:u8` (`Active=0, Completed=1, Cancelled=2`) |
| RUN | `1 objective_id:bytes16, 2 attempt:u32, 3 state:u8, 4 outcome:optional<bytes>` (`Planned=0, Running=1, Paused=2, Completed=3, Cancelled=4, Failed=5`) |
| WORK_SESSION | `1 run_id:bytes16, 2 state:u8, 3 active_binding:optional<bytes16>` (`Active=0, Ended=1`) |
| BINDING | `1 binding:TerminalBindingRef, 2 state:u8, 3 end_reason:optional<u8>` (`Active=0, Ended=1`) |
| ARTIFACT | `1 work_session_id:bytes16, 2 latest_revision:u64` |
| ARTIFACT_MANIFEST | `1 artifact_id:bytes16, 2 revision:u64, 3 schema:u16, 4 manifest:bytes` |
| SIGNAL | `1 work_session_id:bytes16, 2 kind:u16, 3 state:u8, 4 message:str` (`Open=0, Acknowledged=1, Suppressed=2, Resolved=3`) |
| OPERATION | `1 principal_key_id:bytes32, 2 result:OperationResult` |
| EVIDENCE | `1 work_session_id:bytes16, 2 artifact_id:bytes16, 3 seal_sha256:bytes32` |
| ADMIN | `1 admin_kind:u16, 2 payload:bytes` |

Event kinds 1–11 and 14 carry one complete `SnapshotRecord` as payload.
`SOURCE_FACT_ADMITTED` carries `TerminalSourceFact` (§11).

Additional v0.1 nested enums are closed:

```text
SignalKind = u16 { EXECUTION_BLOCKED=1, EXECUTION_FAILED=2,
                   EVIDENCE_GAP=3, POLICY=4 }
AdminKind = u16 { ACTIVATION=1, RETENTION=2, CREDENTIAL=3 }
IndeterminateReason = u8 { TARGET_OUTCOME_UNKNOWN=0,
                           TARGET_DEDUPE_UNAVAILABLE=1,
                           RECOVERY_STATE_INVALID=2 }
```
`SOURCE_GAP` carries `binding_id:bytes16 || expected_source:u64 ||
observed_source:u64 || u8 reason`, where `Jump=0, Conflict=1, Ended=2`.

CREATE/UPDATE documents and events use schema version `1` in v0.1. A new schema,
kind, result, or event is capability-gated and append-only. Unknown or ungated
kinds are refused before admission, never retained as opaque effects.

## 8. Commands, digest identity, and delegated effects

```text
COORD_COMMAND {                              // 0x10
  1 request_id: u32
  2 authority_guard: AuthorityGuard
  3 operation_id: bytes16
  4 command_kind: u16
  5 payload: bytes                          // canonical kind-specific TLV
  6 preconditions: optional<list<Precondition>>
  // field ids >= 7 are unknown command-envelope extensions
}
COORD_COMMAND_RESULT {                       // 0x90
  1 request_id: u32
  2 operation: OperationResult
}
GET_OPERATION_RESULT {                       // 0x11
  1 request_id: u32
  2 authority_guard: AuthorityGuard
  3 operation_id: bytes16
}
OPERATION_RESULT {                           // 0x91
  1 request_id: u32
  2 operation: OperationResult
}
```

Preconditions are sorted lexicographically by `(ObjectKind u16 BE, id bytes16)`.
Duplicate ObjectRefs are malformed even when revisions match. Absent and
present-empty precondition fields are semantically identical; canonical encoding
omits the field and the digest encodes count zero.

The exact operation digest is:

```text
SHA-256(
  ASCII(\"phux-coordinator-operation/v1\\0\") ||
  u16_be(protocol_major=0) || u16_be(protocol_minor=1) ||
  u16_be(command_kind) ||
  u32_be(canonical_payload_len) || canonical_payload ||
  u16_be(precondition_count) ||
    each(ObjectKind:u16_be || id:bytes16 || revision:u64_be) ||
  u32_be(extension_bytes_len) || canonical_unknown_envelope_fields
)
```

`canonical_unknown_envelope_fields` is the byte concatenation of unknown
COORD_COMMAND TLV fields in increasing field-ID order, including minimal field
ID, wire type, length, and exact value. The typed payload is re-encoded
canonically and includes its unknown fields the same way. RequestId,
AuthorityGuard, CoordinatorId, epoch, activation token, and OperationId are
excluded. Thus a reconnect/epoch change preserves identity, while changed kind,
payload, guard list, or extension conflicts.

```text
OperationResult = u8 tag || body {
  0 PENDING       { u64 admitted_epoch, u64 admitted_event_sequence }
  1 SUCCEEDED     { u64 completed_event_sequence, u16 result_kind,
                    u32 payload_len, payload }
  2 REJECTED      { u64 completed_event_sequence, u16 error_code, str message }
  3 INDETERMINATE { u8 reason, optional<EffectAttemptId> }
  4 TOMBSTONED    { bytes32 command_digest, u8 final_state } // 1,2,or3
  5 UNKNOWN       { }
}
```

The durable key is `(CoordinatorId, OperationId)`. First admission stores
principal, digest, PENDING, and any initial event atomically. Same principal and
digest returns stored state; another principal or digest is
`OPERATION_CONFLICT` without payload disclosure. Preconditions, scope,
capability, bounds, and authority guard are checked before admission.

Any delegated terminal effect is represented before dispatch by a durable
outbox row. `PendingBindingId` is a phase-distinct nonzero bytes16 whose same
bytes become BindingId only after a successful spawn:

```text
EffectAttempt = EffectAttemptId || OperationId || u8 EffectKind ||
                EffectTarget || bytes32 effect_digest ||
                u32 payload_len || canonical_payload || u8 EffectState
EffectKind = { SPAWN=1, KILL=2, PROCESS_SIGNAL=3 }
EffectState = { READY=0, DISPATCHED=1, SUCCEEDED=2, REJECTED=3,
                INDETERMINATE=4 }
EffectTarget = u8 tag || body {
  0 PENDING_SPAWN { PendingBindingId, owner_authority_fingerprint:bytes32,
                    owner_incarnation:bytes16, u32 group_len,
                    canonical_L1_GroupId }
  1 ACTIVE_BINDING { TerminalBindingRef }
}
EffectPayload =
  SPAWN(work_session_id:bytes16 || executor_schema:u16 ||
        document_len:u32 || executor_document) |
  KILL(empty) |
  PROCESS_SIGNAL(signal:u8)                  // Interrupt=0, Terminate=1, Kill=2
```

`terminal_group` / `canonical_L1_GroupId` is at most 4 KiB and must name the
group served by the authenticated owner/incarnation. `START_RUN` fields 3–9 map
exactly to SPAWN's PendingBindingId, target, and payload. SPAWN requires target
tag 0; KILL/SIGNAL require tag 1 and an active exact BindingRef.

`effect_digest = SHA-256(ASCII(\"phux-coordinator-effect/v1\\0\") ||
EffectKind:u8 || u32_be(target_len) || canonical_EffectTarget ||
payload_len:u32_be || canonical_payload)`.

A target dedupe adapter accepts `(EffectAttemptId, effect_digest)` and durably
returns the same result for an exact duplicate; changed digest conflicts. A
deduped SPAWN success returns the authenticated owner/incarnation and canonical
ResourceId. The coordinator atomically materializes TerminalBindingRef using
the PendingBindingId bytes, appends BINDING_OPENED, and finalizes START_RUN.
KILL/SIGNAL revalidate the active BindingRef. The coordinator records any target
result and final operation in one transaction. This adapter is internal to the
terminal owner in v0.1 and adds no work frame to L1. If the target cannot provide
dedupe and a crash occurs after DISPATCHED but before a durable target result,
recovery sets INDETERMINATE and MUST NOT redispatch, infer success from a nearby
Terminal, or mint another effect ID.

After a lost reply, clients lookup the same OperationId. They may resend only
the same semantic command/ID after authority continuity. UNKNOWN and
INDETERMINATE never authorize automatic new work. Final bodies retain at least
30 days; pruning first writes TOMBSTONED. Tombstones remain while any state
created by the operation remains, then may be pruned under advertised policy.

## 9. Whole-authority snapshots

v0.1 has exactly one selector: `WholeAuthoritySelector` tag 0. It applies the
principal visibility predicate in §6. `reserve_subscription=true` is legal only
with this selector. No partial or membership-changing subscription exists.

```text
OPEN_SNAPSHOT {                              // 0x20
  1 request_id:u32
  2 authority_guard:AuthorityGuard
  3 selector:WholeAuthoritySelector
  4 reserve_subscription:bool
  5 replace_paused_subscription:optional<SubscriptionId>
}
SNAPSHOT_BEGIN {                             // 0xa0
  1 request_id:u32
  2 snapshot_id:bytes16
  3 authority_guard:AuthorityGuard
  4 base_event_sequence:EventCut
  5 first_cursor:optional<bytes>
  6 subscription_id:optional<bytes16>
  7 lease_expires_at:u64
}
SNAPSHOT_NEXT {                              // 0x21
  1 request_id:u32, 2 snapshot_id:bytes16, 3 cursor:bytes
}
SNAPSHOT_PAGE {                              // 0xa1
  1 request_id:u32, 2 snapshot_id:bytes16, 3 cursor:bytes
  4 page_index:u32, 5 records:list<SnapshotRecord>
  6 next_cursor:optional<bytes>
}
SNAPSHOT_END {                               // 0xa2
  1 request_id:u32, 2 snapshot_id:bytes16, 3 page_count:u32
  4 record_count:u64, 5 canonical_sha256:bytes32
}
SNAPSHOT_APPLIED {                           // 0x22
  1 request_id:u32, 2 snapshot_id:bytes16
  3 subscription_id:optional<bytes16>
  4 initial_credit_count:u32, 5 initial_credit_bytes:u32
}
```

The server charges quota before taking the immutable cut. The lease is exactly
60 seconds, principal/guard/selector-bound, released on disconnect/revocation,
and has at most one NEXT outstanding. Cursors are unpredictable, opaque,
single-successor capabilities of at most 4 KiB. Repeated, foreign, expired,
released, or wrong-guard cursors are `CURSOR_INVALID`; no data is substituted.

Pages apply §6 visibility, contain at most negotiated count/bytes, use contiguous
zero-based page indexes, and contain unique `(ObjectKind,id)` ordered
lexicographically by those bytes. The exact snapshot digest is:

```text
SHA-256(
  ASCII(\"phux-coordinator-snapshot/v1\\0\") ||
  coordinator_id:bytes16 || epoch:u64_be || activation_sequence:u64_be ||
  base_event_cut:u64_be || record_count:u64_be ||
  each(ObjectKind:u16_be || id:bytes16 || revision:u64_be ||
       schema:u16_be || payload_len:u32_be || canonical_payload)
)
```

Zero records hashes this header with `record_count=0`. The client invisibly
stages and verifies guard, indexes, unique keys, totals, and digest before
publication. If BEGIN has no first cursor, END follows BEGIN on the same open
request. Otherwise each NEXT receives one PAGE; a final PAGE with no next cursor
is immediately followed by END with the same request ID, which remains occupied
until END. Nonfinal NEXT ends at PAGE. The OPEN request ends at BEGIN unless the
empty END follows it.

SNAPSHOT_APPLIED supplies the **only** initial credit for a reserved
subscription and receives ACK. COORD_EVENT delivery begins after ACK. If it replaces a
paused subscription, that old subscription closes atomically when BEGIN creates
the new reserved one. Overflow before ACK yields GAP; the snapshot remains a
historical cut, not current state.

## 10. Subscription, GAP, replay, and healing

```text
SUBSCRIBE {                                  // 0x23
  1 request_id:u32, 2 authority_guard:AuthorityGuard
  3 after_event_sequence:EventCut
  4 initial_credit_count:u32, 5 initial_credit_bytes:u32
}
SUBSCRIBED {                                 // 0xa3
  1 request_id:u32, 2 subscription_id:bytes16
  3 next_delivery_sequence:u64
}
COORD_EVENT {                                // 0xb0
  1 subscription_id:bytes16, 2 delivery_sequence:u64
  3 event_sequence:u64, 4 event_id:bytes16, 5 event_kind:u16
  6 object:ObjectRef, 7 object_revision:u64, 8 schema:u16, 9 payload:bytes
}
CREDIT {                                     // 0x24
  1 subscription_id:bytes16, 2 add_count:u32, 3 add_bytes:u32
}
GAP {                                        // 0xb1
  1 subscription_id:bytes16
  2 last_contiguous_delivery:DeliveryCut
  3 last_contiguous_event:EventCut
  4 observed_event:optional<EventSequence>
  5 reason:u8
}
GapReason = { QUEUE_OVERFLOW=0, RETENTION=1, EPOCH_CHANGED=2, SOURCE_GAP=3,
              CURSOR_INVALID=4, REVISION_MISMATCH=5, GRANT_REVOKED=6,
              INTERNAL=7 }
```

States are `RESERVED_SNAPSHOT`, `LIVE`, `PAUSED_GAP`, `REPLAYING`, and `CLOSED`.
SUBSCRIBE creates LIVE and is the only direct-subscription initial-credit
source. SNAPSHOT_APPLIED/ACK moves RESERVED_SNAPSHOT to LIVE. COORD_EVENT debits
one count and its full outer length and sends only when both balances suffice.
CREDIT is legal only in LIVE, uses checked addition, and cannot exceed selected
maxima. Credit acknowledges neither application nor persistence.

DeliverySequence starts at one; client receipt must be exact-next. EventSequence
strictly increases but may skip events hidden by §6. An event applies only when
the visible object's revision is current+1 (absent is zero). Queue/retention/
revision/source/epoch/grant failure emits one GAP through reserved capacity,
discards queued deltas, and moves to PAUSED_GAP. No COORD_EVENT follows until healing.
A bare close is a gap.

```text
OPEN_REPLAY {                                // 0x25
  1 request_id:u32, 2 authority_guard:AuthorityGuard
  3 subscription_id:bytes16, 4 after_event_sequence:EventCut
}
REPLAY_BEGIN {                               // 0xa4
  1 request_id:u32, 2 replay_id:bytes16, 3 subscription_id:bytes16
  4 first_event:optional<EventSequence>, 5 replay_cut:EventCut
  6 first_cursor:optional<bytes>, 7 lease_expires_at:u64
}
ReplayEvent =
  event_sequence:u64 || event_id:bytes16 || event_kind:u16 ||
  object:ObjectRef || object_revision:u64 || schema:u16 ||
  payload_len:u32 || canonical_payload
REPLAY_NEXT {                                // 0x26
  1 request_id:u32, 2 replay_id:bytes16, 3 cursor:bytes
}
REPLAY_PAGE {                                // 0xa5
  1 request_id:u32, 2 replay_id:bytes16, 3 cursor:bytes
  4 page_index:u32, 5 events:list<ReplayEvent>
  6 next_cursor:optional<bytes>
}
REPLAY_END {                                 // 0xa6
  1 request_id:u32, 2 replay_id:bytes16, 3 page_count:u32
  4 event_count:u64, 5 canonical_sha256:bytes32
}
REPLAY_APPLIED {                             // 0x27
  1 request_id:u32, 2 replay_id:bytes16, 3 subscription_id:bytes16
  4 initial_credit_count:u32, 5 initial_credit_bytes:u32
}
```

OPEN_REPLAY is legal only with negotiated EVENT_REPLAY and only against that
principal's PAUSED_GAP subscription. Its `after_event_sequence` MUST equal the
subscription's last contiguous visible EventCut; mismatch is
`MALFORMED_MESSAGE`. The server atomically fixes `replay_cut` while queueing
later visible events. ReplayEvent intentionally has no subscription or delivery
sequence. Insufficient retention returns GAP/RETENTION and leaves the
subscription paused for snapshot replacement. Lease/cursor/page/END request
lifecycle equals §9.

The replay digest is:

```text
SHA-256(
  ASCII(\"phux-coordinator-replay/v1\\0\") ||
  coordinator_id:bytes16 || epoch:u64_be || activation_sequence:u64_be ||
  subscription_id:bytes16 || after_event_cut:u64_be ||
  replay_cut:u64_be || event_count:u64_be || each(ReplayEvent)
)
```

Empty replay hashes the header with count zero. After verified publication,
REPLAY_APPLIED supplies the resumed credit and receives ACK. The same
subscription returns LIVE; next delivery is the prior DeliveryCut+1 and only
events after replay_cut are emitted. Queue overflow during replay sends a new
GAP, invalidates replay, and remains paused. Fresh snapshot replacement is the
only other healing transition. No COORD_HELLO field seeds subscription credit.

## 11. Terminal binding and source admission

```text
TerminalBindingRef =
  BindingId || WorkSessionId || owner_authority_fingerprint:bytes32 ||
  owner_incarnation:bytes16 || resource_id_len:u32 ||
  canonical_L1_ResourceId
TerminalSourceRef =
  BindingId || owner_authority_fingerprint:bytes32 ||
  owner_incarnation:bytes16 || SourceSequence
SourceFactKind = u16 {
  OPENED=1, CLOSED=2, INPUT_RESULT=3, SIGNAL_RESULT=4,
  OUTPUT_RANGE=5, ATTENTION=6
}
TerminalSourceFact =
  TerminalSourceRef || SourceFactKind || schema:u16 ||
  payload_len:u32 || canonical_payload
```

Source fact payloads are exact positional schema 1:

```text
OPENED = empty
CLOSED = optional<i32 exit_status>
INPUT_RESULT = EffectAttemptId || u8 result     // Applied=0, Rejected=1, Unknown=2
SIGNAL_RESULT = EffectAttemptId || u8 result    // Delivered=0, Rejected=1, Unknown=2
OUTPUT_RANGE = u64 first_owner_seq || u64 last_owner_seq ||
               u64 byte_count || bytes32 content_sha256
ATTENTION = u16 kind                            // Bell=1, Blocked=2, Failed=3
```

OUTPUT_RANGE attests order/count/digest only; terminal bytes never enter the
coordinator endpoint.

ResourceId is at most 4 KiB, decoded by the canonical L1 codec, and names a
Terminal-kind resource. Its owner authority/incarnation and ResourceId tuple
identifies the live resource.
Binding records are immutable and append-only; the tuple has at most one active
BindingId. Replacement creates a new binding, and owner loss/restart ends the
old one.

Source admission is available only from the authenticated owner on the active
binding. Sequence starts at one. Exact-next admits fact, advances source
sequence, and appends the coordinator event in one transaction. Repeating any
retained prior sequence with identical kind/schema/canonical payload digest
returns its stored result without a new event. The same sequence with different
content records SOURCE_GAP/Conflict and ends the binding evidence-incomplete. A
jump records the exact missing inclusive range, admits the observed fact, and
advances the source sequence in one transaction; evidence remains explicitly
gapped. A lower non-identical sequence is refused. Every fact after binding end
is refused.

KILL and PROCESS_SIGNAL effects address the exact active BindingId and terminal
tuple; SPAWN addresses only its authenticated PendingSpawn target. The internal
dedupe adapter in §8 checks the applicable target before acting. Interactive
input/output, leases, signals, bootstrap, and history remain terminal-protocol
authority; no coordinator frame tunnels L1 or uses StreamId/BootstrapId as work
identity. No PID/title/cwd/host/ordinal/metadata heuristic adopts a Terminal.

## 12. External activation witness and split brain

The store lock excludes writers only within one image. v0.1 additionally
requires an external activation witness whose state and signing key are not
stored in, backed up with, or clonable from the coordinator store. The witness
holds only `(CoordinatorId, last_authority_epoch, activation_sequence,
active_lease)`. For a new activation it rejects any proposed epoch not greater
than its durable last epoch; renewal accepts only the exact current
epoch/sequence/token/incarnation. It is not a second work coordinator.

```text
ActivationCertificate =
  u8 version(1) || witness_id:bytes16 || coordinator_id:bytes16 ||
  activation_sequence:u64 || authority_epoch:u64 ||
  incarnation_id:bytes16 || fencing_token:bytes32 ||
  issued_at:u64 || expires_at:u64 || witness_key_id:bytes32 ||
  signature:bytes64
```

The Ed25519 signature preimage is exact:

```text
ASCII(\"phux-coordinator-activation/v1\\0\") ||
all certificate fields before signature in the order/width above
```

Witness key/id are pinned outside the store, whose crash-durable monotonic state
MUST NOT roll back. A certificate lasts at most 60 seconds. The witness
atomically grants one unexpired fencing token per CoordinatorId and never grants
the next activation until the old lease expires or the current token holder
submits an authenticated relinquishment containing that exact fencing token;
the witness atomically marks it spent before replying. Renewal retains the same
activation sequence/token/epoch/incarnation; a new writer proposes an epoch
greater than both store and witness and increments activation sequence. The
writer checks an unexpired certificate before every commit and fences itself on
renewal failure. Before the prior certificate expires it sends every connection
a witness-signed COORD_AUTHORITY_RENEWED; clients validate and replace the
certificate or close at expiry. Clients validate signature, pin, times,
coordinator/epoch/incarnation, and token hash before accepting COORD_HELLO_OK.

Activation order is: acquire store lock; validate integrity/schema; mint a fresh
IncarnationId; choose an epoch greater than store and witness; request a witness
certificate over that exact CoordinatorId/epoch/incarnation; durably install the
certificate's epoch, activation sequence, and token hash; recover operations
without unsafe effect redispatch; then bind. Failure occurs before bind.
Restore/copy activation waits for old lease expiry or confirmed token
relinquishment; copying store and authority keys alone cannot activate. Witness
unavailability means coordinator unavailability, not an unfenced fallback.

A client rejects lower epoch/activation, changed CoordinatorId/witness pin,
unapproved authority-key replacement, expired/revoked token, or equal
epoch/activation with another incarnation/token. It quarantines conflicts and
never chooses by clock, event count, reachability, or start time. Higher
authority becomes current only after mTLS authentication, a valid witness
certificate, and a complete snapshot.

One home coordinator owns a lineage. Foreign coordinator records are not merged.
There is no protocol/auth/store downgrade: mismatched major/minor, missing
required capability, non-paired auth mode, newer store schema, or absent witness
refuses. Endpoint absence remains terminal-only compatibility.

## 13. Errors and request completion

```text
COORD_ERROR {                                // 0xc0
  1 request_id:optional<u32>
  2 code:u16
  3 message:str                              // <= 1,024 bytes
}
ErrorCode = {
  VERSION_INCOMPATIBLE=1, FRAME_TOO_LARGE=2, MALFORMED_MESSAGE=3,
  PROTOCOL_ERROR=4, AUTHENTICATION_FAILED=5, CAPABILITY_REQUIRED=6,
  SCOPE_DENIED=7, STALE_AUTHORITY=8, AUTHORITY_FENCED=9,
  OPERATION_CONFLICT=10, REVISION_CONFLICT=11, CURSOR_INVALID=12,
  RESOURCE_EXHAUSTED=13, NOT_FOUND=14, INTERNAL=255
}
```

Message text is diagnostic only and contains no credential, nonce, proof,
secret, environment value, command payload, or Artifact content. Clients branch
on code.

Framing, handshake state, version, authentication, stale/fenced authority,
witness, wrong direction, and unknown frame errors are fatal: send COORD_ERROR
through reserved capacity if possible, then close. Post-COORD_HELLO scope/capability,
revision, operation conflict, cursor, resource, and not-found errors are
request-scoped unless continuity is lost. A bare close makes unfinished
requests/streams indeterminate; clients use lookup or fresh snapshot rules.

Each request ID has one terminal reply. For multi-frame snapshot/replay final
NEXT, END is terminal; PAGE is intermediate. For empty OPEN, END after BEGIN is
terminal; otherwise BEGIN is terminal for OPEN. ACK is terminal for APPLIED.

## 14. Conformance invariants

A conforming implementation proves:

1. Coordinator and L1 frame namespaces reject each other; terminal-only peers
   are byte-for-byte unchanged.
2. Length and type enter fixed storage; connection/type caps are checked before
   payload allocation, and aggregate resources are charged before cuts/queues.
3. Version mismatch precedes authority parsing; mTLS client authentication
   is mandatory on TLS transports; SSH/stdin is unavailable.
4. Missing, unknown, or expired client certificate, wrong CA, expiry,
   noncanonical/unknown scopes, and secret leakage fail closed.
5. A non-clonable external witness grants one unexpired activation token; copied
   stores, concurrent launch, expired leases, unsupported schema, and lock loss
   cannot bind or commit.
6. Restart changes incarnation and advances epoch/activation while preserving
   CoordinatorId, IDs, events, and results.
7. Operation digest vectors cover changed request/epoch retries, reordered and
   duplicate preconditions, absent/empty guards, and unknown extensions.
8. Exact duplicate operations return one result. Changed payload/principal/
   guards conflict. Reply loss at every commit boundary creates no second Run.
9. Delegated effects dedupe by EffectAttemptId at the target; an unprovable
   post-dispatch crash becomes INDETERMINATE and is never redispatched.
10. Only whole-authority scope-filtered snapshots reserve v0.1 subscriptions;
    one predicate governs snapshot/event/replay/lookup visibility.
11. Snapshot/replay empty, one-page, and multi-page vectors pin exact fields,
    END lifecycle, domain-separated digest bytes, and cursor expiry.
12. Direct, snapshot-reserved, and replay-resumed subscriptions have exactly one
    initial-credit source; count and byte credit both gate events.
13. A pre-first-event GAP carries zero DeliveryCut; replay uses ReplayEvent,
    binds the paused subscription, ACK-resumes it, or snapshot replaces it.
14. Per-connection, per-principal, and server-wide quotas isolate stalled or
    reconnecting principals without silent eviction.
15. Source facts require active authenticated binding and exact-next sequence;
    identical duplicate, conflict, jump/gap, and post-end behavior are distinct.
16. Terminal loss/restart ends binding; no resource generation, PID/title/cwd/
    ordinal/metadata guess, StreamId, or BootstrapId can continue work identity.

## 15. Implementation dependency order

1. External witness client/certificate verification, store lock, activation
   fencing, bounded codec, IDs, paired COORD_HELLO, scopes, and visibility predicate.
2. Operation/event store, exact digest, projections, result retention, durable
   effect outbox, target dedupe adapter, and indeterminate recovery.
3. Whole-authority snapshots, leases, aggregate quotas, reserved subscriptions,
   dual credit, GAP, replay, ACK resume, and replacement.
4. Immutable Terminal bindings and exact source-admission state machine.
5. Artifact, Signal, evidence, client, and FFI projections.

No step may add work frames to L1, acknowledge before durable operation
admission, relax aggregate bounds, or emit after an unhealed GAP.

## 16. Coordinator protocol history

| Version | Date | Change |
|---|---|---|
| `0.1.0` | 2026-09-03 | Initial paired-only separate endpoint: bounded allocation/quotas, external activation witness, exact IDs/digests/tags, closed visibility, durable operation/outbox results, whole-authority snapshots, credited events, GAP/replay healing, and terminal bindings. No L1 change. |
