# Compatibility

The wire protocol is `1.0`. Any 1.x client works with any 1.x server, older
or newer. The case to design for is a new client against a server that is
far older: servers run for weeks and are upgraded on their own schedule,
clients update whenever their app does. A new major is a deliberate,
announced break; 0.x cannot talk to 1.x. [Protocol](./protocol.md) has the
framing and handshake this page builds on.

## Versions

`HELLO` and `HELLO_OK` each carry `major.minor.patch`. The server, every
client, and the hub's satellite link compare majors only and refuse another
major with `VERSION_INCOMPATIBLE`. Minor and patch are informational:
`phux status` shows them to explain a peer's behavior, and nothing gates on
them. What a peer can do is its feature bits. (Handing an older server over
to a newer binary compares build versions, not the protocol.)

## A newer client, an older server

The older server answers what it does not know and keeps the connection:

| The client sends | The server answers |
| --- | --- |
| an unknown frame type | `ERROR` `UNSUPPORTED`, uncorrelated |
| an unknown command tag, or an unknown value in a command body | `COMMAND_RESULT` error `UNSUPPORTED` with the request id |
| an unknown value in a request frame (`SPAWN`, `MOVE`, metadata, `LIST_DIRECTORY`, `PATH_QUERY`) | `ERROR` `UNSUPPORTED` with the request id |
| an unknown value in any other frame (`INPUT_*`, `ATTACH`, resize, ...) | `ERROR` `UNSUPPORTED`, uncorrelated; nothing is applied |
| an unknown op or field in a JSON request (`CHECKOUT`, `NATIVE_AGENT`) | the same as an unknown value in a command body |
| unknown tagged fields, or bytes past a record's known tail | skipped |

An "unknown value" is a type byte, tag, enum value, flag bit, or presence
or boolean byte this build did not allocate. Presence and boolean bytes are
strict: `0` and `1` are known and anything else is unknown. Unknown
modifier and role-policy bits are refused, never masked off, so a newer
client's input or restriction is never applied half-understood. A spawn
whose fields break this build's per-kind rules is a correlated spawn
refusal, so a later rule relaxation does not close older servers. Bytes that cannot be read at all (a truncated
field, a bad length, invalid UTF-8 or JSON where it is required) are still
malformed and close the connection with `MALFORMED_MESSAGE`; so does a
frame sent before `HELLO`.

`HELLO` itself always decodes. An unknown color support reads as
truecolor and an unknown output mode as raw; a flag is set when its byte is
nonzero; a limit above this server's cap is clamped to the cap and a zero
limit takes the default. An unknown compression algorithm reads as none.

A client checks a feature bit before it sends the request the bit gates,
and degrades that one feature when the bit is missing. It does not refuse
the connection and does not rely on the `UNSUPPORTED` reply for ordinary
flow; the reply is what keeps a mistake from hanging a request or dropping
the session.

## A newer server, an older client

A newer server sends a new frame kind, event, enum value, or record shape
only to clients whose `HELLO` version or capabilities show they know it.
Clients are still tolerant, because that rule can be gotten wrong.
`phux_protocol::wire::decode::newer_server_fallback` is the shared rule,
and the runtime, the C ABI, the TUI, and the web client all apply it:

- unknown tagged fields and record tails are skipped;
- a reply this build cannot read (`COMMAND_RESULT`, `METADATA_VALUE`,
  `METADATA_KEYS`, `DIRECTORY_LISTING`, `PATH_RESULTS`) settles its request
  as an error with code `0`, `UNREADABLE_REPLY`; an unreadable
  `RESOURCE_SPAWNED` or `RESOURCE_MOVED` settles as an `Unknown` spawn or
  move result. Neither is `UNSUPPORTED`: the server
  answered, so the request may have taken effect. Acknowledged input, a
  spawn, or an agent prompt answered this way (or with an error code or
  spawn error the client does not know) settles as outcome unknown, never
  as refused, so nothing is retyped or re-prompted;
