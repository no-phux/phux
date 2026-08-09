---
audience: contributors
stability: stable
last-reviewed: 2026-08-09
---

# 0081 — Overlay auto-listen and one-command pairing

**TL;DR.** Pairing a phone required reconfiguring and restarting the server,
because the remote listener only existed if `PHUX_WS_ADDR` had been set before
startup — and a restart costs the running sessions, which are exactly what the
user wanted on their phone. The server now binds a TLS listener on its detected
overlay address at startup, gated on the pairing-token store, which rejects
every connection while it is empty. `phux pair` becomes a pure credential
operation: no restart, no lost sessions, no flags. Only the default profile
auto-binds, because a port is global to the host in a way a socket path is not.

Status: Accepted
Date: 2026-08-09
Builds on: ADR-0037 (overlay-agnostic address detection), ADR-0031
(auto-provisioned remote TLS + token store), ADR-0080 (profile isolation)

## Context

The documented path to reaching a phux server from a phone was three commands
plus two hand-copied hex strings:

```sh
phux pair                            # prints a token and a fingerprint
phux server --listen 0.0.0.0:8787    # the port is knowledge the user must have
phux attach --ws wss://<overlay-host>:8787 --token HEX --cert-fingerprint FP
```

Every ingredient already existed and none of them composed. `phux pair` on a
bare machine minted the token, provisioned the certificate, rendered a QR, and
*detected the host's overlay address* — then refused to emit a connect link
because `PHUX_WS_ADDR` was unset and it had no port to pair with the address it
had just found.

The obvious fix — have `phux pair` configure the listener and restart the
server — is wrong. The listener is built once, at startup, from flags and
environment; changing it means a new server process. That discards the running
sessions. "I have work running, I pair my phone, and now I can see that work"
is the entire point, and a flow that begins by destroying the work fails at
exactly the moment it is supposed to deliver.

Reconfiguring a live server instead would need a new wire command plus an
accept loop that admits listeners after startup — a protocol change and a
restructure, for a problem that has a simpler answer.

## Decision

**The listener exists before anyone asks for it.** When no `--listen` /
`--quic` flag and no `PHUX_WS_ADDR` / `PHUX_QUIC_ADDR` is set, the server binds
the first detected overlay address on ports 8787 (WebSocket) and 8788 (QUIC).
Pairing then adds a credential to a door that is already there.

**This is safe to default on because the listener authenticates nobody until
you pair.** Three properties, all pre-existing, combine to make it so:

- it binds a **detected overlay address** (ADR-0037: Tailscale/WireGuard and
  friends), never `0.0.0.0` — so it is not exposed to whatever untrusted
  network the machine is on, and on a host with no overlay it does not exist;
- it is **TLS-only**, with the ADR-0031 auto-provisioned certificate whose
  fingerprint `phux pair` prints;
- it is **token-gated**, and a missing or empty token store rejects every
  connection (`auth::TokenStore::load`, pinned by
  `missing_file_is_empty_store_that_rejects_all`).

Before `phux pair` is ever run, this is a TLS port on an already-authenticated
network that turns everyone away. `PHUX_NO_AUTO_LISTEN=1` suppresses it for
operators who want no unsolicited bind at all.

**Only the default profile auto-binds.** ADR-0080 scopes the socket path per
profile, but a TCP/UDP port is global to the host. Two servers auto-binding
8787 would race, and the loser would be whichever started second — a failure
that would present as "remote access randomly stops working" and be
miserable to diagnose. A development build has no business serving anyone's
phone; `--listen` still works for testing the remote path explicitly.

**`phux pair` derives the port it knows the server used.** With an overlay
address detected and `PHUX_WS_ADDR` unset, it falls back to the same default
port the server auto-binds, so the connect link and QR are complete with no
arguments.

## Consequences

- The one-minute flow is one command: `phux pair`, then scan. Running sessions
  are untouched, because nothing restarts.
- A host on a tailnet now has a listening TCP and UDP port it did not have
  before. It rejects everything until paired, and is invisible off the
  overlay — but it is a real change in default posture, which is why it is
  recorded here and why the opt-out exists.
- Revoking a device is still deleting its line from the token store; with no
  tokens left, the listener returns to refusing everyone.
- A server started before this change has no auto-listener until it restarts.
  ADR-0080's version-skew handoff covers the common case: attaching with an
  upgraded binary re-execs the server in place, panes intact, and the new
  image binds the listener.
- Overlay detection is best-effort by construction (ADR-0037). A raw WireGuard
  or Nebula overlay on an ordinary private range is not detected, so those
  hosts get no auto-listener and keep using `--listen` explicitly. That is the
  same limitation `phux pair`'s address printing already had.

## Alternatives considered

**Reconfigure the running server over the wire.** A `Listen { addr }` command
plus a dynamic accept loop. Rejected as disproportionate: a protocol change
(spec, changelog, version bump) and a restructure of the startup path, to
avoid a restart that binding-up-front avoids entirely. Worth revisiting only
if listeners need to change during a server's life for some *other* reason.

**Have `phux pair` install a service and restart.** Considered and rejected on
the session-loss argument above. It is also actively hazardous with ADR-0080's
corrected restart policy: the supervised server would fail to bind against the
already-running one (`SocketBusy`, non-zero exit) and be restarted every 30s
indefinitely — a flow that reliably manufactures the crash-loop ADR-0080 exists
to eliminate.

**Bind `0.0.0.0` instead of the overlay address.** What the previous
documentation suggested. Rejected: it exposes the port on every interface,
including untrusted networks, and it collides with unrelated local services on
the same port — a real collision was observed on the machine this work was done
on. Binding the specific overlay address is both narrower and less fragile.

**Leave it opt-in behind a config key.** Preserves the old posture exactly, and
preserves the problem: the user must discover the key, the port, and the
address. A default that is safe by construction is worth more than a default
that is merely unchanged.
