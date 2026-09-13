---
audience: contributors
stability: stable
last-reviewed: 2026-08-16
---

# 0091 — The certificate names the advertised address, once, at generation

**TL;DR.** The auto-provisioned remote certificate named only loopback, while
`phux pair` handed devices a routable address — so a client that validates the
server name refused an address phux itself printed. SANs now include the
address the listener binds and the overlay address the connect link carries.
They are chosen **only at generation**: an existing certificate is never
widened, because that rotates the fingerprint every paired device pins.
Coverage is reported instead, by `phux pair`, the listener log, and `phux
doctor`.

Status: Accepted
Date: 2026-08-16
Builds on: ADR-0031 (auto-provisioned remote TLS + pinned fingerprint),
ADR-0037 (overlay-agnostic address detection), ADR-0081 (overlay auto-listen)

## Context

ADR-0031 auto-provisions a persisted self-signed certificate so a remote
listener needs no operator PEM work, and makes its SHA-256 fingerprint the
trust anchor: `phux pair` prints the pin, the device stores it, and the pin —
not the hostname — is what closes the trust-on-first-use window.

The generator gave that certificate three SANs: `localhost`, `127.0.0.1`,
`::1`. Its doc comment stated the reasoning, and the reasoning was half right:
"a fingerprint-pinning consumer does not rely on hostname matching, but valid
SANs keep a conventionally-validating client working on loopback." True for a
pinning consumer; the loopback qualifier was the hole. ADR-0081 then made the
server auto-bind a *detected overlay address*, and `phux pair` emits

```text
phux://connect?url=wss://100.79.155.27:8787&fp=<sha256>&token=<hex>
```

against a certificate claiming none of that. Verified on a live server:

```text
$ openssl s_client -connect 100.79.155.27:8787 -verify_ip 100.79.155.27
verify error:num=64:IP address mismatch
```

phux was handing out an address its own certificate did not claim.

Who this actually affects is narrower than it looks, and worth stating so the
fix is not over-credited. Every phux consumer ignores the server name:
`phux-dial`'s `CertTrust` is `SkipVerify` or `Pinned`, and `PinnedFingerprint`
compares the leaf SHA-256 without reading the name; phux-mobile's verifier
likewise takes `_server_name` and drops it. `--tls-server-name` selects the SNI
phux *sends*, not a name phux *checks*. The affected client is the third party:
`curl --cacert remote-cert.pem`, a browser with the certificate trusted (and
browsers cannot be told to skip name validation), `openssl s_client`. For those,
name validation fails after trust has been granted, and there is no flag to work
around it.

## Decision

**SANs cover the addresses phux advertises, and are fixed at generation.**

- Loopback SANs are unconditional, as before.
- The listener adds its own bind address when that address is specific and
  non-loopback. The auto-bound overlay listener always is, and already holds
  the detected address by the time it builds — so this costs **no detection
  call**, which matters because phux-90j5 moved detection off the startup path
  and it must not creep back.
- `phux pair` adds the host of the connect link it is about to print plus every
  detected overlay address, resolving the address *before* provisioning. It
  already ran detection; only the ordering changed.
- A wildcard bind (`0.0.0.0`, `[::]`) names no address, so it contributes no
  SAN. `phux pair` is what covers that host.

**An already-generated certificate is never regenerated to widen it.** Not on
startup, not by `phux pair`, not by `phux doctor`. Coverage is *reported*:

- `phux pair` warns beside the fingerprint it prints, naming the uncovered
  address and the exact `rm` that would fix it;
- the listener logs a warning when its bind address is uncovered;
- `phux doctor`'s `remote-cert` check is the durable surface, with the remedy
  attached.

Reports use rustls' own `verify_server_name` — the webpki check a rustls client
runs mid-handshake — not a SAN-string comparison, so what phux reports is what
a client will do.

## Why

Because the alternative is silent, total, and unrecoverable. The fingerprint is
the trust anchor and it lives out-of-band on devices phux cannot reach. Widening
SANs means a new certificate, which means a new fingerprint, which means every
paired device now refuses to connect — with no signal saying why and no path
back short of re-pairing each one by hand. The existing generator already
refuses to regenerate over a half-present pair for exactly this reason.

Weighed against that, the defect being fixed is loud, local, and affects clients
phux does not ship. Trading a handshake failure a third-party client reports
immediately for a fleet-wide trust break nobody reports at all is the wrong
direction, so phux will not make that trade on the operator's behalf. Deleting
the pair is a deliberate, documented act and stays theirs.

## Tradeoffs

- **A certificate minted before this change stays narrow forever**, unless the
  operator deliberately regenerates. Every install that has already paired a
  device is in that state. This is why the three reporting surfaces exist: the
  situation is permanent, so the explanation has to be findable.
- **The SAN set depends on which context minted the certificate**, first writer
  wins. A server that auto-bound the overlay address names it; one provisioned
  by `phux pair` on a host with no overlay does not, and never will. The
  certificate is a function of a moment, and `phux doctor` is how you find out
  which moment you got.
- **One certificate parse per listener build** for the coverage check. Off the
  startup path, and nowhere near an accept loop.
- `phux doctor` gains an overlay-detection shell-out (~140ms). Acceptable in an
  interactive diagnostic; it is the startup path that must not pay it.

## Alternatives

**Regenerate when the advertised set grows.** The obvious fix, and the one that
un-pairs every device silently. Rejected above.

**Regenerate only when nothing is paired yet.** Sound where it applies —
an empty token store means no device pinned anything — but it applies exactly
where the problem is absent (a fresh install already gets correct SANs from the
new generator) and not at all where it bites (an install with paired devices).
It would add a state-dependent regeneration rule to buy nothing.

**A wildcard or exhaustive SAN set** — every local interface address, or
`*.ts.net`. Rejected: a certificate that claims addresses the server does not
serve is a worse description of the server than one that claims too few, and the
overlay address is not knowable from the interface list alone (ADR-0037 is
deliberately best-effort).

**Add a `phux pair --regenerate-cert` flag.** Rejected as surface for a rare,
deliberate act that `rm remote-cert.pem remote-key.pem && phux pair` already
performs, using the one idiom the existing partial-pair error already teaches.
`phux doctor` prints that exact command, so the discoverability argument is
answered without a new flag that would read as routine.
