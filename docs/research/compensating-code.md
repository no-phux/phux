# Compensating code in phux

Research, 11 November 2026. Repository examined:
`ed579facc72f692d3779da96f97d5d08a5cac8de` (v0.138.0). This is a research
result. It is not a plan of record and it changes no live contract; the
[decisions](../decisions.md) still stand until one is changed on purpose.

"Compensating code" here means code that does not implement the domain but
exists because of a deficiency elsewhere: a design choice, a missing
invariant, duplicated truth, an ordering the design permits, an upstream gap,
or a fact nobody declares so it must be inferred. The test is counterfactual:
under a different design, would this code have been written at all?

## Result

About 170 clusters of compensating code trace to thirteen design facts. The
rough estimates sum to about 150,000 lines, with double counting where two
areas describe the same seam. Read that as an order of magnitude: well over
100,000 lines of a repository of roughly 870,000 including tests.

| # | Design fact | Redesign | Rough lines |
| --- | --- | --- | --- |
| [R1](./compensating-code-redesigns-1.md#r1-terminal-content-is-replicated-by-running-a-second-emulator-on-a-forwarded-byte-stream) | Clients run a second emulator on forwarded PTY bytes; any gap means a full resync | Ship versioned state diffs against an acknowledged version | 17,000 |
| [R2](./compensating-code-redesigns-1.md#r2-several-copies-of-server-truth-joined-by-mirroring-with-no-single-ordered-history) | Store, memory, engines and wire are separately timed copies; reads carry no version; some writes have no reply | One ordered journal for every fact; snapshots and replies carry a sequence | 15,000 |
| [R3](./compensating-code-redesigns-1.md#r3-three-identities-per-entity-and-wire-ids-that-restart) | Three ids per entity; `u32` wire ids restart and are reused | The durable key is the identity everywhere | 4,000 |
| [R4](./compensating-code-redesigns-1.md#r4-the-wire-has-no-single-declaration) | Hand-written codec with the frame set restated about twelve times; permanent feature bits; metadata used as RPC | One schema declaration; every request answered; a support floor | 18,000 |
| [R5](./compensating-code-redesigns-1.md#r5-one-thread-runs-everything-and-upgrade-is-an-in-place-re-exec-of-that-process) | One thread hosts everything; upgrade re-execs that process | A terminal host per pane; the control plane owns no terminal | 10,000 |
| [R6](./compensating-code-redesigns-2.md#r6-the-hub-collapses-every-consumer-into-one-identity-on-one-link) | The hub collapses all consumers into one identity on one link | The hub routes per-consumer logical connections | 6,000 |
| [R7](./compensating-code-redesigns-2.md#r7-agent-state-is-an-unowned-last-writer-wins-record-with-five-writers) | Agent state is a last-writer-wins record with five writers | State is a claim held by its source; the record is derived | 9,000 |
| [R8](./compensating-code-redesigns-2.md#r8-the-servers-existence-is-a-convention-on-a-socket-path-and-installed-artifacts-are-copies) | Any client starts the daemon; installed shims, hooks and units are copies | The service manager owns the socket; installed files are trampolines | 8,000 |
| [R9](./compensating-code-redesigns-2.md#r9-layout-is-a-client-edited-document-and-placement-is-two-steps) | A split is a spawn plus a client rewrite of the arrangement | Arrangement edits are atomic server operations | 5,500 |
| [R10](./compensating-code-redesigns-2.md#r10-failures-cross-boundaries-as-prose) | Typed errors are flattened to prose and re-parsed | Errors stay data across every boundary | 800 |
| [R11](./compensating-code-redesigns-2.md#r11-integrations-and-the-desktop-client-drive-phux-by-spawning-the-cli) | Integrations and Rune spawn the CLI per verb | One long-lived typed session; verb logic in the client library | 6,000 |
| [R12](./compensating-code-redesigns-2.md#r12-tooling-restates-facts-that-exist-once-elsewhere) | Tooling keeps second copies of facts and gates to compare them | One toolchain source; cache, not a path classifier; generate, not commit | 16,000 |
| [R13](./compensating-code-redesigns-2.md#r13-gaps-in-dependencies-are-patched-around-instead-of-in) | Dependency gaps are patched around | Fix the forks; replace portable-pty; native shell on the phone | 9,000 |

The [cluster index](./compensating-code-index.md) lists every cluster with a
start path.

R1, R3 and R4 each break the wire. R5 has a side benefit: a control-plane
crash would stop ending every program in every pane. Two pieces stay under
any redesign: screen rules as the fallback for agents with no integration,
and an on-demand start path for hosts with no user service manager.

## Method and limits

- Ten read-only explorers swept the repository by area. Each returned
  clusters of sites grouped by root cause, with a path, a line and the
  comment or code that states why the site exists.
- The architecture, decisions, organization, embedding and concepts docs
  were read first. About 50 cited sites across all ten areas were checked
  against the code, and every one matched. The remaining citations are
  unchecked.
- Line counts are rough, mostly non-test estimates.
- "All" is not literal. `phux-client` and `phux-client-ffi` (about 88,000
  lines) were covered by marker sweep plus targeted reads. Parts of
  `phux-server/src/runtime` (`service_http`, `directory`, `path_search`,
  `download`), the Rune iOS tooling and most test bodies were skimmed.
- Nothing was built or run.

Commit history agrees with the inventory. Of 3,793 commits, 1,125 are fixes.
The server has the most (176). Recurring subjects are replay, bootstrap and
replica (49), stuck, hang and stall (46), restore and resume (41), resize and
geometry (34), upgrade and handoff (28).

## What can be deleted now, with no redesign

These are dead or duplicated at the examined commit. Each needs its own
check before removal.

1. **Fallbacks for feature bits every 1.x server sets.** Wire 1.0 first
   shipped in v0.111.0, and a major mismatch is refused at `HELLO`. That
   release already advertised every word-0 bit except `QUIC_STREAMS`, and 13
   extension bits. Only seven extension bits can be absent on a released
   peer (`CREATE_PLACEMENT` onward). The branches on the other 39 or so have
   no reachable peer. The clearest case is the 3 s, 20 ms `GET_STATE` poll
   after kill (`crates/phux-client/src/kill/selection.rs`, `await_reaped`).
   Hub to satellite pairs need the same check.
2. **`INPUT_TERMINAL_REPLY`.** Decoded, authorized, routed, gated by a bit
   every server advertises, then discarded (`handle_terminal_reply` in
   `crates/phux-server/src/runtime/apps/terminal/commands/terminal_input.rs`).
3. **Seven duplicated launch helpers.** `default_shell_command`,
   `shell_command`, `apply_term`, `apply_terminal_id`,
   `apply_server_socket`, `apply_spawn_cwd` and `apply_strict_spawn_cwd`
   exist in `crates/phux-terminal/src/actor/spawn/command.rs` and in
   `crates/phux-terminal-api/src/launch.rs`. Production calls only the
   second.
4. **`probe()`** in `integrations/runtime/src/adapter.ts` has no non-test
   caller. `integrations/pi/src/truncate.ts` is used inside the package only
   by its test, but `src/index.ts` re-exports it, so check for outside
   consumers first.
5. **The Claude and Grok hook scripts** (`integrations/claude/scripts/phux-hook.sh`
   and its Grok twin) are the same 186 lines with seven differing.

## Already wrong or about to be

Found on the way, at the examined commit:

- `integrations/runtime/package.json` exports `./mail`, but no `mail`,
  `message-transcript` or `screen` outputs are tracked under
  `integrations/runtime/dist`.
- `integrations/opencode-v2/index.d.ts` declares 5 options; `src/index.ts`
  has 7 (`maxTerminals` and `serverLifecycle` are missing).
- `docs/spec/wire-schema.json` describes `ActorRef` without the two
  principals that `encode_actor_ref` writes.
- All three entries in `scripts/npm-audit-allow.json` expire on 2026-11-15.
- `scripts/check-website-docs-catalog.sh` is warning-only "pending a
  maintainer decision after one week (around Nov 5, 2026)".
- `scripts/ci/prune-build-cache.sh` omits the `rune-ios-native` cache key
  that `.github/workflows/rune-build.yml` writes.
- Three tests in `scripts/known-failing-tests.txt` are quarantined on macOS
  under one bead with no stated cause.

## What is not compensation

These looked like compensation and are essential: retry receipts and
idempotency keys for lost replies; crash reconciliation of recorded
processes; optimistic concurrency by revision; reconnect ladders after a
real network drop; bounded ledgers and decode limits against hostile peers;
authorization checked on every dispatch; live revocation; atomic file
writes; pid plus start-time identity before signalling; PTY semantics (`EIO`
as end of file, `EINTR` retry, hangup then kill); the bare-escape timeout;
host terminal probing; sanitising untrusted text before a model, a terminal
or a DOM; signature and digest checks on updates; binary portability checks
on release artifacts.
