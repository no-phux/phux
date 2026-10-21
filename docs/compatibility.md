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
major with `VERSION_INCOMPATIBLE`. Minor and patch are informational: they
explain a peer's behavior in `phux status` and decide whether `phux` offers
an upgrade, and nothing gates on them. What a peer can do is its feature
bits.

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

An "unknown value" is a type byte, tag, enum value, or presence byte this
build did not allocate. Bytes that cannot be read at all (a truncated
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
Clients are still tolerant, because that rule can be gotten wrong:

- unknown tagged fields and record tails are skipped;
- unknown events, kinds, close reasons, and error codes are surfaced as
  unknown rather than treated as a protocol failure;
- an unknown `COMMAND_RESULT` tag decodes as error `UNSUPPORTED`;
- unknown values inside a result or event (a spawn or move error, a
  tombstone reason, a path kind, ...) read as an `Unknown` variant that
  carries the raw value.

A client closes only on bytes it cannot read.

## Federation

A hub's link to a satellite is a 1.x client of that satellite. A frame the
hub cannot read for an unknown value is logged and skipped; when it
answered a request the hub relayed, the request settles as `UNSUPPORTED`.
Malformed bytes still end the link. A hub relays a command to a satellite
only when that satellite advertises the feature, and refuses
`UNSUPPORTED_SATELLITE_ROUTE` otherwise.

## Making a change compatible

- Ship new behavior behind a feature bit in
  [`caps/features.rs`](../crates/phux-protocol/src/caps/features.rs).
  Do not bump the major for an additive change; a shipped bit is never
  withdrawn or repointed.
- A new request is a `COMMAND` sub-tag, so an older server answers it with
  a correlated `UNSUPPORTED` instead of an uncorrelated one.
- Grow a positional record only at its tail. A record that is not last in
  its field carries a `u32` length. An optional value in the middle of a
  record carries a presence byte (`0` absent, `1` present); never infer
  presence from how many bytes remain, since that leaves no room for a
  field after it.
- Ids and other leaves are frozen. A new shape is a new tag, a new enum
  value is a new number, and a retired field id is never reused.
- JSON payloads keep `deny_unknown_fields` on requests, so an older server
  refuses an unknown op or field as `UNSUPPORTED` rather than ignoring it;
  answers skip unknown fields and fold unknown enum values to `unknown`.
- Say in the PR what an older client sees, and what an older server does
  with the new request. The [server's protocol tests](../crates/phux-server/tests/protocol)
  (`newer_client*.rs`, `tail_extension.rs`) are where to add the probe.
