---
audience: contributors, agents
stability: stable
last-reviewed: 2026-05-28
---

# Doc conventions

**TL;DR.** Every fact lives in exactly one doc. Each doc has a job and
links to the others instead of restating them. Files declare their
audience, stability, and last-reviewed date in YAML frontmatter. CI
checks that the metadata is present and the links resolve. Read this
file before adding a doc, moving a doc, or asking "where should this
paragraph go."

---

## The doc system in one diagram

```
Orientation (cold-open, ~5 files, stable, ruthlessly small)
  README.md ─────────────► landing page + router
  docs/README.md ────────► public documentation router
  docs/CONCEPTS.md ──────► canonical "what is phux"
  docs/QUICKSTART.md ────► run it
  CONTRIBUTING.md ───────► how to work in the repo
  AGENTS.md, CLAUDE.md ──► agent shell hygiene, loaded every turn

Reference (one source of truth per concept, addressable)
  docs/spec/ ────────────► normative wire (proto / L1 / L2 / L3 / appendices)
  docs/architecture/ ────► process model, threading, transport, etc.
  docs/consumers/ ───────► tui.md, sdk.md (per consumer surface)
  docs/operations.md ────► errors, logging, telemetry, security

Decision (one decision per file, strict)
  ADR/ ──────────────────► Nygard template, ~150 line cap

Discipline (this file + CI)
  docs/CONVENTIONS.md ───► you are here
  scripts/check-docs.sh ─► mechanical enforcement
  just docs-check ───────► wired into `just ci`
```

