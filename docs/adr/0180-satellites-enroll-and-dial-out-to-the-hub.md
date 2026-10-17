---
audience: contributors
stability: stable
last-reviewed: 2026-10-17
---

# 0180 — Satellites enroll with a ticket and dial out to the hub

**TL;DR.** A machine the hub cannot dial (a sandbox VM behind NAT) joins
as a satellite by dialing out. `phux host ticket NAME` on the hub mints a
single-use ADR-0154 ticket whose role is "satellite NAME" and registers an
`inbound://` satellite. `phux host join CODE` on the sandbox enrolls a key
it generated over `phux-enroll/1` and keeps a QUIC connection to the hub
under a third ALPN, `phux-satellite/1`. The hub opens phux streams on that
connection, and its link supervisor adopts it as that satellite's link.

Status: Accepted
Date: 2026-10-17

## Context

Every hub link today is dialed by the hub (ADR-0007, ADR-0038): `quic://`,
`wss://`, or `ssh://`. Each needs the satellite to be reachable, and
disposable agent sandboxes usually are not. They sit behind NAT, have no
listener, and live for hours. A relay connector (ADR-0051/0052) reaches
consumers through a third machine, but it serves consumers and not a hub
link. The owner chose reverse enrollment with no hosted service (bead
phux-lxov.3). The pieces exist: ticket enrollment (ADR-0154), the hub's
link supervisor (`hub/link.rs`), and the connector's supervised dial-out.

## Decision

1. **The ticket decides the role.** `phux host ticket NAME` mints an
   ADR-0154 ticket into the same store, recording `satellite = NAME`, and
   writes `[[satellites]] NAME` with `endpoint = "inbound://"`. It prints a
   join code, `phux://satellite?hub=HOST:PORT&ca=sha256:…&name=NAME&enroll=HEX`.
   The code carries the hub's CA pin (required, never learned) and no bearer
   token. Lifecycle is ADR-0154's: only the hash is stored; the ticket is
   consumed under the store lock before anything is issued; it expires after
   ten minutes. New here: `phux workload revoke t-…` revokes an unspent
   ticket. An unknown, expired, consumed, or revoked ticket is refused alike.
2. **Enrollment is unchanged on the wire.** The sandbox generates a P-256
   key, sends a CSR over `phux-enroll/1`, and checks the reply as `host add`
   does (§8.1 step 3, plus the pinned CA). The hub records the credential
   with `satellite = NAME` and no terminal scope ceiling. It revokes any
   earlier credential of NAME in the same registry write, so one name has
   one live credential. The terminal endpoint never admits a satellite
   credential: registry lookup on every terminal path skips it.
3. **Auth runs one way, protocol the other.** The satellite is the TLS
   client: it pins the hub's CA and presents its satellite certificate under
   `phux-satellite/1`. Only a QUIC listener that verifies workload
   certificates (`paired`) offers that ALPN. The hub admits the connection
   when the certificate maps to an active satellite credential and NAME is
   an enabled `inbound://` entry. Otherwise it closes `AUTH_FAILED` before
   any stream. The hub is the phux client: it opens bidirectional streams,
   each one ordinary phux connection starting with its HELLO. The satellite
   opens none and splices each stream to its own owner socket.
4. **The hub holds the satellite owner's authority**, as an `ssh://` hub
   does through `phux stdio-bridge` (ADR-0038 addendum). The sandbox's
   operator granted it by running `phux host join`. The satellite therefore
   needs no listener and can run in `local` mode.
5. **A link is a link regardless of who dialed.** An `inbound://` entry
   gets the same `run_link` supervisor. Its transport waits for the next
   adopted connection for NAME instead of dialing. HELLO negotiation, the
   relay session, the metadata mirror, and the incarnation fence are
   unchanged. A newer adopted connection for NAME closes the older one. The
   hub does not back off between adoptions; the dialing side owns backoff.
6. **Reconnect and persistence.** The satellite's key and chain are
   owner-only files in its state directory. `[[uplink]]` in `config.toml`
   names them, the hub endpoint, and the CA pin. The server supervises each
   uplink like a connector: it re-reads the files every attempt and backs
   off from 500 ms to 30 s, resetting after 30 s up. The config-reload
   doorbell starts, stops, or redials uplinks, so `host join` needs no
   restart.
7. **Revocation ends the link.** `phux workload revoke sha256:…` is seen at
   the next adoption and, within a second, on a live link, which is closed.
   `phux host disable NAME` stops the supervisor and refuses adoption.
   `phux host rm NAME` also revokes NAME's satellite credentials.

## Why

- ADR-0154 already provides a single-use, expiring, hash-stored authorizer
  over a channel the sandbox can reach. A role field reuses it whole.
- Splicing to the owner socket reuses the one bridge whose authority rule is
  settled (ADR-0038 addendum). A grant that lets the hub act on the
  satellite does not exist and is not invented here.
- Adoption behind the `LinkTransport` seam keeps the relay session code
  ignorant of direction, so every federation test still applies.

## Tradeoffs

- **The hub gets owner authority over the satellite.** A satellite that
  wants a narrower grant needs its own CA to issue the hub a certificate. If
  that is ever wanted, it can be added without breaking this.
- **The hub must run `paired`** for the ALPN to be offered. A hub in any
  other posture refuses every satellite at the handshake, and the satellite
  logs the failure and keeps backing off.
- **A registry with a satellite record cannot be read by older binaries**
  (`deny_unknown_fields`). A downgrade must revoke and remove the record
  first. Registries without one are unchanged.
- **No renewal.** The credential lasts a year. Renewal is a new ticket and
  `join`, which replaces it.
- **One QUIC connection carries the link.** It shares one congestion
  window, which is no worse than a dialed `quic://` link.

## Alternatives

- **Satellite dials the relay as a connector and the hub dials it through
  the relay.** Rejected: it needs a third machine and the relay route
  ceremony for every sandbox.
- **The satellite speaks phux-client to the hub** (inverting the protocol).
  Rejected: every relay, mirror, and fence path assumes the hub is the
  client.
- **A bearer token in the code instead of an enrolled key.** Rejected: a
  replayable secret on a disposable VM, and nothing to revoke per machine.
- **Feed hub streams straight into the satellite's dispatch with a synthetic
  owner identity.** Rejected: it forges an owner-socket peer. The real
  socket keeps that check where the kernel makes it.

## Related

- `docs/spec/workload-auth.md` §8.3, `docs/spec/proto.md` §4.
- ADR-0007, ADR-0038, ADR-0051, ADR-0052, ADR-0098, ADR-0116, ADR-0154.
