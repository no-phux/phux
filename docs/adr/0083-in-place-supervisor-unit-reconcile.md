---
audience: contributors
stability: stable
last-reviewed: 2026-08-12
---

# 0083 — In-place supervisor unit reconcile

**TL;DR.** ADR-0080 corrected the generated unit's restart policy, but the only
way to apply that correction to an already-installed unit was `phux service
install`, which re-renders the unit from scratch — silently dropping flags it
cannot recover — and reloads it, which stops the server and every pane. `phux
service reconcile` replaces only the restart-policy keys of the installed file
and reloads nothing. systemd picks the change up without touching the running
service; launchd cannot, and the command says so rather than claiming a fix it
did not make.

Status: Accepted
Date: 2026-08-12
Builds on: ADR-0055 (`phux service install`), ADR-0080 (the corrected restart
policy), ADR-0071 (the frozen CLI grammar)

## Context

A unit written before ADR-0080 keeps `KeepAlive: true` / `Restart=always`, with
no throttle: a deliberately stopped server comes straight back, and a crashing
one respawns invisibly. `phux doctor` detects this and, until now, told the
user to re-run `phux service install`. Following that hint was expensive in two
independent ways, both confirmed in the code rather than suspected.

**The flags are not recoverable.** `--quic`, `--listen`, `--restore`, `--hub`
and `--socket` survive only inside the rendered unit. Nothing parses a unit back
into a `ServicePlan`, so a re-run renders a unit with every flag the operator
does not retype dropped — their remote listener and hub mode gone, discovered
days later from a device that will not attach.

**The reload is destructive.** `run_install` ends in `launchctl bootout` +
`bootstrap` (macOS) or `systemctl --user enable --now` (Linux). Both stop the
supervised server, and every pane, shell, agent, and subagent dies with it. The
recommendation arrived inside a `doctor` warning that exits 0 and reads as
routine housekeeping, which is when an unannounced destructive step does the
most damage.

So phux's own diagnostic recommended a remedy that could cost a user their
running work, to fix a defect they had no other way to clear.

## Decision

**Rewrite the keys, not the file.** `phux service reconcile` reads the
installed unit, replaces exactly the directives that carry the restart policy —
`KeepAlive` and `ThrottleInterval`; `Restart`, `RestartSec`,
`StartLimitIntervalSec` and `StartLimitBurst` — and leaves every other byte
where it was. No `ServicePlan` is built and no path is resolved, so the flags
problem is structurally impossible rather than carefully avoided: values that
are never re-derived cannot be dropped.

**Refuse what cannot be parsed.** A unit whose policy value is a shape the
patcher does not recognise is reported and left alone. A half-rewritten plist
is one launchd silently declines to load, leaving the user with no supervisor
at all — strictly worse than the policy being corrected.

**"Current" is defined by the patch, not by a predicate.** A unit is up to date
when patching it changes nothing. The generator and the reconciler emit the
policy block from one shared definition, so the two cannot disagree about what
current means, and a reconcile of a freshly installed unit is a proven no-op.

**Say what each platform can actually do.** systemd re-reads a unit file
without touching the running service, so `daemon-reload` puts the corrected
policy in force immediately and the command reports that. launchd cannot: a
loaded job keeps the policy it was bootstrapped with, and only `bootout` +
`bootstrap` replaces it, which stops the job. On macOS the command therefore
reports that the policy is **not** active yet, that it becomes active by itself
at the next login or reboot, and — with the exact commands — what making it
active now would cost, probing the socket the unit itself pins to say whether
any panes are at stake.

**`phux update` reconciles automatically.** Only because the operation is
non-destructive by construction. `phux doctor`'s hint and `install`'s
live-server refusal both point here.

## Why

A new verb rather than a mode of `install`, despite ADR-0071 freezing the CLI
grammar and making every addition a commitment. `install` is defined by a plan
resolved from flags and the ambient environment; reconcile resolves nothing and
reads the installed file. Expressing it as `install --reconcile` would need
every other install flag to conflict with it — and a flag that conflicts with
all of its siblings is a verb in disguise. It would also leave the live-server
refusal telling the user to re-run the command they just ran, differently.

The concept is durable, which is what makes it worth freezing: any future
correction to the supervision policy is applied by the same verb, to units
written by builds that predate the change.

## Tradeoffs

- macOS gets a *file* fix, not a *running* fix. Honest, and the corrected
  policy does arrive on its own at the next login — but a user who wants it now
  still pays the same price they always would have.
- The reconciler understands two file formats textually. It is deliberately
  narrow (a fixed set of keys, a refusal on anything else), but it is still
  parsing, and a plist shape nobody anticipated gets a refusal rather than a
  fix.
- One more verb inside the 1.0 freeze.
- Reconcile corrects the policy only. A unit that is stale in some other way —
  a moved binary, a renamed log path — still needs a reinstall.

## Alternatives considered

**Make `install` detect a legacy-but-live unit and take the in-place path.**
Rejected: `install`'s contract becomes "sometimes I honour my flags and
sometimes I ignore them", decided by state the user cannot see.

**Parse the installed unit back into a `ServicePlan`, then re-render.** This
would fix the dropped-flags problem while keeping one renderer. Rejected: it
makes every unit ever written an input format phux must keep parsing, and it
still rewrites bytes the operator may have edited. Replacing four keys needs no
such contract.

**Byte-diff against a freshly rendered unit to decide what to change.** The
renderers are pure and byte-stable, so the diff is meaningful in principle. In
practice `resolve_plan` bakes in `current_exe`, the installing process's
`PHUX_WS_*` environment, and an `XDG_RUNTIME_DIR`-dependent socket path, so
re-rendering from a different shell legitimately differs and the diff reports
false positives.

**Have `phux doctor --fix` do it.** Rejected: `doctor` diagnoses. A diagnostic
that mutates is a diagnostic people stop running.
