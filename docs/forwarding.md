# Port and ssh-agent forwarding

`phux forward` does two things `ssh` does, through the phux connection you
already have: `-L` carries a TCP port, below, and `--agent` lends your
[ssh-agent](#ssh-agent) to the panes on a server.

`phux forward -L` reaches a TCP port on the machine a phux server runs on
through the phux connection you already have, the way `ssh -L` does, with no
SSH session to keep open and no port opened on that machine.

```sh
phux forward -L 3000:localhost:3000 --remote mini
```

That listens on `127.0.0.1:3000` here and carries every connection to
`localhost:3000` on `mini`. It runs in the foreground until Ctrl-C and prints
one line when it is listening and one for each connection the server
refuses to dial.

The argument is `[BIND_ADDR:]LOCAL_PORT:REMOTE_HOST:REMOTE_PORT`. The local
address defaults to `127.0.0.1`; name another (`0.0.0.0`, or `[::1]` in
brackets) to listen elsewhere. Without `--remote` the forward goes to the
local server. `--remote` dials a registered machine directly, as
[`phux attach`](./remote.md#using-a-machine) does: Unix socket, QUIC,
WebSocket, a relay, or the protocol over `ssh`. It exits non-zero when the
server is older than this feature or has it turned off.

If the phux connection drops, the connections open through it are closed,
the local listener stays, and new connections are served once phux
reconnects. A transfer cut off this way ends early rather than with an
error, so check a length or a checksum where it matters.

For ports only `-L` exists. There is no remote (`-R`) or dynamic (`-D`)
forward, no UDP, and a forward is not remembered across runs. A forward reaches the
server you connect to; it is not relayed on to a
[linked machine](./remote.md#linking-machines) behind it.

## What the server will dial

By default only its own loopback: `REMOTE_HOST` must be `localhost`,
`127.0.0.1`, or `::1`. `localhost` means those two addresses, tried in that
order; the server never looks up a name a client sent. Anything else is
refused:

```text
example.com:80 is not on this server's loopback: phux forwards only to
localhost, 127.0.0.1, or ::1 unless the server's `[forward] allow` lists
the target
```

The server's [configuration](./reference/config.md) widens or removes that:

```toml
[forward]
tcp = true                      # false: not advertised, every command refused
allow = ["db.internal:5432", "10.0.0.5:80", "[fd00::1]:22"]
agent = false                   # true: broker ssh-agent requests, see below
```

An `allow` entry is one `host:port` or `ip:port`, matched exactly against
what the client asked for: no wildcards, no port ranges, no other spelling
of the same host. A listed name is resolved by the server, since its own
configuration vouches for it.

## Security

Forwarding needs `input` on the whole server (`input@global`), the same
rule as `phux download`; a key scoped to a host, a session, or a pane is
refused ([who can do what](./remote.md#who-can-do-what)). It cannot be held
for approval.

Someone who holds full input rights can already run commands on that
machine, and so can already open any connection it can. The loopback-only
default therefore limits mistakes, not a determined holder: it keeps a
typo or a careless script from using the server as a way into the network
around it. The allow-list widens what the *server* dials, as the user the
server runs as, from where the server sits; list a target only if every
holder of input rights on that server may reach it.

The [audit log](./audit.md) records `forward.open` (the target and the
connection) and `forward.close` (bytes each way and why it ended) once per
forwarded connection, never per chunk and never the bytes.

## Protocol

Four commands extend the [protocol](./protocol.md#commands-events-resources)
behind `features_ext.FORWARD_TCP` (`0x0200_0000`), all classified `INPUT` on
the server. They ride the control connection like any command; there is no
separate stream.

| Command | Tag | Body | Answer |
| --- | --- | --- | --- |
| `FORWARD_OPEN` | `0x44` | `addr` (string, at most 253 bytes), `port` (u16) | `FORWARD_OPENED` (`CommandValue` `0x0f`): the forward's id (u64) |
| `FORWARD_READ` | `0x45` | `id` (u64), `max_bytes` (u32, 1 to 262144) | `FORWARD_CHUNK` (`0x10`): bytes, then `eof` |
| `FORWARD_WRITE` | `0x46` | `id` (u64), bytes (at most 262144), `fin` | `Ok`, after the server wrote them all |
| `FORWARD_CLOSE` | `0x47` | `id` (u64) | `Ok` |

`FORWARD_OPEN` is answered once the server has connected, within 10 s.

`FORWARD_READ` is a pull, so the client's `max_bytes` is the window: the
answer carries what the target has sent, else the first bytes within 20 s,
else nothing with `eof` false, and the client reads again. `eof` says the
target closed its side; writes still work. One read per forward is in
flight at a time.

`FORWARD_WRITE` is acknowledged only after the server's write completes, so
the acknowledgement is the backpressure. One write per forward is in flight
at a time, which is also what keeps bytes in order. `fin` shuts down the
write side after the bytes, so the target reads the end while its answer
can still come back; a write after `fin` is `INVALID_COMMAND`.

`FORWARD_CLOSE` ends the forward and anything in flight on it. It is
idempotent.

Errors are typed: `PERMISSION_DENIED` (a target outside loopback and the
allow-list), `UNAVAILABLE` (the dial failed or timed out, or the target
reset the connection, which also ends the forward), `NOT_FOUND` (an unknown
or closed forward), `RESOURCE_EXHAUSTED` (too many forwards, or a second
read or write in flight), `INVALID_COMMAND` (port 0, a zero-byte read, a
write after `fin`), and `UNSUPPORTED` (`tcp = false`). An older server
answers the unknown tags `Unsupported`; clients check the bit before
sending.

## Limits and lifecycle

A connection holds at most 32 forwards, dialing or open. Each ends on
close, with its connection, or when its client goes quiet: a forward no
read or write has touched within a minute of its dial, or none has finished
on for five minutes, is closed. A client that still wants a quiet
connection keeps a read waiting on it, as `phux forward` does, so an idle
database or SSH connection stays up. Forward ids are never reused within a
server process, so a call from before a reconnect is answered `NOT_FOUND`,
never with another connection's bytes.

Every command runs on its own task, so a read waiting on a quiet connection
or a write the target is slow to take delays neither the dispatcher nor
other commands. Each chunk costs one command round trip, so throughput is
about 256 KiB per round trip in each direction: hundreds of megabytes a
second on a local socket, less over a slow link.

The [wire types](../crates/phux-protocol/src/forward.rs) and the
[server module](../crates/phux-server/src/runtime/forward/mod.rs) own the
details.

## ssh-agent

```sh
phux forward --agent --remote mini
```

While that runs, `ssh` and `git` in a pane on `mini` can use the keys in
the ssh-agent on this machine, as with `ssh -A`. It runs in the foreground
until Ctrl-C and needs `SSH_AUTH_SOCK` set here. Each signature a pane asks
for is shown first:

```text
phux: let mini sign with SHA256:Xr0... (me@laptop)? [y/N]
```

Anything but `y` refuses it, and the pane's `ssh` sees an agent that
declined. Listing your public keys is not asked about. With stdin not a
terminal there is nobody to ask, so every signature is refused. `--yes`
signs without asking; see the risk below before using it.

Both sides must opt in. The server does nothing until its
[configuration](./reference/config.md) says `[forward] agent = true`
(restart it to apply), and a client serves its agent only while someone
runs `phux forward --agent` against that server.

### What it does to panes

With `agent = true` the server listens on one Unix socket, owner-only
(`0600`), beside its own socket (`phux.sock.ssh-agent` in the runtime
directory), and starts every pane with `SSH_AUTH_SOCK` naming it. That has
consequences worth knowing before turning it on:

- `SSH_AUTH_SOCK` is fixed when a pane starts. A pane started before the
  server ran with `agent = true` keeps what it had and never sees a
  forwarded agent; start a new pane.
- A pane started with it on gets the broker socket whether or not a client
  is serving, **in place of** any agent the server's own environment
  named. With nobody serving, `ssh-add -l` there fails at once. A pane
  that wants the host's own agent sets `SSH_AUTH_SOCK` back itself.
- The path does not change across a server restart or upgrade, so a pane
  that outlives one keeps working once a client serves again. `phux
  forward --agent` offers again by itself after it reconnects.

With `agent = false`, the default, there is no socket, panes inherit
`SSH_AUTH_SOCK` as they always did, the bit is not advertised, and the
commands are refused.

### What can and cannot cross

The server speaks the ssh-agent protocol on that socket but holds no key.
It passes on exactly two requests, listing keys
(`SSH_AGENTC_REQUEST_IDENTITIES`, 11) and signing
(`SSH_AGENTC_SIGN_REQUEST`, 13), and answers every other one with
`SSH_AGENT_FAILURE` itself: `ssh-add KEY`, `ssh-add -d`, `ssh-add -D`,
locking, and extensions never reach a client. The client checks the same
two types again before touching its agent, refuses a signature with a key
its agent does not hold, and never trusts the server to have filtered. A
message over 256 KiB either way is refused.

A request with no client serving fails at once. One a client does not
answer within 60 s fails then, which is also how long you have at the
prompt; a yes after that signs nothing. If the serving client disconnects,
what was waiting on it fails at once.

One client serves a server at a time: the newest offer wins. A client that
was serving is not told it was replaced, still answers what it had already
been sent, and is not fallen back to when the newer one leaves; run it
again.

### Security

Offering needs `input` on the whole server (`input@global`), as `-L` does.

Once a client serves, **any process in any pane on that server, run by
anyone who can reach that server's user account, can ask your agent to
sign** with any key it holds. The socket is owner-only, but that owner is
the user the server runs as, and root there. That is the risk of `ssh -A`,
unchanged, and the reason for the default: the per-signature question is
the mitigation, and it is only as good as your reading of it. The question
names the server and the key, not what is being signed or which pane
asked, so say yes only when you expect a request from there at that
moment. `--yes` removes the question and leaves you with exactly `ssh -A`:
use it only against a machine where you would run that.

Keys never leave your machine; a holder of the socket gets signatures only
while the command runs. The [audit log](./audit.md) on the server records
`forward.agent.offer` and, for each signature asked for,
`forward.agent.sign` with the key's fingerprint and whether it was signed;
never the data, and not listings.

There is no GPG agent forwarding, no choosing which keys a server may use,
and nothing is remembered across runs. Requests reach the server you
connect to, not a [linked machine](./remote.md#linking-machines) behind it.

### Protocol

Behind `features_ext.FORWARD_AGENT` (`0x0400_0000`), advertised only with
`agent = true`. Both commands are `INPUT` on the server and are answered on
the dispatcher.

| Message | Tag | Body | Answer |
| --- | --- | --- | --- |
| `AGENT_OFFER` | command `0x4f` | none | `Ok`; this connection serves until it ends |
| `agent_request` | event `0x21` | `id` (u64), the ssh-agent request (at most 262144 bytes) | the client sends `AGENT_REPLY` |
| `AGENT_REPLY` | command `0x50` | `id` (u64), the ssh-agent response (at most 262144 bytes) | `Ok`, or `NOT_FOUND` once the server stopped waiting |

`agent_request` is sent only to the serving connection, unjournaled, with
no resource and no stamp, like `document_input`. Messages are ssh-agent
bodies without the four-byte length. The server hands a pane the reply
only if it is the answer type the request calls for
(`SSH_AGENT_IDENTITIES_ANSWER` or `SSH_AGENT_SIGN_RESPONSE`); anything
else, a refusal included, reaches the pane as `SSH_AGENT_FAILURE`. Request
ids are never reused within a server process. At most 64 requests wait at
once and 64 connections to the socket are served at once. `UNSUPPORTED`
answers either command with `agent = false`; an older server answers the
unknown tags `Unsupported`, and an older client ignores the unknown event,
which it is never sent. The
[broker](../crates/phux-server/src/runtime/forward/agent.rs) owns the
details.
