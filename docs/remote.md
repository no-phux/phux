# Remote machines

phux has no accounts and no hosted service. Each machine runs its own
server; you tell one machine how to reach another, once, over the SSH
access you already have.

## Set up a host

Install phux on the remote machine, then from your machine:

```sh
phux host add me@mini
```

Over `ssh me@mini` this checks that `phux` is on the remote `PATH`,
installs a `phux service` there so the server survives logouts and
reboots, switches it to `[policy] mode = "paired"`, opens a QUIC listener on
UDP port 8788, mints a credential for this machine, and records the whole
thing under the name `mini` in your config. Nothing to copy by hand.

From then on:

```sh
phux attach mini          # QUIC straight to the server; ssh is out of the path
phux ls --all             # sessions here and on every registered host
phux new --remote mini scratch
phux host ls
```

If `phux` on the remote is somewhere SSH's non-interactive shell does not
see, say where:

```sh
phux host add me@mini --remote-phux /opt/homebrew/bin/phux
```

Run `phux host add me@mini` again whenever the host stops answering; it
repairs whatever broke.

## What is reachable

Session-level commands take `--remote NAME`: `ls`, `new`, `kill`, `rename`,
`detach`, `upload`, `whoami`. Pane-level commands (`snapshot`, `send-keys`,
`run`, `wait`) act on the server they connect to. To script panes on
another machine, run `phux` there over SSH, or link the machines (below) so
its panes appear here as `mini/@7`.

Several terminals and the desktop app can attach to the same remote
session at once.

## Linking machines

A **hub** is a server that also lists and routes to other servers, its
**satellites**. Panes on a satellite show up in `phux ls --all` and take
targets like `edge/@7`.

```sh
phux host add me@edge --role satellite    # on the hub machine
phux service install --hub                # make this server a hub
```

For a satellite that cannot accept connections (a sandbox behind NAT), it
dials out to the hub instead:

```sh
phux host ticket sandbox     # on the hub: a code good for ten minutes
phux host join -             # on the sandbox: paste the code
```

## Other ways in

**Just SSH.** `phux attach --ssh me@box` runs phux over an ordinary SSH
session. Nothing to configure on either side. Keystrokes and screen
updates travel through SSH, so it is slower than QUIC, and the connection
ends when SSH does.

**A link instead of SSH access.** On the server machine, `phux pair` prints
a one-time link and a QR code. On a client, `phux attach --remote mini
--code 'https://phux.sh/connect?...'` registers the host from it. This is
how the mobile apps will pair.

**A relay.** When neither side can reach the other directly, run
`phux relay run` somewhere both can reach, give the server a `[[connector]]`
entry for it, and mint the client's link with `phux pair --relay-route
NAME`. The relay forwards ciphertext; every connection is TLS end to end
with the server, inside the relayed stream.

## What is exposed, and to whom

Until you set `[policy] mode = "paired"`, a server accepts connections only
over its Unix socket; configuring any listener makes it refuse to start.
`phux host add` sets the mode and binds the QUIC listener for you.

A remote connection must present two things: a client certificate issued by
the server's own certificate authority, and a bearer token. Both come from
pairing. The server pins nothing it learns from a connection; the client
pins the server's authority at pairing time and refuses to connect if it
changes. A remote connection gets a scoped credential, not the owner's
authority; the local socket keeps that.

```sh
phux pair ls                 # tokens the server has issued
phux pair revoke ID
phux pair prune --unused-for 30d
phux workload list           # client certificates
phux workload revoke sha256:...
phux host renew mini         # replace this machine's certificate before it expires
```

Certificates minted by `host add` last 90 days. Attaching warns, and
`phux doctor` reports, when one is within two weeks of expiry.

Files on both sides are listed in [files](./reference/files.md). On the
client they are the `[[remote]]` entries in `config.toml`, a token file and
a certificate per host, and `known-authorities` next to the config.

## When it does not connect

`phux doctor` on the server machine checks the listener, the certificate,
and whether the advertised address is reachable. Common causes:

- **UDP 8788 is filtered.** `host add` reports "is UDP filtered?" and
  registers an `ssh://` route instead, which works but goes through SSH.
  Open the port, or use a relay.
- **The token was revoked or the authority rotated.** The error names the
  host and says to run `phux host add NAME` again.
- **Protocol mismatch.** The error prints the versions both sides speak;
  update one of them. `ssh HOST phux update` updates the remote binary, and
  `ssh HOST phux upgrade` restarts a server that is older than the binary
  already installed there.
