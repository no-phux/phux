---
audience: humans, contributors
stability: evolving
last-reviewed: 2026-09-30
---

# Remote access

**TL;DR.** Connect another host with `phux host add`, then attach to its
terminals directly. Start with the SSH-based setup below; use manual
enrollment, an overlay, or a relay when that route cannot work.

---

## The short way: `phux host add`

**Before you start:** install phux locally and on the remote macOS or Linux
host, using the same release on both. Confirm you can log in with
`ssh me@mini` and that `ssh me@mini phux --version` finds the remote binary.
Replace `me@mini` below with that working SSH destination. Enrollment uses
your existing SSH trust and changes the remote user's service configuration;
use an account whose terminals you are authorized to control.

You do not need to configure an overlay first if the SSH route already works.
For installs and release compatibility, see the [installation guide](./INSTALL.md).

```sh
phux host add me@mini
phux attach mini
```

**Expected:** enrollment prints its checks and saves a host named `mini`.
Attach opens that host's terminal session. Run `hostname` in its shell to
confirm you are on the intended machine. Press `Ctrl-A`, release both keys,
then `d` to detach; `phux attach mini` returns to the running remote session.
Detaching leaves work on the remote server; a remote server crash or reboot
does not preserve live jobs ([continuity boundaries](./operations.md#workspace-continuity-and-update-survival)).
A later attach also starts the registered host's server if you stopped it
by hand; it does not recover the jobs that server used to run.

If enrollment fails, follow the named failing step below. If a saved host
stops connecting, start with [the diagnostic sequence](#troubleshooting);
do not delete its credentials or disable a firewall to guess at a fix.

Enrollment uses SSH trust: an account that can run `phux pair` and read its
token already has this access
([ADR-0055](adr/0055-always-on-server-and-ssh-bootstrapped-enrollment.md),
[ADR-0122](adr/0122-host-add-is-the-front-door.md)).
`phux --remote me@mini` performs the same setup and then attaches.

### What it checks, in order

Each step prints one line as it happens; each failure names the next command.

1. **Check the binary.** Run `ssh me@mini phux --version`. SSH failures
   suggest checking `ssh me@mini`. A missing binary prompts the install command
   (`ssh me@mini 'curl -fsSL https://phux.sh/install | sh'`) or
   `--remote-phux PATH` if the binary is outside the non-interactive shell's
   `PATH`.
2. **Start and supervise the server.** `phux service install --quic` writes
   and starts a per-user launchd or systemd `--user` unit. If a server already
   runs, `--adopt` leaves it alone and arms the unit for its next start.
   Without a service manager, enrollment uses `phux server --ensure` and
   warns that it will not survive reboot. `--no-service` selects this
   unsupervised mode explicitly.
3. **Pair.** `phux pair --json` mints a token only after the server reports
   a bound remote listener. It returns the certificate fingerprint, detected
   overlay addresses, and listeners. No listener means an error and no token.
   An unversioned token store is migrated once, with a server restart so
   listeners re-read it.
4. **Find a direct route.** Probe candidates with the new credentials:
   `--endpoint` if supplied, overlay addresses, then SSH's resolved host
   (`ssh -G`). Register the first response as `quic://HOST:PORT`.
5. **Register the host.** Write `[[remote]]` under the host's name (`mini`
   for `me@mini`, or `--name`), recording the certificate pin and SSH
   destination. Store the token owner-only under the state directory.
   If no direct route answered, register `ssh://me@mini` and retain the first
   candidate as `direct`. Later attaches retry it and promote it once UDP
   is reachable.

Repeating enrollment leaves a working registration unchanged; if its route
fails, enrollment runs again. Certificate renewal has separate rules below.

### Client certificates and renewal

A host with a direct route also gets a workload client certificate
([workload-auth.md](spec/workload-auth.md) §8.1): the key is generated on
this machine, only its signing request crosses ssh, and the entry names the
key and certificate files (`client-cert`, `client-key`) under
`<state-dir>/remotes/`. Every direct dial presents them, which a server in
`[policy] mode = "paired"` requires.

Certificates last 90 days and do not renew automatically:

- `phux host renew mini` enrolls a fresh certificate over the entry's ssh
  destination and changes nothing else (no re-pairing, no service install).
  A certificate from a different workload CA than the one it replaces is
  refused, so a destination that now reaches another machine changes
  nothing.
- `phux host add me@mini` on a registered host renews a certificate that
  expires within 14 days, has expired, or cannot be read, even when the
  route still answers.
- A dial to such a remote (`phux attach mini`, `phux ls --remote mini`, and
  the other remote verbs) warns once with the `phux host renew` command, and
  `phux doctor` reports it as `client-certs`.

Replacement has two phases: enroll, check, and save the new certificate,
then revoke the old one on the host. Failure before saving retains the old
entry and revokes the unused new credential. Failed revocation prints the
`ssh ... phux workload revoke sha256:...` recovery command. If the host never
held the old credential, that is reported rather than counted as revoked.

An entry with only `client-cert` or `client-key` is re-enrolled the same way.
The credential named by its certificate is revoked; if none can be identified,
the command warns.

`phux host add --json` and `phux host renew --json` report the outcome
beside the `"host"` object:

```json
"enrollment": {
  "status": "enrolled",
  "error": null,
  "credential_id": "sha256:...",
  "expires_at": 1767225600,
  "previous_credential_id": "sha256:...",
  "previous_revoked": true,
  "warnings": []
}
```

`status` is `enrolled`, `kept` (the previous certificate still works and is
not due), `failed` (with `error`; the entry keeps the previous certificate,
or none), or `skipped` (a satellite, `--ssh-only`, the manual form, or no
direct route). `credential_id` and `expires_at` (Unix seconds; the day
admission is expected to end) describe the certificate the entry names now,
`null` for none. `previous_revoked` is `null` when nothing was superseded.
`warnings` carries anything left to do by hand. Paths appear in `"host"`;
key bytes never appear anywhere.

### Managing many hosts

```sh
phux host ls                         # remotes and satellites together
phux host show mini                  # inspect its route and auth references
phux host attach mini                # same repair-aware path as phux attach mini
phux host renew mini                 # replace its workload client certificate
phux host rename mini desk           # rename the local label, keep credentials
phux host disable edge               # pause a satellite without forgetting it
phux host enable edge                # resume it
phux host rm desk                    # forget the entry; token file stays put
```

`ls`, `show`, `rename`, `renew`, `enable`, `disable`, and `rm` accept `--json`.
`attach` is interactive. `show`, `rename`, and `rm` accept
`--role remote|satellite` when the same name exists in both registries;
without it they refuse to guess. Enable/disable apply only to satellites.
Renaming changes the local registry label, not the machine's hostname, service,
session names, or the path to its token file. The original SSH destination is
kept so attach repair still reaches the same machine.
The hub reads satellite entries at startup: after enabling, disabling, or
renaming a satellite, restart the hub for the change to affect live routes.

`--role satellite` uses the same steps to register a peer this hub dials for
its users instead of a server you attach to. `--ssh-only` registers an
`ssh://` entry without contacting the host at all.

### What happens when the server is stopped

A deliberate `phux kill --server` on `mini` leaves its service stopped.
The next `phux attach mini` (or `phux --remote mini`) tries these recovery
steps:

1. Dial the saved route. An `ssh://` entry tries its retained `direct` route
   first and promotes it if reachable.
2. If unreachable, start the server over `ssh me@mini` and retry with the
   saved credentials.
3. If still refused, re-pair over SSH and rewrite the entry.
4. If SSH fails, report both the dial and SSH errors with their remedies.

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

Without SSH access, run `phux pair` on the host and pass its one-tap link:

```sh
phux --remote mini --code 'https://phux.sh/connect?url=wss://100.64.0.2:8787&fp=...&token=...'
```

`phux pair --qr` renders the same link for phones. `--code` also accepts
`phux://connect?...`, the spelling printed for older app builds. The link
registers the target's name; later attaches need no code.

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

`phux ls --all` (`-a`) queries this machine and every registered host
concurrently, with a three-second deadline per host. Results are grouped by
machine; unreachable hosts show their reason without failing the listing.
`--json` emits `phux.hosts/v1`, also used by the TUI sidebar
([ADR-0140](adr/0140-sidebar-machines-come-from-a-hosts-provider.md)).

`ls`, `new`, `kill`, `rename`, and `detach` accept `--remote`. They use the
same resolution and QUIC/WSS dial path as `phux --remote`, including SSH
setup for an unregistered host. Under `--json`, setup is refused with
instructions in the error's `remedy` field: a machine-readable call must
not narrate setup or prompt through SSH. Three limits apply:

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

Cockpit's Connect to Host (`cmd+shift+O`) uses the same `[[remote]]` registry
and QUIC/WSS stack. It only dials saved hosts; enrollment and repair stay in
the terminal. An unregistered host is refused with the command to add it.
See [Cockpit's remote hosts](../clients/cockpit/docs/REMOTE_HOSTS.md) for the
`phux-remote` setting and relaunch behavior.

### The mosh-style way: `phux attach --ssh`

When you can ssh to a host that has no overlay and no phux service, attach
through ssh the way mosh does:

```sh
phux attach --ssh me@box
phux attach work --ssh me@box
```

SSH authenticates with its usual host-key check and password or 2FA prompt,
then runs `phux bootstrap` on the host. Bootstrap starts your server if
needed and opens a QUIC listener admitting only this attach's token. The
port, certificate fingerprint, and token return over SSH. phux pins the
fingerprint, connects over QUIC, and closes SSH. The session can then survive
network changes, render locally with predictive echo, and remain on the
server after detach.

Nothing is registered on either side. The listener closes about two minutes
after its last connection, and each cold attach bootstraps again. The host
needs phux installed; a non-interactive ssh shell may not have Homebrew or Nix
on its `PATH`, so name the binary with `--remote-phux /opt/homebrew/bin/phux`.
`--udp-ports 60000-61000` keeps the listener inside one firewall rule.

If the QUIC dial does not connect within a few seconds, or the host's phux
predates `phux bootstrap`, the attach falls back to `ssh -t me@box phux attach`
and says why. A registered `ssh://` host takes the same path.

### Joining a satellite to this hub

`phux host add` (default `--role remote`) attaches *to* another machine. To
have this machine *dial* another as a federation satellite, pass
`--role satellite`:

```sh
phux host add --role satellite mini
```

With phux installed and `ssh mini` working, the command:

1. Confirms phux is on `mini` and installs its per-user service (launchd
   on macOS, systemd `--user` on Linux) with a QUIC listener, so the
   satellite survives logout and reboot.
2. Mints a pairing token there and pins the certificate fingerprint.
3. Registers `mini` in this machine's `[[satellites]]` registry. The token
   is stored owner-only (`0600`) under the state dir; it never lands in
   argv, `config.toml`, or logs.
4. Ensures this machine's per-user service runs with `--hub`. If a unit
   already exists, `--hub` is patched into its argv and existing
   `--quic` / `--listen` / `--restore` / `--socket` arguments stay.
   Reinstalling with only `phux service install --hub` would drop them
   ([ADR-0083](adr/0083-in-place-supervisor-unit-reconcile.md)).

Afterwards this machine is the hub: host-qualified operations reach
`mini` over the hub-and-spoke link. Join stays accountless QUIC on your
overlay; there is no phux-operated relay on this path.

If the local server is already running without `--hub`, the unit is
updated and hub mode starts the next time that server starts — the
running process is not restarted, so panes stay up. `--no-service` skips
installing the *remote* unit only; the local `--hub` ensure still runs.

## Why an overlay

TLS and pairing authenticate a connection; they do not make a server behind
NAT or CGNAT reachable. A WireGuard-class overlay supplies a routable address
([ADR-0037](adr/0037-overlay-network-reachability.md)). phux dials it like a
LAN address, with TLS and token authentication on non-loopback binds
([ADR-0031](adr/0031-remote-consumer-auth-and-encryption.md)).

Certificate pins identify the certificate, not the hostname, so overlay DNS
names work unchanged. Headscale and raw WireGuard are supported alongside
Tailscale. See [overlay reachability](./operations.md#connecting-from-another-network-overlay-reachability)
for the trust model and environment settings.

## Common steps: listen, then pair

Every path below shares the same server-side setup, done once. First the
server needs a remote listener. On the default profile a host on an overlay
network already has one: the server binds its overlay address on 8787 (wss)
and 8788 (QUIC) at startup
([ADR-0081](adr/0081-overlay-auto-listen-and-one-command-pairing.md)).
Otherwise start it on a non-loopback bind — TLS and token auth engage
automatically:

```sh
phux server --listen 0.0.0.0:8787      # TLS WebSocket (= PHUX_WS_ADDR)
# or, for QUIC:
phux server --quic 0.0.0.0:8788        # (= PHUX_QUIC_ADDR)
```

Then pair, on the server host:

```sh
phux pair
```

`phux pair` first asks the running server (the default socket, or
`--socket PATH`) which remote listeners it has bound, and mints nothing when
none would accept the credential: no server running, or a server with no
remote listener, is an error that names the socket and the fix
([ADR-0141](adr/0141-pair-mints-only-against-a-live-listener.md)). The server
re-reads the credential store when it changes, so the token works at the next
connection attempt with no restart, and revocation applies just as promptly.
Legacy anonymous token lines require a one-time explicit
`phux pair --migrate-legacy`. `phux pair` provisions the self-signed
certificate if none exists yet, so the fingerprint it prints is the one the
server presents.

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
minutes by default (`--overlap-seconds 0` cuts over immediately); an existing
absolute expiry is preserved, and an expired credential cannot be rotated.
Revocation and the end of a rotation overlap also disconnect established
sessions using that credential
([remote consumer trust model](./operations.md#remote-consumer-trust-model-opt-in)).
These, `ls`, and `prune` only edit the store and need no running server:

```sh
phux pair rotate <credential-id> --overlap-seconds 300
phux pair revoke <credential-id>
```

For a phone or tablet `phux pair` also prints a one-tap
`https://phux.sh/connect?url=…&fp=…&token=…` Universal Link (an https link so
only the app owning the domain receives the bearer token), a
`phux://connect?…` spelling for older app builds, and with `--qr` a terminal
QR of the same link. Treat all three like the token itself. The link names the
address the server's wss listener is bound to (the overlay address for a
`0.0.0.0`/`::` bind), or `--host HOST:PORT` (or a full `ws://`/`wss://` URL)
when the device reaches the server some other way, such as a MagicDNS name or
a port forward; `--host` still needs a bound wss listener behind it. A wss
listener bound only to loopback, or none at all, gives no link, and `--qr`
then refuses before minting. `--name` labels the server in the device's list.

```sh
# Credentials + a scannable one-tap QR for the device:
phux pair --qr --name studio-mini
```

Prefer QUIC where UDP is open — it handles roaming and connection migration
better. Use `--ws wss://` when UDP is blocked by a network or firewall.

## Paths A-C: an overlay network

phux only ever sees an IP, so every overlay is dialed the same way once both
peers are on it:

```sh
phux attach --quic HOST:8788 --token HEX --cert-fingerprint FP     # preferred when UDP is open
phux attach --ws wss://HOST:8787 --token HEX --cert-fingerprint FP # when UDP is blocked
```

Routable hosts require `--cert-fingerprint` (only loopback trusts the dev
cert).

Bracket IPv6 literals in WebSocket URLs, for example
`--ws 'wss://[fd00::1]:8787'`. The brackets belong to the URL authority;
TCP resolution and the default TLS certificate identity use the bare address.

- **Path A: [Tailscale](https://tailscale.com).** Install it on both ends and
  run `tailscale up`; `tailscale status` lists both peers with their `100.x`
  IP and MagicDNS name (`myhost.tailnet-name.ts.net`), which are
  interchangeable for the pin. Trust extends to Tailscale's coordination
  plane, mitigated by phux's own TLS + token.
- **Path B: [Headscale](https://github.com/juanfont/headscale)**, the
  self-hostable OSS control plane for the same data plane. Run a Headscale
  server, `headscale users create NAME`, `headscale preauthkeys create --user
  NAME`, then on each node
  `tailscale up --login-server https://headscale.example.com --authkey KEY`.
  Dial the assigned `100.x` address.
- **Path C: raw [WireGuard](https://www.wireguard.com).** Generate a keypair
  on each end (`wg genkey | tee privatekey | wg pubkey > publickey`), write
  `/etc/wireguard/wg0.conf` on each, `wg-quick up wg0`, and check `wg show`
  for a recent handshake. There is no MagicDNS; dial the tunnel IP. Server
  side:

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

## Path D: via a reference relay

An overlay gives the client a route to the server. A relay instead accepts
outbound connections from both, so the server's network needs no inbound
listener. The relay terminates TLS on both connections and sees phux traffic
in plaintext. Host it on a machine you trust.

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
[relay operations](./operations.md#running-the-reference-relay); the design is
ADR-0057, building on
[ADR-0051](adr/0051-outbound-dial-out-connector-transport.md) and
[ADR-0052](adr/0052-connector-route-identity-and-config.md).

## Troubleshooting

Work from the server outward. A timeout alone cannot distinguish a stopped
server, wrong address, blocked port, or broken network route.

1. **Check the host and server.** Can you still `ssh me@mini`? On that host,
   run `phux status` and `phux doctor`. If the server is stopped, the normal
   `phux attach mini` path can [repair it over SSH](#what-happens-when-the-server-is-stopped).
   If startup fails, inspect `phux logs --server` there before retrying.
2. **Check the saved route.** Locally, run `phux host show mini`; compare its
   endpoint with the remote listener and address reported by the host.
   An `ssh://` fallback is usable, not proof that enrollment failed.
3. **Check reachability for that route.** Only if you use Tailscale/Headscale,
   inspect `tailscale status` and `tailscale ping <host>`; on WireGuard,
   inspect `wg show` for a recent handshake. Confirm the listener binds an
   address your client can reach and the intended port is allowed. QUIC needs
   UDP end to end. QUIC timing out while WebSocket works suggests a
   UDP-specific problem; use an already configured WebSocket route while
   investigating rather than removing authentication.
4. **Check host firewall admission.** A connection that opens but stalls can
   be a firewall stealth-drop, but it is not conclusive. On macOS, run
   `phux doctor` on the server host and follow its `remote-reachable` remedy:
   allow the exact phux binary through the Application Firewall. Homebrew's
   Cellar path changes after upgrades, so an old allowlist entry may no longer
   apply. Keep the firewall enabled; an overlay does not replace host policy.
5. **Read the actual refusal.** Authentication and certificate errors need
   the remedies below, not more network retries.

### Credentials and certificate failures

- **Auth failure** (HTTP 401 / unauthorized on the WebSocket upgrade; QUIC
  token rejection). The responding endpoint is reachable, but it rejected
  the credential. Confirm it is the intended host and that the saved token
  is present and not revoked. If needed, mint a new token with `phux pair`
  on the trusted server and update the registration. New tokens are live at
  the next connection attempt; no server restart is required.
- **Insecure credential store.** The default store and any path selected by
  `PHUX_WS_TOKENS` must be a regular, non-symlink file owned by the effective
  user with no group or world permissions. Restore owner-only permissions
  (normally `chmod 600 <path>`); authentication fails closed until repaired.
- **Fingerprint mismatch.** The certificate the server presented does not
  match `--cert-fingerprint`. Either the pinned value is stale (the server
  state dir was recreated, regenerating `remote-cert.pem`), an operator
  certificate was substituted via `PHUX_WS_TLS_CERT`/`PHUX_WS_TLS_KEY`, or you
  are dialing the wrong host. Re-run `phux pair` on the server host — it
  prints the persisted certificate's fingerprint beside a fresh credential
  (`phux pair revoke` it if you do not need it) — and compare. Do not "fix" a mismatch by dropping the flag:
  the pin is what closes the trust-on-first-use MITM window.
- **Certificate name mismatch** (`IP address mismatch`, `NotValidForName`,
  `ERR_CERT_COMMON_NAME_INVALID`) from a client that validates the server name
  — `curl --cacert`, a browser with the certificate trusted, `openssl s_client
  -verify_ip`. `phux attach` and the mobile app never hit this: they pin the
  fingerprint and ignore the name. The certificate's subjectAltName is fixed
  when it is generated ([ADR-0091](adr/0091-certificate-names-the-advertised-address.md)),
  so one minted before phux learned to name the overlay address claims only
  loopback and always will. `phux doctor` reports it as `remote-cert` and prints
  the remedy. Regeneration creates a new certificate and fingerprint,
  invalidating every device's pin. Plan to re-pair them all:

  ```sh
  rm ~/.local/state/phux/remote-cert.pem ~/.local/state/phux/remote-key.pem
  phux pair            # regenerates, naming the address it advertises
  phux upgrade         # restarts the server in place so it presents it
  ```

  then re-pair every device against the new fingerprint.
- **MagicDNS name does not resolve.** MagicDNS may be disabled on the tailnet,
  or the client OS resolver is not wired up; fall back to the `100.x` IP from
  `tailscale status`. The pin is on the fingerprint, not the hostname, so
  switching between name and IP needs no re-pairing.

Overlay links are higher-latency than a LAN; remote consumers get better
behavior by requesting state-sync output — see
[remote output modes](./operations.md#output-mode-for-remote-consumers).

## Scope and alternatives

`ssh HOST phux stdio-bridge` remains a valid manual path where SSH is already
the trust boundary — no token or pin is involved on that transport. Hosted
relay infrastructure, rendezvous servers, STUN/TURN, and reverse tunnels
remain deliberately out of scope for the self-host repo; the self-hosted
reference relay (Path D above) is the one carve-out, per ADR-0057. See
[ADR-0037](adr/0037-overlay-network-reachability.md). For the full attach
and pair CLI surface, see [the reference TUI](./consumers/tui.md).