- an unreadable `ATTACHED` or `ATTACH_READY` ends the connection with a
  protocol error instead of leaving the attach waiting;
- any other frame with an unknown value is skipped with a warning;
- unknown spawn and move errors and tombstone, history, and rejection
  reasons decode as an `Unknown` variant (a reason as wire value `255`,
  which a client never encodes); a tombstoned stream reads as released;
- an unknown path status reads as truncated, and a path row of an unknown
  kind is dropped and marks the answer truncated;
- unknown close reasons, detach reasons, error codes, and agent events
  were already surfaced as unknown.

Only malformed bytes close the connection. A `HELLO_OK` or
`BOOTSTRAP_BEGIN` naming an output mode, profile, or engine codec the
client did not offer is malformed too, since the server must pick from the
offer. A server never compresses a frame that settles a request (replies,
`ERROR`, `ATTACHED`, `ATTACH_READY`), because a client could not find the
type byte of one it cannot read; only bulk frames such as
`BOOTSTRAP_CHUNK` and `HISTORY_PAGE` are compressed.

## Federation

A hub's link to a satellite is a 1.x client of that satellite. A frame the
hub cannot read for an unknown value is logged and skipped; when it
answered a request the hub relayed, the request settles as an unknown
outcome, never as refused or `UNSUPPORTED`, since the satellite may have
acted: a spawn or move result carries the reserved result tag `255`
(outcome not stated), anything else an `UNREADABLE_REPLY` error. The hub
never relays a reply it cannot read, since a satellite's ids inside it
would not be translated to the hub's. An
uncorrelated `ERROR` from a satellite is logged and does not end the link.
Malformed bytes still end the link. A hub relays a command to a satellite
only when that satellite advertises the feature, and refuses
`UNSUPPORTED_SATELLITE_ROUTE` otherwise.

## Making a change compatible

- Ship new behavior behind a feature bit in
  [`caps/server_features.rs`](../crates/phux-protocol/src/caps/server_features.rs).
  Do not bump the major for an additive change; a shipped bit is never
  withdrawn or repointed.
- A new request is a `COMMAND` sub-tag, so an older server answers it with
  a correlated `UNSUPPORTED` instead of an uncorrelated one.
- Grow a positional record only at its tail. A record that is not last in
  its field carries a `u32` length. An optional value in the middle of a
  record carries a presence byte (`0` absent, `1` present), as the keyed
  kill and signal commands, `ATTACH_RESOURCE`, and `NATIVE_AGENT_RESULT`
  detail do; never infer presence from how many bytes remain, since that
  leaves no room for a field after it.
- Ids and other leaves are frozen. A new shape is a new tag, a new enum
  value is a new number, and a retired field id is never reused. Value
  `255` is reserved in reason enums and spawn and move result tags: it is
  how a peer holds an unknown one. A fixed-size field such as `PATH_QUERY`'s grows through a new field
  id, not a longer value. The field `wire_type` byte is always `4`.
- Syntax an older peer validates is part of 1.0 as well: a path in
  `PATH_RESULTS` is an absolute POSIX path, and an arrangement names a
  session or a workspace. Changing either needs a feature bit.
- A count or size limit an older peer enforces (path results, directory
  entries, batch and arrangement caps, `APPLY_INPUT`) is part of 1.0.
  Raising one needs a feature bit; the newer peer sends more only when the
  other side advertises it.
- JSON requests are decoded strictly by the server, so an older server
  refuses an unknown op or field as `UNSUPPORTED` rather than ignoring it.
  JSON a client reads (answers, agent stream updates, stored recipes) skips
  unknown fields; native agent states and dispositions fold an unknown
  value to `unknown`.
