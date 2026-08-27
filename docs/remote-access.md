---
audience: humans, contributors
stability: evolving
last-reviewed: 2026-08-22
---

# Remote access over an overlay network

**TL;DR.** Run `phux --remote user@host` and you are done: it resolves an
already-registered host, or pairs one first (from a `phux://connect` code, or
over your existing ssh trust) and remembers it, so every later attach is a
direct QUIC dial with no ssh in the path. `phux host enroll HOST` is the
same setup plus a service unit on the far end. The manual path — put both ends on a WireGuard-class overlay,
mint credentials with phux pair, attach to the overlay address over QUIC or TLS
WebSocket — is documented below for Tailscale, Headscale, and raw WireGuard,
plus troubleshooting for routing, auth, and fingerprint failures. A fourth
path uses the self-hosted reference relay when the server cannot accept an
inbound connection.

---

## Why an overlay

phux already ships everything a remote attach needs except reachability: wss://
(TLS 1.3) and QUIC transports, `phux pair` to mint a bearer token plus a
certificate fingerprint, and a non-loopback bind that engages TLS and token
auth automatically
([ADR-0031](../ADR/0031-remote-consumer-auth-and-encryption.md)). What remains
is purely packet reachability — a self-hosted server behind NAT or CGNAT has no
inbound-reachable address. The sanctioned answer is a WireGuard-class overlay
network ([ADR-0037](../ADR/0037-overlay-network-reachability.md)): an L3
substrate that hands the client a routable address (a `100.x` IP or a MagicDNS
`*.ts.net` name) which phux dials exactly like a LAN address, with zero new
code. Cert pinning is on the fingerprint, not the hostname, so overlay DNS
names work unchanged. phux is overlay-agnostic, and the fully-OSS
Headscale/WireGuard path is first-class, not a downgrade. Hosted relays,
rendezvous servers, and hole-punching are deliberately out of scope. The trust
model and environment knobs live in
[operations.md](./operations.md#connecting-from-another-network-overlay-reachability);
this page owns the step-by-step task.

## The short way: `phux --remote`

One command, and it reads like the one you already type:

```sh
phux --remote me@mini
```

The first time, `mini` is not a registered host, so phux pairs it before
attaching. It walks four rungs, cheapest first
([ADR-0093](../ADR/0093-remote-target-as-a-resolution-ladder.md)):

1. **A registered host** — a `[[remote]]` entry supplies the endpoint, the
   certificate pin, and the token, and the dial is a direct QUIC connection.
   This is the steady state and the only rung that runs once a host is known.
2. **A pasted connect code** — `--code`, below. No ssh, no shell on the far
   end.
3. **A one-time ssh pairing** — runs `phux pair --json` over your existing
   ssh trust, registers what it mints, and dials. Once per host; rung 1
   catches everything after.
4. **A refusal** naming both remedies, when ssh cannot help.

`PORT` defaults to `8788`, the port a server auto-binds on its overlay
address ([ADR-0081](../ADR/0081-overlay-auto-listen-and-one-command-pairing.md)).
Pass `[USER@]HOST:PORT` to say otherwise; the port applies to that dial and
does not rewrite the registry.

The `user@` half is a label, not a wire identity: phux runs one server per
user and the QUIC preamble carries a bearer token, not a username, so which
server you reach is decided by the address and port. `user@` names the ssh
destination for rung 3 and the registry key that remembers the result.

### Pairing without ssh at all

If the host has no ssh you can use — or you would rather not shell into it —
run `phux pair` there, copy the one-tap link it prints, and hand it over:

```sh
phux --remote mini --code 'phux://connect?url=wss://100.64.0.2:8787&fp=...&token=...'
```

That is the same link `phux pair --qr` renders for a phone, so a laptop and a
phone pair through one artifact. The link is registered under the target's
name, and later attaches need no code.

`--no-enroll` refuses the ssh rung outright: an unregistered host is reported
with its remedies named rather than paired.

### The related way: `phux host enroll`

`--remote` pairs a host; it deliberately does **not** install anything there.
When you want the far end to keep a server running across logout and reboot,
use the verb whose subject is that host:

```sh
phux host enroll mini
```

(Before the `phux host` namespace this verb was spelled `phux enroll`. See
[ADR-0066](../ADR/0066-host-namespace.md).)

It confirms phux is installed on `mini`, installs the host's service unit so
the server survives reboot, mints a pairing token there, reads back the
certificate fingerprint and the overlay address, writes the token locally
0600, and registers a `[[remote]]` entry. Afterwards both spellings work with
no flags:

```sh
phux attach mini
phux --remote mini
```

No token, no fingerprint, no address typed by hand. This grants nothing ssh
did not already grant — whoever can `ssh mini` can run `phux pair` there and
read the token themselves ([ADR-0055](../ADR/0055-always-on-server-and-ssh-bootstrapped-enrollment.md)).

A host with no overlay address, or one whose certificate could not be read,
has nothing dialable; enrollment says so and registers an `ssh://` entry
instead. That still gives you `phux attach mini` against a server whose
sessions outlive the connection — it just tunnels through ssh rather than
dialing QUIC. Re-run `phux host enroll` once the overlay is up to upgrade
the transport.

The rest of this page is the manual path: what `enroll` automates, and what
to do when it cannot reach the host.

## Common steps: pair, then listen

Every path below shares the same server-side setup, done once. Pairing order
does not matter: the server re-reads the credential store when it changes, so a
token minted against an already-running listener works at the next connection
attempt, and credential revocation applies just as promptly. Legacy anonymous
token lines require a one-time explicit `phux pair --migrate-legacy`. `phux pair`
never contacts a running server, and it provisions the self-signed certificate
if none exists yet, so the fingerprint it prints is the one the server will
present.

```sh
# On the server host, before starting the listener:
phux pair
```

Its output looks like this (the overlay-address block appears only when a
tailnet or CGNAT-routed address is detected on the host):

```
Credential ID (use with `phux pair rotate|revoke`):
  <credential-id>

Pairing token (a secret — give it to the device once):
  <64-hex token>

Server certificate SHA-256 (verify on the device to defeat MITM):
  <64-hex fingerprint>

Overlay network addresses (dial one of these from the device):
  100.x.y.z

Token written to <state-dir>/remote-tokens
```

Record the token and the fingerprint; every `phux attach` below uses both. The
fingerprint is SHA-256, 64 hex digits, optionally colon-separated.

Keep the non-secret credential ID for lifecycle operations. Rotation prints a
new bearer once and keeps the previous generation valid for at most five
minutes by default; `--overlap-seconds 0` cuts over immediately. An existing
absolute expiry is preserved and can shorten that overlap. An already-expired
credential cannot be rotated and produces no replacement token. Revocation
affects new connections immediately, while already-established sessions
continue until they disconnect:

```sh
phux pair rotate <credential-id> --overlap-seconds 300
phux pair revoke <credential-id>
```

For a phone or tablet, skip the transcription entirely: when the server
address is known — pass `--host HOST:PORT` (or a full `ws://`/`wss://` URL),
or let it fall back to a detected overlay address plus the `PHUX_WS_ADDR`
port — `phux pair` also prints a one-tap
`phux://connect?url=…&fp=…&token=…` deep-link carrying the URL, fingerprint,
and token together, and `phux pair --qr` renders that same link as a
scannable terminal QR. Treat the link and QR like the token itself: they
carry the credential. `--name` labels the server in the device's list.

```sh
# Credentials + a scannable one-tap QR for the device:
phux pair --qr --host 100.x.y.z:8787 --name studio-mini
```

Then start
the listener on a non-loopback bind — TLS and token auth engage automatically:

```sh
phux server --listen 0.0.0.0:8787      # TLS WebSocket (= PHUX_WS_ADDR)
# or, for QUIC:
phux server --quic 0.0.0.0:8788        # (= PHUX_QUIC_ADDR)
```

Prefer QUIC where UDP is open — it handles roaming and connection migration
better. Use `--ws wss://` when UDP is blocked by a network or firewall.

## Path A: Tailscale

[Tailscale](https://tailscale.com) is the frictionless on-ramp.

1. Install Tailscale on both the server host and the client device.
2. Run `tailscale up` on each.
3. Confirm both peers appear in `tailscale status`.
4. Find the server's address: `tailscale status` prints both the `100.x.y.z`
   IP and the MagicDNS name (like `myhost.tailnet-name.ts.net`).

Then dial from the client:

```sh
# QUIC (preferred when UDP is open):
phux attach --quic myhost.tailnet-name.ts.net:8788 --token HEX --cert-fingerprint FP

# TLS WebSocket fallback (when UDP is blocked):
phux attach --ws wss://myhost.tailnet-name.ts.net:8787 --token HEX --cert-fingerprint FP
```

Routable hosts require `--cert-fingerprint` (only loopback trusts the dev
cert). The pin is fingerprint-based, so the MagicDNS name and the `100.x` IP
are interchangeable — no re-pairing when you switch between them. The honest
tradeoff: trust extends to Tailscale's coordination plane, mitigated by phux's
own TLS + token riding on top.

## Path B: Headscale

[Headscale](https://github.com/juanfont/headscale) is a self-hostable,
fully-OSS control plane for the same data plane, for operators who will not
depend on a third-party coordinator. The client tooling is identical.

1. Run a Headscale server.
2. Create a user and a preauth key:
   `headscale users create NAME`, then
   `headscale preauthkeys create --user NAME`.
3. Join each node:
   `tailscale up --login-server https://headscale.example.com --authkey KEY`.
4. Verify both peers with `tailscale status`.

Dial exactly as in Path A, using the Headscale-assigned `100.x` address (or
its DNS name if configured):

```sh
phux attach --quic 100.64.0.2:8788 --token HEX --cert-fingerprint FP
# or
phux attach --ws wss://100.64.0.2:8787 --token HEX --cert-fingerprint FP
```

## Path C: Raw WireGuard

A hand-rolled [WireGuard](https://www.wireguard.com) overlay works the same
way — all three paths look identical to phux, which only ever sees an IP.

1. Generate a keypair on both ends:
   `wg genkey | tee privatekey | wg pubkey > publickey`.
2. Write a minimal `/etc/wireguard/wg0.conf` on each end. Server side:

   ```ini
   [Interface]
   Address = 10.8.0.1/24
   ListenPort = 51820
   PrivateKey = <server privatekey>

   [Peer]
   PublicKey = <client publickey>
   AllowedIPs = 10.8.0.2/32
   ```

   Client side (the `Endpoint` goes on whichever side can see the other's
   public address):

   ```ini
   [Interface]
   Address = 10.8.0.2/24
   PrivateKey = <client privatekey>

   [Peer]
   PublicKey = <server publickey>
   AllowedIPs = 10.8.0.1/32
   Endpoint = server.example.com:51820
   PersistentKeepalive = 25
   ```

3. Bring the tunnel up on both ends: `wg-quick up wg0`.
4. Verify a recent handshake with `wg show`.

Dial the peer's tunnel address:

```sh
phux attach --quic 10.8.0.1:8788 --token HEX --cert-fingerprint FP
# or
phux attach --ws wss://10.8.0.1:8787 --token HEX --cert-fingerprint FP
```

With raw WireGuard there is no MagicDNS; use the tunnel IP or your own DNS.

## Path D: via a reference relay

Paths A-C put both ends on one overlay so the client can reach the server's
address. A relay inverts the direction: the server dials out to a relay you
host, and consumers dial the relay — nothing on the server's network needs
to accept an inbound connection. The tradeoff is stated plainly: the relay
terminates TLS on both legs and sees phux traffic in plaintext. Self-hosting
the relay on a trusted machine is the mitigation.

Set up the route end to end:

1. On the relay host, run `phux relay pair --route ROUTE`, save its printed
   tunnel token out of band, then start
   `phux relay run --listen 0.0.0.0:4433`.
2. On the server host, put that tunnel token in a mode-`0600` file and add:

   ```toml
   [[connector]]
   relay = "RELAY_HOST:4433"
   token-file = "/home/me/.local/state/phux/relay-route.token"
   cert-fingerprint = "RELAY_FP"
   ```

3. Start or restart `phux server`. It supervises every configured connector;
   `--connect RELAY_HOST:4433` selects one exact entry for diagnosis.
4. Attach the consumer, using the route as TLS SNI and the server's ordinary
   `phux pair` token as the consumer credential:

   ```sh
   phux attach --quic RELAY_HOST:4433 --tls-server-name ROUTE \
     --cert-fingerprint RELAY_FP --token SERVER_TOKEN
   ```

`RELAY_FP` pins the relay's certificate on both network legs.
`SERVER_TOKEN` crosses the relay opaquely and is verified by the server;
the tunnel token only authorizes the connector to claim its enrolled route.
The connector re-reads its token file on every redial, so rotation is
`phux relay pair --route ROUTE`, replace the file, then restart either side
when immediate cutover is required.

An unknown route fails the TLS handshake. An enrolled route with no live
tunnel closes as route-offline. A bad tunnel token or certificate pin leaves
the local server running and produces an `outbound connector lost; scheduling
redial` diagnostic. A bad `SERVER_TOKEN` resets only that consumer stream;
the tunnel and other consumers remain live. Full relay state-file,
revocation, and trust-boundary details are in
[operations.md](./operations.md#running-the-reference-relay); the design is
ADR-0057, building on
[ADR-0051](../ADR/0051-outbound-dial-out-connector-transport.md) and
[ADR-0052](../ADR/0052-connector-route-identity-and-config.md).

## Troubleshooting

Failures fall into a few classes, and the symptom tells you which one you have.

- **No route / connection timed out / connection refused.** An overlay
  problem, not a phux problem. Check `tailscale status` (both peers listed and
  not `offline`) and `tailscale ping <host>` on Tailscale/Headscale, or `wg
  show` for a recent handshake on raw WireGuard. Confirm the server binds an
  address the overlay routes (`0.0.0.0:8787` or the overlay IP itself) and
  that no host firewall drops the port. QUIC needs UDP end to end — if QUIC
  times out but wss:// works, UDP is blocked; stay on `--ws`.
- **Auth failure** (HTTP 401 / unauthorized on the WebSocket upgrade; QUIC
  token rejection). The link is fine; the bearer token is missing, mistyped,
  or was revoked. Mint one with `phux pair`; it is live at the next connection
  attempt, with no restart. The 401 is returned before any phux frame is read,
  so a 401 proves reachability.
- **Insecure credential store.** The default store and any path selected by
  `PHUX_WS_TOKENS` must be a regular, non-symlink file owned by the effective
  user with no group or world permissions. Restore owner-only permissions
  (normally `chmod 600 <path>`); authentication fails closed until repaired.
- **Fingerprint mismatch.** The certificate the server presented does not
  match `--cert-fingerprint`. Either the pinned value is stale (the server
  state dir was recreated, regenerating `remote-cert.pem`), an operator
  certificate was substituted via `PHUX_WS_TLS_CERT`/`PHUX_WS_TLS_KEY`, or you
  are dialing the wrong host. Re-run `phux pair` on the server host — it
  re-prints the persisted certificate's fingerprint without contacting the
  running server — and compare. Do not "fix" a mismatch by dropping the flag:
  the pin is what closes the trust-on-first-use MITM window.
- **Certificate name mismatch** (`IP address mismatch`, `NotValidForName`,
  `ERR_CERT_COMMON_NAME_INVALID`) from a client that validates the server name
  — `curl --cacert`, a browser with the certificate trusted, `openssl s_client
  -verify_ip`. `phux attach` and the mobile app never hit this: they pin the
  fingerprint and ignore the name. The certificate's subjectAltName is fixed
  when it is generated ([ADR-0091](../ADR/0091-certificate-names-the-advertised-address.md)),
  so one minted before phux learned to name the overlay address claims only
  loopback and always will. `phux doctor` reports it as `remote-cert` and prints
  the remedy. Widening it means a **new certificate and a new fingerprint**,
  which un-pairs every paired device; do it deliberately or not at all:

  ```sh
  rm ~/.local/state/phux/remote-cert.pem ~/.local/state/phux/remote-key.pem
  phux pair            # regenerates, naming the address it advertises
  ```

  then re-pair every device against the new fingerprint.
- **MagicDNS name does not resolve.** MagicDNS may be disabled on the tailnet,
  or the client OS resolver is not wired up; fall back to the `100.x` IP from
  `tailscale status`. The pin is on the fingerprint, not the hostname, so
  switching between name and IP needs no re-pairing.

Overlay links are higher-latency than a LAN; remote consumers get better
behavior by requesting state-sync output — see
[operations.md](./operations.md#output-mode-for-remote-consumers).

## Scope and alternatives

`ssh HOST phux stdio-bridge` remains a valid manual path where SSH is already
the trust boundary — no token or pin is involved on that transport. Hosted
relay infrastructure, rendezvous servers, STUN/TURN, and reverse tunnels
remain deliberately out of scope for the self-host repo; the self-hosted
reference relay (Path D above) is the one carve-out, per ADR-0057. See
[ADR-0037](../ADR/0037-overlay-network-reachability.md). For the full attach
and pair CLI surface, see [the reference TUI](./consumers/tui.md).
