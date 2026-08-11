---
audience: contributors
stability: stable
last-reviewed: 2026-08-09
---

# 0071 — What phux 1.0 commits to

**TL;DR.** 1.0 is a promise about the *consumer surface* — CLI grammar, exit
codes, `--json` documents, config schema, action/hook/widget vocabulary, MCP
tools, file locations — under semver with the existing deprecation cycle. It
is explicitly **not** a promise about the wire, which keeps its own `0.x` line
under ADR-0061. The compatibility unit is the release, not the frame.

Status: Proposed
Date: 2026-08-07

## Context

phux ships at `0.12.1` with mature release machinery — release-please owns the
tag and changelog, `just release-preflight` proves local coherence, an interop
gate and a Homebrew tap follow — and no written statement of what a major
version would mean. The README's Status section names surfaces as "still
pre-1.0" without saying what 1.0 would change about them.

Two version axes already exist and are routinely conflated. The workspace
version (`0.12.1`) tracks the binary. `PROTOCOL_VERSION` (`0.7.0`, ADR-0070)
tracks the wire, and [ADR-0061](./0061-capabilities-add-versions-break.md)
already declares that a wire `minor` bump is a fleet-wide break with no grace
window: a `0.7.0` client cannot talk to a `0.6.0` server at all, for anyone,
for any reason. That is deliberate — a silently half-compatible peer is worse
than a loud refusal — but it means the wire cannot honestly carry a 1.0
compatibility promise while the ADR-0070 client kernel, history lanes, and
frontend adapters are mid-migration.

Meanwhile the surfaces consumers actually script against are stable in
practice and have been for several releases: the CLI grammar is pinned by
ADR-0065 and a generated reference, `--json` documents carry their own
`schema_version`, the validation vocabulary has one source of truth, and
deprecated spellings already warn on a published removal schedule. Those
surfaces have earned a promise and are not getting one.

Some cleanups are free before a 1.0 and expensive after it: the deprecated CLI
spellings whose published removal window (`v0.12.0`) has passed, the
ADR-0031 policy types that are `pub` in the publishable `phux-protocol` crate
but that no wire path constructs, and wire shapes the spec allocates but
production never builds (the ADR-0030 demotion cascade).

## Decision

1. **1.0 freezes the consumer surface**, enumerated: the CLI grammar and its
   exit codes, every `--json` document (additive-only within a major, each
   carrying its `schema_version`), the config file schema, the
   action/hook/widget vocabulary, the MCP tool names and arguments, and the
   documented file and socket locations. Breaking any of them requires a major
   bump plus the deprecation cycle already implemented in
   `crates/phux/src/deprecations.rs`.

2. **1.0 does not freeze the wire.** `PROTOCOL_VERSION` keeps its own `0.x`
   line under ADR-0061's semantics, and `phux-protocol` stays `0.x` on
   crates.io. A 1.0 binary may ship a `0.8` wire.

3. **The compatibility unit is the release.** One deployment — server, local
   clients, satellites, relays — runs one release. Two consequences are in
   1.0 scope rather than optional: a one-command update path so a lockstep
   fleet is upgradable in practice, and an unbypassable HELLO version gate so
   a mismatched peer fails loudly at the handshake instead of part-way through
   a session.

4. **Anything that becomes a breaking change after 1.0 is resolved before
   it.** Two on the surfaces this ADR opened with — the overdue
   deprecated-spelling removals, and dead `pub` surface in the published
   `phux-protocol` crate — plus the agent-surface preconditions in point 7. Wire shapes the spec allocates but production never
   constructs — the ADR-0030 demotion cascade — stay tracked rather than
   blocking, precisely because point 2 leaves the wire unfrozen; what 1.0 owes
   there is that `docs/CONCEPTS.md` and the wire agree, not that the cascade
   has run.

5. **What 1.0 explicitly does not include** is named, so its absence is not
   read as a regression: the ADR-0070 native engine-state program beyond the
   protocol layer that already shipped, the native GUI, Blackbird workload
   authentication, per-consumer federation sub-identities, and the residual
   Mosh-SSP loss bound.

