# phux.sh — content & positioning north star

The single source of truth for what this site says and how it says it. Every
page, every piece of copy, and the synced docs build against this. If a sentence
on the site contradicts this file, this file wins (or this file is wrong — fix it
here first).

> **Status: the landing is BUILT.** This file is no longer aspirational. The
> narrative landing at `src/pages/index.astro` ships the sections below, in
> order, with the live `<PhuxTerminal>` wasm island as the hero. When you change
> the landing, change this file in the same commit — they are meant to track.

---

## Thesis (build the whole site on this)

> **you and your agents share the same terminals.** phux makes every terminal a
> first-class object on a wire — panes are just a view, and the terminal
> underneath is something anything can drive: you, a gui, or an agent.

This is the wedge: **co-presence**. Not "a better tmux," not "an agent tool" —
the one thing only phux offers is humans and agents reading and writing the
*same* terminal objects at the same time, because a terminal is addressable on a
wire instead of trapped behind a screen.

- The **passthrough is the proof.** (Demoable, true today — the live island.)
- The **wire is the product.** (Spawn / observe / drive a terminal as an object;
  the tui is the on-ramp, not the essence.)
- The **co-presence is the point.** (Humans and agents on the same objects. The
  reason the wire matters — stated as a direction, honestly, not over-sold.)
- The **panes are just a view.** The multiplexer TUI is *one consumer* of the
  wire. A GUI could render it; an agent drives it headless. Never imply the
  splits/chrome are the essence — they're the on-ramp.

## Audience priority

1. **Primary — modern-terminal humans.** ghostty / kitty / wezterm users who
   lose graphics, kitty-keyboard, sixels the moment they run tmux/zellij. This is
   the demoable, relatable, true-today pain. Lead here.
2. **Strategic — agent & tooling builders.** People who'd build *on* the L1 wire:
   coding agents, build orchestrators, fleets. This is the point of the project.
   Always present, the elevation, never the cold open.
3. **Tertiary — contributors.** Rust devs, spec readers. Served by the wire +
   architecture + decisions (ADR) pages. Routed to, not marketed at.

## The message ladder (how a reader should move)

1. **Hook (the wedge):** you and your agents share the same terminals. panes are
   a view; every terminal is an object on a wire anything can drive.
