---
audience: contributors
stability: stable
last-reviewed: 2026-10-05
---

# 0149 — Relay routes ride the TLS server name everywhere

**TL;DR.** A relay route is spelled the way ADR-0052 already routes on it:
as the TLS server name. A `[[remote]]` entry gains `tls-server-name`, a
connect link gains `sni`, and `phux pair --relay-route ROUTE` mints a link
whose `quic` is the relay, whose `fp` is the relay's pin, whose `sni` is
the route, and which carries no `url`. The runtime's dial plan offers the
entry's name, so `phux attach NAME`, `--remote`, the headless verbs, and
every embedder reach a relay-tunneled server with no new flag.

Status: Accepted
Date: 2026-10-05

## Context

ADR-0057 Open Question 2 left relay paths out of connect links. A consumer
could reach a tunneled server only by hand
(`phux attach --quic RELAY --tls-server-name ROUTE --cert-fingerprint
RELAY_FP --token TOKEN`): the registry, the link format, and the
runtime's dial plan all derived SNI from the endpoint host. `phux pair`
also refused a relay-only server, since ADR-0141 gates minting on a bound
listener and the connector is not one. ADR-0052 already rejected a
consumer-side route concept: SNI is the route, and `--tls-server-name`
is its knob.

## Decision

1. **One field, named for the bytes.** `[[remote]]` gains
   `tls-server-name`. The runtime's `Resolved` and `Target` carry it, and
   QUIC and WebSocket plans offer it in place of the endpoint-derived
   name. It is a general SNI override, validated as a DNS name (no IP
   literal, since TLS sends no SNI for one). It is not a separate route
   concept. `phux host add` takes it as `--tls-server-name` on the manual
   form, `--role remote` only, and never on `ssh://`.
2. **The link key is `sni`.** A relay link is
   `?quic=quic://RELAY&sni=ROUTE[&name=][&fp=RELAY_FP]&token=`. `sni`
   requires `quic` and makes `url` optional, because the relay has no
   WebSocket leg. Links without `sni` parse exactly as before, still
   requiring `url`. Unknown keys stay ignored and duplicate keys stay
   refused. `--code` registers a relay link as its `quic` endpoint plus
   `tls-server-name`.
3. **Old parsers fail closed.** Omitting `url` is deliberate. A consumer
   that predates `sni` refuses a relay link for the missing `url` instead
   of dialing the relay with the wrong SNI and getting a TLS failure with
   no explanation.
4. **`phux pair --relay-route ROUTE [--relay HOST:PORT]`.** The door is
   the server's `[[connector]]`. The ADR-0141 gate becomes "the server
   answers on its socket and the config names the connector the link
   dials". `--relay` picks among several connectors and is required only
   when there is more than one. The route is checked against the relay's
   route grammar (ADR-0057 Decision 8). The credential is the server's
   own token. It crosses the relay opaquely and the server verifies it.
5. **The trust model is unchanged.** The link pins the relay, which
   terminates TLS (ADR-0051 Decision 6). Making the relay blind is
   phux-9ys5's decision. This ADR neither preempts nor precludes it.

## Why

- SNI already carries the route on the wire. Spelling it the same way in
  every config surface means one concept, and an embedder resolving a
  registry entry gets relay support with no code of its own.
- A missing `url` is the cheapest correct downgrade signal for clients
  that predate `sni`.

## Tradeoffs

- `RemoteConfigEntry` denies unknown fields, so an older `phux` refuses a
  config holding a routed entry. That was already true for every added
  registry key.
- The relay-link mint cannot prove the tunnel is up. The listener report
  carries no connector rows, and adding them would be a wire change. A
  link minted while the connector is down fails as route-offline.
- `--qr` works for relay links, but a phone app that predates `sni` will
  refuse them until it learns the key.

## Alternatives

- A `route` key and a `relay-route` field: a second name for the SNI
  bytes, already rejected by ADR-0052.
- Keeping `url` mandatory with a placeholder: old clients would dial it.
- `[[remote]] relay = "HOST:PORT"` beside a server endpoint: the consumer
  never dials the server directly, so the endpoint would be fiction.
- Reporting connectors in `GET_STATE` to gate the mint: a wire change for
  a pre-flight check. Deferred.

## Related

- Beads: phux-do1 (this decision), phux-9ys5 (blind relay, open).
- ADRs: [0051](./0051-outbound-dial-out-connector-transport.md),
  [0052](./0052-connector-route-identity-and-config.md),
  [0057](./0057-minimal-reference-relay.md) (resolves Open Question 2),
  [0031](./0031-remote-consumer-auth-and-encryption.md) (link ownership),
  [0141](./0141-pair-mints-only-against-a-live-listener.md) (gate amended
  for relay mints),
  [0133](./0133-one-client-runtime-below-every-binding.md) (runtime owns
  the dial plan).
