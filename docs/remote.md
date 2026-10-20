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
`detach`, `upload`, `download`, `whoami`. Pane-level commands (`snapshot`, `send-keys`,
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
dials out to the hub instead. **Joining trusts the hub with full satellite
owner authority**, not an ordinary scoped observer grant: each reverse
stream is spliced into the satellite owner socket. The hub must be paired;
the satellite may remain in local mode. Join only a hub you trust to run
commands as the satellite's serving user. The
[substrate decision](./substrate.md) keeps that join between one person's
own machines.

```sh
phux host ticket sandbox     # on the hub: a code good for ten minutes
phux host join -             # on the sandbox: paste the code
```

## Other ways in

**SSH bootstrap and fallback.** `phux attach --ssh me@box` uses SSH to
start/bootstrap the remote server and request a per-attach listener, token
and certificate pin. After a short QUIC probe, the session normally runs
directly over pinned QUIC; SSH has exited and is not its lifetime data
channel. The transient listener/token live in memory, linger briefly for
reconnect, and are not preserved through upgrade. Policy must permit the
listener and UDP must be reachable. If bootstrap or QUIC is unavailable,
the CLI says why and falls back to `ssh -t HOST phux attach`: a remote TUI
with SSH-carried updates, not local rendering or roaming. SSH authentication
failure or missing remote phux is an error, not a useful fallback.
Saved `ssh://` routes can attempt the same bootstrap; they do not promise
SSH-carried data. See the [bootstrap implementation](../crates/phux/src/commands/ssh_bootstrap.rs).
Manual `phux stdio-bridge` is a separate framed transport, not this fallback.

**A link instead of SSH access.** On the server machine, `phux pair` prints
a secret pairing link and QR code. The bearer is displayed once but remains
reusable until revoked/expired; the enrollment ticket is single-use and
expires after ten minutes. A displayed QR includes enrollment; scripted or
piped pairing must request `--enroll` when needed. On a client,
`phux attach --remote mini --code 'https://phux.sh/connect?...'` registers
the host from it. This is how the mobile apps will pair.

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
changes. Normal remote consumer ingress gets a scoped credential, not
the owner's authority. Reverse hub links have the stronger trust described
above. A bearer token admits a connection; it never substitutes for mTLS
scope.

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

## Connection authority

The ordinary owner Unix socket is full same-UID authority, not a process
sandbox. Delegated agent sockets are different: they stamp a narrowed
principal/session grant even though they are local. Any process of that UID
can still reach the owner socket; delegation is attribution/least privilege,
not containment. See [delegated execution](./agents.md#delegated-execution-identity).
A reverse satellite link deliberately gives its trusted hub owner authority
on that satellite ([uplink seam](../crates/phux-server/src/uplink.rs)).

For scoped dispatch, preserve requested selectors and credential ceiling
provenance. Their conjunction is evaluated against the authoritative
routing topology, not flattened into an admission-time pane list. A group
move can remove authority immediately and a later move back can restore
scope matching. Check before existence-disclosing lookup, forwarding,
mutation or queue insertion; the kind catalog's discovery is not a grant.
[Policy enforcement](../crates/phux-server/src/policy/enforce.rs) and the
[group-move tests](../crates/phux-server/src/policy/tests.rs) own this boundary.

Revocation/expiry, removal of the credential, or a ceiling that no longer
contains its live grant withdraws **existing** connection authority as well
as future admission. Fence dispatch first, release leases/subscriptions and
relay state, cancel queued input, then close. This does not kill workloads.
The [revocation watcher](../crates/phux-server/src/runtime/revocation.rs)
distinguishes confirmed registry/token verdicts from transient read failures
and gives broken registry replacement a grace interval; do not replace that
with stale-good admission or a blanket all-errors-revoke rule. New remote
admission still uses strict [workload material validation](../crates/phux-server/src/runtime/workload_auth.rs).

Ticket expiry prevents a new join; it does not retract an issued identity.
Revoke the satellite workload credential on the hub to end its authenticated
uplink; removing the registered host controls federation membership/routing.
These are distinct from revoking a consumer pairing bearer or deleting a
client-side remote entry. Use the relevant generated
[host](./reference/cli.md#phux-host) and [workload](./reference/cli.md#phux-workload)
commands, not credential-file edits as an improvised revocation procedure.

## When it does not connect

`phux doctor` on the server machine checks the listener, the certificate,
and whether the advertised address is reachable. Common causes:

- **UDP 8788 is filtered.** `host add` reports "is UDP filtered?" and
  registers an `ssh://` route instead. It can bootstrap direct QUIC or
  fall back to a remote TUI over SSH as described above. Open the port,
  or use a relay.
- **The token was revoked or the authority rotated.** The error names the
  host and says to run `phux host add NAME` again.
- **Protocol mismatch.** The error prints the versions both sides speak;
  update one of them. `ssh HOST phux update` updates the remote binary, and
  `ssh HOST phux upgrade` restarts a server that is older than the binary
  already installed there.
