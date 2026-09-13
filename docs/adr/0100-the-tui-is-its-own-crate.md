---
audience: contributors
stability: stable
last-reviewed: 2026-09-08
---

# 0100 — The TUI is its own crate

**TL;DR.** The reference TUI moves out of `phux-client` into `phux-tui`.
`phux-client` is the headless client library: connection, transports, the
stdin parser, the replay journal, the exit vocabulary, and the agent verbs.
`phux-tui` is everything that needs a screen: the attach driver, the
libghostty replicas, the ratatui chrome, the dispatcher, the overlays. The
dependency runs one way, and the `tui` cargo feature is gone.

Status: Accepted
Date: 2026-09-08

## Context

ADR-0020 confined `ratatui` to `phux-client` and moved the pane-interior
substrate into `phux-client-core` so that boundary was compiler-enforced.
It left a second boundary enforced only by a cargo feature: `phux-client`
was at once the interactive TUI (`attach/driver`, `render/`, the input
dispatcher, sixty thousand lines) and the headless library behind every
`phux` agent verb and the `phux-mcp` adapter (`snapshot`, `send_keys`,
`wait`, `watch`, `state`, twenty thousand lines). The `tui` feature, on
by default, was the only thing separating them.

That arrangement had concrete costs. `phux-mcp` had to remember to say
`default-features = false`, and a CI script existed solely to prove no
`ratatui` reached its graph. Doc paths and prose called `phux-client` "the
ratatui TUI client" while `docs/consumers/sdk.md` called it "the library
behind the CLI"; both were true, which is the problem. Every headless
module that the TUI happened to reach into (`layout_ops`, `agent_meta`,
`perf`, `vcs`) could stay `pub(crate)`, so the real API the TUI consumed
from the library was never written down. Test builds unified tokio
features across the two halves, hiding a missing `test-util` declaration
until the halves were separated. And the settings work that follows this
ADR needs a place for TUI-owned configuration state that is neither the
config crate nor the agent library.

## Decision

1. **`phux-tui` is a new workspace crate** holding every module the `tui`
   feature used to gate: `attach/{driver, paint, render, reflow, rendered,
   repaint, input_dispatch, actions, action_registry, server_frame, fleet,
   focus, copy, context_menu, onboarding, plugin_actions, plugin_panes,
   sidebar_zones, record, reload, stdout_writer, terminal_probe,
   tty_input}` and all of `render/` (chrome, overlays, theme). Its tests,
   benches, and insta snapshots move with it. It re-exports the headless
   attach vocabulary under `phux_tui::attach` so the driver keeps its
   paths and embedders can name one crate.

2. **`phux-client` is the headless client library, and only that.** It
   keeps `attach::{connection, input, input_replay, outcome, quic, ws}`
   and the verb modules. It links no `ratatui`, no `rustix`, no
   `phux-crash`, no `phux-plugin`, and never enables tokio's `io-std`. The
   `tui` and `native-engine` features are removed; `testkit` stays.

3. **The dependency is one-way.** `phux-tui` depends on `phux-client`;
   nothing in `phux-client` may name `phux-tui`. What the TUI needs from
   the library is now `pub`, which is what makes it an API: the layout-key
   owner, the connection's test seams under the `testkit` feature, the
   exit-status formatter, the named-key constructor.

4. **`phux-client-core` is unchanged.** Both crates depend on it; both
   re-export `layout`, `multi_pane`, `predict`. The ADR-0020 boundary
   between chrome and substrate stands; this ADR adds the boundary between
   chrome and headless control plane beside it.

5. **The binary depends on both.** `phux attach` and the rendered
   `phux snapshot` go through `phux_tui::attach`; every other verb goes
   through `phux_client`. `phux-mcp` depends on `phux-client` alone.

## Why

A crate boundary is the only boundary Rust checks. A feature flag protects
nothing on the default path, and the default path is the one every
contributor builds; the flag mostly documented an intent. Splitting the
crates turns "headless consumers must not link a terminal" from a script's
assertion into a property of the dependency graph: `phux-mcp` cannot
compile the chrome because no edge leads there.

The split also names the interface. Forty-odd `crate::` paths from the TUI
into the headless half became `phux_client::` paths, and each private item
they reached became a public one with a doc comment. That list is the
contract a native or web front end would code against; before, it did not
exist as a thing.

The smaller reason is build hygiene. Each half now has exactly the
dependencies it uses, its own dev-dependency graph, and its own feature
set, so a change in the chrome does not re-key tokio for the agent verbs
and a missing test feature fails where it is missing.

## Tradeoffs

- **One more crate** (seventeen). The workspace already treats crates as
  the unit of boundary, so this follows the house pattern rather than
  adding a new kind of thing.
- **Public surface grows.** Items the TUI reached into become `pub` on
  `phux-client`. They were already load-bearing; they are now visible and
  documented. The crate is `publish = false`, so this is not a semver
  commitment.
- **Path churn.** `phux_client::attach::{run_with_predict_dial, record,
  status_bar, action_registry, render}` become `phux_tui::attach::...`.
  Only the binary and one conformance test named them.
- **`native-engine` moves with the driver, still not optional in
  practice.** Its `cfg(not(...))` fallback branches were never compiled by
  any gate before this ADR, and `cargo check -p phux-tui
  --no-default-features` does not build today (the fallback names
  `engine::ghostty`, which that build configures out). The split made the
  dead feature visible; deleting or repairing it is follow-up work, not
  this decision.

## Alternatives

**Keep the feature flag and add a lint.** A `check-no-ratatui` script
already existed for `phux-mcp`. It catches one consumer and says nothing
about the interface between the halves. Rejected as the status quo.

**Move the verbs out instead (`phux-sdk`).** Same graph, different name
for the new crate. Rejected because every existing consumer, doc, and
`phux_client::` path names the headless half as `phux-client`; moving the
TUI churns the binary's attach entry only.

**Fold the driver into `phux-client-core`.** Rejected: the substrate is
deliberately frontend-neutral and wasm-safe (ADR-0025's reversal), and the
driver owns a controlling terminal.
