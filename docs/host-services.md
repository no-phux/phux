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
are relayed, never followed. One deadline of 25 s bounds the whole open:
finding and starting the service, minting a token, connecting, any retry,
and the response head. So an endpoint must answer its head promptly, and
long-running work must be used asynchronously: OpenCode's
`POST /api/session/{id}/prompt` admits the input and returns, and progress
arrives on the `GET /api/event` stream. Blocking endpoints such as
`/api/experimental/session/{id}/wait`, `/shell`, and `/generate` can outlast
the deadline and are then answered `UNAVAILABLE`.

`SERVICE_HTTP_READ { exchange, max_bytes }` (at most 1 MiB) is answered with
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
status` prints one `http://127.0.0.1:PORT` or `http://[::1]:PORT` line. A
line naming no origin (`stopped`) means stopped, so the server runs
`opencode service start` once and asks again; any other origin is refused
`UNAVAILABLE` ("non-loopback origin refused") and nothing is started. The
server mints its own session token (`opencode api server.pair`, then
`GET /auth/connect/{code}`) and sends `Authorization: Basic` with user
`opencode`; clients cannot reach `/api/pair` or `/auth/*` themselves
(`INVALID_COMMAND`). Origin and token are cached in memory behind one lock,
which also serializes discovery: concurrent opens wait for a single
`service start`. A refused connection rediscovers the origin. A `401` that
carries the service's Basic challenge mints a new token and resends the
retained request, at most once per 30 s; any other `401`, or one inside that
window, is relayed. Each exchange is one loopback HTTP/1.1 connection.

## Limits and lifecycle

A connection holds at most 16 exchanges, opening, open, or reading, with one
read at a time per exchange. Its memory is bounded too: the request bodies
of exchanges still opening (kept for a retry) share 16 MiB, and each read
buffers at most 1 MiB, so at most 16 MiB of reads are in flight. Exchanges
end at body end, on close, and with their connection. While a connection
holds any, it checks every 15 s for abandoned ones: an exchange never read
within a minute of its head (an open whose caller stopped waiting, or a
hub's relayed open that timed out there) and one not read for five minutes
are closed. The client runtime also closes an exchange itself when the open
that produced it was abandoned before its head arrived.
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
