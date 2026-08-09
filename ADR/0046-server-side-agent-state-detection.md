---
audience: contributors
stability: stable
last-reviewed: 2026-08-08
---

# 0046 — The server derives agent state; detection is level-triggered

**TL;DR.** ADR-0040 defined `phux.agent/v1` but left it with no writer, so
`state` stayed `unknown` forever. The server now derives it: a per-terminal
detector re-reads the OSC title and the live screen on a timer, matches
region-scoped declarative rules, and writes the record. An explicit
`SET_METADATA` supplying `state` outranks the detector for as long as the pane
holds the agent it names. Unmatched means `idle`, never `blocked`.

Status: Accepted
Date: 2026-07-12

## Context

[ADR-0040](./0040-agent-identity-metadata.md) put agent identity and lifecycle
in one normative L3 record, `phux.agent/v1`, and named the verbs that carry it.
It did not say who writes it. In practice nobody does: the only writer that
shipped is a human running `phux agent set`, so `state` reads `unknown` on
every pane, and the reference TUI's sidebar falls back to an OSC-title
substring guess that can only ever produce `idle`. The states the record
defines — `working`, `blocked`, `done` — have never been displayed.

[ADR-0036](./0036-agent-asked-detection.md) placed a passive screen detector
last in its source ladder, behind the `phux-ask` sentinel and an opt-in hook.
That ordering is right for the `Asked` *event*, which needs an id and a
question string only the agent knows. It is the wrong ordering for a *state*
value, which every pane needs continuously and which no agent CLI reports.

## Decision

1. **The server is a producer of `phux.agent/v1`.** One detector per PTY-backed
   Terminal, driven by its own interval on the terminal actor's `select!`
   (adaptive: ~500 ms unidentified, ~300 ms identified, ~100 ms while confirming
   a `working` → `idle` transition). PTY bytes never wake it, so a chatty agent
   costs no extra detector work; the grid scan is skipped whenever the actor's
   dirty flag is clear and something has already been derived — a clean grid
   cannot yield a different answer than the last one, whatever that answer was.
   The dirty flag is consumed only by a tick that actually scans, so a tick that
   skips can never launder "no evidence" into a derived state.
2. **Detection is level-triggered.** Each tick re-derives the state from
   scratch — identify the PTY's foreground process, read the title, read the
   live viewport, match rules, publish if the answer changed. Nothing is
   remembered but the last published tuple.
3. **The title outranks the screen.** libghostty already parses OSC 0/2 and the
   actor already mirrors it; the detector reads that mirror, and a
   title-derived rule beats any screen-derived rule.
4. **Rules are region-scoped data, not code.** A rule is
   `{id, state, priority, region, predicate-tree, flags}`, shipped as a TOML
   manifest per agent kind and compiled once at load. Regions are structural
   sub-slices of the *live* screen — the title, the bottom non-empty lines, the
   text below the last horizontal rule, the body of the live prompt box — never
   the scrollback. An invalid manifest is logged at `warn` and dropped whole.
5. **Fail safe.** An identified agent whose screen matches no state-bearing
   rule is `idle`. Never `blocked`. A rule may declare `skip-state-update`,
   meaning the screen is a transcript viewer, pager, or picker and carries no
   information about the agent: freeze the last state rather than guess.
6. **Asymmetric hysteresis.** Transitions that demand a human are instant;
   transitions that release one are debounced. `blocked` and `working` publish
   on the first tick that sees them. `working` → `idle` — the ambiguous
   direction, where a spinner may merely have been cleared mid-redraw — holds
   for three confirmations at ~100 ms, capped at ~700 ms, bypassed when a rule
   shows positive idle evidence. A ~3 s startup grace suppresses publication
   while an agent paints its splash.
7. **Publication is edge-filtered.** The record is written only when the derived
   `(kind, name, state)` tuple changes. An agent that is `working` and spewing
   output for ten minutes produces zero metadata writes and zero
   `METADATA_CHANGED` frames.
