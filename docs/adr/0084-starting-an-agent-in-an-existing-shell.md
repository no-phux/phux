---
audience: contributors
stability: stable
last-reviewed: 2026-08-14
---

# 0084 — Starting an agent in an existing shell

**TL;DR.** `phux agent start` owns starting an agent in a pane that already
contains an available shell. It resolves the same integration argv as `phux
launch`, but quotes and types that argv into the live shell instead of spawning
it directly. By default it binds only the requested name, waits for a
post-submit detector publication to supply and verify the kind, and retains the
name whenever input delivery might have occurred.

Status: Accepted
Date: 2026-08-14
Builds on: ADR-0042 (direct-argv launch), ADR-0046 (server-side agent
detection), ADR-0053 (acknowledged input), ADR-0071 (the 1.0 CLI surface), and
ADR-0076 (transition-based agent waits)

## Context

`phux launch` creates a pane and can therefore hand a structured argv to
`SPAWN_TERMINAL`. Starting an agent in a pane that already exists is a different
operation. The pane's child is a live shell, phux does not own its layout or
working directory, and success means more than accepting bytes: the requested
agent must subsequently be identified by the detector.

Treating this as a placement mode of `launch` would hide the most important
semantic split. ADR-0042 deliberately excludes shell evaluation from `launch`;
an existing shell can only be driven by typing a command line. Conversely,
inventing a second integration resolver would let launch templates resolve to
different programs depending on which verb consumed them.

The operation also crosses an irreversible boundary. Before input reaches the
PTY, a provisional metadata bind can be removed safely. Once delivery might
have happened, removing the name can leave a running agent with no stable
handle. Readiness, identity, and rollback therefore have to be one contract.

## Decision

1. **Starting in place is the `agent start` verb, not a `launch` flag.** It
   creates, splits, moves, and focuses nothing. `launch` succeeds when a new
   pane exists; a waited `agent start` succeeds when an agent submitted to an
   existing pane has produced detector-backed identity and state.

2. **The integration owns the launch argv; the existing shell owns its
   evaluation.** `agent start` uses the same `resolve_launch` and
   `prepare_for_launch` path as `launch`, including session identity and
   environment preparation. It then renders every environment value and argv
   element as one POSIX-shell-quoted command line and submits that line plus
   Enter through one acknowledged `APPLY_INPUT` batch. Control characters,
   invalid environment names, an empty argv, and an overlong line are refused
   before the pane is touched. This is an intentional exception to ADR-0042's
   no-shell property, bounded to an explicitly selected live shell.

3. **An available shell requires positive prompt evidence.** With OSC 133
   shell integration, a `Prompt` or `Input` mark on the cursor row means the
   shell is available. Marks elsewhere but not on that row are positive
   evidence that it is not. No marks, an unreadable screen, or an unresolvable
   cursor make the answer unknown, not available. The verb fails closed for
   both “not available” and “unknown”; `--force` explicitly skips only this
   check. It does not skip name uniqueness, pane occupancy, cwd agreement,
   target locality, manifest, or delivery checks.

4. **The existing pane keeps its cwd.** `agent start` never types `cd` or
   otherwise changes a human's shell state. If the resolved integration cwd
   differs from the pane's known cwd, the command is refused and the caller
   should use a matching shell or `phux launch`.

5. **Detection kind and integration id are separate namespaces.** `--kind`
   selects the detection manifest and the identity that readiness must confirm.
   `--integration` selects the launch template. Because the config loader does
   not expose a total mapping between those namespaces, the integration id
   defaults by convention to the kind slug and can be overridden explicitly.

6. **A waited start requires a loaded detection manifest.** Without one, phux
   cannot identify the requested kind or make a readiness assertion, so it
   refuses before binding a name or typing bytes. `--no-wait` permits submission
   without that assertion, while preserving every other precondition.

7. **The provisional bind writes the name only.** The detector owns `kind` and
   derived `state`. Pre-declaring the requested kind would prevent composition
   from replacing a wrong value, allowing a different executable to satisfy
   readiness under a client-authored identity. The detector's first publication
   after submission must move the bound record away from `unknown`; readiness
   then verifies that the detector-supplied kind equals `--kind`.

8. **Readiness is a post-submit publication, never a pre-existing level.** The
   wait observes a transition from the bound `unknown` record into any derived
   state. It does not succeed merely because a pane was already `idle`, and it
   does not depend on whether idleness came from a positive rule, such as
   Claude's OSC 9;4 signal, or from the detector's fail-safe. Timeout and kind
   mismatch happen after submission and therefore retain the bind.

9. **Rollback follows proof of non-delivery.** If `APPLY_INPUT` proves that no
   bytes reached a PTY, the client removes the bind only after a
   read-compare-delete confirms it is still the exact record this invocation
   wrote. A transport failure, `INPUT_DELIVERY_UNKNOWN`, an unexpected reply,
   timeout, departure, or kind mismatch cannot prove non-delivery and retains
   the name. Leaving an inspectable handle is safer than orphaning a possibly
   running agent. A concurrent writer's changed record is never deleted.

## Consequences

- `agent start` is intentionally less general than typing arbitrary text: it
  accepts only a resolved integration, a bounded quoted line, and an exact
  existing target.
- Shell integration is the only client-visible proof that a shell is at a
  prompt. Pushing this answer into authoritative pane metadata is additive
  future work; until then, `--force` is the explicit escape hatch.
- Integration authors can keep one launch template for new-pane and in-place
  starts, but must remember that the latter crosses a shell parser after phux
  quotes each word.
- A failed invocation can leave a name bound to an `unknown` record. That is a
  deliberate recovery surface, not leaked metadata: callers can inspect the
  pane and clear the name once they know no agent is running.

## Alternatives

**Add placement flags to `phux launch`.** Rejected because pane creation and
in-place shell submission have different success claims, safety preconditions,
delivery mechanisms, and failure recovery.

**Infer availability from process names or an empty-looking prompt.** Rejected:
process ancestry cannot prove which program owns the foreground, and screen
appearance without semantic marks cannot distinguish a shell prompt from an
interactive application. Unknown must fail closed.

**Write the requested kind into the bind.** Rejected because it turns a claim
into detector truth and can permanently bless the wrong occupant.

**Delete the bind on every failure.** Rejected because failures after possible
delivery can leave a live agent with no addressable name, while retaining the
bind preserves an inspectable recovery path.
