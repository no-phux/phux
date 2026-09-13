# phux.sh / docs.phux.sh — content & positioning north star

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

Two facts that sit on that wedge, stated affirmatively, never as a comparison
table:

- **Blocked is a fact when the harness emits.** `AgentSession` is a producer-fed
  stream. Screen detection is compatibility for harnesses that do not emit yet.
- **Join a machine with no account.** `phux --remote` pairs once; later dials
  are direct QUIC. The join is yours. There is no phux Cloud in the hero.

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
- **What's here, what isn't.** State the facts. Don't lead with a disclaimer
  and don't decorate every page with "pre-alpha."
- Confident about the architecture. Gaps live in concepts.

## What we DON'T say

- ❌ "A better tmux for everyone." (It will drown. Its right to exist is the
  passthrough niche + the agent-wire bet, not general muxing.)
- ❌ Lead with federation. (Vision footnote, not a reason anyone shows up.)
- ❌ Treat the CLI/MCP agent loop as unfinished. It works today; JSON may
  move. AgentSession is in this tree (`phux status --json` for older
  releases). Don't say "still landing" as if the verbs are vapor.
- ❌ Treat panes/splits as the product. (They're a view.)
- ❌ Say Cockpit or hub-and-spoke federation is "designed, not wired."
  Both ship. The unshipped list is a public SDK crate and an on-disk journal.
- ❌ Hand-wave the demo's safety. (The demo runs phux-edge — a curated shell —
  as WASM in a Durable Object: no OS, no processes, nothing to break out of.)
- ❌ Capitalize the wordmark. It is always `phux`. The macOS app bundle is
  `Phux Cockpit` because that is the filename; prose still says Cockpit.
- ❌ A vs-herdr comparison table, a herdr.dev visual clone, or "21 agents
  detected out of the box." Detection is compatibility. Emit is the product.
- ❌ A phux account, waitlist, or hosted relay as the way machines join.

---

## Per-page content map

### `/` — landing (the persuasion surface) — BUILT

`src/pages/index.astro`. The `<PhuxTerminal client:load>` wasm island is the
hero; the narrative sections run top to bottom below it.

Marketing lives on `phux.sh`. Documentation lives on `docs.phux.sh`. Nav is
Docs / Apps / Agents / GitHub.

1. **Hero** — headline, short lede, Install + Docs, then the live terminal
   as the product. No SYS badge, no install wall above the fold.
2. **Fleet inbox demo** — one short, replayable sequence: an agent works,
   asks for approval, and its exact terminal rises into the inbox. The blocked
   state remains legible without motion.
3. **Features** — five rows, phux's own ladder: panes are a view, same
   objects / many consumers, co-presence (blocked is a fact when the
   harness emits), bytes stay bytes, your machines / no account. Each row
   links into docs.
4. **Install** — the closer. CLI and Cockpit copy-paste commands, then
   Homebrew and the full install guide. Checksums and platform notes stay
   here, not in the hero.

### `/overview` — docs landing on docs.phux.sh

`src/pages/overview.astro`. The docs host root 301s here. Surfaces (CLI,
Cockpit, Web, Agents), pick-your-path (new / tmux / agents / peer), then Get
started / Build / Resources columns. This is the persuasion surface for
readers who already arrived to read; it does not replace `docs/README.md`
(`/docs`), which stays the full index.

### `/concepts` — the mental model
Synced + curated from `docs/CONCEPTS.md`. The terminal as the unit; the wire in
layers; views as consumers; co-presence. This is where "panes are a view" gets
fully explained.

### `/quickstart` — run it today
Synced from `docs/QUICKSTART.md` (+ `INSTALL.md`, `operations.md`).
Build-from-source, the prefix keys, attach/detach.

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
Synced from `docs/adr/`. `docs/adr/README.md` -> `/decisions`; each `NNNN-*.md` ->
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
