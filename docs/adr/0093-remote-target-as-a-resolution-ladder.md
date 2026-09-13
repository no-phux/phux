---
audience: contributors
stability: stable
last-reviewed: 2026-08-22
---

# 0093 — `--remote user@host` is a resolution ladder, not a new transport

**TL;DR.** `phux --remote [USER@]HOST[:PORT]` gives the remote path the
spelling operators already know from ssh. It adds no transport, no wire
change, and no trust model: it resolves a target to a `[[remote]]` entry —
from the registry, from a pasted `phux://connect` code, or from a one-time
ssh pairing — and hands it to the existing dial. `user@` is a label for
pairing and lookup, never a wire identity.

Status: Accepted
Date: 2026-08-22

## Context

Every ingredient of a remote attach shipped and composed correctly, and
almost nobody could reach them in one step. A first attach to a new host
was:

```sh
phux host enroll mini        # or: phux pair on the far end, then
                             # phux host add mini quic://… --cert-fingerprint … --token-file …
phux attach mini
```

Two verbs, in an order the operator has to know, with a registry concept
(ADR-0055) sitting between the intent and the result. The intent itself is
one of the most rehearsed motions in the terminal: `ssh user@host`. Nothing
in phux's architecture required the two-step shape — QUIC is built end to
end (ADR-0007), a routable bind provisions TLS and demands a token
automatically (ADR-0031), a server binds its overlay address on port 8788
without being asked (ADR-0081), and `phux attach NAME` already resolves a
registered host to an endpoint, a pin, and a token. The gap was purely the
front door.

The temptation, given that gap, is to make `--remote` a *dialing* flag: parse
`user@host`, open QUIC to `host:8788`, done. That is the version that cannot
be built. The listener is token-gated by construction, and a store with no
matching credential rejects every connection — which is exactly the property
that makes auto-listen safe to default on. So `--remote` must either carry a
credential, find one, or mint one. It is a *resolution* problem wearing a
transport problem's clothes.

## Decision

### 1. `--remote` resolves to a `[[remote]]` entry and then reuses the existing dial

The flag produces a `RemoteEntry` and calls `run_attach_remote`. It
constructs no `Dial`, opens no socket, and knows nothing about QUIC beyond
the default port. Every transport, reconnect, and recording behavior it gets
is the behavior a registered host already had.

### 2. The ladder has four rungs, cheapest first

1. **Registered.** A matching `[[remote]]` entry — matched by the exact
   `user@host` spelling, then by bare host, then by endpoint host. The third
   and second matches are what make a host enrolled as `mini` answer to
   `--remote me@mini`. This is the steady state and involves no ssh.
2. **A pasted connect code.** `--code 'phux://connect?…'` — byte-identical to
   what `phux pair --qr` renders for a phone. Registers the host from the
   link, then dials. The laptop equivalent of scanning the QR, and the
   fully ssh-free cold path.
3. **A one-time ssh pairing.** No entry, no code: run `phux pair --json` on
   the far end over the operator's existing ssh trust, register the result,
   dial. Rung 1 catches every later invocation, so ssh appears exactly once
   per host and never again.
4. **A refusal that names both remedies.** When ssh cannot help, print the
   `--code` form and the `phux host enroll` form rather than a bare failure.

### 3. Rung 3 pairs; it does not provision

`phux host enroll` installs a launchd/systemd unit on the far end.
`--remote`'s ssh rung deliberately passes `install_service: false` and runs
one command there: `phux pair --json`. An attach that silently leaves a
supervised service behind on another machine is a side effect far past what
the request implies, and it is not reversible by re-running the attach. The
always-on setup keeps its own verb, and the success message names it.

### 4. `user@` is a label, not a wire identity

phux runs one server per user (ADR-0003) and the QUIC preamble carries a
bearer token, not a username. Which server a dial reaches is decided by
address and port. The `user@` half therefore does exactly two jobs: it names
the ssh destination for rung 3, and it is the registry key that remembers the
result. Two users on one host are two ports or two registry entries — never
one endpoint disambiguated on the wire.

### 5. The registry name excludes the port; an explicit port overrides per-dial

`--remote mini` and `--remote mini:8788` are one machine, so they resolve to
one entry. An explicit `:PORT` rewrites the endpoint for that dial only and
never edits `config.toml`: it is a statement about this connection, not a
correction to what `mini` means.