6. **The agent surface is inside the freeze, enumerated rather than assumed.**
   Point 1 named categories; the agent verbs grew faster than the categories
   were read, so they are listed. **Verbs and flags:** `agent list` / `show` /
   `explain` (`--file`, `--kind`, `--title`, `--format`) / `set` (`--name`,
   `--kind`, `--state`, `--attention`, `--session`) / `clear` /
   `install-claude`; `agent wait
   [TARGET] --until STATE... --timeout SECS --json`, `--until` spelling exactly
   `idle|working|blocked|done` with `unknown` deliberately unspellable and
   `idle,blocked,done` the default set; `agent send-keys TARGET KEYS...
   --expect-agent --expect-kind --json`; `agent prompt TARGET TEXT`
   (`--expect-agent`, `--expect-kind`, `--wait`, repeatable `--until`,
   `--timeout`, `--json`); `agent answer TARGET --id ID` with exactly one of
   `--choice` or `--text` and the explicit `--allow-unlisted` override; `agent
   start NAME --kind KIND --target TARGET` (`--integration`, `--timeout`,
   `--no-wait`, `--force`, trailing argv, `--json`); `agent install-claude`
   and `agent uninstall-claude`; `spawn`; `launch`; `%name` selectors over
   explicit names only and `#tag` selectors over pane tags; `watch --until
   EVENT --timeout SECS`; `worktree new --json`; `skill`; `snapshot --tail[=N]`
   and `--unwrap` with `--rendered`'s
   conflict set; `wait --regex`, `--tail[=N]`, `--output-only`. **`--json`
   documents:** `AgentExplainJson` (v1, `capture` +
   `explain`); the `agent wait` document (v1: `terminal`, `satisfied`, `edge`
   as `{from,to,via}`, `baseline`, `state`, `agent`, `observations` as
   `{edges,pushes,polls}`, `detection`); the `agent send-keys` document (v1:
   `terminal`, `agent`, `keys`, `verified`, `delivery`, `operation_id`,
   `attempts`); the `agent prompt` receipt (v1: `terminal`, `delivery`,
   `operation_id`, `agent`, `pre_submit_state`, nullable
   `staleness_bound_ms`, `attempts`, `submit_ms`, `transition_observed`,
   nullable `matched_by`, `edge`, `waited_ms`, `degraded_to_polling`); the
   `agent answer` receipt (v1: `terminal`, `ask`, `answer`, `source`,
   `operation_id`, `delivered`); the `agent start` result (v1: `terminal`,
   `name`, `kind`, `integration`, `started`, `ready`, and readiness provenance
   when waiting); the worktree binding (v1: `branch`, `path`, `session`,
   `terminal_id`); `ScreenState`'s additive
   `soft_wrap {lines, scrollback}`, `truncated`, `truncated_reason`, `title`,
   held at `SCHEMA_VERSION` 3 and probed by presence, per
   [ADR-0077](./0077-agent-read-surface.md). **Event vocabulary:** the `watch`
   stream's `agent_state` event name and its `name` / `kind` / `session` /
   `state` / `attention` / `from` fields, including a present-and-null `state`
   as the tombstone — the stream carries no `schema_version` by design, so the
   event-name vocabulary *is* the contract. `watch` also freezes the closed
   gate names `agent_state`, `asked`, `bell`, `command_finished`,
   `command_started`, `dirty`, `idle`, `pane_closed`, `pane_spawned`,
   `title_changed`, `unknown`. **Error codes:**
   `capture_unreadable`, `capture_invalid`, `unknown_agent_kind`,
   `no_agent_record`, `agent_departed`, `agent_mismatch`, `invalid_key_spec`,
   the acknowledged-input, ask-validation, agent-start, and watch families
   enumerated in `commands::json_err::codes`.
   **Exit-code semantics, both load-bearing:** `3` keeps the published
   partial-view meaning (the target may exist behind an unreachable satellite,
   so a retry is correct); and `agent wait` / `agent send-keys` use the shared
   resolver, so a partial-fleet miss is `1` carrying `partial_view` in
   `error.code` rather than the `3` the rest of the `agent` family spends.

7. **Three preconditions the freeze does not survive without.** (a) Every
   stable error code lives in `commands::json_err::codes`, matching the closed
   single-file vocabulary advertised to consumers. (b) The former `phux_agent`
   action multiplexer is frozen as ten distinct tools: `phux_agent_list`,
   `show`, `explain`, `set`, `clear`, `wait`, `send_keys`, `prompt`, `answer`,
   and `start` (each with the full prefix). (c) Anything ratified out
   of [ADR-0075](./0075-agent-name-addressing.md) (the `%` sigil),
   [ADR-0076](./0076-agent-prompt-and-lifecycle-wait.md) (`agent prompt`, its
   `--wait`, and its receipt document) and
   [ADR-0078](./0078-alternate-screen-history.md) (`snapshot --transcript` and
   its `transcript` payload) is carved in by amending point 6 in the same PR
   that ships it. No agent verb is discovered post-freeze.

## Why

The surfaces a 1.0 should protect are the ones other people's scripts and
agents depend on, and for phux those are the CLI and its JSON — not the
frames. An agent driving `phux run --json` cares that the document shape
survives an upgrade; it never sees a discriminant. Freezing the consumer
surface buys the whole benefit of a 1.0 for the people who have one.

Freezing the wire at the same moment would buy nothing and cost honesty.
ADR-0070 retired `TERMINAL_SNAPSHOT` outright and the client kernel that
consumes the new bootstrap lifecycle is still being migrated onto its
frontends; a 1.0 stamp on that would be a promise made during a move.

Keeping both axes at `0.x` until the wire settles is the option that looks
safest and is not: it withholds a promise phux can already keep, on surfaces
that have not broken in months, from exactly the consumers most likely to
automate against them.

## Tradeoffs

A 1.0 product on a `0.x` wire will surprise anyone who reads a version as one
number, and "your 1.0 client cannot talk to my 1.0 server" is a genuinely
confusing sentence. The mitigation is that the mismatch is never silent —
the HELLO gate refuses and names both versions — and that the release, not
the frame, is what a user is told to match.

Choosing the CLI and JSON as the frozen surface makes CLI mistakes expensive:
a badly named flag survives to the next major. That cost is real and is the
point; it is what the generated reference and the deprecation table exist to
make visible before the freeze rather than after.

Naming out-of-scope work in an ADR risks reading as abandonment. It is the
opposite: the native program is the largest thing phux is building, and 1.0 is
deliberately not blocked on it.

## Alternatives

**Freeze the wire at 1.0 too.** One version, one promise, no explaining. It
would require either stopping the ADR-0070 migration at its current line or
committing to compatibility shims that ADR-0061 exists to refuse. Rejected as
a promise phux cannot currently keep.

**Stay `0.x` until the wire settles.** Defensible and self-consistent, but it
withholds a commitment on surfaces that are already stable and already being
scripted against, and it leaves the pre-1.0 cleanups (dead `pub` types,
overdue deprecations) with no forcing function.

**Split release trains** — version the CLI and the protocol crate
independently with separate tags and changelogs. This is the fully honest
shape and is what the two axes already imply, but it doubles the release
machinery for one repository whose crates are published as a single tag.
Deferred: the two version numbers are documented as independent, which
captures most of the benefit without the second train.
