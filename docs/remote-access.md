---
audience: humans, contributors
stability: evolving
last-reviewed: 2026-09-18
---

# Remote access

**TL;DR.** Reach another machine with one command: `phux host add me@mini`
sets it up over the ssh you already have — confirms phux is there, starts and
supervises its server, pairs, finds a direct route, registers it — and from
then on `phux attach mini` dials it directly. A server you stop by hand is
restarted by the next attach. No account, no hex strings typed by hand; the
manual, overlay, and relay paths are below for when that command cannot.

---

## The short way: `phux host add`

```sh
phux host add me@mini
phux attach mini
```

The first command reads like the `ssh me@mini` you already type, and it uses
that same trust: anyone who can ssh to the host can run `phux pair` there and
read the token, so this grants nothing ssh did not already grant
([ADR-0055](adr/0055-always-on-server-and-ssh-bootstrapped-enrollment.md),
[ADR-0122](adr/0122-host-add-is-the-front-door.md)). Prefer to attach
straight away? `phux --remote me@mini` does the same setup and then attaches.

### What it checks, in order

Each step prints one line as it happens; each failure names the next command.

1. **phux is on the host.** `ssh me@mini phux --version`. ssh failing is
   reported as that, with `ssh me@mini` to check; no phux there gets the
   install one-liner (`ssh me@mini 'curl -fsSL https://phux.sh/install | sh'`)
   or `--remote-phux PATH` when it is installed somewhere a non-interactive
   shell does not look.
2. **A server is running and will keep running.** `phux service install
   --quic` writes the host's per-user unit (launchd on macOS, systemd
   `--user` on Linux) and starts it. A server that is already running is
   left alone and the unit is armed for its next start (`--adopt`); a host
   with no service manager gets an unsupervised `phux server --ensure` and a
   warning that it will not survive a reboot. `--no-service` asks for that
   deliberately.
3. **Pairing.** `phux pair --json` on the host mints a token and reports the
   certificate fingerprint and the detected overlay addresses. A token store
   that predates versioning is migrated once, and the server restarted so its
   listeners re-read it.
4. **A direct route.** Every candidate is dialed briefly with the credentials
   just minted: `--endpoint` if you gave one, each overlay address, then the
   host ssh itself connects to (`ssh -G`). The first that answers is
   registered as `quic://HOST:PORT`.
5. **Registration.** The entry lands in `[[remote]]` under the host's name
   (`mini` for `me@mini`; `--name` to choose), with the token owner-only
   under the state dir, the certificate pin, and the ssh destination it was
   set up through. Nothing answered? The entry is `ssh://me@mini`, which still
   attaches through ssh, and the first candidate is kept as `direct` so a
   later attach can try it again and promote it once UDP is open.

Running it again on a registered host is safe: if the saved route answers it
says so and changes nothing; if not, it sets the host up again.

`--role satellite` uses the same steps to register a peer this hub dials for
its users instead of a server you attach to. `--ssh-only` registers an
`ssh://` entry without contacting the host at all.

### What happens when the server is stopped

Stopping the server on `mini` by hand — `phux kill --server`, or your own
`kill` — leaves its unit loaded and stopped on purpose: a deliberate stop
stays stopped. The next `phux attach mini` (or `phux --remote mini`) walks the
same ladder an operator would:

1. dial the saved route; an `ssh://` entry with a kept `direct` route tries
   that route first and promotes it if it answers;
2. nobody answered: start the server over `ssh me@mini` and dial again with
   the saved credentials — no re-pair;
3. still refused: re-pair over ssh and rewrite the entry;
4. ssh itself failed: report the dial error and the ssh error together, with
   both remedies.

`--no-enroll` stops after the first dial. The headless verbs (`ls --remote`
and friends) never shell out from a `--json` call.

### The manual form

When the credentials were minted elsewhere — a phone paired from a QR, or a
host you cannot ssh to — register exactly what you hold:

```sh
phux host add mini quic://100.64.0.2:8788 --token-file ~/.local/state/phux/remotes/mini.token --cert-fingerprint AB:CD:...
phux host add mini ssh://me@mini            # ssh trust only; no credentials
```

`NAME ENDPOINT`, or an endpoint URI alone (`--name` to label it), is the
manual form; anything else is an ssh destination. Each form refuses the
other's flags by name.

### Pairing without ssh at all

If the host has no ssh you can use, run `phux pair` there, copy the one-tap
link it prints, and hand it over:

```sh
phux --remote mini --code 'https://phux.sh/connect?url=wss://100.64.0.2:8787&fp=...&token=...'
```

