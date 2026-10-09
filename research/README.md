---
audience: contributors, agents
stability: stable
last-reviewed: 2026-09-09
---

# research/

**TL;DR.** Scratch tier per [`docs/CONVENTIONS.md`](../docs/CONVENTIONS.md).
Active reference notes live here; ratified findings move to
[`archive/`](./archive/) with a top banner pointing at the ADR that
absorbed them. Nothing in `research/` is authoritative — for current
behavior, follow the cross-link to the ADR or to the relevant
`docs/` reference doc.

## Files

- [`2026-10-09-rex.md`](./2026-10-09-rex.md) —
  October 2026 public surface of Rex, and the phux gaps still open
  against it.
- [`2026-09-20-cockpit-craft.md`](./2026-09-20-cockpit-craft.md) —
  scratch direction for the Cockpit craft work (`phux-3gpg`).
- [`2026-09-09-superlogical-demo/`](./2026-09-09-superlogical-demo/README.md) —
  reconstruction of a Superlogical remote-host terminal demo and the phux gap
  map that turned it into beads.
- [`2026-09-09-ci-compute-audit.md`](./2026-09-09-ci-compute-audit.md) —
  CI and release compute measurements behind the current build ownership.
- [`2026-08-15-agent-detection-fixture-audit.md`](./2026-08-15-agent-detection-fixture-audit.md) —
  what each agent-detection fixture depicts, and the re-capture checklist
  (`phux-w7z2.40`).
- [`2026-08-12-osc-9-4-claude-code.md`](./2026-08-12-osc-9-4-claude-code.md) —
  raw-byte capture of Claude Code's OSC 9;4 progress reports; the captures in
  the sibling directory are test fixtures.
- [`2026-05-25-libghostty-renderstate.md`](./2026-05-25-libghostty-renderstate.md) —
  `libghostty-vt`'s `RenderState` read API and dirty-tracking model.

## archive/

Ratified or superseded notes, each with a banner linking to its replacement.

- [`archive/2026-05-26-state-sync-algorithm.md`](./archive/2026-05-26-state-sync-algorithm.md) —
  ratified by [ADR-0018](../docs/adr/0018-lazy-state-synchronization.md).

## Conventions

- Every file declares `stability: scratch`.
- No file in `research/` is linked from any `stable` doc.
- When a note is ratified by an ADR or absorbed into a reference doc,
  move it to `archive/` with a banner pointing at the new home.
