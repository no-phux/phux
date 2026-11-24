---
group: build
order: 4
---

# Compatibility

The rules that let any 1.x client work with any 1.x server and keep files on
disk readable across upgrades and rollbacks. Design for a new client against
a much older server. Peers compare protocol majors only and refuse another
with `VERSION_INCOMPATIBLE`; what a peer can do is its feature bits.

## A newer client, an older server

The older server answers what it does not know and keeps the connection:

| The client sends | The server answers |
| --- | --- |
| an unknown frame type | `ERROR` `UNSUPPORTED`, uncorrelated |
| an unknown command tag, or an unknown value in a command body or JSON request | `COMMAND_RESULT` error `UNSUPPORTED` |
| an unknown value in a request frame (`SPAWN`, `MOVE`, metadata, `LIST_DIRECTORY`, `PATH_QUERY`) | `ERROR` `UNSUPPORTED` with the request id |
| an unknown value in any other frame (`INPUT_*`, `ATTACH`, resize, ...) | `ERROR` `UNSUPPORTED`, uncorrelated; nothing applied |
| unknown tagged fields, or bytes past a record's known tail | skipped |

- An unknown value is a type byte, tag, enum value, flag bit, or presence or
  boolean byte this build did not allocate. Presence and boolean bytes are
  strict: only `0` and `1` are known.
- Unknown modifier and role-policy bits are refused, never masked.
- Unreadable bytes (truncated field, bad length, invalid UTF-8 or JSON where
  required) and any frame before `HELLO` close the connection with
  `MALFORMED_MESSAGE`.
- `HELLO` always decodes: unknown color support reads as truecolor, an
  unknown output mode as raw, an unknown compression as none; a flag is set
  when its byte is nonzero; an over-cap limit is clamped and a zero limit
  takes the default.

A client checks the feature bit first; `UNSUPPORTED` only keeps a mistake
from hanging a request.

## A newer server, an older client

A server sends a new frame, event, enum value, or record shape only to a
client whose `HELLO` shows it knows it. It streams a resource only to a client
whose `HELLO` `RESOURCE_KINDS` (`u64`, bit `1 << tag` per kind) lists that
kind:

- A session `ATTACH` keeps an unlisted resource in the snapshot but neither
  subscribes nor bootstraps it. `ATTACH_RESOURCE` of one is refused
  `CODEC_UNAVAILABLE`; the connection stays.
- A client that spawns or publishes one stays its producer, but nothing of
  it is streamed. A Terminal spawn is refused `CODEC_UNAVAILABLE`.
- A kind tag of 64 or more is never streamed.
- A `HELLO` without the field reads as tags 0 to 4 (`Terminal` through
  `Document`).

A server compresses only bulk frames (`BOOTSTRAP_CHUNK`, `HISTORY_PAGE`),
never one that settles a request, so an older client can always find the
type byte of a reply it cannot read. A spawn that breaks a per-kind rule is
a correlated refusal, never a closed connection.

Clients stay tolerant anyway, through
`phux_protocol::wire::decode::newer_server_fallback`:

- Unknown tagged fields and record tails are skipped.
- An unreadable reply (`COMMAND_RESULT`, `METADATA_VALUE`, `METADATA_KEYS`,
  `DIRECTORY_LISTING`, `PATH_RESULTS`) settles its request as error `0`,
  `UNREADABLE_REPLY`. An unreadable `RESOURCE_SPAWNED` or `RESOURCE_MOVED`
  settles as an `Unknown` result. Either is outcome unknown, never refused:
  the request may have taken effect, so input and prompts are not resent.
- An unreadable `ATTACHED` or `ATTACH_READY` ends the connection with a
  protocol error.
- Any other frame with an unknown value is skipped with a warning.
- Unknown spawn and move errors, and tombstone, history, and rejection
  reasons, decode as `Unknown` (a reason as wire value `255`, never encoded by
  a client). A tombstoned stream reads as released.
- Unknown organization states, outcomes, lifetimes, and exit policies, a
  `NATIVE_AGENT_RESULT` disposition, close and detach reasons, error codes,
  and agent events read as `Unknown`; the rest of the frame decodes.
- An unknown path status reads as truncated; a path row of unknown kind is
  dropped and marks the answer truncated.