That is the same link `phux pair --qr` renders for a phone, so a laptop and a
phone pair through one artifact. `--code` also accepts the link's
`phux://connect?...` spelling, which `phux pair` prints on a second line for
older app builds. The link is registered under the target's name, and later
attaches need no code.

`PORT` on a `--remote` target defaults to `8788`, the port a server auto-binds
on its overlay address
([ADR-0081](adr/0081-overlay-auto-listen-and-one-command-pairing.md)). The
`user@` half is a label, not a wire identity: phux runs one server per user
and the QUIC preamble carries a bearer token, so which server you reach is
decided by the address and port
([ADR-0093](adr/0093-remote-target-as-a-resolution-ladder.md)).

### Managing a remote host's sessions without attaching

The session verbs take the same `--remote` target, placed after the verb, so
you can create, list, rename, and kill sessions on another machine from a
local shell:

```sh
phux new --remote me@mini -s build --json -- make watch
phux ls --remote me@mini
phux rename --remote me@mini build ci
phux kill --remote me@mini ci
```

`ls`, `new`, `kill`, `rename`, and `detach` accept it. Each one resolves the
target through the same ladder as `phux --remote` and dials the same QUIC or
WSS endpoint, so a host added once needs nothing more here (and a cold host
is set up over ssh the first time, exactly as attach would). With `--json` a
cold host is refused instead of set up, with the remedies in the error's
`remedy` field: setup narrates on stderr and ssh may prompt, and a
machine-readable call must do neither. Three limits are deliberate:

- `--remote` and `--socket` cannot combine: one names a local socket, the
  other a network dial.
- `phux kill --server --remote HOST` is refused. The server accepts its stop
  command on the local socket only, so run `phux kill --server` on that host.
- An `ssh://` registry entry is refused. It carries an interactive attach
  over `ssh -t` and nothing else; `phux host add HOST` gives it a direct
  QUIC endpoint the session verbs can dial.

`phux new --remote` without `--json` creates the session and attaches to it,
like the local form. With no `--cwd`, a remote session starts in the far
server's default directory: a path on this machine names nothing there.

### From Cockpit