### 6. The root copy is scoped like the root `--rec`

`phux --remote me@mini` exists so the naked attach reads like `ssh`. A root
`--remote` in front of a verb is refused post-parse with the remedy named,
the same shape ADR-0065 gave the root `--rec`. `phux attach --remote` is
where `--code` and `--no-enroll` live.

## Rationale

- **A ladder is the only shape that can be honest.** A single-rung
  `--remote` either fails on every cold host (useless) or weakens the pin
  posture to trust-on-first-use (a real regression against ADR-0031, traded
  for convenience the ladder delivers without it). Ordering the rungs by cost
  lets the common case cost nothing and the cold case stay pinned.
- **Reusing `[[remote]]` keeps one source of truth.** A parallel
  "ad-hoc target" store would drift from the registry `phux host ls` prints
  and `phux doctor` checks. Writing the entry means the second attach is
  indistinguishable from an enrolled one — which is the whole promise.
- **The connect code was already the right artifact.** It carries endpoint,
  pin, and token in one string, and phux-mobile already parses it. Making
  `--code` accept it costs one parser (pinned to its builder by a round-trip
  test) and gives laptops the flow phones have had since ADR-0031.
- **Naming the ssh rung's blast radius is the difference between a
  convenience and a surprise.** Pairing grants no authority ssh did not
  already grant — ADR-0055's argument, unchanged. Installing a service does,
  which is why that stays behind a verb whose subject is the host.

## Tradeoffs

- **A cold `--remote` shells out to ssh by default.** Mitigated by
  `--no-enroll`, by printing what it is doing before it does it, and by the
  rung doing strictly less than `phux host enroll`. An operator who wants
  zero implicit ssh has one flag.
- **`--remote` writes config as a side effect of an attach.** Deliberate —
  an unremembered pairing would make every attach cost an ssh round trip —
  but it does mean an attach can change `config.toml`. The write is the same
  `add_or_update` the host verbs use, and `phux host ls` shows the result.
- **Bare IPv6 targets cannot carry a port.** `fd7a::1:8788` is ambiguous, so
  an unbracketed literal is read as a host with no port. Brackets are the
  documented escape.
- **Three match rules for one target.** Widening from exact spelling to bare
  host to endpoint host is what makes existing enrollments answer, but it
  means two entries can both plausibly match a target; the first rule that
  hits wins, and the order is fixed rather than scored.
- **A stale pin now fails at attach time rather than at enroll time.** Rung 1
  trusts what the registry holds; a rotated certificate surfaces as a refused
  dial. The remedy is re-pairing, which `--code` makes a one-liner.

## Alternatives considered

- **Trust-on-first-use with a fingerprint prompt.** The ssh model, and the
  obvious way to make a cold dial succeed without ssh. Rejected: ADR-0031
  refuses an unpinned routable dial on purpose, and a prompt that appears
  exactly once — at the moment the operator most wants to get on with it —
  is a prompt that gets accepted unread. The connect code delivers the pin
  out of band instead, which is strictly stronger and barely slower.
- **`--remote` as a pure dialer, with credentials from flags.** `phux
  --remote me@mini --token … --cert-fingerprint …`. Rejected: that is
  `phux attach --quic` with a friendlier host spelling, and it reintroduces
  the two-64-hex-strings problem ADR-0055 exists to remove.
- **A separate ad-hoc target cache, distinct from `[[remote]]`.** Rejected:
  two registries of the same concept, one of them invisible to
  `phux host ls`.
- **Make `--remote` a global flag.** Rejected for the reason ADR-0065 kept
  `--json` verb-scoped: it would advertise itself on ~40 verbs that cannot
  honor it. Only the attach paths take it.
- **Auto-install the service on the ssh rung** (i.e. reuse `phux host
  enroll` wholesale). Rejected on blast radius; see Decision 3.

## Related

- ADR-0007 — the QUIC transport this rides.
- ADR-0031 — remote consumer auth: the pin-or-refuse posture rung 2 and 3
  both preserve, and the `phux://connect` link `--code` parses.
- ADR-0055 — the `[[remote]]` registry, the ssh bootstrap argument, and the
  registry-name-beats-local-session rule this inherits.
- ADR-0066 — the `phux host` namespace `--remote` writes into and defers to.
- ADR-0065 — the root-flag scoping pattern Decision 6 copies.
- ADR-0081 — auto-listen, which is why port 8788 is the sane default.
