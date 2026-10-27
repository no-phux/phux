# Remote machines

phux has no accounts and no hosted service. Each machine runs its own
server, and you tell one machine how to reach another once, over the SSH
access you already have. After that, you attach to the other machine's
sessions directly, from the terminal or the desktop app.

```sh
phux host add me@mini
phux attach mini
```

**Access to a phux server is access to everything it can do**, including
starting programs as the user it runs as. A server listens only on a local
socket until you set it up for remote use, and every remote connection must
present credentials from pairing.

## Adding a machine

Run this on your own machine:

```sh
phux host add me@mini --install
```

Over `ssh me@mini`, this:

1. Checks that `phux` is on the remote `PATH`. With `--install`, it installs
   phux there with the [install script](./install.md#install-script) when
   it's missing, or runs `phux update` there when it's older than yours,
   both on your release channel. Without `--install`, a missing phux is an
   error.
2. Installs a service there, so the server survives logouts and reboots.
3. Sets the remote server's `[policy] mode` to `"paired"` and opens a QUIC
   listener on UDP port 8788.
4. Issues a certificate and token for this machine.
5. Saves the whole thing under the name `mini` in your config.

There is nothing to copy by hand. Run `phux host add me@mini` again whenever
the host stops answering: it checks the setup and repairs what broke.

If a non-interactive SSH shell doesn't find `phux` on the remote machine, as
with some Homebrew or Nix installs, say where it is:

```sh
phux host add me@mini --remote-phux /opt/homebrew/bin/phux
```

The machine's entry remembers that path, and where `--install` put phux, so
later attaches, server restarts, and `phux host renew` run the same one.

| Option | Does |
| --- | --- |
| `--name NAME` | Saves the machine under another name. |
| `--install` | Installs phux on the machine, or updates an older one, before setting it up. |
| `--accept-new-host-key` | Trusts the SSH host key of a machine you have never connected to; a changed key is still refused. |
| `--quic-port PORT` | Uses another UDP port than 8788. |
| `--endpoint HOST:PORT` | Registers this address instead of the ones found on the host. |
| `--no-service` | Starts the server without a service, so it won't come back after a reboot. |
| `--ssh-only` | Registers an `ssh://` entry without contacting the host. |
| `--session NAME` | Attaches to this session on arrival. |

## Machines on your tailnet

With [Tailscale](https://tailscale.com), phux lists the machines on your
tailnet so you don't have to type their names:

```sh
phux host discover            # every Mac and Linux machine on the tailnet
phux host discover --login    # sign in to Tailscale, or reconnect it, first
```

It reads the Tailscale client on this machine and shows each machine's
`MagicDNS` name, whether it's online, and whether you already added it. Add
one by that name:

```sh
phux host add mini.tail1234.ts.net --name mini --install --accept-new-host-key
```

`--accept-new-host-key` is safe here because the tailnet already vouches for
the address. Machines that run Tailscale SSH need no SSH key.

`--login` opens the Tailscale app if it isn't running, reconnects a client
that's signed in but disconnected, or starts a sign-in and prints its link.
It doesn't wait for the browser: run `phux host discover` again afterwards.

The desktop app does all of this from **Add Machine…** (in the File menu,
the palette, and Settings > Hosts & connections): it lists your tailnet's
machines, signs you in to Tailscale in the browser, and shows each setup
step as it happens.

## Using a machine

```sh
phux attach mini                 # QUIC straight to the server; SSH is out of the path
phux ls --all                    # sessions here and on every registered machine
phux new --remote mini scratch   # create a session there
phux host ls                     # registered machines
```

`ls`, `new`, `kill`, `rename`, `detach`, `upload`, `download`, and `whoami`
take `--remote NAME`. Pane commands such as `snapshot`, `send-keys`, `run`,
and `wait` act on the server they connect to. To script panes on another
machine, run `phux` there over SSH, or [link the machines](#linking-machines)
so its panes show up here as `mini/@7`.

Several terminals and the desktop app can attach to the same remote session
at once.

## Who can connect

A server's `[policy] mode` decides who can connect:

| Mode | Lets in |
| --- | --- |
| unset or `local` | Only programs on this machine, through the local socket. Configuring any network listener or relay makes the server refuse to start. |
| `paired` | The local socket, plus network connections that present an enrolled client certificate and a token. |

`phux host add` sets `paired` for you.

A remote connection needs two things from pairing: a client certificate
issued by the server's own certificate authority, and a token. The client
pins that certificate authority when it pairs and refuses to connect if it
changes. A token alone never gets anyone in.

Certificates from `phux host add` last 90 days. Attaching warns, and
`phux doctor` reports, when one is within two weeks of expiring.
`phux host renew mini` replaces it.

### A paired phone can see panes but cannot close or organize them

Older pairing enrolled devices without the newer `organize` and `terminate`
permissions. Update phux on the server, find the phone's credential with
`phux workload list`, then explicitly repair that device on the server:

```sh
phux workload repair-paired sha256:DEVICE_CREDENTIAL_ID
```

Reconnect the phone afterwards. Its certificate, key and expiry stay the
same; no server restart or re-pair is needed. Repair accepts only an active
human device with exactly the old full-control pairing ceiling, not custom
scopes or automation. New pairings include all terminal permissions. Loading
a credential or marking it `human` never silently adds permissions.

## Who can do what

**A machine you add with `phux host add` can do everything you can do on the
remote server**, including typing into panes. Only add machines you control.

A CI runner, or a person who should list sessions but not type, gets a
separate certificate with a narrower scope from `phux workload add-key`. The
steps, expiry, rotation, and revocation are in
[Host-scoped read keys](./runbooks/host-observe-keys.md).

Programs on the server's own machine are different. **The local socket gives
full access to any program running as the same user.** phux can't tell those
programs apart.

## Attaching over SSH

`phux attach --ssh` needs no pairing and no service:

```sh
phux attach --ssh me@box
```

SSH logs you in (password and 2FA prompts work), starts the server on the
remote machine if needed, and opens a QUIC (UDP) listener for this attach
alone.
Then SSH exits and the session runs directly over QUIC, so it survives
network changes and draws locally. The listener and its token live only in
memory and go away shortly after you detach.

The remote server's policy must allow the listener, and UDP must reach it.
If it can't, phux says why and falls back to `ssh -t box phux attach`: the
terminal UI runs on the remote machine and SSH carries its screen. An SSH
login failure, or a remote machine without phux, is an error, not a
fallback.

`--udp-ports 60000-61000` keeps the listener in a range one firewall rule
can cover.

## Pairing with a link

When you can't SSH to the server, pair with a link instead. On the server
machine:

```sh
phux pair
```

This prints a pairing link and a QR code. On the client:

```sh
phux attach --remote mini --code 'https://phux.sh/connect?...'
```

**The link is a secret.** Anyone who has it can connect. The token in it
works until you revoke it, or until `--expires` if you set one. The
enrollment ticket that comes with a QR code works once and expires after ten
minutes. A printed or piped link needs `--enroll` to include one.

`phux pair --qr` shows only the QR code, for pairing a phone without showing
the token on screen. `phux pair --secret-file PATH` writes the link to a
private file instead of printing it.

## Relays

When neither machine can reach the other directly, run a relay somewhere both
can reach:

```sh
phux relay pair --route home     # on the relay machine: prints a token and a fingerprint
phux relay run --listen 0.0.0.0:4433
```

Give the server a `[[connector]]` entry with the relay's address, the token
file, and the relay's fingerprint, then mint the client's link on the server:

```sh
phux pair --relay-route home
```

The server dials out to the relay, so it needs no open port. **The relay sees
only ciphertext.** Every connection runs TLS end to end with the server,
inside the relayed stream. `--max-conns` caps how many connections a relay
accepts (64 by default).

## Linking machines

A **hub** is a server that also lists and routes to other servers, its
**satellites**. A satellite's panes show up on the hub with targets like
`edge/@7`.

```sh
phux host add me@edge --role satellite    # on the hub machine
phux service install --hub                # run this machine's server as a hub
```

A satellite that can't accept connections, such as a sandbox behind NAT,
dials out to the hub instead:

```sh
phux host ticket sandbox     # on the hub: prints a code good for ten minutes
phux host join -             # on the sandbox: paste the code
```

**Joining a hub gives the hub full control of the satellite.** The hub can do
anything on the satellite that you can, as the user the satellite's server
runs as. Join only a hub you own and trust. The hub must run in `paired` mode;
the satellite can stay `local`.

An expired code stops new joins but doesn't undo one. To cut off a joined
satellite, revoke its credential on the hub with `phux workload revoke`, and
remove it with `phux host rm sandbox`.

### Minimal boxes

[Minimal](https://minimal.dev) runs coding agents in isolated boxes on your
own machine. One command makes every box join this machine's phux:

```sh
phux host minimal setup
min session activate          # as usual; the box shows up in phux as min-NAME
phux ls                       # the box's sessions, under min-NAME
```

![phux host minimal setup, then a plain min session activate joins the box](assets/minimal-demo.gif)

`setup` writes a `phux` Minimal loadout that copies a Linux phux into each
box and starts its server there, adds it to Minimal's `default_loadouts`, and
installs a per-user service that runs `phux host minimal watch`. On macOS the
box's phux is this phux's own version for Linux, checked like `phux update`
checks one, and refreshed when that version changes; on Linux it is the phux
you ran. Run `setup` again after the hub's QUIC port changes.

The watcher notices each new box within a couple of seconds, mints a
single-use ticket for it, and hands the code to `phux host join -` inside the
box over `min session exec`. The box dials this machine at
`host.min.internal`, the name Minimal gives this machine's loopback inside a
box, so this server needs `[policy] mode = "paired"` and a QUIC listener
bound to loopback or all addresses. A box's panes then appear here as
`min-NAME/@N`, in the terminal and in the desktop app. When a box is
destroyed, its satellite entry goes and its credential is revoked.

**The box gets no authority here.** A satellite credential opens no terminal
on the hub; the hub drives the box, never the reverse, so an agent sealed in
a box still can't reach your other panes.

Agents in a box can still talk to yours. `phux mail` and `phux claim` in a
box use this machine's store, in the box's name, so an agent there writes
`phux mail send hub/@749 ...` and the agent in your pane `@749` gets it as
mail from `min-NAME/@N` ([Mail across a hub](./agents.md#mail-across-a-hub)).

A box activated with `--no-loadouts` has no phux and is skipped. A
`--network none` box, and an `--network own_ip` box whose egress rules don't
admit `host.min.internal`, can't reach the hub, so they never join. The
default `host_ip` box can.

| Command | Does |
| --- | --- |
| `phux host minimal setup` | Loadout, default, hub, and watcher service. `--no-service` skips the service; `--box-phux PATH` picks the Linux phux boxes get. |
| `phux host minimal sync` | One pass: join new boxes, forget destroyed ones. |
| `phux host minimal watch` | `sync` every two seconds, as the service does. |
| `phux host minimal remove` | Stops the watcher, removes the loadout, and forgets and revokes every box. |

`--hub HOST:PORT` overrides the address boxes dial, and `--min PATH` the
Minimal CLI, when `min` is neither on `PATH` nor in `~/.local/bin`.

## Managing credentials

```sh
phux pair ls                     # tokens this server has issued
phux pair revoke ID
phux pair prune --unused-for 30d
phux workload list               # enrolled client certificates
phux workload revoke sha256:...
phux host renew mini             # replace this machine's certificate
```

Revoking a credential refuses new connections and closes the live ones it
let in, without a restart. It doesn't stop any program running in a pane.
Revoke a credential with these commands, not by editing files.

Removing a machine from your list (`phux host rm`) only forgets it here. It
doesn't revoke anything on the server.

[File locations](./reference/files.md) lists every credential file on both
sides.

## Troubleshooting

`phux doctor` on the server machine checks the listener, the certificate,
and whether the advertised address is reachable.

**`could not install phux`.** The install script failed on the machine.
Release builds exist for Apple silicon macOS and glibc Linux on x86_64 and
arm64. Run the script there yourself to see all of its output.

**`Host key verification failed`.** SSH has never connected to that machine.
Connect once with `ssh` to check and save its key, or pass
`--accept-new-host-key` when something else, such as a tailnet, vouches for
the address.

**`no answer within 5s; is UDP filtered?`** `phux host add` couldn't reach
UDP 8788, so it registered an `ssh://` route instead. Attaching over it
tries direct QUIC and falls back to `ssh -t` running the terminal UI
there. Open the port, or use a relay.

**The token was revoked or the server's certificate authority changed.**
The error names the machine and says to run `phux host add NAME` again.

**`incompatible protocol`.** The two machines run releases too far apart. The
error names both versions. Stop the remote server, then start a fresh one:
`ssh HOST kill <pid>` when its pid is known, otherwise
`ssh HOST pkill -f 'phux server'`, then `ssh HOST phux server --ensure`. Stopping it
closes the panes it is running.

**Same protocol major, older remote server.** `ssh HOST phux update` updates
the remote binary, and `ssh HOST phux upgrade` moves its running server onto
the binary already installed there. Panes survive that hand-over.

More in [Troubleshooting](./troubleshooting.md).