Cockpit's Connect to Host (`cmd+shift+O`) reads this same `[[remote]]`
registry and dials through the same QUIC/WSS stack, so a host that
`phux --remote NAME` reaches is one Cockpit reaches by NAME. It does only
the first rung of the ladder: setup stays in the terminal, and an
unregistered host is refused with the command that adds it. Details,
including the `phux-remote` setting and relaunch behavior, are in
[Cockpit's remote hosts](../clients/cockpit/docs/REMOTE_HOSTS.md).

### The mosh-style way: `phux attach --ssh`

When you can ssh to a host that has no overlay and no phux service, attach
through ssh the way mosh does:

```sh
phux attach --ssh me@box
phux attach work --ssh me@box
```

ssh authenticates you, with its host-key check and any password or 2FA prompt
on your terminal as usual, and runs `phux bootstrap` on the host. That starts
your server there if none is running, and has it open a QUIC listener for this
one attach that admits only a token minted for it. The port, the certificate
fingerprint, and the token come back over the ssh channel. phux pins the
fingerprint, dials the listener, and ssh exits. From then on the session rides
QUIC, so it survives a network change, renders locally with predictive echo,
and stays on the server when you detach.

Nothing is registered on either side. The listener closes about two minutes
after its last connection, and each cold attach bootstraps again. The host
needs phux installed; a non-interactive ssh shell may not have Homebrew or Nix
on its `PATH`, so name the binary with `--remote-phux /opt/homebrew/bin/phux`.
`--udp-ports 60000-61000` keeps the listener inside one firewall rule.

If the QUIC dial does not connect within a few seconds, or the host's phux
predates `phux bootstrap`, the attach falls back to `ssh -t me@box phux attach`
and says why. A registered `ssh://` host takes the same path.

The rest of this page is the manual path: what `host add` automates, and
what to do when it cannot reach the host.

### Joining a satellite to this hub

`phux host add` (default `--role remote`) attaches *to* another machine. To
have this machine *dial* another as a federation satellite, pass
`--role satellite`:

```sh
phux host add --role satellite mini
```

One command, typically under a minute if `mini` already has phux and you
can `ssh mini`:

1. Confirms phux is on `mini` and installs its per-user service (launchd
   on macOS, systemd `--user` on Linux) with a QUIC listener, so the
   satellite survives logout and reboot.
2. Mints a pairing token there and pins the certificate fingerprint.
3. Registers `mini` in this machine's `[[satellites]]` registry. The token
   is stored owner-only (`0600`) under the state dir; it never lands in
   argv, `config.toml`, or logs.
4. Ensures this machine's per-user service runs with `--hub`. If a unit
   already exists, `--hub` is patched into its argv and existing
   `--quic` / `--listen` / `--restore` / `--socket` arguments stay. A
   naive `phux service install --hub` would drop them
   ([ADR-0083](adr/0083-in-place-supervisor-unit-reconcile.md)).

Afterwards this machine is the hub: host-qualified operations reach
`mini` over the hub-and-spoke link. Join stays accountless QUIC on your
overlay; there is no phux-operated relay on this path.

If the local server is already running without `--hub`, the unit is
updated and hub mode starts the next time that server starts — the
running process is not restarted, so panes stay up. `--no-service` skips
installing the *remote* unit only; the local `--hub` ensure still runs.

## Why an overlay

phux already ships everything a remote attach needs except reachability: wss://
(TLS 1.3) and QUIC transports, `phux pair` to mint a bearer token plus a
certificate fingerprint, and a non-loopback bind that engages TLS and token
auth automatically
([ADR-0031](adr/0031-remote-consumer-auth-and-encryption.md)). What remains
is purely packet reachability — a self-hosted server behind NAT or CGNAT has no
inbound-reachable address. The sanctioned answer is a WireGuard-class overlay
network ([ADR-0037](adr/0037-overlay-network-reachability.md)): an L3
substrate that hands the client a routable address (a `100.x` IP or a MagicDNS
`*.ts.net` name) which phux dials exactly like a LAN address, with zero new
code. Cert pinning is on the fingerprint, not the hostname, so overlay DNS
names work unchanged. phux is overlay-agnostic, and the fully-OSS
Headscale/WireGuard path is first-class, not a downgrade. Hosted relays,
rendezvous servers, and hole-punching are deliberately out of scope. The trust
model and environment knobs live in
[operations.md](./operations.md#connecting-from-another-network-overlay-reachability);
this page owns the step-by-step task.

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
`https://phux.sh/connect?url=…&fp=…&token=…` link carrying the URL,
fingerprint, and token together, and `phux pair --qr` renders that same link
as a scannable terminal QR. It is an https Universal Link rather than a
custom `phux://` scheme so that only the app which owns the domain can
receive it — a custom scheme is not exclusive on iOS, and the link carries a
bearer token. The same link is printed a second time as `phux://connect?…`
for app builds that predate Universal Link support. Treat the link, the QR,
and the second spelling like the token itself: they carry the credential.
`--name` labels the server in the device's list.

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
[ADR-0051](adr/0051-outbound-dial-out-connector-transport.md) and
[ADR-0052](adr/0052-connector-route-identity-and-config.md).

## Troubleshooting

Failures fall into a few classes, and the symptom tells you which one you have.

- **No route / connection timed out / connection refused.** An overlay
  problem, not a phux problem. Check `tailscale status` (both peers listed and
  not `offline`) and `tailscale ping <host>` on Tailscale/Headscale, or `wg
  show` for a recent handshake on raw WireGuard. Confirm the server binds an
  address the overlay routes (`0.0.0.0:8787` or the overlay IP itself) and
  that no host firewall drops the port. QUIC needs UDP end to end — if QUIC
  times out but wss:// works, UDP is blocked; stay on `--ws`.
- **Connect succeeds, then hangs forever; `phux ls` on the server is fine.**
  This is a host firewall stealth-drop, not an overlay failure. On macOS the
  Application Firewall completes the TCP handshake and never delivers the
  bytes to phux, so the server logs nothing and UDS/loopback checks stay
  green. phux ships adhoc-signed, so it is not covered by "automatically
  allow signed software" and needs an explicit allowlist entry. That entry
  is keyed to the exact binary path — Homebrew's
  `/opt/homebrew/Cellar/phux/<version>/bin/phux` changes on every upgrade,
  which silently breaks a previous allow. `phux upgrade` re-execs the
  installed path (not a deleted tempfile) so the *current* Cellar binary
  can be allowlisted; the next version bump still needs a new allow.
  `phux doctor` on the server host probes the bound non-loopback listener
  and names this as `remote-reachable`. Until release binaries are
  Developer ID signed and notarized, the durable workaround on a host that
  already lives behind Tailscale/WireGuard is to turn the Application
  Firewall off, or re-allow the new Cellar path after every upgrade. Check
  with
  `/usr/libexec/ApplicationFirewall/socketfilterfw --getglobalstate`.
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
  when it is generated ([ADR-0091](adr/0091-certificate-names-the-advertised-address.md)),
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
[ADR-0037](adr/0037-overlay-network-reachability.md). For the full attach
and pair CLI surface, see [the reference TUI](./consumers/tui.md).
