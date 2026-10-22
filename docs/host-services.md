# Host services

A client can speak HTTP to a service on a phux server's host through the
phux connection it already holds, without reaching that service's port:
this is how the phone talks to the host's OpenCode. phux authorization is
the only gate. The server names its services; the one service is
`opencode`, the host's OpenCode background service. The three commands
below extend the [protocol](./protocol.md#commands-events-resources) and
are classified `INPUT` on the server, like `GET_FILE`: a consumer that can
type into a shell here can already run `opencode`. Removing a device's grant
removes its OpenCode access; no OpenCode credential ever reaches the client.

## Commands

`SERVICE_HTTP_OPEN`, `SERVICE_HTTP_READ`, and `SERVICE_HTTP_CLOSE` are
command tags `0x3d`-`0x3f`, behind `features_ext.HOST_SERVICE_HTTP`
(`0x1000`).

`SERVICE_HTTP_OPEN` carries an optional satellite `host`, the service, the
method, an origin-form target (path and query starting with `/`), headers,
and the whole request body (at most 8 MiB; at most 128 headers and 64 KiB of
head). It is answered with `SERVICE_HTTP_HEAD` (`CommandValue` `0x0c`: the
exchange id, status, and headers) once the response head arrives; redirects
are relayed, never followed.

`SERVICE_HTTP_READ { exchange, max_bytes }` is answered with
`SERVICE_HTTP_CHUNK` (`0x0d`: bytes and `end`): what is ready now, else the
first bytes within an idle deadline of 20 s, else nothing with `end` false,
so the client simply reads again. That pull is how an event stream flows,
with natural flow control. The chunk with `end` also ends the exchange.

`SERVICE_HTTP_CLOSE` releases an exchange early and ends a read in flight.
It is idempotent, so closing an ended exchange succeeds.

Errors are typed: `NOT_FOUND` (`214`, an unknown service or exchange),
`UNAVAILABLE` (`215`, the service is not installed or did not start, and the
message says which), `INVALID_COMMAND` (a bad method, target, or header),
and `RESOURCE_EXHAUSTED` (too many exchanges, or a read already in flight).
An older server answers the unknown tags `Unsupported`; clients check the
bit before sending.

## Headers and the OpenCode service

The server drops hop-by-hop headers (and any the `Connection` header names)
both ways, replaces the client's `Host`, `Authorization`, `Cookie`,
`Proxy-*`, `Content-Length`, and `Expect` with its own, and never relays
`Set-Cookie`. Body bytes pass verbatim, `Content-Encoding` included.

For `opencode` the server runs the resolved OpenCode executable (`PATH`,
then `~/.opencode/bin/opencode`; never a login shell). `opencode service
status` prints one `http://127.0.0.1:PORT` or `http://[::1]:PORT` line;
anything else means stopped, so the server runs `opencode service start`
once and asks again. It mints its own session token (`opencode api
server.pair`, then `GET /auth/connect/{code}`) and sends `Authorization:
Basic` with user `opencode`. Origin and token are cached in memory; a
refused connection rediscovers the origin, and a `401` mints a new token once
and resends the retained request. Each exchange is one loopback HTTP/1.1
connection.

## Limits and lifecycle

A connection holds at most 16 exchanges, opening, open, or reading, with one
read at a time per exchange. An exchange nobody read for five minutes is
closed at the next open, and every exchange closes with its connection.
Exchange ids are never reused within a server process, so a read from before
a reconnect is answered `NOT_FOUND`, never with another exchange's bytes.
Opens and reads run on their own tasks, so a read held open on an event
stream delays neither the dispatcher nor the bulk lane.

A hub relays a named `host` only to a satellite that advertises
`HOST_SERVICE_HTTP` (otherwise `UNSUPPORTED_SATELLITE_ROUTE`, nothing
forwarded) and maps the satellite's exchange to one of its own. The hub's
link is one connection on the satellite, so the hub's consumers share its
16 exchanges there.

The [wire types](../crates/phux-protocol/src/service_http.rs) and the
[server module](../crates/phux-server/src/runtime/service_http/mod.rs) own
the details.
