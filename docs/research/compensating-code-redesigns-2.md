# Compensating code: root causes R6 to R13

Part of [Compensating code in phux](./compensating-code.md), research at
`ed579facc7ec`. Paths are relative to the crate named nearby, or to
`crates/phux-server/src/` for bare `state/`, `runtime/`, `hub/` and `upgrade/`
paths. The [cluster index](./compensating-code-index.md) gives a start path
for every cluster.

R1 to R5 are in [part one](./compensating-code-redesigns-1.md).

## R6. The hub collapses every consumer into one identity on one link

**The fact.** A hub terminates each consumer's connection and re-originates
it over a single link. The satellite sees one client. Replies and events on
the link are not ordered against each other.

**What it costs (roughly 6,000 lines, sharing the relay's 2,400 with
R1).** Hub-side shadow ledgers for leases,
attaches, viewer marks, spawn provenance and event attribution
(`state/satellite_spawns.rs`, `state/lease_table.rs`,
`hub/operation_fence.rs`); pending marks and rollbacks around every relayed
command; request-id remapping across six pending maps; a metadata mirror for
two keys; relayed events restamped but not retained, so every cursor on a
satellite scope is owed a gap. In the TUI, a 1,000-line orphan ledger with
TTLs that kills satellite panes whose placement was refused
(`phux-tui/src/attach/driver/orphans.rs`). In the CLI, unreachable
satellites arrive as uncorrelated `ERROR` frames ahead of the reply.

**Redesign: the hub routes; it does not impersonate.** Each consumer gets
its own logical connection to the satellite, carried over the hub's link
with the consumer's identity attached. The satellite then applies its own
leases, subscriptions, dedupe and journal to that consumer as it would to
any client. The hub keeps discovery, admission and routing. Fleet views are
a client-side merge of per-host answers, so a missing host is a typed
per-host result. The repository already has a relay built this way:
`quictun` forwards streams without understanding them.

**What disappears.** Every shadow ledger, the pending marks, id and
request retagging, the operation fence, the metadata mirror, the gap owed on
satellite cursors, and the TUI's orphan ledger (with R9).

**The price.** More logical connections per client. The satellite must
trust the hub's statement of who the consumer is, or the hub must mint a
scoped credential per consumer.

## R7. Agent state is an unowned, last-writer-wins record with five writers

**The fact.** Nothing declares which program occupies a pane or what state
it is in, so identity comes from `tcgetpgrp` plus argv matching and state
from regular expressions over the title and viewport. The result is stored
as a `phux.agent/v1` JSON value that any writer replaces whole. Screen
rules, hooks, an `AgentSession` stream, OSC 7501 and explicit
`SET_METADATA` all write it. A screen that matches no rule reads as `idle`.

**What it costs (about 9,000 lines plus 1,100 lines of rule manifests).**

- Timers that hide the scrape's ambiguity: 3 s startup grace, three idle
  confirmations, a 700 ms hold, three tick cadences, 5 s re-identify
  (`phux-terminal/src/agent_detect/mod.rs:56-107`).
- Rules that silently died when Claude Code changed its busy glyph, caught
  only because two backstops still matched
  (`phux-agent-rules/rules/claude.toml:35-55`). A 351-line explainer
  exists because the `idle` fallback hid dead rules.
- Two rank ladders, four ownership sets shadowing the store, a provenance
  side table because "the bytes alone cannot say who wrote them", and
  `invalidate_published` when the store changes underneath the detector.
- In the integrations, three packages each rebuild show, parse, verify
  ownership, write, with generation fences and exit-time clears
  (`integrations/{pi,omp,opencode-v2}/src/lifecycle.ts`, about 1,200
  lines), plus a `PHUX_AGENT_REPORTING=0` switch for a second reporter.
- `agent wait` cannot trust a level read of `idle`, so it waits for a
  transition. The ask channel is smuggled through the window title.

**Redesign: state is a claim held by its source.** Each source opens a
typed claim bound to the pane's occupant (process group and start time) and
to the source's own lifetime. The published state is the highest-ranked live
claim's last report. Nobody writes the record; it is derived. The screen
detector is the lowest-ranked claimant and reports `unknown` when no rule
matches. A program phux launched has its kind declared at spawn. A question
is a typed event with its text.

**What disappears.** The ladders, ownership sets, provenance table and
invalidation; every integration's ownership protocol and the reporting
switch; exit-time clears (the claim ends with its holder); the title
sentinel; and the reason `agent wait` cannot use a level match.

