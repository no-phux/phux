---
audience: contributors
stability: stable
last-reviewed: 2026-07-27
---

# 0062 — Headless resize and the window-size policy

**TL;DR.** `phux resize TARGET COLSxROWS` sets a pane's grid with no TTY,
over the `TERMINAL_RESIZE` frame the wire already carries. An explicit resize
applies immediately even with a client attached, but does not permanently
outrank the `defaults.window-size` policy — `manual` is the one knob that
makes a size durable. The verb reads the server's real geometry back and
exits nonzero when it differs.

Status: Accepted
Date: 2026-07-27

## Context

Every path that sized a pane went through a *viewport*: a client attaches,
reports its outer geometry, and the server folds that report across every
subscriber under `defaults.window-size` to produce the Terminal's one
authoritative `(cols, rows)`. A headless caller has no TTY to measure, so the
viewport it would contribute is the 80x24 no-TTY fallback — the same fallback
that made a session-scoped `ATTACH` from `phux rec` shrink a human's panes
(ADR-0060). `phux new` had no size flag, `phux snapshot --rendered
--cols/--rows` sized only the composite and left the pane alone, and there
was no resize verb. For a project whose thesis is that agents are first-class
users, an agent could read, drive, and record a pane but not size one.

It cost twice in one feature. The committed recording demo asset is stuck at
80x24, and `rec_does_not_resize_the_recorded_pane` had to stand up a real
`phux attach` on a real PTY purely to obtain a pane whose size was not 80x24
— otherwise an errant `ATTACH` would resize an 80x24 pane to 80x24 and the
test would pass while the bug shipped.

## Decision

**No new wire.** `TERMINAL_RESIZE` (`0x23`, L1 §3.1) already names one
Terminal and its exact cell dimensions, is C→S, requires no attach or
subscription, and the reference server has driven `TIOCSWINSZ` from it since
it landed. No new command tag, no `ServerFeature` bit, no version bump.

**An explicit resize applies, and loses to a later view event.** The server
does not consult the window-size policy for `TERMINAL_RESIZE`: the resize
takes effect immediately whether or not anyone is attached. It is not
sticky. Under the view-derived policies (`smallest`, `largest`, `latest`) the
next attach, detach, or `VIEWPORT_RESIZE` recomputes geometry from the
attached views and supersedes it; under `manual` no view event ever does, so
an explicit resize is the *only* thing that sets the size and it holds.

**The verb verifies instead of assuming.** `TERMINAL_RESIZE` has no ack — the
S→C `TERMINAL_RESIZED` at `0x92` is spec-only — so `phux resize` sends the
frame and then reads the pane's dimensions back with `GET_STATE` on the same
connection, reports the geometry the server actually holds, and exits nonzero
with a diagnostic naming `window-size` when it is not the one requested.

## Why

The interaction with an attached human was the real decision, and all three
answers were defensible. Refusing while anyone is attached makes the verb
useless in exactly the co-driving case the project exists for. Silently
losing to the policy is worse than refusing, because the caller reads success
and gets 80x24 — the failure this whole slice is about. Winning permanently
would require a per-pane "explicitly sized" override that beats the policy,
which is a second policy axis duplicating what `window-size = "manual"`
already means; ADR-0027 named `Manual` "hold a fixed size, implies a future
resize verb" precisely so this would not become a second knob.

That leaves apply-and-report, whose only real weakness — a caller cannot tell
whether the size stuck — is removed by the read-back. The read-back is
ordered rather than racy because the server handles one connection's frames
in arrival order and `handle_terminal_resize` updates the registry `dims`
synchronously, which is the field `GET_STATE` projects. (`GET_SCREEN` would
*not* be sound: it is served from the pane actor's screen mailbox, and the
actor's `select!` polls that arm ahead of the resize arm.)

One verb, not also a `phux new --cols/--rows`: `phux new` without `--json`
attaches, so a size flag there would be silently overridden by the real TTY's
viewport on the default path — a flag that does nothing is the same class of
trap as a resize that does nothing.

## Tradeoffs

A script that resizes a pane a human is attached to under the default
`smallest` policy gets a nonzero exit even though the resize did happen and
will hold until the next viewport event. That is deliberate — a transient
size is not the size the caller asked for — but it means such a script must
either tolerate the exit code or ask its operator for `window-size =
"manual"`.

The read-back costs one extra round trip per resize, and consults server
bookkeeping rather than the pane actor's libghostty grid. The two can only
diverge through a bug; `crates/phux/tests/resize_e2e.rs` asserts against
`GET_SCREEN` (the actor's own projection) specifically so that divergence
fails a test rather than misleading a caller.

## Alternatives

**Refuse while a client is attached.** Legible and trivially correct, and it
gives up the case that matters: an agent sizing a pane in a session a human
also has open. Rejected.

**A per-pane sticky override.** A pane flagged "explicitly sized" that every
viewport recompute skips. It works, and it means a pane can silently stop
tracking the window a human is resizing, with no config surface saying so and
no way to clear it but another verb. `window-size = "manual"` is the same
capability with a name, a scope, and a documented default. Rejected.

**A new `RESIZE_TERMINAL` command with a typed reply.** Would give a real ack
instead of a read-back, and would cost a command tag plus a `ServerFeature`
bit for a frame that already exists and already works. Rejected under
ADR-0061: an additive shape exists, so the extension does not get to be a
protocol change.
