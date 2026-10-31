# Host operations on the protocol

File transfer and Git checkout methods share the [protocol](../protocol.md)
command envelope and authenticated dispatch, but have distinct filesystem reach.

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
tests](../../crates/phux-server/src/runtime/upload.rs) own continuation and
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
[server handler](../../crates/phux-server/src/runtime/checkout.rs), and the
shared [`phux-git`](../../crates/phux-git/src/checkout.rs) module own the
details.
