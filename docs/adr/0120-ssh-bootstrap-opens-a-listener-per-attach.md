---
audience: contributors
stability: stable
last-reviewed: 2026-09-13
---

# 0120 — ssh bootstrap opens a listener for one attach

**TL;DR.** `phux attach --ssh HOST` runs `phux bootstrap` on the host over
ssh. That starts the server if needed and sends it `OPEN_LISTENER`, a
local-socket-only command that binds a QUIC listener on demand and admits
only an in-memory token minted for it. The client pins the fingerprint that
came back over ssh, dials QUIC, and ssh exits. The listener closes after its
linger and never survives an upgrade.

Status: Accepted
Date: 2026-09-13

## Context

An operator who can `ssh HOST` already trusts that host, and phux's remote
path still asked for more. A direct QUIC dial needed a listener: the overlay
auto-listener (ADR-0081) exists only where Tailscale or WireGuard does, and
`phux host enroll` installs a service to get one. A plain VPS with a public
address and neither got an `ssh://` entry (ADR-0055 §5): `exec ssh -t HOST
phux attach`, which renders on the host, cannot roam, and has no predictive
echo.

mosh and rose show the other shape. ssh authenticates and bootstraps a UDP
server, the port and key come back over ssh's stdout, and the session leaves
ssh behind. phux differs in one respect that shapes everything below: its
server is long-lived and per user (ADR-0003), and its listeners were bound
once, at startup.

## Decision

1. **`OPEN_LISTENER`** (command tag `0x1c`, feature bit
   `OPEN_LISTENER = 0x00800000`, [L1.md](../spec/L1.md) §5.6) carries a
   transport (`QUIC = 0`, the only one defined), an optional inclusive port
   range, and a linger in seconds. The server binds `[::]`, or `0.0.0.0`
   without IPv6, and replies `OkWith(Json)` with the port, the certificate
   fingerprint, a token, and the effective linger. It is refused with
   `PERMISSION_DENIED` on every transport but the Unix socket, like
   `SHUTDOWN`.
2. **Admission is scoped to the listener.** It admits only the token it
   minted, which is held in memory as a SHA-256 verifier and never written to
   the store. Pairing-store tokens are refused there, and closing the
   listener revokes its token. The token is outer admission only (ADR-0116);
   the session then rides ordinary QUIC.
3. **Lifetime.** A listener closes once no connection has been open through
   it for its linger, before the first connection or after the last. The
   linger defaults to 120s, longer than a remote client's 60s reconnect
   window, and is capped at an hour. It is not a runtime flag, so a graceful
   upgrade drops it, and so does server exit.
4. **`phux bootstrap`** is hidden. It ensures the server exactly as naked
   `phux` does, seeding the default session and upgrading an older running
   build in place. A server it starts is marked as started without a login
   shell, as a service unit is (phux-87rr). It then sends `OPEN_LISTENER` and
   prints one JSON line that adds `phux_version` and `protocol_version`.
5. **`phux attach --ssh DEST`** runs `ssh -T DEST phux bootstrap` with
   prompts and stderr on the terminal. It reads the last JSON line, asks
   `ssh -G` for the real hostname, and refuses a protocol `major.minor`
   mismatch. A 5s probe dial comes next, then the ordinary pinned QUIC
   attach, with reconnect, input replay, and predictive echo unchanged. When
   UDP does not connect, the host cannot bootstrap, or the protocols differ,
   it falls back to `ssh -t DEST phux attach`. When ssh itself fails, or phux
   is missing on the host, it reports the failure and does not fall back.
6. **`ssh://` registry entries bootstrap first.** This amends ADR-0055 §5,
   which exec'd ssh unconditionally. `--remote`'s ssh rung, which registers
   `ssh://` for a host with nothing dialable, now reaches QUIC this way.

## Why

Opening the listener at runtime lets its lifetime match the attach that asked
for it. The alternative that avoids a wire change, a graceful re-exec with
`--quic` added, disconnects every attached client to serve one. It also
writes the listener into the runtime flags that every later upgrade
re-applies, so one bootstrap from a café would leave a public UDP port open
for good.

Local-only keeps authority where it already was. Whoever can send
`OPEN_LISTENER` already owns the socket, and ssh proves that ownership to the
dialing side. Like enrollment (ADR-0055), the command grants nothing ssh did
not already grant.

Keeping the token in memory means nothing accumulates across attaches and
revoking it takes no bookkeeping. The fingerprint arrives over the
authenticated ssh channel, so pinning it is not trust on first use.

QUIC rather than a stdio tunnel keeps ssh out of the session. ADR-0098
refuses an ssh-auth-suffices transport, and a tunnel would lose roaming.

## Tradeoffs

- A public UDP port is open for each attach's life, behind TLS 1.3, a pinned
  certificate, and a 256-bit token, bounded by the linger. `--udp-ports`
  keeps it inside one firewall rule.
- The first bootstrap on a host with no certificate mints one naming only
  loopback. Pinning ignores names, so bootstrap is unaffected, but a later
  `phux pair` cannot widen the certificate (ADR-0091) and warns instead.
- A reconnect after the host's server restarts or upgrades finds the listener
  gone and fails; re-running the attach bootstraps again. Re-bootstrapping
  from the reconnect loop is follow-up work.
- No NAT traversal. Filtered UDP means the `ssh -t` fallback, because
  STUN-style rendezvous is out of scope for self-hosting (ADR-0037).
- Every cold attach pays an ssh round trip, as mosh does.
- The host needs phux installed. Uploading a binary would cross the trust
  boundary ADR-0074 guards, so bootstrap refuses and names the remedy.

## Alternatives

**Graceful re-exec with the listener added.** No wire change, but every
attached client disconnects to serve one new client, and the listener
becomes permanent.

**Listeners for freshly started servers only.** The smallest change, but it
misses the common case, a server that is already running.

**An ssh-stdio tunnel.** ADR-0098 refuses ssh-auth-suffices, and a tunnel
has neither roaming nor independence from the ssh process.

**Minting into the pairing store with an expiry.** Every attach would write
the store, tokens would pile up until expiry, and each would be valid on
every listener rather than one.

**STUN hole punching, as rose does.** It depends on a third-party rendezvous
server, which the self-host repository keeps out of scope.
