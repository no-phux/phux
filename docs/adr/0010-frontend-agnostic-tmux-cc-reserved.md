---
audience: contributors
stability: stable
last-reviewed: 2026-05-30
---

# 0010 — phux is TUI-first, non-TUI not precluded; tmux control mode reserved as compat option

**TL;DR.** phux is TUI-first: the reference TUI is the only consumer that has ever exercised the wire. The architecture must not preclude a non-TUI frontend — domain code carries no frontend-specific assumption and rendering goes through one seam — but that property is structural, derivable from the wire on paper, not validated by a second consumer. tmux control mode is not on the roadmap. ADR-0017 later reclaims the `CC_FRONTEND` capability bit reserved here.

Status: Accepted (forward-compat)
Date: 2026-05-25
Superseded in part by [ADR-0017](./0017-tui-not-protocol-privileged.md): tmux control mode is no longer reserved as a compat option and the CC_FRONTEND capability bit is reclaimed; the frontend-agnostic principle stands.
See [ADR-0013](./0013-libghostty-bytes-on-wire.md) and [ADR-0015](./0015-protocol-layering.md) for the byte-stream L1 substrate that the "native wire" below now names.

## Context

Ghostty 1.3.0 ships partial tmux control mode (CC) parsing. The 1.3.0
release notes are explicit on both points:

> Significantly more tmux control mode parsing, but not hooked up to
> the GUI yet.

> Ghostty 1.4 will continue to iterate and improve the desktop
> application…we're working hard on making Ghostty scriptable, enabling
> a true Tmux control mode, graphical preferences, and more.

