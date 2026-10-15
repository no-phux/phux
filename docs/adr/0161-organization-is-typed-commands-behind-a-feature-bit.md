---
audience: contributors
stability: stable
last-reviewed: 2026-10-13
---

# 0161 — Organization is typed commands behind a feature bit

**TL;DR.** Session, resource, arrangement, and Intake edits are typed
commands on the existing `COMMAND` envelope, advertised by the
`ORGANIZATION` bit in `features_ext`. Each edit carries an operation id, moves
and arrangement replaces carry an expected revision, and each answers `Ok`, `Accepted`, or a typed refusal, and is
journaled with its actor. The intercepted session keys and the TUI layout
key become a translation layer for old clients.

Status: Accepted
Date: 2026-10-13

## Context

Session lifecycle reaches the server through `SET_METADATA` on keys the
spec calls opaque but the server intercepts: `phux.session.create/v1`,
`phux.session.name/v1`, and `phux.session.keep_empty/v1`. `SET_METADATA`
has no reply, so a create is confirmed by reading back a nonce-keyed result
and a refusal is silence. Layout is a whole-value last-write-wins CBOR blob
under `phux.tui.layout/v1/<session-id>`, with no compare-and-swap, so two
clients editing a session's tabs overwrite each other.
[ADR-0056](./0056-cross-session-terminal-move.md) and
[ADR-0050](./0050-explicit-spawn-ownership.md) leave placement geometry to
that client-written key. [ADR-0160](./0160-sessions-own-resources-and-window-is-retired.md)
makes sessions and arrangements typed server records, which need a typed
surface.

## Decision

1. **Commands.** The organization commands are new `Command` variants
   under the existing envelope: `GetOrganization` (paged), `SessionCreate`,
   `SessionRename`, `SessionSetLifetime`, `SessionDelete`,
   `ResourceCreate`, `ResourceMove`, `ResourceMoveMany`, `InboxResolve`,
   `OrganizationUndo`, `ResourceRename`, `ResourceTerminate`,
   `ResourceRemove`, `ResourceRelaunch`, `ArrangementGet`, and
   `ArrangementReplace`, plus `SessionKill` behind its own `SESSION_KILL`
   bit (amendment below). (`ResourceAbandon`, tag `0x2a`, was retired
   unshipped and its tag stays reserved.) Workspace commands are
   reserved for the workspace stage. No frame tag is reused or
   reinterpreted. Byte layouts live in `docs/spec/L1.md` §5.7.
2. **Every mutating command carries an `OperationId`.** Reads
   (`GetOrganization`, `ArrangementGet`) carry none: repeating one has no
   effect to deduplicate. The id is a caller-drawn
   16-byte id with the same shape as the `APPLY_INPUT` and create keys
   ([ADR-0126](./0126-idempotent-create.md)). A retry with the same id and
   payload returns the original result without repeating effects; the
   same id with a different payload is refused as a duplicate.
3. **Structural edits carry an expected `Revision`.** Moves, relaunch,
   Intake resolution (wire `InboxResolve`), and `ArrangementReplace` are
   compare-and-swap: a mismatch is refused as a conflict naming the current revision. Session
   arrangements may reference members only. Renames, lifetime changes,
   removes, and deletes carry none (amendment below).
4. **Results.** A command answers `Ok` with value and revision,
   `Accepted` when execution effects follow as events, or `Refused` with a
   typed reason: unauthorized, not found, conflict, stale, not empty,
   unsupported, invalid argument, name taken, execution uncertain, lease
   held, or duplicate operation. `Refused` and `Accepted` are sent only in
   reply to organization commands; translated legacy writes and older
   commands never receive them, because older decoders reject a frame with
   an unknown result tag.
5. **Events.** Organization changes are new `AgentEvent` variants on the
   journal ([ADR-0123](./0123-events-are-journaled.md)), stamped with the
   actor and operation id. A gap means refetch `GetOrganization`. Terminal
   bytes stay on their own streams.
6. **Feature bit.** `ServerFeatureExt::ORGANIZATION = 0x10` in the trailing
   `features_ext` word ([ADR-0137](./0137-server-feature-word-extends.md)).
   A server sets it only when it enforces this whole surface. A client
   that does not see it treats organization as read-only and says so.
7. **Read compatibility.** `ResourceInfo` gains its session handle in its
   extension block, so a client joins resource to session without a
   window. Legacy `GET_STATE` keeps `SessionSnapshot.windows`, synthesized
   as exactly one window per non-empty session with a stable id, and
   reports `keep_empty` as `lifetime == Persistent`.
8. **Translation layer.** While old clients exist, the server translates
   writes to `phux.session.create/v1`, `phux.session.name/v1`, and
   `phux.session.keep_empty/v1` into `SessionCreate`, `SessionRename`, and
   `SessionSetLifetime`, attributed to the writing client. `keep_empty:
   true` and `empty: true` mean persistent; a bare create means ephemeral.
   The legacy broadcasts continue. `phux.tui.layout/v1/<session-id>` is
   read from and written to the session's arrangement. Named projections
   (`<prefix>.layout/v1/<session>`,
   [ADR-0129](./0129-projections-are-named-by-key.md)) and third-party
   layout keys stay opaque metadata. A legacy destructive operation keeps
   its destructive meaning.
9. **Removal.** The translation layer is deleted once every first-party
   client and the last two released versions speak `ORGANIZATION`.

This supersedes in part ADR-0056 and ADR-0050: placement geometry is the
typed arrangement, not client-written metadata. Grants for each command
are [ADR-0163](./0163-actors-and-delegated-agent-grants.md).

## Why

Typed commands give the caller what metadata cannot: a reply, a reason for
refusal, idempotent retry, and conflict detection. Riding the `COMMAND`
envelope reuses the dispatch, scope classification, and journal stamping
every other command already has, so no new frame family or connection is
needed. A feature bit lets new clients detect the surface without a
version break ([ADR-0061](./0061-capabilities-add-versions-break.md)).
Translating the old keys inside the server, rather than keeping both
models authoritative, means there is one source of truth from the first
release.

## Tradeoffs

- The server carries a translation path and a synthesized legacy snapshot
  until the removal condition holds.
- Old clients still see last-write-wins semantics on the layout key; only
  new clients get conflict refusals.
- `GetOrganization` and the event stream are a second read path beside
  `GET_STATE` for as long as legacy clients exist.

## Amendment: server-side kill, fewer revisions (2026-10-13)

Killing a session was a client workflow: terminate each member, poll until
each was inactive, remove each, then delete the session at a tracked
revision. Every client carried that state machine and its timeouts, and
work that joined meanwhile made the delete refuse. `SessionKill` (tag
`0x37`, gated on `features_ext.SESSION_KILL`) does it on the server in one
step under the legacy kill's committed teardown, work that joined
included, and answers with the member count. Renames, lifetime changes,
removes, and deletes dropped their expected revision: a label is last
writer wins, and a remove or delete is guarded by its own condition
(inactive execution, no members), so the revision only cost a round trip.
A server ignores the field when an older client sends it.

## Alternatives

**Keep metadata, add a reply frame.** A reply alone does not give
revisions, typed refusals, or attribution, and it keeps lifecycle hidden
behind keys the spec calls opaque.

**A separate organization endpoint.** It would duplicate authentication,
scope enforcement, and journaling, and clients would hold two connections
to one server.

**Break old clients.** Released TUIs, mobile, and desktop builds would lose
session create and rename against a new server, which the compatibility
policy does not allow inside one minor version.
