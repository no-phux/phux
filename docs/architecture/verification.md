---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-09-12
---

# Quality bar: testing and performance

**TL;DR.** Tests run in three layers (unit tests beside code, property tests
for codec and state-machine invariants, snapshot tests for wire bytes and
rendered frames). The unit pool is `just test` (`cargo nextest run
--workspace`); the full root PR bar is `just ci-full`. Performance is
measured, not guessed: crate benches, a release profile tuned for shipped
binary speed, and opt-in mutation runs that are not a CI gate.

## Test strategy

Tests are organized in three layers. All three run today.

1. **Unit tests** colocated with the code they cover, plus crate
   integration tests under `crates/*/tests/`. The workspace pool is
   `just test` (`cargo nextest run --workspace`). `just ci` includes that
   pool plus compile-free contract gates (fmt, lint, rustdoc, deny,
   `just docs-check`, and others listed in CONTRIBUTING.md). It does not
   spawn real PTY-backed servers.

2. **Property tests** (`proptest`) for invariants that should hold across
   arbitrary inputs. They live in `phux-protocol` (codec roundtrip),
   `phux-core`, `phux-client-core` (session kernel), and `phux-tui`:
   - Protocol codec roundtrip: encode, decode, and the result equals the
     input. This is the codec's primary safety net.
   - State-machine invariants, for example that after any sequence of
     commands the layout tree stays well-formed.
   - Replay equivalence: for any PTY byte stream, writing those bytes into a
     fresh `Terminal` on the client reproduces the same visible grid the
     server's `Terminal` saw, up to the documented downsampling rewrites. The
     snapshot-on-attach synthesis algorithm is checked the same way:
     synthesize, replay into a fresh `Terminal`, and compare the resulting
     `RenderState` snapshots. See
     [`state-sync.md`](./state-sync.md) for the synchronization model this
     verifies and
     [`research/2026-05-25-libghostty-renderstate.md`](../../research/2026-05-25-libghostty-renderstate.md)
     §7 for the synthesis algorithm.

3. **Snapshot tests** (`insta`) for outputs that should change only on
   purpose:
   - Wire bytes of representative messages in
     `crates/phux-protocol/tests/frame_wire_snapshots.rs`, so an accidental
     format change is loud rather than silent.
   - Rendered TUI frames and chrome, via a cell-grid to ASCII-art helper.

## Real-server and smoke lanes

These spawn real processes and sit outside `just ci` on purpose. A `just ci`
failure always means a deterministic defect; a green `just ci` is not the
PR bar.

- `just e2e` — the fast e2e lane: ignored `*_e2e.rs` binaries against
  PTY-backed servers (headless `run`/`wait`, agent-record loop, spatial
  edits, wall-clock perf). CI's `test` job runs this; `just ci` does not.
- `just agents-fleet-smoke` — hermetic argv/control-flow gate for the
  placed-fleet example. No live server.
- `just ci-full` — `just ci` plus `just e2e` plus `just agents-fleet-smoke`.
  That is the full root PR bar.

The herdr parity work uses a repeatable gate in
[`../../scripts/parity-gate.sh`](../../scripts/parity-gate.sh), surfaced as
`just parity-check-list` and `just parity-gate`. The list/check mode is cheap:
it proves the named scenarios are present and still point at real scripts,
just targets, tests, and example/plugin assets. The run mode is explicit
because several scenarios spawn real PTYs, tmux, or the full CI gate.

The gate names eight evidence surfaces:

- `install-contract`: install docs/scripts/release artifact contract checks.
- `examples-smoke`: examples/agents against a real `phux` binary.
- `plugin-demo`: checked-in plugin discovery, validation, and actions.
- `real-pty-run-wait`: the ignored e2e lane for real PTY `run`/`wait`.
- `tui-probe`: black-box attach through an isolated tmux terminal.
- `visual-qa-hooks`: captured TUI probe output with screen and cursor markers.
- `docs-check`: the doc-system gate from this conventions layer.
- `full-quality-gates`: `just ci`, including fmt, lint, docs, tests, deny, and
  rustdoc.

Each user-visible parity child task records four receipts in the work ledger:
automated verification, a real-surface artifact, adversarial checks, and
cleanup. Evidence files live under `.omo/evidence/`; they are execution
artifacts, not product docs.

## Mutation testing

Mutation testing with `cargo-mutants` is opt-in, not a `just ci` gate.
[`../TESTING_MUTATIONS.md`](../TESTING_MUTATIONS.md) owns the runner,
budgets, and how to read killed/survived/unviable results. There is no
required mutation score.

## Performance

phux does not optimize speculatively. What is measured today:

- Crate benches: `phux-server` (`capture`, `server_measure`),
  `phux-client-core` (`history`), `phux-tui` (`render_frame`).
- `just perf-echo` — reproducible keystroke-echo latency against an
  isolated release server.
- Server integration tests under `crates/phux-server/tests/perf_*.rs`
  and `benchmark_budget.rs`.

The release profile uses fat LTO and a single codegen unit, since the speed
of the shipped binary is a goal in its own right.

## Status

| Gap | Today | Owner | Tracked |
|---|---|---|---|
| A fixed published set of throughput, fanout, and reattach numbers as a regression gate | Benches and `perf-echo` exist; they are not a required CI check with pinned budgets. | — | not scheduled |
