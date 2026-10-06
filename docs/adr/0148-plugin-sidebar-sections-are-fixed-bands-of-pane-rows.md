---
audience: contributors
stability: stable
last-reviewed: 2026-10-05
---

# 0148 — Plugin sidebar sections are fixed bands of pane rows

**TL;DR.** A plugin manifest's `[[sidebar]]` entry declares a titled
section with a `format` template and a reserved `rows` count. The TUI
lays it between Agents and Sessions at a fixed height. Its rows are this
session's panes, each rendered from tokens the client already holds; a
pane appears only when every token it names resolves.

Status: Accepted
Date: 2026-10-05

## Context

Plugins already contribute status-bar widgets, palette actions, and
panes. The sidebar had no plugin surface.
[ADR-0112](./0112-stable-split-sidebar-navigation.md) fixed the strip as two
panels whose positions never move with population, and
[ADR-0140](./0140-sidebar-machines-come-from-a-hosts-provider.md) deferred
generic plugin sections. Any contribution must keep that spatial contract
and must not add a wire surface ([ADR-0017](./0017-tui-not-protocol-privileged.md)).

## Decision

1. **Schema.** `[[sidebar]]` takes `id`, `title`, `format`, and optional
   `rows` (default 3, range 1–8). Unknown keys are refused.
2. **Rows are panes.** Every leaf of every window in the attached
   session, in window then leaf order, is a candidate. `format` names
   tokens from a closed vocabulary (`agent`, `cwd`, `exit`, `index`,
   `state`, `title`, `window`) that resolve from the tab label, the OSC
   title, the cwd event, the OSC-133 exit code, and the `phux.agent/v1`
   record. A pane contributes a row only if every named token resolves.
   Manifest load rejects an unknown token with a did-you-mean.
3. **Fixed band.** A section occupies its header plus `rows`, whatever its
   population: an empty section shows the quiet dash, an overflowing one
   ends in an inert `+N`. Sections sit between Agents and Sessions and are
   laid only while those two keep eight body rows; the last declared
   section yields first. At most four sections are laid.
4. **Click focuses.** A row commits `focus-pane { window, pane }`.

## Consequences

- A section never moves Sessions. Agents and Sessions shrink by the
  band's height when a section is declared, not when it fills.
- No subscription, poll, or wire frame is added; sections update on the
  same chrome refresh as agent rows.
- A plugin cannot show arbitrary data. It can surface what panes already
  report: a process sets its OSC title, an agent record, or a command exit.

## Alternatives

- **Rows from an arbitrary L3 key.** Rejected for now: the TUI would need
  per-pane watches on plugin-chosen keys, and no CLI writes arbitrary
  metadata today.
- **A provider command per section,** like the hosts provider. Rejected:
  a poll and a subprocess per section to restate state the client
  already holds live.
- **Population-sized sections.** Rejected by ADR-0112's rule that status
  changes never move navigation.
