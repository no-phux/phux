---
audience: contributors
stability: evolving
last-reviewed: 2026-06-06
---

# 0028 — Runtime log control

**TL;DR.** Logs are an operator surface and a leak surface. phux gives
operators structured, leveled logging (`tracing` + `RUST_LOG` /
`PHUX_LOG` / `PHUX_LOG_FORMAT`), makes input atoms **self-narrating and
redaction-safe** so a `trace!(?input)` records a keystroke's shape but never
its text or pasted bytes, and creates log sinks `0o600`. Delivered in slices;
Slice 1 lands the redaction + sink hardening + the keystroke-leak fix.

Status: Accepted (forward-compat)
Date: 2026-06-06

## Context

`tracing` is already the logging substrate (`telemetry::init` /
`init_client`), with `RUST_LOG`, `PHUX_LOG`, and `PHUX_LOG_FORMAT` knobs
([operations.md]). Two gaps make it unsafe and incomplete to lean on:

1. **It leaks secrets.** Client→server input is structured atoms (ADR-0024):
   `KeyEvent` carries the layout-resolved `text`, `PasteEvent` carries the raw
   clipboard `data`. Both derived `Debug`. The server's PTY-handoff path logs
   `trace!(?input, …)` when a pane has no PTY or the encoder gates an event —
   so at `RUST_LOG=phux=trace` a user's typed passwords and pasted secrets land
   verbatim in the log file. That file was created at the default `0o644`,
   group/world-readable on a shared box — exactly the multi-user box
   [operations.md] names as the trust model.

2. **No first-class operator story.** There is no `phux logs` discovery verb;
   an operator must know the per-pid path convention to find the client log.

[operations.md] is the home for the logging *facts*; this ADR is the home for
the *decision* to treat logs as a controlled, non-leaking operator surface and
to phase the work.

## Decision

phux treats logs as a deliberate operator surface with a redaction invariant:

1. **Self-narrating input atoms.** The atom types own a redaction-safe
   narration. `KeyEvent` and `PasteEvent` get hand-written `Debug` impls that
   emit only structural facts (action, physical key, modifiers, `text_len` /
   `data_len`) — never the literal text or bytes. `InputEvent::narrate()` is the
   one-line companion. Because the leak is the *type's* `Debug`, every
   `{:?}`/`?input` call site is fixed at once, including ones this ADR's PR must
   not touch.
2. **Log sinks are `0o600`.** Both `telemetry` file paths create the sink
   owner-only before any appender writes, and re-tighten a pre-existing looser
   file. No-op on non-Unix.
3. **Leveled / structured logging stays the operator dial** — `RUST_LOG`
   filters, `PHUX_LOG_FORMAT=json` for machine parsing. Unchanged; documented
   as the runtime control surface.

### Slice roadmap

- **Slice 1 (this PR):** input-atom narration + redaction-safe `Debug`; fix the
  pre-existing `trace!(?input)` keystroke/paste leak (closed automatically by
  (1)); harden sinks to `0o600`; tests asserting a sentinel secret never appears
  in narration/`Debug` and that the sink is `0o600`.
- **Slice 2+ (forward-compat):** a `phux logs` discovery/tail verb (locate and
  follow the active sink); per-target log-level control at runtime over the
  control plane; optional audit hooks ([operations.md] "no audit logging"
  limitation). These do not change the Slice 1 invariants.

## Why

The redaction belongs on the *type*, not at each call site: call sites forget,
and one forgotten `?input` re-leaks every keystroke. A hand-written `Debug` is a
structural guarantee the compiler routes every formatter through. `0o600` is the
cheapest possible fix for a file that, even redacted, carries timing and
structural detail an attacker on the same box should not get. Slicing keeps the
security-critical fix shippable now without blocking on the larger `phux logs`
UX.

## Tradeoffs

- A hand-written `Debug` can drift from the struct (a new secret-bearing field
  added without redacting it). Mitigated by the sentinel-secret tests and a
  documented invariant; a field whose content is sensitive must be summarized,
  not printed.
- `narrate()` and the `Debug` impls are slightly more verbose than a derive, and
  `narrate()` is a small additional surface to keep in sync with `Debug`.
- `0o600` pre-creation adds one `open`/`chmod` per process start — negligible.

## Alternatives

- **A `tracing` redaction layer / field visitor.** Strip sensitive fields in the
  subscriber. Rejected: it only protects fields we remember to name, runs for
  every event, and leaves the raw value reachable by any other formatter; the
  type-level fix is narrower and total.
- **Drop the `?input` logs entirely.** Loses a genuinely useful "input arrived
  but was gated/discarded" diagnostic; redaction keeps the signal without the
  payload.
- **Encrypt the log file.** Far more machinery than the threat (a same-UID-box
  reader) warrants; `0o600` + the kernel boundary ([operations.md]) is the
  proportionate control.

[operations.md]: ../docs/operations.md