Anything that's not in those four layers is scratch (see
[research/](#research-and-scratch-content) below).

---

## One fact, one home

The single recurring failure mode of the previous doc tree was the same
fact landing in four files and drifting independently. Each layer above
has a **single owner** for each kind of content:

| Question | Owner | Don't restate it in |
|---|---|---|
| What is phux? | `docs/CONCEPTS.md` | README, VISION, ARCH, DESIGN |
| What's the wire byte? | `docs/spec/*` | ARCH, ADRs (link instead) |
| How does the server process model work? | `docs/architecture/process-model.md` | SPEC, READMEs |
| What does the TUI's keybind syntax look like? | `docs/consumers/tui.md` | SPEC, README |
| Why did we pick X over Y? | `ADR/NNNN-*.md` | Anywhere else |
| What's the long arc? | `docs/vision.md` | README, CONCEPTS (link only) |

If you find yourself writing a paragraph that already exists somewhere
else, **link instead**. If the existing version is wrong, fix it in
place — don't fork.

---

## Frontmatter (required on every doc)

Every Markdown file under `docs/`, `ADR/`, `research/`, and every
top-level `.md` (AGENTS, CLAUDE, CONTRIBUTING) starts with YAML
frontmatter:

```yaml
---
audience: <one or more of: humans | agents | consumers | contributors>
stability: <stable | evolving | scratch>
last-reviewed: YYYY-MM-DD
---
```

**The repo-root `README.md` is the one exception.** It is the project
landing page on GitHub and on a future docs site; visible YAML at the
top degrades the first impression. The README declares the same
metadata via an HTML comment instead:

```html
<!--
audience: humans, contributors, agents
stability: stable
last-reviewed: 2026-05-28
-->
```

The `check-docs.sh` gate accepts this form for `README.md` only.

Field semantics:

- **`audience`**. Who is this written for? Comma-separated if more than
  one. The four values:
  - `humans` — end users (eventual; rare today)
  - `agents` — AI coding agents loaded into context every turn
  - `consumers` — downstream implementers of `phux-protocol`
  - `contributors` — people working in this repo
- **`stability`**. How often is this expected to change?
  - `stable` — changes are rare, reviewed carefully. README, CONCEPTS,
    the SPEC files, accepted ADRs.
  - `evolving` — actively in flux. Subcommand docs while the CLI
    surface is still settling.
  - `scratch` — work-in-progress, not authoritative. `research/`.
- **`last-reviewed`**. ISO date of the last full read-through.
  Bump it when you've verified the file is still correct end-to-end,
  not when you've made a small edit. This is the freshness signal CI
  surfaces to agents.

There is no `last-updated` field — `git log` already answers that and
mechanical edits shouldn't reset freshness.

---

## TL;DR block (required on every doc)

The first H1 is followed by a `**TL;DR.**` paragraph of roughly 50
words (hard cap: 75). It is the summary that:

- Lets a returning reader page in the gist before deciding to read on
- Lets an AI agent load only the summaries of many docs into context
  and decide which to expand

Rules:

- Self-contained — readable without the rest of the doc
- States the doc's *job*, not its outline. Bad: "covers sections on X,
  Y, Z." Good: "the normative L1 Terminal-substrate frames; every L1
  consumer implements these."
- No links inside the TL;DR. If you need to link, you're describing the
  outline, not the job.

---

## Where new content goes

A flowchart for "I have something to write":

1. **Is it a decision** — picking between viable options, or saying no
   to a design? → ADR.
2. **Is it a wire byte, frame, or normative behavior**? → `docs/spec/`.
3. **Is it the internal process / data / threading model**? →
   `docs/architecture/`.
4. **Is it a user-facing surface of a specific consumer** (the TUI's
   keybinds, the SDK's API)? → `docs/consumers/`.
5. **Is it errors, logging, telemetry, security**? →
   `docs/operations.md`.
6. **Is it the long arc / future shape**? → `docs/vision.md`.
7. **Is it "what is phux"**? → `docs/CONCEPTS.md`. Nowhere else.
8. **Is it the landing page / router**? → `README.md`.
9. **Is it scratch research that hasn't crystallized**? →
   `research/`, with `stability: scratch`.

If you can't place it on the flowchart, the doc system is missing a
home. File a bd ticket against the `[epic]` docs-restructure (phux-dfz)
or its successor before inventing a new top-level `.md`.

---

## ADR template

```markdown
---
audience: contributors
stability: stable
last-reviewed: YYYY-MM-DD
---

# NNNN — Short title

**TL;DR.** ~50 words: the decision, in one paragraph.

Status: <controlled vocabulary — see below>
Date: YYYY-MM-DD

## Context
What is the situation that calls for a decision?

## Decision
What was decided.

## Why
Why this and not the alternatives. Be specific — "performance" is not
a reason.

## Tradeoffs
What we give up. Often the most useful section to future-you.

## Alternatives
One short paragraph per real alternative. Not an essay.
```

Hard cap: **~150 lines**. If a decision needs more, the body belongs in
`docs/architecture/` and the ADR points at it.

### `Status:` controlled vocabulary

One line. Exactly one of:

- `Proposed` — drafted and under review, not yet ratified. The gate and
  the ADR template both accept it; an ADR sits here until it is accepted
  or withdrawn.
- `Accepted`
- `Accepted (forward-compat)` — the invariants are committed to, the
  implementation is deferred to a later milestone
- `Superseded by ADR-NNNN`
- `Deprecated`

No multi-line statuses. No prose qualifiers on the line. If a
qualification is important, it goes in the TL;DR or the body.

### When to write an ADR

- Picking between viable approaches with long-term consequences.
- Closing off a design space (deciding *against* something).
- Anything you'd want to explain to a new contributor on day one.

### When NOT to write an ADR

- Bug fixes.
- Refactors that don't change behavior.
- Anything purely internal to a single function.
- "I made a small architectural tweak." — that's a code comment or a
  PR description, not an ADR.

---

## Implementation status

phux is spec-first, so `docs/spec/` and `docs/consumers/` routinely describe
behavior before it exists. That is legitimate. What is not legitimate is a
reader — human or agent — being unable to tell which sentences are shipped.
The failure has happened: `docs/consumers/tui.md` §10 specified a
`phux capture --record` verb and a server-side output tee as accepted design,
asserted them in the present tense, and was never built. It contradicted what
eventually shipped, and only human review caught it.

### The rule

**In `docs/spec/` and `docs/consumers/`, any described surface the reference
implementation does not provide carries an implementation-status marker, and
the marker names a code symbol so CI can check it.** Design with no marker
reads as shipped. Design that is not yet worth marking belongs in `ADR/`,
which is the tier whose whole job is to hold decisions ahead of code.

### The marker

The vocabulary is the one `docs/spec/proto.md` §7 already used for its
message-catalog `Status` column, promoted from informative to checked:

- `shipped` — the reference implementation provides it.
- `partial` — on the wire or in the code, but at least one end does not yet
  produce or consume it.
- `spec-only` — described here, no implementation.

Two carriers, one mechanism:

**Catalog tables.** A table row whose last cell is one of the three words is
resolved against the wire constants in `crates/phux-protocol/src/wire/`:
`shipped` and `partial` require `TYPE_<NAME>` or `COMMAND_TAG_<NAME>` to
exist, `spec-only` requires that it not. The gate fails in *both* directions,
so implementing a `spec-only` frame and forgetting to flip the row is a
CI failure rather than a slow rot.

**Prose sections.** A section with no catalog row carries a machine-readable
comment immediately above a reader-visible callout:

```markdown
<!-- impl-status: spec-only; probe: RolePolicy -->
> **Status: spec-only.** Nothing in `phux-server` implements roles or
> takeover; every subscription behaves as `PRIMARY`.
```

`probe` is one or more comma-separated Rust identifiers (`::` and `-`
allowed, so `Command::Windows` and a `"kebab-case"` string literal both
work). The gate searches every `crates/**/*.rs` line that is not a comment:
`spec-only` requires zero matches, `shipped` and `partial` require at least
one. A `> **Status` callout without a marker comment above it is itself a
violation — an unchecked status claim is the thing this mechanism exists to
prevent.

### Why this and not the alternatives

A **cross-reference gate mapping every spec section to an implementing
symbol** was rejected: it demands a symbol for every paragraph, including the
many that describe emergent behavior no single item owns, and the maintenance
cost falls on every spec edit rather than on the rare unbuilt one.

**Banning unimplemented design from `docs/spec/` outright** was rejected
because the spec is normative and forward-looking by construction. `INPUT_RAW`
holds discriminant `0x13` precisely so nothing else takes it; that reservation
has to live in the spec, not in an ADR.

What is left is marking, and the only marking worth having is marking a
machine can falsify. No linter can catch an author who writes unmarked prose
about an unbuilt feature — that is uncatchable, and this mechanism does not
claim otherwise. What it does catch is the *stale* claim, in both directions,
which is the drift that actually recurs: prose that outranks reality long
after reality changed.

---

## Style rules

These are the house contract, not suggestions. The standing complaint
about the docs is over-produced AI prose; the rules below exist to keep
it out. They apply to every committed doc unless a rule names a narrower
scope.

### Mechanics

- **No emojis in committed files.** Plain prose only. (Inherited from
  CONTRIBUTING.md and the existing repo norm.)
- **No future-tense in stable docs.** If a `stable` doc describes
  something not yet implemented, say so explicitly — don't write as if
  it exists. `evolving` stability is the safety valve; "designed, not
  built" is the honest phrasing.
- **Wire docs use SHALL / SHOULD / MAY** per RFC 2119, only in
  `docs/spec/`. Outside the spec, prefer plain prose.
- **Cross-reference by relative path**, not by URL. The dead-link gate
  checks relative paths; URLs are not verified and silently rot.
- **Don't restate the type system.** Module and item docs explain
  *intent* (why this exists, what the constraint is, who calls it).
  The type signature already explains *what*.
- **No internal ticket IDs or commit SHAs in prose.** The one exception:
  you may reference a tracked bead by id when pointing at known future
  work (name it as tracked work; do not describe its contents).

### Wordmark and voice

- **The wordmark is always lowercase `phux`**, even at the start of a
  sentence.
- **Voice is terminal-native: precise, dry, complete sentences.** No
  superlatives ("killer feature," "centerpiece," "blazing,"
  "revolutionary"). No curt one-word fragments standing in for an
  argument. No "load-bearing" as a filler intensifier.
- **No hype absolutes about a pre-alpha system** ("cannot degrade,"
  "phux will not," "tmux structurally cannot"). Argue the architecture,
  not the slogan; don't repeat coined taglines verbatim.

### Honest maturity

- **phux is pre-alpha and spec-first.** State what works *today* versus
  what is a direction. Never write unbuilt behavior in present tense in a
  `stable` doc.
- **Divergence honesty.** When the code and the target shape disagree,
  state the current code reality, mark the divergence inline, and point
  at the ADR that owns the target plus its tracking bead. Never document
  aspiration as shipped, and never silently drop a roadmap capability.
- **No competitor comparison tables** built on unverifiable claims.
  Positioning is the substrate-vs-product argument
  ([ADR-0009](../ADR/0009-phux-vs-mux-positioning.md)) in plain prose.

### TL;DR discipline

The `**TL;DR.**` block (see above) states the doc's *job* in 75 words or
fewer, is self-contained, carries no links, and is **not** repeated as
the first body paragraph. A TL;DR followed by a paragraph that restates
it is padding — cut one.

---

## CI enforcement (`just docs-check`)

The discipline layer is mechanically checked. See
[`scripts/check-docs.sh`](../scripts/check-docs.sh). Current gates:

| Gate | What it catches |
|---|---|
| frontmatter-present | A doc missing the YAML header |
| frontmatter-valid | Invalid `audience` / `stability` / `last-reviewed` |
| tldr-present | A doc whose first non-header content isn't `**TL;DR.**` |
| dead-link | A relative link that doesn't resolve |
| adr-status | An ADR with a non-vocabulary `Status:` line |
| spec-version-sync | `docs/spec/CHANGELOG.md` head version vs `phux-protocol`'s declared protocol version |
| impl-status | A `shipped` / `partial` / `spec-only` claim in `docs/spec/` or `docs/consumers/` that the code contradicts, and a `> **Status` callout with no marker behind it |

All run under `just docs-check`, which is in `just ci`. Adding a check
is welcome — open a PR against `scripts/check-docs.sh` and reference
this file.

---

## Research and scratch content

`research/` is the explicit scratch tier. Files there:

- Have `stability: scratch` in their frontmatter
- Are NOT linked from any `stable` doc
- Get moved to `research/archive/` (or deleted) when ratified by an ADR
  or absorbed into a reference doc

If a research note has been overtaken by an ADR, leave a top banner
pointing at the ADR before archiving, so search hits land somewhere
useful.

---

## When this file is wrong

You found a case where:

- The flowchart doesn't have a home for a real piece of content
- A "one home" rule is creating perverse linking
- A CI gate is too strict (or too loose) for a real workflow

File a ticket against the docs epic (or its successor) before
inventing a workaround in tree. This file is the contract; drift here
turns the whole system back into the mess it replaced.
