---
group: connect
order: 0
---

# Remote machines

Reach another machine's phux server: add it once over SSH, then attach to
its sessions directly. There are no accounts and no hosted service.

For messaging with another person rather than controlling your own
machines, use [conversations](./chat.md): one native messaging invite,
explicit private routing, and no terminal sharing.

```sh
phux host add me@mini --install
phux attach mini
```

**Access to a phux server is access to everything it can do**, including
starting programs as its user. A server listens only on its local socket
until you set it up for remote use. [Security](./security.md) covers scopes,
held actions, and the audit log.

## Adding a machine

`phux host add me@mini` works over `ssh me@mini`. It checks that phux is
there (`--install` installs or updates it on your release channel),
installs a service so the server survives reboots, sets
`[policy] mode = "paired"`, opens a QUIC listener on UDP 8788, enrolls a
client certificate and token for this machine, and saves the entry as
`mini`. Run it again whenever the host stops answering: it checks the setup
and repairs what broke.

A server already running there opens its listener in place, with its panes
kept. If it can't (another program has the port), the machine is left as it
was: pass `--quic-port` to pick another.

SSH signs in once for every step ([SSH](./ssh.md)). If a non-interactive
shell can't find phux on the host (some Homebrew or Nix installs), pass
`--remote-phux /opt/homebrew/bin/phux`; the entry remembers it. Every flag is
in the [CLI reference](./reference/cli.md#phux-host-add).

A second server on the same machine at another port is another host:
`phux host add mini:9788` next to `mini` saves `mini-9788` and leaves `mini`
alone. `phux attach mini-9788` and `--remote mini:9788` reach it; pass
`--name` to pick the name yourself.

### Machines on your tailnet

```sh
phux host discover            # Mac and Linux machines on your tailnet
phux host discover --login    # sign in to or reconnect Tailscale first
phux host add mini.tail1234.ts.net --name mini --install --accept-new-host-key
```

`--accept-new-host-key` is safe here because the tailnet vouches for the
address. Your own devices can skip pairing
([tailnet owner devices](./config.md#tailnet-owner-devices)). The desktop
app's **Add a machine** does the same.

## Using a machine

```sh
phux attach mini                 # QUIC straight to the server
phux ls --all                    # sessions here and on every registered machine
phux new --remote mini scratch   # create a session there
phux host ls
```

Session commands (`ls`, `new`, `kill`, `rename`, `detach`, `upload`,
`download`, `whoami`, `forward`) take `--remote NAME`. Pane commands act on
their own server; to script another machine's panes, run phux there over SSH
or [link the machines](#linking-machines). Several clients can attach to one
remote session at once.

## Who can connect

`[policy] mode` decides:

| Mode | Lets in |
| --- | --- |
| unset or `local` | The local socket only. A configured network listener or relay makes the server refuse to start. |
| `paired` | The local socket, plus network connections with an enrolled client certificate and a token. |

The certificate is issued by the server's own certificate authority, which
the client pins at pairing and checks on every connection. A token alone
never gets anyone in. Certificates from `phux host add` last 90 days;
`phux doctor` warns two weeks before expiry and `phux host renew mini`
replaces one.

## Who can do what

**A machine added with `phux host add` can do everything you can on that
server**, including typing into panes. Add only machines you control.
**The local socket gives full access to any program running as the same
user.** Narrower keys, read-only keys, and grants for mirrored pages,
windows, and displays are in [Security](./security.md#scopes).

## Attaching over SSH

`phux attach --ssh me@box` needs no pairing and no service
([SSH](./ssh.md#attaching-over-ssh)).

## Pairing with a link

When you can't SSH to the server, run `phux pair` there. It prints a link
and a QR code; on the client:

```sh
phux attach --remote mini --code 'https://phux.sh/connect?...'
```

**The link is a secret.** It works once and expires unused after ten
minutes; the device that pairs keeps working until revoked. The token and
enrollment ticket sit after the `#`, so a browser never sends them to
phux.sh. `phux pair --qr` shows only the QR, for a phone.

## Relays

When neither machine can reach the other, run a relay both can reach:

```sh
phux relay pair --route home          # on the relay: prints a token and a fingerprint
phux relay run --listen 0.0.0.0:4433
```

Give the server a `[[connector]]` entry with the relay address, token file,
and fingerprint ([config](./reference/config.md)), then mint the client's
link with `phux pair --relay-route home`. The server dials out, so it needs
no open port. **The relay sees only ciphertext**: TLS runs end to end with
the server inside the relayed stream.

## Linking machines

A **hub** lists and routes to other servers, its **satellites**, whose panes
appear on the hub as `edge/@7`.

```sh
phux host add me@edge --role satellite    # on the hub
phux service install --hub
```

A satellite that can't accept connections dials out instead:

```sh
phux host ticket sandbox     # on the hub: a code good for ten minutes
phux host join -             # on the sandbox: paste the code
```

**Joining a hub gives the hub full control of the satellite**, as the user
the satellite's server runs as. `phux host ticket --mail-only` and
`phux host join --mail-only` share only mail and claims, with no terminal
access. The hub must run `paired`; the satellite can
stay `local`. A satellite credential opens no terminal on the hub. To cut a
satellite off, `phux workload revoke` its credential on the hub and
`phux host rm` it.

### Mail across a hub

On a joined satellite, `phux mail`, `phux claim`, and the MCP mail and claim
tools use the hub's store, so agents on both sides share mail and claims
([mail and path claims](./agents.md#mail-and-path-claims)). Each side
addresses as it would at home: in box `min-api`, the hub's `@749` is
`hub/@749`; on the hub, the box's `@3` is `min-api/@3`.

The hub files everything a satellite does under that satellite's name. A
satellite can write to any address and list every claim, but cannot read,
take, or acknowledge the hub's mail or renew or release its claims. If the
hub is unreachable the command fails; it never falls back to the local
store. A machine added with `phux host add --role satellite` keeps its own
mail. Claims compare paths as written, so a box that sees the project at
another path does not conflict with the hub.

### Minimal boxes

[Minimal](https://minimal.dev) runs agents in isolated boxes. One command
makes every box join this machine's phux:

```sh
phux host minimal setup
min session activate     # the box appears as min-NAME
```

![phux host minimal setup, then a plain min session activate joins the box](assets/minimal-demo.gif)

`setup` writes a Minimal loadout that copies a Linux phux into each box,
makes it a default loadout, and installs a service running
`phux host minimal watch`. The watcher joins each new box with a single-use
ticket; the box dials `host.min.internal`, so this server needs
`mode = "paired"` and a QUIC listener on loopback or all addresses. Each box
gets a session `min-NAME` whose pane is a shell in the box. Destroying the
box ends the session and revokes its credential. Boxes with
`--no-loadouts`, `--network none`, or egress that blocks
`host.min.internal` never join. `sync`, `watch`, and `remove` are in the
[CLI reference](./reference/cli.md#phux-host-minimal).

## Managing credentials

```sh
phux pair ls                     # tokens: unused, active, expired
phux pair revoke ID
phux pair prune --unused-for 30d
phux workload list               # enrolled client certificates
phux workload revoke sha256:...
phux host renew mini
```

Revoking refuses new connections and closes the live ones it admitted, with
no restart. It does not stop programs in panes. `phux host rm` only forgets
a machine locally; it revokes nothing. [File locations](./reference/files.md)
lists every credential file.

## Troubleshooting

`phux doctor` on the server checks the listener, the certificate, and
whether the advertised address is reachable.

- **`could not install phux`**: release builds exist for Apple silicon macOS
  and glibc Linux (x86_64, arm64). Run the install script on the host to see
  its output.
- **`Host key verification failed`**: connect once with `ssh` to save the
  key, or pass `--accept-new-host-key` when something else vouches for the
  address.
- **`no answer within 5s; is UDP filtered?`**: UDP 8788 is blocked, so an
  `ssh://` route was registered. Open the port or use a relay.
- **Token revoked or certificate authority changed**: run
  `phux host add NAME` again.
- **`incompatible protocol`**: the releases are too far apart. Follow
  [Versions](./troubleshooting.md#versions) on the remote machine.
- **Older remote server, same protocol major**: `ssh HOST phux update`, then
  `ssh HOST phux upgrade`; panes survive.
- **A paired phone can't close or organize panes**: it was enrolled before
  those permissions existed. On the server, `phux workload repair-paired
  sha256:ID` (from `phux workload list`), then reconnect the phone.
