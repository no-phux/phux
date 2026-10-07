---
audience: contributors
stability: stable
last-reviewed: 2026-10-07
---

# 0154 — Devices without ssh enroll with a single-use ticket over their own ALPN

**TL;DR.** A device that cannot reach the server over ssh (a phone) enrolls a
workload certificate with a single-use enrollment ticket that `phux pair
--enroll` mints into the connect link. It dials the server's QUIC listener
under a second ALPN, `phux-enroll/1`, sends the ticket and a CSR for a key it
generated itself, and gets back the issued chain. The private key never
leaves the device; on a phone it lives in the platform keystore and the
binding signs through a callback. A paired QUIC listener asks for, but no
longer requires at the TLS layer, a client certificate, so the enrollment
ALPN can answer a device that has none yet; the terminal ALPN still refuses a
connection without an enrolled certificate before reading any byte of it.
Relays and WebTransport get the same identity by carrying an end-to-end TLS
session inside their stream, so `paired` no longer refuses either.

Status: Accepted
Date: 2026-10-07
Builds on [ADR-0116](./0116-workload-auth-is-mtls.md) (which left
mobile enrollment to "its own authorizer") and
[ADR-0153](./0153-clients-pin-the-workload-ca.md).

## Context

`paired` admits a TLS connection only with an enrolled client certificate.
`phux host add` enrolls over ssh (`workload-auth.md` §8.1). A phone has no
ssh and no way to reach `phux workload add-key`: it pairs by scanning a QR
link that carries a bearer token and pins. So today a paired server cannot
serve a phone, and `paired` refuses relay connectors (the relay terminates
TLS, so a consumer's certificate never reaches the server) and WebTransport
(no browser presents one). Those three are why the `workload-auth.md` §8
transitional posture still exists (phux-pjc5.7).

## Decision

1. **A ticket authorizes one enrollment.** `phux pair --enroll` mints a
   256-bit ticket beside the bearer token and puts it in the link as
   `enroll=` (and in `--json` as `enrollment_ticket`). The server keeps only
   its SHA-256, with the scope ceiling the credential will get (default every
   verb at `global`, as `host add` enrolls), the credential's lifetime
   (default one year), and the ticket's own expiry (default ten minutes), in
   an owner-only `<state-dir>/enrollment-tickets` written like the workload
   registry. A ticket is consumed by its first use, valid or not past that
   point; an expired or consumed one refuses. The ticket is a secret like the
   bearer token, shown once; no key material is ever in the link.
2. **Enrollment has its own ALPN.** The configured QUIC listener also offers
   `phux-enroll/1`, in every policy mode, so a device can enroll before its
   operator turns `paired` on. On that ALPN the device opens one stream and
   sends `version u8 = 1 | ticket_len u8 | ticket | csr_len u32 BE | CSR DER`
   (at most 16 KiB); the server answers `status u8 | len u32 BE | body` with
   status 0 and the issued chain (PEM, leaf then CA) or status 1 and the one
   word `refused`, then closes. The CA signs only the CSR's public key, with
   the fields `host add` gets. The ALPN carries no phux frame, no bearer
   token, and no grant: a connection on it ends after one answer.
3. **A paired listener asks for the certificate but decides after the
   handshake.** The QUIC client verifier accepts a handshake without a
   certificate, still verifying any certificate presented against the CA.
   The terminal ALPN then refuses a connection whose certificate is absent or
   not an active registry credential, with the same application close as
   before and before its first stream is read, so no phux byte and no bearer
   preamble is processed (`workload-auth.md` §3 moves the refusal from "the
   TLS layer" to "before any stream is read"). The WSS listener is unchanged:
   it has no enrollment ALPN and still requires the certificate in its
   handshake.
4. **The key stays in the keystore.** The binding takes a device key as a
   callback (`public key`, `sign`), builds the CSR with it, and presents the
   issued certificate with a rustls signer that calls back for every
   handshake signature (ECDSA P-256, what Secure Enclave and StrongBox hold).
   The client validates the reply as `host add` does: two certificates, the
   leaf carrying its own key and verifying as a client certificate under a
   CA, and that CA the one it pinned (ADR-0153).
5. **Relays carry an end-to-end TLS session.** Through a relay, a consumer
   runs TLS 1.3 inside the spliced stream, terminated by the server's
   connector with its own certificate and the paired client verifier. The
   connector tells it from today's bearer preamble by its first byte (a TLS
   record is 0x16; a preamble's length starts 0x00). The bearer preamble then
   rides inside. The relay parses nothing new and now sees nothing: the
   per-hop authority ADR-0116 accepted becomes end-to-end. Under `paired` a
   connector admits only such sessions, and `paired` stops refusing
   connectors. A relay link (`phux pair --relay-route`) now carries the
   server's `ca` beside the relay's `fp`; a client pins it keyed by
   `<relay leaf>@<route>`, since one relay serves many routes, and never
   learns it unprompted (the relay's leaf does not authenticate the server).
   The inner session offers the enrollment ALPN too, so `--enroll` works
   through a relay. A server whose certificate predates its CA has no `ca`
   to give, and its relayed consumers keep the plain stream (outside
   `paired` only).
6. **WebTransport uses the same inner session.** The WebTransport listener
   applies item 5's sniff to its stream: a native client can present an
   enrolled certificate; a browser cannot hold one yet, so under `paired` a
   browser session is refused until phux-web carries a key and a TLS stack
   (WebCrypto keys cannot sign synchronously for rustls). The bearer still
   rides the `CONNECT`, so nothing follows the inner handshake there, and
   `paired` stops refusing the WebTransport listener.

## Why

- A ticket is the smallest authorizer that is single-use, bounded in time,
  and rides the channel the phone already pairs over. Possession of the
  ticket is exactly the authority the scanned bearer token already conveys,
  so enrollment grants nothing pairing did not.
- A separate ALPN keeps the terminal protocol free of enrollment frames
  (`workload-auth.md` §1) and makes "a connection without a certificate"
  mean one thing per ALPN, decided in one place.
- Relaying TLS inside the stream needs no change to the relay, keeps SNI
  routing, and fixes the one place authority was per-hop.

## Tradeoffs

- **A paired QUIC listener completes handshakes for certificate-less
  peers.** Refusal moves from inside the handshake to right after it, still
  before any stream byte is read. A bug in that check would admit a
  certificate-less peer, so it is one check with its own adversarial tests.
- **Whoever scans the QR first enrolls.** As with the token, a link seen by
  someone else is theirs to use; single use and a ten-minute expiry bound it,
  a legitimate device that loses the race fails loudly, and the server logs
  the ticket and the credential it became so the operator can revoke it.
- **No renewal without a new ticket in v1.** A device re-enrolls by scanning
  a new link; credentials default to a year. Certificate-authenticated
  renewal on the enrollment ALPN is a natural extension.
- **Browsers stay outside `paired`** until phux-web can hold a key.
- **A relayed connection keeps one stream.** Per-Terminal QUIC streams would
  bypass the inner session, so a relayed consumer never negotiates them (it
  never did: the relay splices one stream).

## Alternatives

- **An enrollment metadata key on the terminal protocol.** Rejected: the
  device would need a session before it has an identity, and §1 keeps
  workload material off the terminal endpoint.
- **A separate enrollment port.** Rejected: one more port to open, forward,
  and advertise in the link.
- **Key generated by the server and delivered in the link.** Rejected: a
  private key in a QR code (`workload-auth.md` §8 forbids it).
- **A blind relay forwarding QUIC by SNI.** Viable, but it needs QUIC Initial
  decryption and datagram tunnelling in the relay; the inner session gets the
  same end-to-end property with no relay change.

## Related

- `docs/spec/workload-auth.md` §3, §8.2 — the enrollment ALPN and ticket.
- [ADR-0051](./0051-outbound-dial-out-connector-transport.md) — the relay
  never parses consumer bytes; item 5 keeps that.
- phux-pjc5.7 — the transitional posture, retired once devices have enrolled.
