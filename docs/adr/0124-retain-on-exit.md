---
audience: contributors
stability: stable
last-reviewed: 2026-09-14
---

# 0124 — Retain on exit: exit is a facet, close is a purge

**TL;DR.** A spawner may ask the server to keep a Terminal after its process
exits. The resource stays in the inventory as `Exited`, with its exit status,
signal, and times, answers reads, and refuses input. It closes with the
ordinary `RESOURCE_CLOSED` when retention expires, a bound evicts it, or a
kill purges it. Retention is opt-in per spawn and off by default.

Status: Accepted
Date: 2026-09-14

## Context

When a PTY reaches EOF the server reaps the child and, under the same lock,
removes the pane and retires its wire id. The exit status goes into
`RESOURCE_CLOSED`, the `pane_closed` event, and the `pane_exit` hook, and is
kept nowhere. A waiter that arrives late gets `TERMINAL_NOT_FOUND` and cannot
tell "exited 0" from "never existed". Signal deaths are flattened to an absent
status. `ResourceLifecycle::Exited` and `ControlAction::Exited` are defined on
the wire and never sent. An agent that runs a job in a pane and reads the
result afterwards has no way to do so without racing the exit.

## Decision

1. **Opt in per spawn.** `SPAWN_RESOURCE` gains field 16 `retain_secs:
   optional<u32>`. Absent is today's behavior byte for byte. `0` asks for
   the server default, `defaults.retain-on-exit-secs` (600); any value is
   capped by `defaults.retain-on-exit-max-secs` (86,400). Server config
   `defaults.retain-on-exit` (bool, default false) makes retention the
   default for spawns that omit the field. The field is Terminal-only.
2. **Exit is a facet.** When a retained Terminal's process exits, the server
   reaps it (no OS zombie), keeps the engine with its grid and history, sets
   `lifecycle = Exited`, records `ExitFacet { exit_status, signal, reason,
   exited_at_ms, retained_until_ms }`, and emits `terminal_control
   { lifecycle: Exited, action: Exited }`. It does not emit
   `RESOURCE_CLOSED`.
3. **Inspection.** `ResourceInfo` gains `lifecycle`, `exit`, and
   `input_holder`, carried in a new snapshot extension block because the
   facet row is positional. `GET_SCREEN`, `GET_TERMINAL_STATE`, `HISTORY_*`,
   and `ATTACH_RESOURCE` keep working. Input answers `INPUT_NOT_WRITTEN`,
   `SIGNAL_TERMINAL` answers `INVALID_COMMAND`, and `RESIZE_TERMINAL` is a
   no-op.
4. **Close is the purge.** The resource closes with `RESOURCE_CLOSED
   { reason: EXITED }` at expiry, or when the retained set exceeds
   `defaults.retain-on-exit-max` (256, oldest first). `KILL_RESOURCE` and
   `KILL_RESOURCES` purge it at once with `reason: KILLED`, idempotently; the
   kill authority is the cleanup authority. `AgentSession` children cascade
   at the purge, not at the process exit.
5. **Signals are reported.** `RESOURCE_CLOSED` gains field 4 `signal:
   optional<i32>`, and the server stops flattening signal deaths, for
   retained and non-retained Terminals alike.
6. **Upgrade.** A graceful upgrade does not carry retained resources; the new
   image reports each as closed with `reason: SERVER_SHUTDOWN`.
7. **Gate.** `ServerFeature::RETAIN_ON_EXIT = 0x02000000`.
8. **Consumer surface.** This amends the ADR-0071 freeze: `phux spawn
   --retain[=SECS]` requests retention, and the new noun `phux resource`
   gains `show TARGET` (MCP `phux_resource_show`), which reports a resource's
   lifecycle and exit facet. Its `wait` verb is ADR-0123's and its `methods`
   verb is the kind catalog's; `phux resource wait` answers `exited` from a
   retained exit.

The normative text is `docs/spec/L1.md` §1.1, §3.1, and §9.1.

## Why

- **One lifecycle per id.** Exit state lives in the registry beside the live
  descriptor, and the resource stays there until the purge. `GET_STATE`
  remains the single inspection surface and the wire id stays valid. A side
  table of exited resources would give one id two lifecycles, which is how
  zombies happen.
- **Consumers already handle the purge.** `RESOURCE_CLOSED` is still the one
  frame that ends a resource, so no consumer learns a new ending.
- **Bounded by construction.** A time bound, a count bound, and an explicit
  purge authority cap what retention can hold.
- **Opt-in keeps every client unchanged.** Turned on everywhere, the TUI would
  show a dead pane for every exited shell. Cockpit's rule that an ended shell
  closes its pane holds because Cockpit never sets the field.
- **The extension block ends positional growth.** A per-row suffix on the
  facet list would be misread by an older decoder as the next row; a
  field-tagged block lets later snapshot facts arrive without a sixth list.

## Tradeoffs

- A retained Terminal holds its grid and history until purged, up to the
  count bound.
- Once anyone retains, the TUI must render a retained pane: its last grid, an
  exited mark, input refused.
- An upgrade drops retained resources; a waiter then reads `gone`.
- Without retention a late waiter still gets `gone`, which is honest but not
  an exit status.

## Alternatives

- **Retention as L3 metadata.** Rejected: metadata is dropped with the
  Terminal and does not answer inventory reads.
- **A separate list of exited resources.** Rejected: two lifecycles for one
  id, and a second place every consumer must look.
- **A wider `ExitStatus` union on `RESOURCE_CLOSED`.** Rejected: an additive
  `signal` field is the compact form and leaves `exit_status` untouched.
- **Retain by default.** Rejected: dead panes everywhere, and memory held for
  every exited shell.