**What stays.** Screen rules and argv matching for agents with no
integration. That inference is the product's fallback, and its timers are
the price of scraping. Under the redesign it is one small source, not the
foundation.

## R8. The server's existence is a convention on a socket path, and installed artifacts are copies

**The fact.** Any client may start the daemon. Liveness is a connect probe
on a filesystem socket. The launchd plist or systemd unit is a rendered copy
of server configuration. The Claude shim, hook scripts, skill stub and
restore wrapper are written once into user files and not rewritten by a
binary upgrade. Panes inherit `PHUX_*` variables, including server
configuration, and dev builds resolve the same default paths.

**What it costs (about 8,000 lines).** Probe, spawn lock, stale-socket
reap and readiness polling in every verb
(`crates/phux/src/commands/server/spawn.rs`); byte-preserving text patches
to installed units and an "adopt pending" marker because an armed unit and a
stopped one look identical (`commands/service.rs`,
`commands/service/reconcile.rs`); `SHIM_SCHEMA = 6` with per-schema
consequence text kept in two places; a `doctor` verb of about 2,000 lines,
most of whose checks detect a condition that failed silently; dev-versus
production guards by path sniffing, an environment scrubber as a Cargo test
runner, and refusal checks in several crates.

**Redesign: one owner for "running", and installed files with no logic.**
The service manager owns the listening socket and starts the server on first
connect, so there is no probe, lock, reap or readiness poll, and a restart
keeps the socket. The unit is static and parameterless; configuration is
read at run time. Every installed artifact is a one-line trampoline into the
binary (`exec phux agent shim-run claude "$@"`, `phux agent hook claude
EVENT`), so replacing the binary replaces the behaviour. The server strips
its own configuration from the environment it gives panes, which need only
their socket and terminal id. Build profile is a compile-time constant
carried in `HELLO`, and a mismatched profile is refused there.

**What disappears.** The spawn election, unit patching and adoption marker,
schema stamps and stale detection, the two 186-line shell hook scripts
(seven lines differ between them), about half of `doctor`, and the
path-sniffing isolation guards.

**What stays.** Hosts with no user service manager (containers, some SSH
sessions) still need an on-demand start path. That residue is the essential
part of today's code.

## R9. Layout is a client-edited document, and placement is two steps

**The fact.** A split is a spawn followed by a client read-modify-write of
the whole stored arrangement against a revision. Two storage generations
coexist (a legacy metadata key and organization records). Active tab and
"where was I" are re-derived by each client after every attach.

**What it costs (about 5,500 lines).** A read fence, heal logic, deferred
edits and compare-and-swap retries in the TUI (about 1,300); the same shape
again in Rune with a 3 s patience timer and "the layout could not be saved"
(`clients/rune/src/desk/splits.rs`); a separate, divergent implementation in
the web client that does not read the arrangement at all; key space versus
handle space conversion in the client core; the server-side translation
layer. Rune and the web client each implement the Intake, most recent, then
create ladder.

**Redesign: arrangement edits are server operations.** `split(entry,
direction, ratio, launch)`, `close`, `move`, `swap`. The server applies each
atomically with the launch intent, which is what the public terminal
workflow already commits. Clients receive the arrangement through the same
snapshot and event stream as everything else. A client's last place is a
small per-credential view record on the server.

**What disappears.** The fences, heals, CAS loops, placement orphans and
the legacy layout key, and the web and Rune clients stop diverging.

## R10. Failures cross boundaries as prose

**The fact.** The wire has typed error codes, but they are flattened to
text at the dial boundary, at the CLI's stderr and in the runtime's display
strings.

**What it costs (about 800 lines, spread thin).**
`is_fatal_refusal_detail` matches `"401"`, `"403"` and two phrases
(`phux-client-runtime/src/reconnect.rs:86-106`). `phux upgrade` chooses its
retry by substrings of the refusal text. Rune classifies by `contains`
on lowercase messages, with the note "a fingerprint's digits can spell one".
The integrations use regular expressions such as `/server is upgrading/`
and `not found|no such|does not exist`. `doctor` classifies a firewall drop
by `contains("refused")`.

**Redesign: an error is data everywhere.** One shape, `{code, retry:
never | after | reconcile, detail}`, preserved through runtime, FFI, CLI
JSON and generated TypeScript. Text is for people.

## R11. Integrations and the desktop client drive phux by spawning the CLI