Only malformed bytes close the connection. A `HELLO_OK` or `BOOTSTRAP_BEGIN`
naming an output mode, Terminal profile, or engine codec the client did not
offer is malformed. An unknown `BOOTSTRAP_BEGIN` codec decodes as `Unknown`:
on a resource of unknown kind or a document kind, the client refuses that one
resource and carries on; on a Terminal or AgentSession it is malformed.

## Federation

A hub's satellite link is a 1.x client. A frame it cannot read is logged and
skipped, never relayed; a relayed request it answered settles as unknown
outcome (spawn and move result tag `255`, else `UNREADABLE_REPLY`). An
uncorrelated satellite `ERROR` keeps the link; malformed bytes end it. A hub
relays a command only to a satellite advertising its feature, else
`UNSUPPORTED_SATELLITE_ROUTE`.

## Legacy TUI layouts

Older clients read and write `phux.tui.layout/v1/<session-id>`. The server
decodes the legacy v3 envelope and commits it through the typed arrangement
authority, never storing the bytes, so a bad or refused write changes
nothing. A `GET` encodes the current arrangement, and every arrangement
change is broadcast on the key. Current writers use the typed arrangement
when both peers support it. The bridge is part of the 1.x promise.

## Making a change compatible

- A new resource kind is a new kind tag. A client lists in `RESOURCE_KINDS`
  exactly the kinds it decodes.
- A new codec for an existing kind is not covered by `RESOURCE_KINDS`. Send
  it only to a client that offers it at `HELLO`, or behind a feature bit.
- Ship new behavior behind a feature bit in
  [`server_features/ext.rs`](../crates/phux-protocol/src/caps/server_features/ext.rs);
  the original word is frozen. A shipped bit is never withdrawn or
  repointed.
- A new request is a `COMMAND` sub-tag, so the refusal is correlated.
- Grow a positional record only at its tail. A non-final record carries a
  `u32` length. A mid-record optional carries a presence byte (`0` absent,
  `1` present); never infer presence from remaining bytes.
- Ids and leaves are frozen. A new shape is a new tag, a new enum value a new
  number, and a retired field id is never reused. Value `255` is reserved in
  reason enums and spawn and move result tags for "unknown". A fixed-size
  field grows through a new field id. `wire_type` is always `4`.
- Syntax and limits an older peer validates are part of 1.0: absolute POSIX
  paths in `PATH_RESULTS`, arrangements naming a session or workspace, and
  count caps (path results, directory entries, batches, arrangements,
  `APPLY_INPUT`). Changing one needs a feature bit, and the newer peer sends
  more only when the other side advertises it.
- Servers decode JSON requests strictly, so an older one refuses an unknown
  op or field. Clients skip unknown JSON fields and fold unknown enum values
  to `unknown`.
- Say in the PR what an older client sees and what an older server does.
  Probes: the server's `newer_client*.rs` in
  [`tests/protocol`](../crates/phux-server/tests/protocol) and the codec's
  [wire contract](../crates/phux-protocol/tests/it/wire_contract)
  (`newer_server.rs`, `tail_extension.rs`).

## Persisted formats

Files on disk are read by newer builds, and by older ones after a rollback.
Field and variant names are the format: keep a renamed field's stored name
with `#[serde(rename)]`, since on a `#[serde(default)]` field a rename loses
the value silently. Formats with named fields are held to append-only
fixtures frozen from releases (`phux-frozen`, in
`crates/<crate>/tests/fixtures/frozen/<format>/`;
`scripts/check-frozen-fixtures.sh` refuses edits). A check fails when a
released fixture loses a value or the newest differs from what this build
writes or leaves a field empty. To freeze a change, run the test with
`PHUX_BLESS_FROZEN=<format>` and give every new field a value by hand.

## Deprecating a CLI spelling

A renamed verb or flag keeps its old spelling for at least one full release
as a hidden alias that runs the replacement. The alias prints one stderr
warning naming the replacement (none under `--json`) and is absent from
`--help` and shell completions. `no_unregistered_hidden_cli_surface` in
`crates/phux/src/tests/aliases.rs` fails on any other hidden verb or flag.
The next deprecation restores the deprecation table, its reference page, and its audit
from `git show d4d856229^` (`crates/phux/src/deprecations.rs`,
`crates/phux/src/refdocs/deprecations.rs`,
`crates/phux/tests/configuration/deprecated_aliases.rs`).
