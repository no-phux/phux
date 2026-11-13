# Host operations on the protocol

File transfer and Git checkout methods share the [protocol](../protocol.md)
command envelope and authenticated dispatch, but have distinct filesystem reach.
[Background push](#background-push) is the one request the server makes itself.
[Host checklist](#host-checklist) says what works on the host.

## File transfer

`PUT_FILE` uses a live Terminal to select the destination host, not to grant
arbitrary filesystem writes. The server chooses the sandbox root and name;
offsets and overlapping bytes continue the same principal's immutable
upload. Only whole-file digest verification, sync and rename publish a final
path. Digest mismatch discards the partial file and sidecar: restart rather
than trying to repair retained corrupt bytes. Completed artifacts belong to
the user and are not silently TTL-deleted; quotas refuse with a remedy.
Keep payloads and completed paths out of routine telemetry. Transfer files
through this lane, not fragmented PTY input. The [upload implementation and
tests](../../crates/phux-server/src/runtime/apps/terminal/upload.rs) own continuation and
quota behavior; [CLI](../reference/cli.md#phux-upload) and
[environment](../reference/environment.md) own syntax and limits.

`GET_FILE` is the pull, behind `features_ext.FILE_DOWNLOAD`. It addresses a
machine, not a pane: an optional `host` names a satellite of the serving hub,
and no `host` reads the server itself, the way `PATH_QUERY` browses. The
client names an absolute path and the server reads any regular file its own
user can read, in chunks of at most 8 MiB. That is deliberately no narrower
than the server grants already: a consumer holding `INPUT` on this server can
type `cat` into any of its shells, so the command is classified `INPUT` on
the server. A hub relays a named host only when that satellite advertises
`FILE_DOWNLOAD`; otherwise it refuses `UNSUPPORTED_SATELLITE_ROUTE` and
forwards nothing, so an older satellite never sees an unknown tag. Each
chunk is one stateless request that returns the file's current size, the
bytes, and, on the chunk that reaches the end, a whole-file SHA-256; a
client resumes from its own offset and must not expose a file whose digest
differs. There is no size cap and no sandbox on the reading side; the
destination disk is the client's. The disk read runs in the bulk command
lane beside `PUT_FILE`, off the connection dispatcher. The
[download implementation](../../crates/phux-server/src/runtime/download.rs) and
the [client helper](../../crates/phux-client/src/download.rs) own the read and
the exclusive, verify-then-publish write;
[`phux download`](../reference/cli.md#phux-download) owns syntax.

## Checkout methods

`CHECKOUT` (command tag `0x3c`, behind `features_ext.CHECKOUT` = `0x400`)
reads and stages the Git checkout a Terminal is in: status with per-file
line counts, one file's diff, a page of history, branches, a directory
listing, the start of a file, the branch's pull request and checks (through
the host's `gh`), and stage or unstage. It names a Terminal and one typed
request; the server that owns the Terminal resolves the checkout from the
Terminal's live working directory, so a client never names a host path.
Paths in requests and answers are relative to the checkout's top level; an
absolute path, `..`, anything under `.git`, or a path crossing a symbolic
link is refused. The command is classified `INPUT` on the named Terminal
(not on the server, as `GET_FILE` is), since an `INPUT` holder can already
run `git diff` or `git add` there. The reach is the whole checkout: a
Terminal whose working directory is inside a large repository, such as
dotfiles tracked at `$HOME`, lets its `INPUT` holders read every file in that
repository outside `.git`. Index writes that overrun their deadline are
stopped with `SIGTERM` before `SIGKILL`, so Git can remove `index.lock`.

The request and its answer cross the wire as bounded JSON inside the command
envelope (requests at most 64 KiB, answers at most 4 MiB, text at most
1 MiB and cut flagged `truncated`). Answers skip unknown fields and fold
unknown enum values to `unknown`, so a newer server can add detail. A
refusal is a typed answer (`not_a_repository`, `invalid_path`, `timeout`,
`gh_unavailable`, `no_pull_request`, ...), not a command error; a missing
Terminal or a denied grant stays a command error. Git and `gh` run off the
dispatcher under a deadline and an output cap, with
`GIT_OPTIONAL_LOCKS=0`, `GIT_TERMINAL_PROMPT=0` and `GIT_LITERAL_PATHSPECS=1`.
A hub relays a satellite Terminal's checkout only to a satellite that
advertises `CHECKOUT`; otherwise it refuses `UNSUPPORTED_SATELLITE_ROUTE`.
The [request and answer types](../../crates/phux-protocol/src/checkout.rs), the
[server handler](../../crates/phux-server/src/runtime/apps/terminal/checkout.rs), and the
shared [`phux-git`](../../crates/phux-git/src/checkout.rs) module own the
details.

## Background push

A phone that is not connected can still be told that an agent needs it. The
phone registers a **push grant**: `SET_METADATA` at `Global` scope under
`phux.push/v1/<device>`, a UTF-8 JSON object.

```json
{"gateway":"https://push.example/v1/push","grant":"<secret>","host":"mini","finished":true}
```

| Field | Value |
| --- | --- |
| `gateway` | Where the server posts. `https://`, or `http://` on loopback only; at most 512 bytes. |
| `grant` | The bearer secret the gateway minted for the device: 16 to 256 URL-safe ASCII bytes. |
| `host` | The id the device knows this server by, 1 to 128 bytes, echoed so a tap routes. |
| `finished` | Optional, off when absent. `true` also asks for the finished notice below. |

Unknown fields are ignored, so a server that predates `finished` stores the
grant and sends asks only. A value that does not parse is not stored. A
server keeps at most 32 grants and remembers which connection wrote each.
Deleting the key withdraws the grant.

The server posts a notice only to grants whose writing connection is gone; a
connected device sees the same thing live. The request is one `POST` to
`gateway` with `Authorization: Bearer <grant>` and a JSON body:

```json
{"schema_version":1,"host":"mini","pane":"local:7","question":"q1","action":"reply"}
{"schema_version":1,"host":"mini","pane":"local:7","question":"blocked","action":"open"}
{"schema_version":1,"host":"mini","pane":"local:7","action":"finished"}
```

`pane` is `local:<id>`, or `satellite:<host>:<id>` on a hub. Nothing else
leaves the host: no agent name, session name, question text, or screen
content. The gateway chooses the alert's words.

- **An agent asked** (`"action":"reply"`): sent to every grant, once per
  question id, when an `asked` event is raised for the pane.
- **An agent is blocked** (`"action":"open"`, `"question":"blocked"`): sent
  to every grant when the pane's agent record turns `blocked` from any other
  state while no ask is pending on the pane. This is how an agent known only
  by its screen (a rule matched its permission prompt) reaches a phone: it
  never raises `asked`. Staying `blocked` sends nothing; leaving and
  re-entering it sends again. `blocked` is not a real question id, so an
  answer pinned to it is refused and the phone opens the pane instead.
- **An agent finished** (`"action":"finished"`, no `question`): sent only to
  grants with `"finished": true`, when the pane's
  [agent record](../program-sequences.md#the-agent-record) goes from
  `working` straight to `done`. `working` to `blocked` is the block's; a
  record that is cleared, a pane that closes, a second `done`, and `done`,
  `idle`, `done` with no `working` between send nothing, so one run sends
  once. `working` to `idle` sends nothing either: an agent phux knows only
  by its screen reads as `idle` at its prompt and never as `done`.

A gateway MUST refuse a notice whose `action` it does not know rather than
alert it as an ask. Sends are not retried. The
[push module](../../crates/phux-server/src/push/mod.rs) owns the limits and
the exchange.

## Host checklist

`features_ext.HOST_CHECKLIST` (`0x0080_0000`) gates `HOST_CHECKLIST` (command
tag `0x43`, no body), answered `OkWith(Json)` with what works on the host:
the `checklist` document of its `phux doctor --json`
([What works here](../troubleshooting.md#what-works-here)). It needs
`INVENTORY` on the server, like `GET_STATE`; a failure to build the list is
`UNAVAILABLE` with the reason. An older server never advertises the bit, and
a client must not send the tag without it.
