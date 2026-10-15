---
audience: contributors
stability: stable
last-reviewed: 2026-10-15
---

# 0172 — TUI actions and background jobs are declared once

**TL;DR.** A TUI action is one row in `phux-tui`'s action registry: name,
reference text, palette placement, and the handler `run_action` calls.
phux-config keeps the action names as config vocabulary, and a test pins
the registry to them. Work an action runs off the input loop is a `Job`
on one facility, with a per-kind in-flight guard. The driver receives every
job's result in one `select!` arm as a `JobDone`: a toast, plus an optional
inventory refresh.

Status: Accepted
Date: 2026-10-15

## Context

Adding `organize` (ADR-0170) to the TUI touched six places for one action:
the name in `phux_config::vocab::ACTION_NAMES`, a palette row in
`REGISTRY` (or a reason in `NON_PALETTE_ACTIONS`), a `run_action` match
arm, an `ActionEffects` field and its apply call, and the generated
reference. ADR-0026 accepted that multi-touch change as the price of its
drift test; this ADR amends its decision 5 so the registry row is the
declaration and the test guards only the config-owned names.

Running organize in the background took another channel pair on the
session loop, two `DispatchCtx` fields, a `Finished` enum merging two
receivers in the driver's `select!`, a handler per result type, and a
process-global `AtomicBool` to refuse a second run. Plugin actions had
already built the same plumbing once.

## Decision

1. **One row per action.** `action_registry::ACTIONS` is the only
   declaration of a TUI action: `name`, `description`, `params`,
   `palette` (`Row { title, category, args }` or `Hidden { reason }`), and
   `run`, the handler. Every handler takes one `ActionCall` (the action,
   the dispatch context, the focused pane, the pane slots, the effects).
   `run_action` looks the row up and calls it; there is no match. The
   palette, the which-key labels, and `docs/reference/actions.md` render
   from the same rows.
2. **phux-config owns the names.** Config validation (keybindings, hooks,
   `phux config check`) needs the action vocabulary without depending on
   the TUI, so `ACTION_NAMES` stays in `phux_config::vocab`. A phux-tui test
   asserts that `ACTIONS` declares each name exactly once and nothing else.
   Adding an action is one name there, one row here, and `just docs-gen`.
3. **One background-job facility.** An action that runs work off the
   input loop puts a `Job` (a kind, an optional starting notice, a future)
   on `ActionEffects::job`. The effect pass starts it on `Jobs`, which
   refuses a job of an exclusive kind while another of that kind runs and
   otherwise spawns it. The future returns a `JobDone`. The driver awaits
   `Jobs::next` in one `select!` arm, shows the toast, and refreshes the
   host inventory when asked. The production attach shares one `Jobs` per
   process, so a job's guard and result survive session switches and
   reconnects.

## Why

One declaration cannot drift: a row has its handler, and the agreement
test fails at the single remaining place that can disagree, the
config-owned names. One job facility gives every future background action
its guard, notice, delivery, and driver handling. Without it, each action
adds a channel, a context field, and a select arm.

## Tradeoffs

- Dispatch is a linear scan over about sixty rows per resolved action, not
  a compiled match. That is negligible at keystroke rates.
- Handlers share one signature, so a handler may destructure fields it
  does not need with `..`.
- The process-wide `Jobs` is shared state. It is scoped to the facility
  and its test fixtures build their own.

## Alternatives

**phux-config generates the registry, or the TUI generates the names.**
The config crate would have to know about handlers, or a build step would
cross the dependency edge. Both cost more than one agreement test.

**Handlers spawn directly from `run_action`.** That keeps the effect field
out, but `run_action` would stop being synchronous and side-effect free,
and dispatch tests would need a runtime and would start real plugin
processes.