- Say in the PR what an older client sees, and what an older server does
  with the new request. The probes live in the [server's protocol
  tests](../crates/phux-server/tests/protocol) (`newer_client*.rs`) and the
  [codec's wire contract](../crates/phux-protocol/tests/it/wire_contract)
  (`tail_extension.rs`, `wire_roundtrip/hello_growth.rs`).

## Persisted formats

A file on disk is an old writer and a newer reader, like the wire, and in a
rollback the other way round. Field and variant names are the format. A
renamed Rust field keeps its stored name with `#[serde(rename)]`. On a
`#[serde(default)]` field a rename fails nothing: the old file parses and the
value is gone. A round trip of today's type cannot see that, so these formats
are held to a file frozen from a release (`phux-frozen`):

| Format | Fixture |
| --- | --- |
| Upgrade blob | `phux-server`: `upgrade-blob` |
| `org/v1.sqlite3`: receipts | `phux-core`: `org-receipts`, every `Op` variant |
| `org/v1.sqlite3`: sessions, resources, arrangements | `upgrade-blob` carries the same types |
| `remote-tokens` | `phux-server`: `remote-tokens` |
| `workload-keys` | `phux-server`: `workload-keys` |
| `enrollment-tickets` | `phux-server`: `enrollment-tickets` |
| `plugin-startup/<id>/*.pid` | `phux-server`: `plugin-pidfile` |
| `plugin-runs.jsonl` | `phux-plugin`: `plugin-runs` |
| `work-v1.sqlite`: objective and mail `result_json` | `phux-work`: `objectives`, `mail` |
| `workspace.json` | `phux`: `workspace-archive` |
| `plugins.lock` | `phux`: `plugins-lock` |
| `trusted-projects.toml` | `phux`: `trusted-projects` |
| `organize/last.json` | `phux-client-core`: `organize-journal` |
| `onboarding.json`, `collapsed-projects.json`, `agents-filter.json` | `phux-tui`: one each |
| `phux.agent/v1` | `phux-resource`: `agent-record` |

Fixtures live in `crates/<crate>/tests/fixtures/frozen/<format>/` (the blob's
in `tests/fixtures/upgrade-blob/`) and are append-only:
`scripts/check-frozen-fixtures.sh` refuses an edit, so a sweeping rename cannot
rewrite the proof along with the code. A check fails when a released fixture
loses a value, when the newest no longer matches what this build writes, and
when the newest leaves a field empty. To freeze a change, run the test with
`PHUX_BLESS_FROZEN=<format>`, which writes the next fixture, and fill in what
that leaves empty. A field skipped when empty is invisible to the last check,
so give a new one a value by hand.

The rest have a reason not to:

| Format | Why no fixture |
| --- | --- |
| `work-v1.sqlite`: start `result_json` | Every field is required, so a rename fails the retry that reads it rather than losing a value. Its objective is frozen as `objectives`; `StartSnapshot` is private to a file too large to touch. |
| `ideas/spool/*` | A short-lived send queue. A lost `permanent_error` only retries a message that will fail again. |
| Agent stream records | The blob carries stamped lines as opaque text, replayed to consumers and not parsed on resume. Their shape is the public record contract. |
| `phux.tui.layout/v1` values | Opaque to the server; the blob carries the bytes. The client's CBOR envelope has only required fields and a version it checks, so a rename is refused, not lost. |
| `known-authorities`, `relay-tokens`, `server-starts.log` | Line formats parsed by hand, with no field names to rename. |
| TLS, CA and ACME files, `*.token`, `.phux-channel` | PEM or plain text in a standard shape. |
| Service unit, plist, wrapper | Generated, and rewritten in place on reconcile. |
| Update `manifest.json` | Written and read by key, with no type to rename. A missing `binaries` fails the rollback loudly. |
| `.cast` | Has its own golden tests (`golden_v3_round_trips_through_both_writers_unchanged`). |
| `config.toml` and layers | Written by people, and `deny_unknown_fields` makes a rename fail loudly. |
| `update-check.json`, `usage/state.json`, logs, reports | Caches and diagnostics: losing them costs a recompute. |