8. **Authority, bounded by the occupant.** *An explicit `SET_METADATA` on
   `phux.agent/v1` that supplies a `state` outranks the detector, which makes no
   further derived write to that Terminal while the pane still holds the agent
   the declaration describes. An explicit write that supplies only identity
   (`name` / `kind` / `session`) is preserved field-for-field and the detector
   fills `state` around it. The detector deletes only records it itself wrote.*
   On **positive evidence** that the declared occupant is gone — an occupancy
   read that succeeded and found no agent, or a different one, never a read that
   failed — the detector **withdraws** the declaration instead: `state` becomes
   `unknown`, `attention` is cleared, `name` / `kind` / `session` survive, the
   record is never deleted, and the derivation resumes from there. Amended; see
   Tradeoffs. The normative wording is [`docs/spec/L3.md`](../docs/spec/L3.md)
   §3.7.
9. **Built-ins are capture-backed.** The binary ships manifests for Claude
   Code, Codex, OpenCode, Pi, and OMP. Every screen predicate is pinned to an
   unedited `phux snapshot --json` viewport captured from the named CLI; idle,
   working, blocked, and historical-dialog false-positive behavior are tested
   before a manifest ships.
10. **Staleness is answered by re-identification, not a TTL.** Identity is
   re-derived every ~5 s from the PTY's foreground process group, and a vacancy
   is confirmed across consecutive reads before anything is given up. When the
   agent is gone, a record the detector owns is deleted and a declared record is
   withdrawn to `unknown` (point 8). A dead process does not keep a live badge,
   whoever wrote it. Nothing in the record is timestamped: liveness lives in the
   detector, which is what keeps point 7's idle fleet at zero writes.
11. **An occupant change is a correction, not a retraction.** A pane whose
   foreground agent changes kind, or restarts under a new process group, is
   reported as a re-identification: one `SET` rewrites the `kind` and `name` the
   detector authored and pairs them with `unknown` — the one state that cannot
   describe the wrong process — while a `kind` or `name` an explicit writer set
   is preserved. A retraction would broadcast a tombstone and open a hole a
   waiting consumer reads as death. The correction is a latency improvement and
   not the correctness mechanism: every derived write reasserts the detector's
   current `kind`, so a dropped correction heals on the next publication.

No wire change: no frame, tag, `AgentEvent` variant, or `PROTOCOL_VERSION` bump.
The detector rides the shipped `SET_METADATA` / `METADATA_CHANGED` path.
`AgentEvent::Asked` (ADR-0035/0036) keeps its own source ladder untouched — the
detector writes metadata only, having no honest question text to report.

## Why

**Level-triggered, and this is the reason the rest follows.** The cheap design
is an edge-triggered reporter: a hook that fires on start and stop.
Edge-triggered reporters are lossy. One missed `stop` — the agent was killed,
the hook crashed, the wrapper was bypassed — and the pane is wedged in a lie
with no path back to truth, because nothing will re-assert the correct value. A
periodic re-derivation from the screen re-establishes ground truth on the next
tick and is self-healing by construction. The trade is explicit: we accept a
scan every few hundred milliseconds per agent pane and the fuzziness of matching
text a third party is free to restyle, in exchange for a state value that cannot
get permanently stuck. Hooks stay welcome as an *additive*, higher-confidence
source — decision point 8 is the door they walk through — but they may not be
the only source.

**Regions, not the whole screen.** The string "do you want to proceed?" inside a
diff in the scrollback means nothing. The same string inside the live prompt box
means the agent is blocked. Scoping a predicate to a structurally derived
sub-slice turns a fuzzy text problem into a mostly structural one, and it is
what makes a hand-written manifest defensible rather than a pile of substrings.

**Fail safe toward idle.** The two error directions are not symmetric. A missed
notification costs a glance at a pane the user was going to visit anyway. A
false `blocked` leaves a red row that never clears, and a sidebar that cries
wolf is worse than no sidebar: the user stops reading it.

