---
audience: contributors, agents
stability: evolving
last-reviewed: 2026-05-29
---

# 0022 — phux as a tool for agents

**TL;DR.** phux is the *terminal-capability substrate* agents use, not an
agent host (no agents ship in phux). Every consumer — the human TUI, an
agent CLI, an MCP adapter, a script — talks to the **same** per-user
server over the same control plane; they differ only in their
**projection** of the one source-of-truth libghostty `Terminal`. The TUI
projects it to VT bytes (it renders, like tmux); agents project it to
**structured data** (cells, semantics, command results). The **CLI + its
JSON schema is the stable contract**; the wire underneath stays
additive/versioned. Design for the capability ceiling (native session
persistence + a control plane over real terminals, not one-off shells)
while maximizing zero-shot legibility for today's bash-trained models.

Status: Accepted
Date: 2026-05-29
See [ADR-0102](./0102-resources-the-server-serves-kinds.md) for the current scope: projections are per resource kind, not Terminal alone.

## Context

Today an agent's only terminal is a fire-and-forget command runner: no
session persistence (every call is a fresh shell), no interactivity
(TUIs/REPLs/prompts are routed around, not driven), no live observation
(output is sampled, not watched), no structured state (scrollback is
scraped with heuristics). phux already removes the hard blockers — one
persistent server per user ([ADR-0003](./0003-server-process-model.md)),
structured *input* atoms ([ADR-0006](./0006-input-mirrors-libghostty.md)/
[ADR-0008](./0008-use-libghostty-types-directly.md)), a frontend-agnostic
wire ([ADR-0010](./0010-frontend-agnostic-tmux-cc-reserved.md),
[ADR-0017](./0017-tui-not-protocol-privileged.md)) — and runs a real VT
engine that already parses semantics (OSC 133 prompt/command marks,
titles, hyperlinks). Of ~30 wire frames, only two (`TERMINAL_OUTPUT`,
`TERMINAL_SNAPSHOT`) are human-shaped (raw VT bytes); everything else is
already structured. So the agent surface is a small *additive* surface,
not a rewrite — but it is the project's headline goal and deserves a
settled shape. The hard part is that current models are post-trained on
bash-shelling, not on "the best thing," so the surface must be useful now
yet built for a capability ceiling we cannot yet measure.

## Decision

1. **One core, many projections.** All consumers share the server, the
   control plane, and the source-of-truth per-pane `Terminal`. A consumer
   is defined by how it *projects* that Terminal: the human TUI →
   VT bytes (rendering consumer, like tmux); agents → structured cells +
   semantics + command results. No consumer is privileged — extends
   [ADR-0017](./0017-tui-not-protocol-privileged.md). [ADR-0013](./0013-libghostty-bytes-on-wire.md)
   holds: VT bytes stay the TUI's payload (it runs libghostty); structured
   shapes are a *second* projection for consumers that don't render, **not**
   a reintroduction of cell-diffs on the human path.

2. **The CLI + JSON schema is the stable contract; the wire is flexible.**
   Permanence lives in the verb set and data shapes (versioned via a
   `schema_version` field from day one). The wire's structured-query and
   event taxonomy stay additive — reserved ranges
   ([appendix-reserved](../spec/appendix-reserved.md)) + the TLV
   migration (`phux-i58`) make field/kind additions non-breaking. We get
   to be wrong about the wire and recover without breaking agents.

3. **Zero-shot legible, ceiling-built.** Put novelty in the *capability*,
   not the syntax: tmux-shaped verbs (`send-keys`, `capture`, `split`,
   `ls`), POSIX exit codes (`run`'s exit == the child's), JSON, `--help`,
   `--json`. A model never trained on phux operates it by analogy on day
   one; a future model goes further with the same surface.

4. **Eventing: a poll floor + additive semantic push, conditions
   CLI-side.** `snapshot` (pull) plus `wait`/`watch` implemented as
   poll-until-condition is the floor — it always works and needs no
   shell-integration. A `SUBSCRIBE` stream of **extensible tagged events**
   (command started/finished{exit}, title, bell, pane spawned/closed,
   coarse dirty/idle) is the additive fast-path. `wait`/`watch`
   *conditions* (`--until <regex>`, `--idle`, `--command-done`) are
   matched in the CLI, never encoded as a fixed wire enum, so they evolve
   without wire changes. Shell-integration (OSC 133) enhances `run`; an
   idle-detection fallback covers its absence.

5. **Thin CLI, server-side extraction.** The binary is a thin client
   (connect → request → JSON → exit, or hold-and-stream for
   `run`/`watch`); the *server* extracts grid + semantics from its
   authoritative `Terminal` (reusing `grid.rs`'s walk). This keeps the
   tool reimplementable in any language and is what makes MCP just a thin
   adapter over the same structured surface — not a separate core. The CLI
   contract is independent of *where* extraction runs, so an initial
   attach-and-walk implementation may precede the server-side query frame
   without changing the contract.

## Consequences

- **Additive, low-risk.** Nothing here regresses the TUI path or removes a
  frame. The floor (poll) can never be wrong; push events, semantics, and
  the server-side query are layered on top and individually reversible.
- **Underused before it's trained on — accepted.** Today's models
  (including the one writing this) will reach for bash out of habit; the
  surface is built for the agent that's coming, with legibility so it's
  usable before then. Dogfooding via `examples/agents/` + a CLI-wrapping
  skill is the acceptance test: if those are awkward to write, the
  schema/verbs are wrong.
- **`run` leans on shell-integration.** Its command-output+exit semantics
  are best with OSC 133; the idle-detection fallback is necessarily
  fuzzier. cwd (OSC 7) is a known gap (`phux-cs6`).
- **Schema churn is the standing risk.** Mitigated by `schema_version`,
  the CLI-stability discipline, and keeping the wire additive — but the
  JSON contract must be reviewed like a public API, because it is one.

## Alternatives rejected

- **In-process agent API (link phux-client).** Rejected: forecloses
  non-Rust consumers and conflates "tool for agents" with "agent host."
- **Make structured the *primary* wire (demote VT bytes).** Rejected: VT
  bytes are optimal for the libghostty-bearing TUI; this would regress the
  human path for no agent gain. Projection, not replacement.
- **Bake `wait` conditions into wire frames.** Rejected: a one-way door on
  the least-known part of the design; conditions belong in the CLI.