**The fact.** The npm integrations may only run the `phux` binary, one
process per verb. Rune, which already links the client runtime, shells out
to the CLI for host management, onboarding, colours and permissions.
`crates/phux` is documented as "argument parsing and dispatch" and holds
about 107,000 lines under `src`, in-source tests included.

**What it costs (about 6,000 lines).** A spawn governor with process-group
kill and a stdin lifeline; a double deadline repeated in five places; about
1,000 lines of hand-written validators mirroring Rust shapes; text-protocol
parsing and exit codes cross-checked against documents; a 1 s cache whose
only purpose is amortising spawn cost; a pane view rebuilt from long-poll
plus HTML scrape; version probes on every hook; in Rune, binary lookup,
20 ms `try_wait` polls and loose JSON parsers.

**Redesign: one long-lived typed session per integration.** The binary
still owns the wire. An integration starts one `phux` child and talks to it
over stdio with generated types (the MCP server is already this shape). It
gets notifications instead of polling, one handshake instead of per-call
probes, and typed errors (R10). Verb logic moves from the binary crate into
the client library, and Rune calls it directly.

**What stays.** Shell-hook harnesses that offer only command hooks still
get one process per event. With R8 that process is a single `phux agent
hook` call.

## R12. Tooling restates facts that exist once elsewhere

**The fact.** Several independent decisions each create a second copy of
something and then a gate to compare the copies.

**What it costs (about 16,000 lines of scripts, TypeScript and workflow).**

- Two first-class toolchain paths (Nix and mise) plus native and container
  CI. Every tool version is a three-to-six-copy fact, with a sync check, a
  parity check, digest tables and a doctor. Copies with no check exist
  (`plugin-index.yml:49`, `npm@12.2.1` in four workflow sites).
- CI routed by a TypeScript classifier that guesses what to run from
  changed paths, re-derives Cargo's dependency closure, and has tests that
  keep `ci.yml`, `plan.ts` and the justfiles in step (about 2,600 lines
  with tests), beside a content-addressed build cache that already knows
  what changed.
- Generated artifacts committed and then byte-compared: reference pages,
  `wire-schema.json`, `scripts/install.sh`, the wasm engine, integration
  bundles, release manifest fixtures.
- Tests that race real schedulers against shipped millisecond budgets, so
  there is a nextest override file with nine measured-starvation essays, a
  quarantine list and a report tool.
- Five `Cargo.lock` files and several package manifests sharing one
  version.
- Release state reconstructed by polling GitHub because Actions events are
  lossy.
- A worktree lease because two agent sessions sharing a checkout produced
  results that described the other session's commit.

**Redesigns.** Pick one toolchain source and have the other read it or
drop it. Run everything and let the content-addressed cache skip what is
unchanged, which needs test results to be cacheable actions there. Generate
artifacts at build or publish time instead of committing them (R4 removes
most). Give tests a virtual clock and let them wait on journal sequence (R2)
instead of on silence. Put the standalone workspaces under one lock. Drive a
release from one workflow run. Stamp each build with the tree hash it built
and compare, which needs no ownership.

## R13. Gaps in dependencies are patched around instead of in

**The fact.** Several dependencies are forks or partly replaced already.

**What it costs (about 9,000 lines).**

- libghostty (consumed from a fork): snapshot takes the terminal by value,
  dirty bits have one reader, `pwd` is never populated from OSC 7, the
  OSC 133;D exit status is not retained, there is no title-change
  notification. Each has a workaround cluster in `phux-terminal`.
- portable-pty: half replaced. A whole crate supplies the missing adopt
  constructor, there is an own `posix_spawn` path, a reverse map from
  signal names back to numbers built by probing the library, and fabricated
  error text so another function can strip it
  (`phux-terminal/src/actor/fd_shrink.rs:64-80`).
- wtransport: a hand-rolled HTTP/3 accept and QPACK scanner to see
  duplicate `Authorization` headers.
- The Rune UI framework (unreleased, pinned by git revision): a drawn
  keyboard, fixed safe-area insets, app-timed gestures, a UITextInput
  bridge with token and revision fences, lost-release repair, decode
  admission, and a twice-daily agent that chases upstream. About 5,000
  lines.

**Redesign.** Fix the fork where the fork is yours: a borrowing snapshot,
row version stamps, change callbacks, retained exit status. Replace
portable-pty with a small PTY module that uses `pidfd` and `kqueue` process
events, which also removes exit polling. On the phone, the native shell
route already documented in `docs/embedding.md` (UIKit owns chrome, text
input and gestures; the runtime supplies the draw list) removes most of the
Rune stand-ins.
