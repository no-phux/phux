---
group: build
order: 5
---

# Host operations

The [protocol](./protocol.md) commands that reach the server's host: files,
Git checkout, host services, push, and the host checklist. Tags, verbs, and
feature bits are in the [kinds reference](./reference/kinds.md); check the bit
before sending. A hub relays a request naming a satellite `host` only to a
satellite that advertises the bit, and otherwise refuses
`UNSUPPORTED_SATELLITE_ROUTE`.

## File transfer

`PUT_FILE` uses a live Terminal to pick the destination host; the server picks
the sandbox root and name. Offsets continue the same principal's immutable
upload. Only whole-file digest verification, sync, and rename publish it; a
mismatch discards the partial. Completed files are never TTL-deleted.

`GET_FILE` reads any regular file the server's user can read, by absolute
path, from the server or a named satellite `host`, in chunks of at most
8 MiB. It is `INPUT` on the server. Each chunk is stateless and returns the file's current size; the last
also returns a whole-file SHA-256. Resume from your own offset; never expose
a file whose digest differs.

## Checkout methods

`CHECKOUT` reads and stages the Git checkout a Terminal is in, including its
pull request and checks via the host's `gh`. The server resolves the checkout from the Terminal's working
directory; clients never name a host path.

- Paths are relative to the checkout top. Absolute paths, `..`, `.git`, and
  symlink crossings are refused.
- It is `INPUT` on the Terminal and reaches the whole repository outside
  `.git` (all of `$HOME` for dotfiles tracked there).
- Bodies are JSON: requests at most 64 KiB, answers 4 MiB, text 1 MiB with
  cut text flagged `truncated`. Skip unknown fields; fold unknown enum values
  to `unknown`.
- A refusal is a typed answer; a missing Terminal or denied grant is a
  command error.

An optional trailing byte names the answer contract (none is 1). Under
`CHECKOUT_V2` the server answers contract 2: `show` with a 4 to 64 hex digit
`sha` returns the commit, its changed files, and its patch (send it only with
the bit); a stage that landed but whose status re-read failed answers
`applied` (read status again); a host without `git` refuses
`git_unavailable`. Contract-1 clients never see those two.

[`phux git`](./reference/cli.md#phux-git) and the MCP tool `phux_git` wrap
it. The [types](../crates/phux-protocol/src/checkout.rs) own the shapes.

## Host services

`SERVICE_HTTP_*` speaks HTTP to a service on the server's host over the
existing connection. The one service is `opencode`. The commands are `INPUT`
on the server; revoking a device's grant revokes its access, and no OpenCode
credential reaches the client. The server accepts only a loopback origin from
`opencode service status`; any other is refused `UNAVAILABLE` and nothing
starts.

- **`SERVICE_HTTP_OPEN`**: optional `host`, service, method, origin-form
  target, headers, whole body (at most 8 MiB, 128 headers, 64 KiB of head).
  Answers `SERVICE_HTTP_HEAD`. Redirects are relayed. One 25 s deadline
  covers the open through the response head, so call long work
  asynchronously (`POST /api/session/{id}/prompt`, then `GET /api/event`).
- **`SERVICE_HTTP_READ`** (at most 1 MiB): bytes ready now, else the first
  within 20 s, else none with `end` false; read again. `end` ends the
  exchange.
- **`SERVICE_HTTP_CLOSE`**: idempotent; ends a read in flight.

Errors: `NOT_FOUND` (unknown service or exchange), `UNAVAILABLE` (not
installed or not started), `INVALID_COMMAND` (bad method, target, or header;
`/api/pair` and `/auth/*` are refused), `RESOURCE_EXHAUSTED` (too many
exchanges or a concurrent read). The server drops hop-by-hop headers,
replaces `Host`, `Authorization`, `Cookie`, `Proxy-*`, `Content-Length`, and
`Expect`, and never relays `Set-Cookie`.

A connection holds at most 16 exchanges, one read each at a time; a hub's
consumers share its 16 on each satellite. An exchange unread a minute after
its head, or idle five minutes, is closed. Ids are never reused within a
server process.

## Background push

A phone registers a push grant with `SET_METADATA` at `Global` scope under
`phux.push/v1/<device>`:

```json
{"gateway":"https://push.example/v1/push","grant":"<secret>","host":"mini","finished":true}
```

`gateway` is `https://` (or `http://` on loopback), at most 512 bytes.
`grant` is the gateway's bearer, 16 to 256 URL-safe ASCII bytes. `host` is
the server's id on the device, 1 to 128 bytes. `finished` is optional.
At most 32 grants are kept; deleting the key withdraws one.

The server notifies only grants whose writing connection is gone, with one
unretried `POST` to `gateway` and `Authorization: Bearer <grant>`:

```json
{"schema_version":1,"host":"mini","pane":"local:7","question":"q1","action":"reply"}
{"schema_version":1,"host":"mini","pane":"local:7","question":"blocked","action":"open"}
{"schema_version":1,"host":"mini","pane":"local:7","action":"finished"}
{"schema_version":1,"host":"mini","pane":"server","question":"<approval id>","action":"approve"}
```

`pane` is `local:<id>` or `satellite:<host>:<id>`. Nothing else leaves the
host.

- `reply`: once per question id, on an `asked` event.
- `open`: when the agent record enters `blocked` with no ask pending.
  `blocked` is not a question id; an answer to it is refused.
- `finished`: to `finished` grants only, when the agent record goes from
  `working` straight to `done`.
- `approve`: when an action is held for [approval](./security.md). The device
  reads `phux.approval/v1/<id>` and writes `approve` or `deny` to
  `phux.approval.decide/v1/<id>`. `pane` is the action's one Terminal, or
  `server`.

A gateway MUST refuse an unknown `action` rather than alert it as an ask.

## Host checklist

`HOST_CHECKLIST` answers the `checklist` document of the host's
`phux doctor --json`, or `UNAVAILABLE` with the reason. It needs `INVENTORY`
on the server.
