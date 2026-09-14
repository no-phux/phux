---
audience: contributors
stability: stable
last-reviewed: 2026-09-13
---

# 0122 — `phux host add HOST` is the front door to a machine

**TL;DR.** One verb reaches another machine: `phux host add [USER@]HOST`
confirms phux is there, starts and supervises its server, pairs, dials the
direct routes, and registers the first one that answers — or an `ssh://`
route with the candidate kept to promote later. `host enroll` becomes a
hidden alias. A registered host whose saved route stops answering is
repaired in the order an operator would try by hand: start it over ssh, retry
the saved credentials, only then re-pair.

Status: Accepted
Date: 2026-09-13

## Context

ADR-0066 gave `phux host` two ways to add a machine: `add NAME ENDPOINT`
for credentials minted elsewhere and `enroll HOST` for the ssh-driven
setup. A transcript of a first attempt (2026-09-13) showed which one people
reach for: `host add` — and it demanded a `quic://` URI, a certificate
fingerprint, and a `phux pair` run the user then performed on the wrong
machine. The verb that would have worked was two lines lower in `--help`
and never found.

The same transcript held the second failure. A registered host whose server
had been stopped by hand could not be attached: the dial timed out, the
repair rung of ADR-0093 re-paired instead of restarting, and the remedy the
operator knew was to ssh in and type `phux`. The launchd unit was loaded
and stopped — a clean exit stays stopped under ADR-0080's restart policy,
by design — so nothing was going to bring it back on its own.

## Decision

1. **`phux host add` takes an ssh destination.** `host add [USER@]HOST`
   is the ssh form: confirm `phux --version` there; run
   `phux service install --quic`, answering the live-server refusal with
   `--adopt` and any other failure with `phux server --ensure`; run
   `phux pair --json`, migrating a pre-versioned token store once; list
   the direct routes — an operator `--endpoint`, the detected overlay
   addresses, the host `ssh -G` resolves — and dial each briefly with the
   minted credentials; register the first that answers as `quic://`, else
   `ssh://HOST`. Every step prints one line; every failure names the next
   command. `host add NAME ENDPOINT` and `host add URI` stay as the manual
   form. `host enroll` is a hidden deprecated alias with the same flags.
   `machine` is a visible alias of `host`.
2. **The entry remembers how it was made.** `[[remote]]` gains `ssh` (the
   destination enrolled through) and `direct` (a paired `quic://` route
   kept beside an `ssh://` endpoint). `host ls --json` carries both under
   schema version 1; readers tolerate new keys.
3. **A registered attach repairs in rungs.** `phux attach NAME` and
   `phux --remote NAME` share one ladder: an `ssh://` entry with a `direct`
   route probes it first and promotes it when it answers; an unanswered
   direct route starts the server over `ssh` and retries the saved
   credentials; a refused one re-pairs; an ssh failure reports both errors
   with both remedies. `--no-enroll` stops after the first dial.
4. **A satellite is never repaired.** `--role satellite` uses the same ssh
   form but keeps neither `ssh` nor `direct`: a hub dials it on its own
   schedule and ADR-0038's trust direction stays one-way.

## Why

- **The verb people type is the verb that has to work.** A second verb
  reachable only through `--help` is a design the transcript already
  falsified. Folding the ssh form into `add` costs one argument-shape rule
  (URI or `NAME ENDPOINT` is manual, anything else is ssh) and removes the
  wrong turn.
- **Starting is cheaper and safer than re-pairing.** A stopped server is
  the common cause of an unanswered dial. Re-pairing first rotates a
  credential that was fine and rewrites an entry that was right; starting
  first changes nothing on either side when the credentials still work.
- **The probe is the registration.** Registering an overlay address the
  probe could not reach only moved the failure to the attach. Dialing each
  candidate once, with the credentials just minted, makes the entry a
  statement about what answered rather than what was advertised.
- **Adopting a live server is what "make sure it is running" means.**
  `service install` refusing a live incumbent is correct for an operator
  who might not know one exists; for a setup verb it is the state wanted.

## Tradeoffs

- **One verb, two argument shapes.** `host add mini` and `host add mini
  quic://...` do different things. Each form refuses the other's flags by
  name, and the help's first paragraph is the ssh form.
- **A probe adds up to five seconds per candidate to a cold add.** Only on
  a path where nothing answers, where the alternative was a wrong entry.
  Tests shorten it through an undocumented environment seam.
- **`ssh` and `direct` are two more keys under `deny_unknown_fields`.** An
  older phux reading a newer config refuses it; the same is true of every
  key added since ADR-0055.
- **Repair shells out to ssh from an attach.** It always did (ADR-0093);
  what changes is that the first thing it does is start rather than mint.

## Alternatives

- **Keep `enroll`, improve its discoverability.** Rejected: the transcript
  shows the user did not read past `add`, and no help text fixes a verb
  that is the wrong name.
- **Repair by re-pairing, as before, but faster.** Rejected: it does not
  restart a stopped server unless the service install happens to reload
  the unit, and it rotates credentials nobody asked to rotate.
- **Restart on any exit in the service unit.** Rejected by ADR-0080: a
  deliberate `phux kill --server` must stay stopped.