— *[Ghostty 1.3.0 Release Notes](https://ghostty.org/docs/install/release-notes/1-3-0)*, 2026-03-09

The parser scaffold lives at `src/terminal/tmux.zig` plus
`src/terminal/tmux/{control,layout,output,viewer}.zig` in the upstream
repo. iTerm2 has shipped a mature CC consumer against tmux for years.

This raises a real question for phux: should we pursue CC as a peer
frontend protocol, so that any CC-aware terminal (iTerm2 today,
Ghostty when 1.4+ binds its parser to the GUI) renders phux panes as
native splits of that terminal — chrome, GPU, tab bar and all?

It's tempting. It's also the wrong default. CC is fundamentally a
*degraded* version of what phux already does.

## Decision

**phux is TUI-first. The native wire protocol (SPEC §8; bytes-on-wire
for pane content + structured envelopes for everything else, per
ADR-0013) is the primary and only required frontend wire format, and
the reference TUI is the only consumer that has ever exercised it. CC
is reserved as an optional future adapter — no implementation, no
roadmap commitment, no v0.2+ promise.**

But: phux-server, phux-protocol, and phux-client MUST NOT preclude a
non-TUI frontend. The invariants below keep domain code free of
frontend-specific assumptions so that adding a second consumer later
is purely additive work, not a refactor. This is a *structural*
property — derivable from the wire format on paper — **not a
validated** one: no non-TUI consumer has been built, so subtle
TUI-coupling could survive undetected until one is. See "Validation
status" below.

## Why native > CC

phux's native wire (VT bytes + structured envelopes, ADR-0013) versus
CC's region-redraw + tmux-shaped layout messages. The native wire is
strictly more capable along the axes that matter to phux:

- **Predictive local echo** (ADR-0007) — phux clients hold a
  libghostty `Terminal` locally and speculatively `vt_write`
  keystrokes into it (or into an overlay), reconciling when the
  authoritative `PANE_OUTPUT` bytes arrive. CC has no equivalent
  signal and no way to surface the speculative/authoritative
  distinction.
- **Multi-attach with per-client viewport sizes.** SPEC §8.4
  snapshot fallback is per-client: `PANE_SNAPSHOT` carries a fresh
  VT replay byte sequence synthesized for the attaching client's
  geometry. CC's update stream assumes one consumer's geometry.
- **Satellite-routed sessions** (ADR-0007) — relies on the wire
  being ours to evolve, and on byte payloads being opaquely
  forwardable through a hub. CC freezes us against tmux's framing
  and forces re-encoding at every hop.
- **`AGENT_HOOKS` and future capability bits** (SPEC §6.2). Adding
  new ServerFeature variants is a one-line change on a wire we own.
  CC would have to invent extension mechanisms that aren't
  tmux-shaped.
- **Per-client byte-stream rewriting for capability downsampling.**
  Truecolor → 256 → 16 rewriting happens server-side per client
  (ADR-0013). CC has no equivalent — every CC consumer gets the
  same stream.

We also inherit libghostty's input/style atoms directly (ADR-0008),
and under ADR-0013 we inherit libghostty's grid model directly on
both ends of the wire. Every upstream Ghostty wire feature — new
keys, new SGR forms, selection APIs, Kitty graphics, sixel,
hyperlinks, future image protocols — lands on phux via `cargo
update` with zero protocol work. CC would re-translate those into
tmux-shaped messages and lose fidelity at the seam, every time.

## Why CC is worth reserving anyway

One scenario, and one only: a CC-aware terminal renders phux panes as
native splits without any phux UI being drawn. The user gets phux's
persistence + multi-attach + native wire protocol + satellite
roadmap, with Ghostty/iTerm2's native chrome and GPU rendering on
top.

That's a real win — but it's a *secondary* frontend, not a peer to
the native protocol. Our TUI client still wants the native wire
(bytes + structured envelopes, into a local libghostty `Terminal`).
Our future GUI client (DESIGN §14) still wants the native wire. CC
is "free integration with terminals we didn't write, when those
terminals do the work to consume it."

So we leave the door open. We don't walk through it until someone
walks through from the other side.

## Must-not-preclude invariants for v0.1

These are bugs in v0.1 even though no CC adapter is shipping.

1. **No frontend-specific assumption in domain code.** phux-server,
   phux-core, and phux-protocol never assume "exactly one frontend
   protocol exists." The frame emission path (SPEC §8) is the only
   place that knows about the native wire shape (PTY-byte forwarding
   + snapshot synthesis per ADR-0013); everything upstream (PTY pump,
   terminal state, layout) is frontend-agnostic. This is the *same*
   shape as the `Transport` trait pattern from ADR-0007 — multiple
   impls behind one boundary — applied at the rendering layer instead
   of the network layer.

2. **`ServerFeature` reserves `CC_FRONTEND`.** SPEC §6.2 grows one
   bit, unset in v0.1. The bit communicates "this server can speak
   tmux control mode in addition to native" to clients during HELLO.
   The wire cost is zero (it's one bit in an existing bitset). The
   architectural cost is zero (no code path consumes it yet).
   Precedent: `AGENT_HOOKS` (SPEC §6.2) does the same thing — reserve
   the capability today, ship code later, never break wire format
   when we do.

   *Overturned 2026-05-26 by ADR-0017:* the `CC_FRONTEND` bit is
   reclaimed. The bit slot is reserved (unassigned), not held for a
   CC adapter. A future CC consumer mounts L1/L2/L3 like any other
   consumer; it does not get a dedicated capability bit.

3. **Frame emission is not the only code path that touches pane
   state.** Today, the byte-forwarding path (ADR-0013) and the
   snapshot synthesizer that walks `grid_ref()` are the readers of
   pane state for the native wire. That's fine, but it's a convention,
   not an enforced invariant. If a future PR makes the snapshot
   synthesizer or the byte forwarder the *only* place that can read
   pane state (e.g. by moving the terminal handle behind a
   `PaneOutputEmitter` API), reject it. A CC adapter is another
   reader; the pane needs to keep being readable by N consumers.
   (Pre-ADR-0013 this invariant was phrased in terms of
   `phux-server/src/diff/compute.rs`; the principle is the same.)

4. **No PaneState field elides information CC would need.** The
   server retains screen-region info today as a byproduct of
   libghostty-vt's render-state. We do not optimize that away in v0.1
   on the assumption that only native-wire consumers exist. If we
   ever want to strip render-state for memory reasons, we add it
   under a feature flag, not as a default.

## When (and whether) to actually build it

Trigger conditions to revisit this ADR:

- **Ghostty 1.4 ships CC GUI binding and it's usable.** Verify with a
  manual smoke test against tmux first; if iTerm2's CC works in
  Ghostty 1.4, phux is one adapter crate away from working there too.
- **A concrete user wants phux ↔ iTerm2 today.** No theoretical
  demand; we need someone whose workflow this unblocks.
- **A contributor offers to build the adapter** and is willing to own
  its conformance tests against both iTerm2 and Ghostty.

Until one of those, the answer is: deferred indefinitely. The
invariants above are the entire investment.

## What's deliberately NOT in scope

Two things future contributors will propose. Both are rejected.

- **Ghostty pushes events into phux.** ("On split-created, Ghostty
  notifies phux-server so the new pane gets registered for
  persistence.") This requires Ghostty to grow a plugin host or
  event-bus protocol. Ghostty's stated integration surface is
  *scripting* — AppleScript on macOS, D-Bus on Linux, `ghostty
  +action` for CLI IPC — not a subscription API. Don't design around
  it; don't wait for it. The control arrow always goes *from* phux
  *to* Ghostty (via CC, when we ever build it), never the reverse.

- **phux-server embedded as a library inside Ghostty.** Tempting (one
  binary, no IPC) and would let phux ship as a Ghostty "plugin" if
  Ghostty ever supported plugins. Rejected because the IPC boundary
  is load-bearing for ADR-0003 (single server, many clients), ADR-
  0007 (satellites), and the headless-server property the multi-
  attach story depends on. Ghostty calling phux over a socket is
  fine; Ghostty linking phux as a library is the wrong layering.

## Tradeoffs

- **One unused ServerFeature bit on the wire forever.** Cost: zero
  bytes (it's a bit in an existing bitset). Same logic as the
  satellite tag byte on `SessionId` per ADR-0007: one-time invariant
  to keep a v0.2+ direction additive.
- **We commit to a discipline.** Future PRs that fuse frontend logic
  into domain logic ("just inline the diff emission into the PTY
  pump, it's faster") have to be rejected. Same discipline already
  applies for the Transport trait under ADR-0007; this ADR extends
  it to the rendering layer.
- **If Ghostty never ships CC GUI binding, the reserved bit is
  wasted.** Acceptable. The cost was zero.

## Alternatives considered

- **Skip the ADR; existing ADRs already imply frontend-agnosticism.**
  True in spirit, false in practice — none of 0001-0009 spell out
  "the rendering layer is replaceable." A future PR that hard-wires
  diff-emission into the pane lifecycle would not be obviously
  wrong against the existing ADR set. This ADR closes that gap.

- **Commit to building the CC adapter on a timeline.** Rejected.
  Ghostty 1.4's CC binding is announced, not shipped, and iTerm2 is
  not enough of a draw on its own to justify the conformance work.
  Better to leave the architectural seam and decide later than to
  commit work to an unverified target.

- **Reject CC permanently and remove the reserved bit.** Rejected.
  The cost of reservation is zero; the cost of un-reservation later
  (a wire-format break) is non-zero. ADR-0007 set the precedent for
  this asymmetry.

## Validation status (resolved 2026-05-30, path (b))

The "Decision" above is a claim about what the wire protocol permits.
It is not a claim about what has been demonstrated. Be honest about
which one you're reading.

What the wire protocol does: it carries pane-content as VT bytes
(SPEC §8, per ADR-0013), input atoms (SPEC §9), and capability bits
(SPEC §6.2) without any field that names "TUI" or assumes a
terminal renderer. Nothing in SPEC.md precludes a non-TUI consumer
— a byte-stream wire is in fact *more* friendly to non-libghostty
consumers (recording, inspection, replay) than the pre-ADR-0013
phux-defined cell semantics were.

What the reference implementation does: it serves exactly one client
shape, `phux-client`'s TUI renderer. No GUI client, no structured-
output exporter, no replay viewer, no accessibility consumer has
ever been built against this protocol. The "frontend-agnostic" claim
is therefore **structural** — derivable from the wire format on
paper — **not validated** — proven by a second consumer in the
field.

This distinction matters. Subtle TUI-coupling is exactly the kind
of bug that survives until a non-TUI consumer surfaces it. Four
things to watch:

- **`PANE_OUTPUT` is opaque VT bytes (ADR-0013).** A consumer that
  wants structured cells must either embed libghostty (one
  dependency line in Rust), wire up a `vte`-class parser, or
  operate at the byte-stream level. This is intentional — bytes
  on the wire are *more* friendly to non-libghostty consumers
  than a phux-defined structured cell grid would have been — but
  it remains structural rather than validated: no recording /
  inspection / replay consumer has been built against it.
- **`RenderingMode` in SPEC §6.2** (formerly splitting `Diff` from
  `VtReplay`) reduces to a single shape under ADR-0013. Any
  `RenderingMode` enum surfacing CC support is one new variant; the
  pre-existing `Diff` variant is dead and tracked for removal.
- **Input atoms (ADR-0006/0008) are re-exported from libghostty.**
  They reflect terminal-input semantics: KEY/MOUSE/FOCUS/PASTE with
  KIP-flavored modifiers. A GUI consumer would translate at the
  boundary. Whether that translation is lossless is unproven.
- **Status-bar widgets (DESIGN §6) emit ANSI.** Today widgets render
  to ANSI for the client to paste into a terminal cell row. A GUI
  consumer can't consume that — it would want a `StatusFrame` wire
  type carrying structured cells or higher-level intent. Nothing in
  this ADR says we ship one in v0.1, but the absence is a tell.

Resolution (phux-3dj): path (b). The earlier text deferred a choice
between (a) shipping a minimal non-TUI reference consumer before v0.1
— a structured-output exporter or a recording/replay viewer — and (b)
downgrading the Decision from "frontend-agnostic (validated)" to
"TUI-first, non-TUI not precluded." Nobody is building a non-TUI
consumer this pass, and (a) without follow-through would be theater,
so the Decision above is now stated as TUI-first. The must-not-preclude
invariants are retained in full; only the implication that they have
been *validated* is dropped.

The four risks listed above (VtReplay / non-VT renderers, grid-op
assumptions in `PaneDiff::ops`, lossiness of input-atom translation,
and ANSI-vs-structured status-bar output) remain **unvalidated**.
Revisit this section and re-open the question if a non-TUI consumer
is ever built; the structural invariants are the entire investment
until then.

## Related

- ADR-0013 — libghostty bytes on the wire (supersedes ADR-0002).
  The current substrate of the "native > CC" argument: bytes +
  structured envelopes + libghostty `Terminal` on both ends, giving
  automatic feature parity with upstream Ghostty and a wire CC
  cannot match.
- ADR-0002 — diff-based protocol (superseded; historical context for
  the original "native strictly better than CC" framing).
- ADR-0003 — single server, many sessions (the headless property a
  CC adapter would consume).
- ADR-0007 — Mosh-class transport + satellites (the same "trait for
  multiple impls" pattern, applied at the network layer; this ADR
  applies the same pattern at the rendering layer).
- ADR-0008 — re-export libghostty's input/style atoms (the reason
  upstream Ghostty wire features land on phux automatically; CC
  would lose this).
- SPEC §6.2 — ServerFeature bitset (where `CC_FRONTEND` lives).
- SPEC §8 — pane state synchronization (the native wire protocol —
  bytes-on-wire per ADR-0013 — that CC cannot match).
- DESIGN §14 — "Out of scope, but on the radar" (the user-facing
  surface where CC is mentioned).
- [Ghostty 1.3.0 release notes](https://ghostty.org/docs/install/release-notes/1-3-0) — the source for the CC roadmap quotes above.
- Upstream parser scaffold: `ghostty-org/ghostty:src/terminal/tmux.zig`
  and `src/terminal/tmux/{control,layout,output,viewer}.zig`.
