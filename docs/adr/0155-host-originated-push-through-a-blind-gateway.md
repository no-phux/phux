---
audience: contributors
stability: stable
last-reviewed: 2026-10-08
---

# 0155 — Background push is host-originated, through a blind gateway

**TL;DR.** A suspended phone hears about an agent's question because the
user's own server posts a content-free notice to a phux-operated push
gateway, which forwards it to APNs. The phone registers a per-device grant
with the gateway and writes it to each server as `phux.push/v1/<device>`.
No relay holds a wire, no first-party service sees terminal bytes, and the
server never holds APNs credentials.

Status: Proposed
Date: 2026-10-08

## Context

phux-mobile ADR-0015 defers background notifications to a "Stage-2 relay
that holds the wire for suspended devices, watches for `Asked`, and sends a
privacy-minimal APNs push". phux-mobile ADR-0037 then requires any
phux-operated relay to be blind: it never terminates the TLS session that
carries phux frames. A blind relay cannot watch for `Asked`. The two
decisions leave Layer-B with no shape, and bead `phux-1hnw` records the
alternative this ADR takes: a daemon-initiated push, as its own decision
rather than a silent substitution.

What already exists: the server sees every ask through one arbiter
(ADR-0036) and broadcasts it through one function; the phone parses a
routing-only payload (`host`, `pane`, `action`, now `question`); the
account backend holds the first-party entitlement projection every paid
service authorizes against (phux-mobile ADR-0037 decision 4); L3 `Global`
metadata is a client-owned key store the server can read.

## Decision

1. **The server originates the push.** When an ask is broadcast for a pane
   and a registered device's connection is gone, the server posts one
   notice to that device's gateway and nothing else. A device that is still
   connected gets nothing: it already projects the ask. One push per
   `(device, ask id)`.
2. **The device registers with a grant, not a token.** The phone mints a
   random secret, registers `(grant, APNs token, environment)` with the
   gateway under its account, and writes `{gateway, grant, host}` to each
   server as `phux.push/v1/<device>` ([L3](../spec/L3.md) §3.11). `host` is
   the id the phone knows that server by, echoed back so a tap routes. The
   server stores the key as ordinary metadata and remembers which
   connection wrote it.
3. **The notice is content-free.** `host`, `pane` (the binding's
   `local:<n>` spelling), `question` (the ask id), `action`. No pane name,
   no question text, no output, ever. The gateway maps the grant to a
   device token, checks `phux.pro` in the entitlement projection, and sends
   the APNs alert with a fixed body; the phone fetches the question over
   its own pinned wire after the tap.
4. **The gateway is a small first-party HTTPS service** beside the account
   backend. It holds the APNs key and the grant table. It never connects to
   a phux server, never sees a wire, and refuses an unknown or unentitled
   grant. The URL travels in the grant value; the server requires `https`
   (plain `http` only on loopback, for a local gateway under test).
5. **The relay stays blind and unchanged.** ADR-0051 decision 6 needs no
   amendment for this. The hosted relay of phux-mobile ADR-0037 remains a
   separate deliverable.

## Why

- A blind relay and a wire-watching relay cannot both exist. Moving the
  watch to the server, which already does it, dissolves the contradiction
  instead of amending the relay's trust model.
- The server is the one party that legitimately holds the ask; posting
  four opaque ids is the least it can disclose and still let a phone
  route. The gateway sees ids a phone chose and ids the server numbered;
  neither names a project, a file, or a question.
- A grant per device, minted by the phone, means the server never learns
  the APNs token or the account, and revocation is one row at the gateway
  plus one key delete on the server.
- Letting the grant carry the gateway URL avoids a server config knob. A
  client that can write `Global` metadata can already spawn a shell and
  run `curl`, so the URL adds no capability a connected client lacks.
- Entitlement is enforced where ADR-0037 already puts it: at the service,
  from the webhook-fed projection, never from a client claim.

## Tradeoffs

- The server makes an outbound HTTPS call. It is the first one it makes;
  the implementation is a single `POST` over `tokio-rustls` with Mozilla's
  root store, no redirects, no retries. A down gateway costs one warning
  per ask.
- A push can only follow an ask the server saw. A pane the server does not
  classify as asked (no sentinel, hook, or stream) produces no push, the
  same gap the in-app notification has.
- "Absent" means "the connection that wrote the grant is closed". A phone
  whose socket iOS has not yet torn down gets no push for a few seconds;
  it still gets the local notification when it next connects.
- Two deployables (server, gateway) share a four-field JSON contract. The
  spec is its home; the phone's parser and the server's encoder both test
  against it.

## Alternatives

- **Relay holds the wire (ADR-0015 as written).** Needs a relay that reads
  frames, which ADR-0037 forbids, or a side channel from the server to the
  relay, which is this decision with more parts.
- **Server sends APNs directly.** Every host would need the app's APNs
  private key. Not distributable.
- **Phone polls through Background App Refresh.** Minutes of latency at
  iOS's discretion; the design doc's flow is "answer while it is waiting".
- **Sealed sender capabilities (the competitor's shape).** The daemon holds
  a token sealed to the relay's key. Equivalent trust, more cryptography,
  and it still needs the daemon to originate the event.