**Rules as TOML.** Agent TUIs churn on their own cadence. A manifest a user can
edit, and one we can replace in a patch release, decouples that churn from ours;
a `match` arm in the server does not.

## Tradeoffs

- **Points 8 and 10 contradicted each other as accepted.** Point 8 made a
  declaration outrank the derivation "until the record is `DELETE`d" — an
  edge-triggered handoff, the shape the Why section rejects — while point 10
  claimed a dead process does not keep a live badge, which the code could honor
  only for records the detector owned. The implementation resolved that toward
  8, so a declared pane whose agent was `SIGKILL`ed stayed `working` until the
  pane closed, and the shipped Claude hook shim declared a state on every hook,
  which put every Claude pane in that bucket. Withdrawal is the narrowest
  repair: no derived value is written over a declaration and no record the
  server did not author is deleted, so this adds one permission rather than
  reversing a rule. Its price is that a declaration is no longer unconditionally
  durable — declare state about a pane you do not occupy and it can be withdrawn
  from under you.
- **Screen matching is brittle.** An agent restyles its prompt box and the
  manifest goes stale. The decay is bounded by the fail-safe — toward `idle`,
  not toward a permanent alarm — and `PHUX_AGENT_RULES_DIR` lets a user fix a
  manifest without a release. `PHUX_AGENT_DETECT=0` disables detection entirely.
- **A manifest rule is only as good as the screen it was written against, and
  it fails SILENTLY.** This bit us during implementation: the first draft was
  written against an imagined Claude TUI — a box-drawn dialog, a `? for
  shortcuts` idle hint — and every screen rule in it matched *nothing* in the
  shipped CLI. Nothing failed loudly. `idle` is the fail-safe, so a pane sitting
  on a live permission prompt simply reported `idle` forever, and the unit tests
  went green because they fed the matcher the same invented screens the rules
  were written from. A synthetic screen tests the matcher against itself. The
  rule is therefore: **every screen rule must be justified by a captured
  viewport, committed as a golden fixture** (`src/agent_detect/fixtures/`), and
  a rule whose provenance cannot be restated must be deleted rather than
  guessed. Three were.
- **An idle pane is no longer strictly free.** It is close: a timer wake, plus a
  foreground-pgid read every ~5 s, with the grid scan skipped while clean.
- **Process introspection is platform-specific.** Foreground pgid is portable;
  argv is `/proc` on Linux and a `sysctl` on macOS. Elsewhere the detector never
  identifies an agent and stays quiet.
- **The server now interprets bytes it previously only stored.** L3's opacity
  rule still governs the read/write path, but not this one conventional key
  ([`docs/spec/L3.md`](../docs/spec/L3.md) §3.7 says so). Confining the exception
  to a single normative, ADR-owned key keeps
  [ADR-0017](./0017-tui-not-protocol-privileged.md)'s dumb-server stance intact
  everywhere else.
- **The client had to change with it.** Every agent-state change previously
  routed to a full-frame repaint, harmless only because the state never changed;
  a live detector on that path is a full-screen clear per tick. This work
  therefore also implements the `RepaintLevel` accumulator
  [ADR-0029](./0029-one-cursor-authority-and-repaint-scheduler.md) accepted and
  never built, plus an in-place chrome paint. This ADR supersedes nothing.

## Alternatives

**Agent-side hooks as the only source.** Rejected: lossy and unrecoverable per
the argument above, and it makes the sidebar blank until every agent vendor opts
in. Retained as an additive source that outranks the detector.

**A phux-owned wrapper every agent launches through.** Reliable where it is
used, absent where it is not — and a user who types `claude` at a prompt has
bypassed it. The detector must work on the pane the user actually spawned.

**Client-side detection.** Rejected: every consumer would re-derive the same
answer with a different heuristic — the drift ADR-0040 set out to end — and a
detached pane would report nothing.

**A new `AgentEvent::StateChanged`.** Rejected for the reason ADR-0040 already
gave: an event leaves a late subscriber nothing to read back, and an open
vocabulary on a positional wire body is a versioning trap.
