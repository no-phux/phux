---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-09
---

# Cockpit shared workspace implementation

**TL;DR.** Keep shared workspace decoding and mutation in Rust, project bounded
typed snapshots through the native C ABI, and reconcile Cockpit by stable
identity. Reuse existing GET_STATE and layout metadata operations on the current
protocol. Separate catalog membership, live replicas, and client-local focus.
Validate with wire-level interleaving tests and two clients on an isolated real
server before testing the user's server read-only.

## Context

Behavior is specified in [PRODUCT.md](PRODUCT.md). Investigation baseline:
`3b01d691`; implementation rebased onto dependency upgrade `6bf51aff`, using
Rust 1.98.1 and Native's Cockpit v0.10.1 lineage.

- `crates/phux-client-ffi/src/lib.rs::apply_attached` retains session summaries
  and selected-session replica IDs, discarding registry ownership information.
- `clients/cockpit/src/providers/phux/host.zig::drainReadiness` copies session
  summaries only at the initial attach barrier. Palette refresh enumerates this
  cache rather than asking the server.
- `Model.admitAndSelectCurrentRemoteTerminal` admits `refs[0]`; Cockpit's saved
  composition is presently independent from Phux's shared layout.
- `crates/phux-client-core/src/layout` owns the v1/v2 CBOR workspace decoder.
  Its `WindowState` lacks shared window identity. Registry `WindowId` is a
  different identity and must not be substituted for it.
- `crates/phux-client/src/layout_ops.rs` reads/writes the per-session
  `phux.tui.layout/v1/<session>` metadata key with whole-value last-write-wins
  semantics. The TUI already subscribes to this same authority.

## Proposed changes

### Rust workspace authority

Require stable window identity in the current shared layout schema and retain
it across topology edits. This is a pre-1.0 cutover: readers and writers use one
schema, with no identity-omission migration or overlap heuristics. Unsupported
stored values produce an explicit unavailable state without overwriting them.
Confirmed absence of metadata on a fresh session derives its initial windows
from the authoritative roster; absence is distinct from an explicitly empty
composition.

The FFI retains a bounded whole-server roster independently from terminal
emulators. The actual attached session is separate from GET_STATE's server-wide
recent-focus fields. Correlated refresh combines registry discovery with the
selected session's layout metadata. The host schedules bounded periodic refresh;
one pending request prevents unbounded accumulation.

Expose sized/versioned catalog, workspace-window, and flattened split-node
records. Getters borrow immutable published storage until the next mutation.
Validate capacity and topology before publication. A malformed replacement
preserves the last-good snapshot with an observable refusal state.

Typed mutations carry expected snapshot revision, captured session, and stable
target identity. They update the Rust workspace, issue existing SET_METADATA,
and confirm the winning value with GET_METADATA. This is not a new CAS protocol.
Refresh/mutation IDs share collision checks with existing terminal operations;
interleaved terminal output and operation replies retain their original owner.

### Cockpit projection

The provider publishes roster/workspace revision changes even when no terminal
replica changes. Catalog browsing allocates no emulator. An externally added
layout leaf requests a bounded subscription before it gains live input authority.

Reconcile shared windows into native tab trees atomically. Preserve stable
window IDs, surviving focused terminal IDs, and local native-window placement.
Store per-session local selection separately from shared topology. Session
activation clears the old session's active projection and selects the exact
requested destination after attach; it does not blindly select `refs[0]`.

Existing durable spawn correlation remains responsible for creation acceptance
and live publication. Its captured placement becomes a typed shared mutation.
Remote topology is refreshed from Phux, not restored as an authoritative copy
from Cockpit's local persistence. Local scratch follows its existing path.

### SDK provenance lane

Independently repair Native's lost fragment provenance through Cockpit's
composed Zig/markup view. Preserve authored file/hash/span and widget identity;
verify source write-back and last-good malformed reload in primary/secondary
windows. Upgrade the pinned owned fork only after scoped SDK tests and review.

## Testing and validation

- Product 1, 2, 7, 11: Rust identity/codec and unsupported-schema tests, bounded topology
  projection, mutation confirmation, and TUI/client round trips.
- Product 3, 4, 10: FFI frame-driven discovery and operation interleaving;
  same request ID, stale session, late response, removal, and capacity cases.
- Product 5, 6, 12-16: native projection tests for exact A-B-A destinations,
  surviving local focus, detached native windows, incarnation fences, malformed
  snapshots, replacement-replica capacity, and local/remote separation.
- Product 8, 9, 17: existing Cockpit tests plus live two-client create/split/
  reorder/resize/close/reconnect with server metadata as the topology oracle.
- Historical bug regressions must fail on actual baseline behavior before the
  fix. New API contracts need meaningful invariants, not invented build failures.
- Use project-scoped doctor, Rust formatting/clippy/tests and C ABI checks,
  then `just cockpit-test` with Phux compiled, source-root verdict verified.
  Expand to full shared-input CI and real-server tests before merge.
- Live automation is serial, publisher-PID bound. CPU captures prove structure,
  not AppKit glyph fidelity. Never drive terminal input into the user's real work.

## Work ownership

The parent owns Cockpit projection and final integration in
`.worktrees/cockpit-workspace`. The Rust writer owns core/FFI and mechanical
Rust call-site migration in `.worktrees/cockpit-workspace-rust`. The SDK writer
uses its own Native-fork worktree. Blackbird coordinates shared paths and ABI
handoffs. Each writer commits locally; independent review precedes parent
acceptance. Phux lands as a linear reviewed PR after the SDK dependency is
available from the owned fork.
