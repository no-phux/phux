---
audience: contributors
stability: stable
last-reviewed: 2026-10-07
---

# 0153 — Clients pin the workload CA, which issues the server certificate

**TL;DR.** A server provisioning its TLS certificate asks the workload CA
for it and presents the chain (leaf, then CA). Clients pin the CA at
pairing (`phux host add`, a connect link's new `ca`), like SSH
`known_hosts`. A server presenting another CA is refused with
`authority_changed`, naming both fingerprints and the re-pair command, and
never silently re-trusted. Rotation is `phux workload authority --rotate`:
a new CA, a new server certificate under it, and every client re-pairs. No
cross-signing in v1. Existing certificates are never re-issued, and pairs
made before this keep working: a client that pins only a leaf learns the CA
on its first connection to a server that presents one.

Status: Accepted
Date: 2026-10-07
Amends [ADR-0116](./0116-workload-auth-is-mtls.md) ("the CA fingerprint
replaces the leaf pin, so CA rotation is explicit re-pairing"): it decides
how the pin is made, how a change surfaces, and how existing pairs migrate.

## Context

`workload-auth.md` §2 says the CA issues the server's identity and clients
pin the CA, surfacing `authority_changed` on a change. The server still
minted a self-signed leaf (`ensure_self_signed_for`), every client pinned
that leaf, and nothing named an authority change. A leaf pin forbids ever
re-issuing the server certificate: every device holds it, and some (phones)
are out of reach. Three questions were open: when a client learns the CA,
what a changed CA does, and how rotation works without stranding the
server and phone already in use.

## Decision

1. **The CA issues the server certificate, once.** When the server's TLS
   pair is missing (first routable listen, `phux pair`, `OPEN_LISTENER`),
   the workload CA (created first if absent, as ADR-0116 allows) issues a
   `serverAuth` leaf naming the same addresses a self-signed one would. The
   certificate file holds the chain, leaf then CA, so every listener
   presents the CA and `phux pair` reads it back. A CA that cannot issue
   (insecure state directory, partial pair) logs why and falls back to a
   self-signed leaf: a listener is never lost to it. An existing pair is
   never touched (ADR-0091), so a server provisioned before this keeps its
   self-signed leaf until the operator rotates.
2. **The authority is what the leaf verifies under.** A presented chain
   names a CA when its last certificate is a trust anchor the leaf verifies
   under for server authentication now. Names are not checked; phux pins.
   The canonical spelling is `sha256:` and 64 lowercase hex digits
   (`phux workload authority` prints the same).
3. **Pin at pairing, trust on first pair.** `phux pair --json` reports
   `ca_fingerprint` and the connect link gains `ca=` after `fp=` (direct
   links only; a relay link pins the relay, which terminates TLS).
   `phux host add` and `--code` record it. A pin lives in
   `known-authorities` beside `config.toml`, one line per server keyed by
   its leaf pin, not in `[[remote]]`: a client also records pins unprompted
   (item 5), and `[[remote]]` refuses unknown keys, so an older `phux`
   would refuse the whole file. A re-pair writes a new leaf pin and so
   starts a new line.
4. **A changed CA is a hard refusal.** A client pinning a CA accepts a leaf
   that CA issued, or exactly the leaf pinned beside it (a server whose
   certificate predates its CA). A chain naming any other CA is refused even
   beside a matching leaf: the CA moved, and every client certificate it
   issued with it. So is a leaf that chains to nothing and matches no pin.
   The refusal is `DialError::AuthorityChanged`, worded "the server's
   certificate authority changed: pinned X, presented Y", fatal to every
   reconnect loop, with the remedy `phux host add NAME` after confirming the
   operator rotated. The CLI's ssh repair ladder may re-pair over ssh, as it
   already did for a changed leaf: that is the operator's ssh trust, not
   the TLS peer's word.
5. **Migration is trust on first connect.** A client holding only a leaf
   pin (every pair before this) dials with that pin. When the pinned leaf
   chains to a CA, the CA is recorded. The chain cannot be forged by anyone
   without the CA's key, and the pinned leaf authenticates the channel it
   arrived on. Embedders without the registry (`RemoteClient` in
   phux-mobile) get `learned_authority()` and `set_authority_pin()` and
   store the pin themselves.
6. **Rotation re-pairs everything.** `phux workload authority --rotate`
   mints a new CA, re-issues the server certificate under it with the old
   leaf's addresses, and keeps every replaced file beside the new one as
   `*.retired-<unix>`. The server presents it after a restart (`phux
   upgrade`). Every client then refuses by name until re-paired, and every
   workload credential re-enrolls, since its certificate chains to the old
   CA. An operator-supplied certificate (`PHUX_WS_TLS_CERT`) is never
   touched.

## Why

- A CA pin is what lets the server certificate be re-issued (new
  addresses, renewal) without touching devices, and it lets a client tell
  "this server moved under a new authority" apart from "wrong host".
- Pinning at pairing reuses the one channel the operator already trusts
  (ssh for `host add`, the scanned QR for a phone). Learning from the first
  authenticated connection costs nothing in trust: the leaf pin the client
  already holds is the authentication.
- Re-issuing nothing that exists, and keeping the leaf pin beside the CA,
  is what lets this ship without stranding the production server or a
  paired phone that predates it.

## Tradeoffs

- **Servers provisioned before this stay leaf-pinned** until their operator
  runs `--rotate` and re-pairs every device. Rotating is a choice, not a
  migration step, because it forces exactly that re-pair.
- **No cross-signing.** A rotation cannot be bridged: every client re-pairs.
  A future version could present the new CA cross-signed by the old one, so
  pinned clients follow a rotation on their own.
- **No re-issue verb.** The operator verb only rotates. Removing the server
  pair (the ADR-0091 remedy) already makes the next listen issue a new leaf
  under the same CA, which CA-pinned clients accept; a `--reissue-server`
  verb is the natural next step once phones pin the CA.
- **`known-authorities` is a second file** beside the registry, and the
  registry no longer holds every pin. Deleting a line forgets a pin, and the
  next connection learns it again.
- **Learning is per process.** A long-lived CLI process that learns a pin
  keeps dialing with the leaf pin until it restarts; the leaf pin is the
  stricter of the two, so this never weakens a dial.

## Alternatives

- **Re-issue every server certificate under the CA on upgrade.** Rejected:
  it changes the leaf every paired phone pins, which is exactly the strand
  this must avoid.
- **Append the CA beside an existing self-signed leaf so legacy servers
  teach it too.** Rejected for v1: the CA would not have issued the leaf,
  so the chain proves nothing, and the only payoff is a later transparent
  re-issue that `--rotate` does not offer anyway.
- **A `ca-fingerprint` key in `[[remote]]`.** Rejected: an unprompted write
  of a key older binaries refuse would break the config for every other
  `phux` on the machine.
- **Silent cross-signed rotation.** Deferred, as above.

## Related

- [ADR-0116](./0116-workload-auth-is-mtls.md) — amended (rotation is
  explicit re-pairing; this decides the mechanics).
- [ADR-0091](./0091-certificate-names-the-advertised-address.md) — an
  existing certificate is never widened or re-issued.
- [ADR-0149](./0149-relay-routes-ride-the-tls-server-name-everywhere.md) —
  relay links pin the relay, so they carry no `ca`.
- `docs/spec/workload-auth.md` §2 — the pin, `authority_changed`, and the
  reference implementation's migration.
