---
audience: contributors
stability: stable
last-reviewed: 2026-05-28
---

# 0009 — phux vs coder/mux: positioning

**TL;DR.** phux is a terminal-multiplexer protocol substrate; coder/mux is an agent-orchestration product. They live at different layers and could coexist — a Mux-class product could ship on top of phux. They intersect only at remote execution and diverge everywhere else, so phux does not absorb agent-runner or workspace-management features.

Status: Accepted
Date: 2026-05-25

## Context

`coder/mux` is the most product-developed adjacent project in the
libghostty / agentic-dev space. It ships an Electron desktop + browser
app for parallel agent workspaces: isolated worktrees (local, git
branches, SSH), multi-model agent runner (Claude/GPT/Grok/Ollama),
VS Code extension, cost dashboards, opportunistic compaction.

Because Mux uses `coder/ghostty-web` (libghostty compiled to WASM +
xterm.js-compatible API) for terminal rendering and because it overlaps
with phux's swarm/satellite story on the surface ("parallel agentic
work, including via SSH"), a future contributor will at some point ask
whether phux should pivot toward what Mux already does — or whether Mux
makes phux redundant.

This ADR exists so the answer to that question is on paper.

## Decision

**phux is a terminal-multiplexer protocol substrate. Mux is an
agent-orchestration product. They live at different layers and could
coexist; a Mux-class product could ship on top of phux.**

phux's scope, in one line: a wire protocol + reference server/client
for cell-level pane synchronization across local + remote panes, with
multiplexer semantics (sessions/windows/panes) and forward-compat for
Mosh-class transport and satellite federation.

Mux's scope, in one line: an opinionated product UX for running many
agents in parallel against isolated codebases.

The two intersect at one capability — remote execution over SSH — and
diverge everywhere else.

## What goes where

| Concern                               | phux           | Mux               |
|---------------------------------------|----------------|-------------------|
| Wire protocol for pane diffs          | Yes (SPEC)     | None (in-process) |
| Multi-pane / multi-window / sessions  | Yes            | Per-workspace VT  |
| Remote terminal across a network      | Yes (UDS → QUIC, ADR-0007) | SSH only |
| Predictive local echo                 | Yes (ADR-0007) | N/A (no remote VT protocol) |
| Server-side terminal emulator         | Yes (libghostty-vt) | In-process WASM (ghostty-web) |
| Headless server with N attached clients | Yes          | No (single Electron app) |
| Agent runner / model selection        | No             | Yes (core product) |
| Git worktree management UI            | No             | Yes               |
| Compaction / cost dashboards          | No             | Yes               |
| Browser frontend                      | Future (post-v0.1) | Yes (ships today) |
| Product surface (workspace pickers,   | No             | Yes               |
| sidebars, mode prompts)               |                |                   |

## Why this isn't redundant

A naive read of the overlap ("both touch agents, both use libghostty,
both can hit a remote box") suggests one displaces the other. It
doesn't, because:

1. **The terminal surface is at different layers.** Mux terminates the
   terminal inside its own process via WASM libghostty. There is no
   wire format between Mux's renderer and its emulator — they're the
   same address space. phux's emulator and renderer are deliberately
   separated by a wire protocol (SPEC.md §8); that separation is what
   enables headless servers, multi-attach, satellite federation, and
   predictive echo. None of that is on Mux's roadmap because it isn't
   the product Mux is building.

2. **A Mux-class product could ship on top of phux.** Mux's product
   features (workspace isolation, agent dispatch, costs UI) are policy
   layered on top of "I have N parallel terminals I can address." That
   substrate is exactly what phux exposes. A future product could use
   phux as its multi-pane transport and add the agent-orchestration UX
   on top. The reverse isn't true: phux can't be a feature of Mux
   without re-inventing the wire protocol Mux deliberately doesn't need.

3. **Different correctness targets.** phux owes wire-format stability,
   predictive-echo correctness, and snapshot replay across detach/
   reattach. Mux owes prompt UX, agent loop correctness, and workspace
   isolation. The places we obsess don't overlap with the places they
   obsess.

## What phux steals from Mux's product vocabulary

Mux's user-facing concepts — isolated workspaces, git-divergence view,
agent status sidebar, mode prompts, opportunistic compaction — are
hints about command-plane primitives a future agent-aware client might
want. phux has `AGENT_HOOKS` reserved as a `ServerFeature` bit
(SPEC §6.2) for exactly this kind of layered policy. The bit is empty
today; when it grows shape, Mux's published vocabulary is the cheapest
source of "what concepts will the product layer want to express."

Filed as a follow-up at the spec level, not in this ADR.

## What this means for incoming PRs

If a contributor proposes:

- "Let's add a built-in agent runner / model picker / cost dashboard
  to phux" → out of scope. That's product policy. Reject (with a
  pointer to this ADR).
- "Let's add a multi-pane protocol to Mux" → not our problem; we're
  not Mux.
- "Let's make phux's spec friendly to an agent-orchestration product
  layered on top" → in scope. SPEC §6.2 (capabilities), §11
  (commands), and the unspec'd `AGENT_HOOKS` bit are the seams.
- "Let's merge phux and Mux into one tool" → no. They solve different
  problems. The merge would either turn phux into a product (losing
  the substrate's value to other products) or turn Mux into a
  protocol library (losing the focused product surface).

## Status of this ADR vs the others

This ADR doesn't supersede or amend any prior decision; it disambiguates
the project's positioning so the existing ADRs (especially
ADR-0007 satellites + ADR-0003 server process model) aren't mistakenly
re-litigated as "but Mux does this differently."
