# Port forwarding

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

Only `-L` exists. There is no remote (`-R`) or dynamic (`-D`) forward, no
UDP, and a forward is not remembered across runs. A forward reaches the
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
