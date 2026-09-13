---
audience: contributors
stability: stable
last-reviewed: 2026-08-15
---

# 0087 — Elastic status-bar space is row-wide slack, not slot layout

**TL;DR.** The `spacer` widget is paid out of the status row's leftover
width — measured after every content-sized widget, split evenly across
every spacer in the bar, and zero the moment the row overflows. Slots
stay unsized placement rules; they do not gain a two-pass width budget.

Status: Proposed
Date: 2026-08-15

## Context

`[status]` composes three slots (`left`, `center`, `right`), each a list
of widgets concatenated with no separator. The composer measures each
slot's natural width, then *places* the results: left flush at column 0,
right flush against the last column, center centered in the gap that
survives. Slots are never given a width; they are positioned against one.

phux-be1m asked for a `spacer`: "flexible expanding space so users can
push widgets apart within a slot". That phrasing has no referent in this
layout. A slot has no boundary for a widget to expand to, so "within a
slot" is not a region — and the obvious repair, giving slots a width
budget and a two-pass layout, is a redesign of the placement algorithm
that also has to answer how a slot's claimed width interacts with the
narrowing priority order and the center gutters.

ADR-0071 freezes the widget vocabulary at 1.0, so `spacer` had to be
either defined properly now or left unbuilt. Shipping it as a fixed-width
pad would spend the name on semantics nobody asked for.

## Decision

A widget may declare itself **elastic** (`StatusWidget::elastic`). The
`spacer` kind is the only one that does, and the composer treats elastic
widgets as follows:

1. Elastic widgets render nothing from `render`, so they contribute zero
   to every slot's natural width and are invisible to the fitting pass.
2. On a row that **fits**, the leftover width — `width` minus the three
   natural widths minus the center gutters — is the row's **slack**. It
   is split evenly across every elastic widget in the bar, in reading
   order (left slot, then center, then right), remainder to the earliest.
   Each is then paid via `render_within(ctx, share)`.
3. On a row that **overflows** there is no slack, every spacer is handed
   zero, and the existing narrowing policy runs on the real widgets
   untouched.

Slots keep their current contract: unsized, placed, content-measured.

## Why

Slack is the only quantity in this layout that is both well-defined and
the thing users actually mean. "Push these apart" is a statement about
the *row*, not about a slot: the left slot's right-hand edge is wherever
the right slot's content starts, which is exactly what row-wide slack
computes. Defining it per slot would require inventing slot boundaries
that the placement algorithm does not have and that nothing else in the
chrome would honour.

Zero-natural-width also makes the feature free to ignore. Every existing
config composes byte-identically, because with no spacers the split hands
out nothing and the fits-path is the original code. And a spacer can
never push content off a narrow terminal — it is the first thing to
yield, without needing a `min-cols` the user has to remember.

Paying spacers through the existing `render_within` seam means a styled
spacer paints its gap for free: the registry's `style` decorator already
fills unstyled cells, so `style = { bg = ... }` colours the blanks.

## Tradeoffs

A bar that uses a spacer effectively gives up the center slot: the
spacers consume the gap the center is centered in, so a centered widget
is squeezed out. This is stated in `docs/consumers/tui.md` §8.4.2 rather
than prevented, because the center slot is the supported way to centre
something and a config using both is asking for two contradictory things.

Two spacers in different slots split slack evenly rather than by any
notion of which region "deserves" it. Even is arbitrary but predictable;
weighted shares would be a `weight` option, and that option belongs to a
design that has a reason for it, not to the freeze.

`StatusWidget` gains a method for one widget kind. That is a real smell,
mitigated only by the method being a default-`false` capability query
that the two decorators forward, so third-party widgets are unaffected.

## Alternatives

**Fixed-width pad.** `{ kind = "spacer", width = 4 }`: trivial, and
already expressible as a `text` widget of spaces. It would have spent the
name `spacer` on a meaning that forecloses the elastic one, which is the
one the bead wanted.

**Two-pass slot layout.** Give each slot an allotted width and let
elastic widgets expand inside it. This is the "real" layout engine, and
it requires deciding a slot's allotment before its content is known —
which is the question the current placement algorithm exists to avoid
answering. Rejected as a redesign disproportionate to one widget kind,
and one that could not be walked back after 1.0.

**Leave `spacer` unbuilt.** Defensible, and what phux-be1m's own notes
recommended in the absence of a decision. Rejected because the decision
turned out to be available: row-wide slack is well-defined, cheap, and
degrades correctly, so the freeze is better served by shipping it than by
leaving the name reserved indefinitely.