2. **Proof (seen):** the live wasm island renders the real terminal stream —
   osc 8 hyperlinks, 24-bit color, a live prompt — the same bytes a gui or an
   agent gets off the wire. That live frame is the pitch. (The full
   passthrough set — sixel, kitty keyboard — is the structural claim; the edge
   demo's curated shell shows osc 8 + truecolor. Don't claim the demo renders
   protocols it doesn't.)
3. **Reframe (the idea):** what you're looking at is just a *view*. Underneath,
   every terminal is a first-class object on a wire — spawn, observe, drive.
4. **Why it can't be mangled (structural):** phux never re-parses. The same
   libghostty engine runs on both ends, so every protocol — and every future
   one — passes through by construction. No comparison table; the architecture
   *is* the argument.
5. **The point (the bet, honest):** because the terminal is a wire-addressable
   object, humans and agents are co-present on it. Structured agent state is a
   *local projection* (CLI + JSON), not a privileged service on the wire. This is
   where it's going. Stated as a direction.
6. **Depth (routed):** the wire (L1/L3), consumers, the architecture, the
   decisions.

## The structural argument (our sharpest, most defensible claim)

Don't argue "tmux can't do X today" — tmux 3.4+ keeps bolting on sixel, OSC 8,
extended keys. **Don't use a comparison table.** Argue the **architecture**: a
re-parsing multiplexer must implement every protocol manually and will always
lag. phux shares the VT engine across the wire, so it gets all of them — present
and future — for free. That's a structural property, not a feature-race lead.
This is why the project deserves to exist.

## Voice & tone

- Terminal-aware, lowercase wordmark (`phux`) with tactical monospace for code,
  protocol symbols, versions, and terminal output. General interface and prose
  use proportional type so technical signals retain their emphasis.
- Precise, technical, dry. No hype, no superlatives, no "revolutionary."
- **Honest about maturity.** It's pre-alpha, spec-first. Say so. This audience
  respects "here's the bet, here's what works today, here's where it's going."
- Confident about the architecture, modest about the timeline.

## What we DON'T say

- ❌ "A better tmux for everyone." (It will drown. Its right to exist is the
  passthrough niche + the agent-wire bet, not general muxing.)
- ❌ Lead with federation. (Vision footnote, not a reason anyone shows up.)
- ❌ Oversell agents as a shipped feature. (It's the point and the direction;
  don't claim it's done.)
- ❌ Treat panes/splits as the product. (They're a view.)
- ❌ Hand-wave the demo's safety. (The demo runs phux-edge — a curated shell —
  as WASM in a Durable Object: no OS, no processes, nothing to break out of.)

---

## Per-page content map

### `/` — landing (the persuasion surface) — BUILT

`src/pages/index.astro`. The `<PhuxTerminal client:load>` wasm island is the
hero; the narrative sections run top to bottom below it.

1. **Hero + live proof** — headline is the wedge: "you and your agents share the
   same terminals." Subhead: panes are a view; every terminal is an object on a
   wire anything can drive. The wasm island sits directly under it as immediate
   proof — launch-gated (poster of a real session + click to go live; the
   Worker's session cap means auto-connect would burn capacity). The one
   copy-paste install path (from source; brew when bottles ship) sits directly
   below the island, inside the first scroll.
2. **The passthrough is the proof** — the rendered stream is the *actual* bytes a
   gui or an agent gets off the wire, not a screenshot. Demo caption: "the same
   bytes a gui or an agent gets off the wire." Plus the honest demo-backend
   note: phux-edge, a curated os-less shell as WASM in a Durable Object — no
   network, nothing persists past the session.
3. **Not tmux** — the structural argument: tmux re-parses and always lags; phux
   never re-parses because the same libghostty engine runs on both ends. No
   comparison table — the architecture is the argument.
4. **A terminal is an object on a wire** — spawn / observe / drive; L1 (bytes +
   input) and L3 (metadata + links) at a glance. The panes you saw are one
   consumer. Links to `/wire` and `/concepts`.
5. **The wire is the product** (the wedge, internally) — the tui is the
   on-ramp; humans + agents are co-present on the same terminals; the tui is a
   *pure consumer* with no protocol privilege (per ADR-0017). Links to
   `/consumers/tui`. NOTE: "the wedge" is positioning vocabulary for THIS file —
   it never appears in public copy. The site states the fact; it doesn't name
   the move.
6. **Built for agents** — structured agent state is a *local projection*: CLI +
   JSON, not gRPC on the wire. The agent SDK copies what the phux-web browser
   client already does. Early/the direction — state it plainly, never say
   "honestly" (being honest is shown, not claimed). Links to
   `/consumers/agents`.
7. **Status** — v0.0.x pre-alpha, stated once, plainly: the README's three-tier
   stable / real-but-moving / designed-not-wired line, plus license and the
   GitHub link. Never claim a distribution channel (brew, crates.io) before it
   ships.
8. **Get going** — router cards: quickstart / concepts / the wire / consumers /
   github.

### `/concepts` — the mental model
Synced + curated from `docs/CONCEPTS.md`. The terminal as the unit; the wire in
layers; views as consumers; co-presence. This is where "panes are a view" gets
fully explained.

### `/quickstart` — run it today
Synced from `docs/QUICKSTART.md` (+ `INSTALL.md`, `operations.md`).
Build-from-source, the prefix keys, attach/detach. Honest pre-alpha caveats.

### `/wire` — the protocol (the crown jewel for builders)
Synced from `docs/spec/`. L1 terminals (bytes + input), L3 metadata and links.
This is the agent-facing surface — the page that makes the "agent substrate"
claim concrete. Should read like a real spec, not marketing.

### `/consumers` — who drives the wire (NEW, promoted to top-level nav)
Synced from `docs/consumers/`. The reference TUI, the web client, the MCP
surface, and the agent SDK. This is where "the tui is one consumer" and "the
agent SDK copies phux-web" become concrete. `consumers/tui` and
`consumers/agents` are the two the landing links into.

### `/architecture` — for contributors
Synced from `docs/architecture/`. Process model, crate graph, two-renderer
model, threading, transport. Links to rustdocs + crates.io when they exist.

### `/decisions` — the ADR index (NEW, promoted to top-level nav)
Synced from `ADR/`. `ADR/README.md` -> `/decisions`; each `NNNN-*.md` ->
`/decisions/adr-NNNN`. The record of why phux is shaped the way it is — ADR-0017
(tui not protocol-privileged) and ADR-0030 (engine-delegated wire, projection
consumers) are the load-bearing ones for the landing's claims.

---

## The live demo (what the hero actually is)

The hero is a **live `<PhuxTerminal>` wasm island**, not a recorded GIF. It runs
the real phux-web browser client (Rust/WASM) against **phux-edge** — the phux
server compiled to WASM inside a Durable Object, backed by a curated, OS-less
shell — over a WebSocket. There is no scripted choreography to record — the
visitor is looking at, and can type into, a real phux terminal.

It is **launch-gated**: the static poster (a captured frame of a real session,
regenerated by `scripts/capture-poster.ts`) renders first and with JS off; the
wasm client and an edge session spin up on click. The Worker keeps a global
session cap + per-IP rate limit, and sessions close on idle / hard-max.

What it must convey (true by construction, because it's the real stream):

1. The rendered output is the **actual terminal byte stream** off the wire —
   OSC 8 hyperlinks, 24-bit color, a live prompt — not a re-render. A browser
   canvas drew it; a gui or an agent would consume the same bytes. (The wider
   passthrough set — sixel, kitty keyboard — is the structural claim; the
   curated edge shell doesn't emit those, so the demo copy doesn't claim them.)
2. The caption ties view → wire: **"the same bytes a gui or an agent gets off
   the wire."**
3. The safety note is **non-negotiable**: a curated, OS-less shell as WASM in a
   Durable Object — no network, no processes, nothing persists past the
   session. We invite typing only alongside that note.

If the demo backend isn't deployed (`PUBLIC_PHUX_DEMO_WS` empty), the island
renders the poster with a "coming online" state; if the backend is unreachable,
it says so and keeps the poster — the landing copy still stands.
