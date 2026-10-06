---
audience: contributors
stability: stable
last-reviewed: 2026-09-29
---

# 0141 — Pair mints only against a live listener

**TL;DR.** `phux pair` asks the running server which remote listeners it has
bound (the listener report `GET_STATE` already carries) before it mints. No
server, or a server with nothing bound, is an error and nothing is minted. The
connect link names the address the wss listener is actually bound to, not a
port pairing guessed. Lifecycle actions (`ls`, `prune`, `rotate`, `revoke`)
still only edit the store.

Status: Accepted
Date: 2026-09-29
See [ADR-0149](./0149-relay-routes-ride-the-tls-server-name-everywhere.md) for the gate a `--relay-route` mint passes instead.

## Context

[ADR-0081](./0081-overlay-auto-listen-and-one-command-pairing.md) made pairing
a pure credential operation that never contacted a server, and derived the
link's port from `PHUX_WS_ADDR` or the auto-bind default. The listener that
link points at only exists while a server is running and actually bound it.
With no server, a server that exited, a non-default profile (which never
auto-binds), or a bind that failed, `phux pair` still minted into the store and
printed a link and a QR for an address nothing served, and said nothing. The
phone scan then failed with no hint why, and every retry left another live
credential behind.

## Decision

1. **A mint asks the server first.** Before touching the store, `phux pair`
   dials the local socket (`--socket`, `PHUX_SOCKET`, or the profile default)
   and reads `GET_STATE`'s remote-listener report. No answer, or no row with
   `bound: true`, exits 1 with the socket, each disabled row's reason, and the
   remedy. Nothing is minted and no link is printed.
2. **The link names the bound wss listener.** Without `--host`, the URL is the
   wss bind: its own address when that is routable, the first overlay address
   for an unspecified (`0.0.0.0`/`::`) bind, and nothing for a loopback bind.
   `ws_addr` and `quic_addr` in `pair --json` are the bound addresses.
3. **A link request that cannot connect is refused before minting.** `--host`
   needs a bound wss listener behind it, since the link is a WebSocket URL.
   `--qr` needs a link at all. `--host` itself stays the operator's claim
   (MagicDNS, a port forward), unchecked beyond that.
4. **Pairing does not start a server.** The refusal names `phux service
   install` or `phux server --ensure`. `phux host add` has already started
   one. When a server disabled its listeners at boot because the legacy
   store would not load, enrollment answers the refusal by migrating,
   restarting it with `phux upgrade`, and pairing once more.
5. **No wire change.** The listener report is the existing trailing
   `SessionSnapshot` field `phux doctor` already reads.

## Why

A credential is only worth minting when something will accept it, and the
server is the one party that knows what it bound. Asking it replaces a guess
(an env var the server may never have seen, a default port a failed or gated
bind never took) with a fact. Failing loudly at pair time puts the error on
the machine that can fix it, not on a phone that can only say "cannot
connect".

Not auto-starting matches the non-session verbs (`status`, `doctor`, `kill`,
`host`), which report a missing server rather than spawning one. A server
spawned by `pair` would take its listener configuration from whatever
environment `pair` ran in, would not be supervised, and would usually bind
nothing anyway on a non-default profile. The QR would die at the next logout,
which is the same failure one step later. `attach` and `new` spawn because
they need a session. `pair` needs a listener, and a spawn does not produce one.

## Tradeoffs

- The documented "pair before starting the listener" order is gone. Start the
  listener, then pair. The store is re-read per connection, so nothing else
  about ordering changes.
- A `--no-service` `phux host add` against a host with no overlay and no QUIC
  bind now fails at pairing instead of registering an `ssh://` entry around a
  token nothing accepts. `--ssh-only` remains the explicit form.
- In the first two seconds after a server starts, the auto-overlay listener
  may not be bound yet (detection runs inside the accept set), so a `pair` in
  that window is refused and must be rerun.
- `pair` cannot tell whether `--tokens PATH` is the store the server reads.
  The report does not carry the store path, and adding it is a wire change
  this fix does not need.

## Alternatives

**Ensure a server the way `attach` does.** Rejected above: the spawned server
is unsupervised, inherits the caller's environment, and on non-default
profiles binds no listener, so the check that follows would refuse anyway,
after an unwanted daemon started.

**Warn and mint anyway.** Keeps the orphan credentials and the dead QR. The
warning scrolls past the QR the user is about to scan.

**A dedicated "listeners" wire request.** Unnecessary. `GET_STATE` already
carries the report, versioned, and `phux doctor` depends on it.
