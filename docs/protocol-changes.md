# Wire-change checklist

For the existing formats and dispatch, see [Server protocol](./protocol.md).

- Allocate type bytes in `wire/frame/catalog.rs`, sub-tags in
  `*tags.rs` beside it; duplicates fail to compile.
- Append field ids to `tlv_body!` in `wire/field/`.
- Gate new behavior with a row in
  `crates/phux-protocol/src/caps/server_features/ext.rs`. The original feature
  word is frozen. The server's HELLO support table and listener policy decide
  which known bits each connection sees. Additive changes keep the protocol
  version. Keep each shipped bit assigned to its original capability.
- Grow a positional record only at its tail; never reorder, retype, or
  remove a field. A non-final composite record needs a length prefix and a
  mid-record optional a presence byte ([compatibility](./compatibility.md#making-a-change-compatible)).
- Classify any new command in `kinds.rs`, or dispatch denies it.
- Regenerate the schema (`PHUX_UPDATE_WIRE_SCHEMA=1 cargo nextest run -p
  phux-protocol wire_schema_matches`), fill in any `?` types, and
  `just docs-gen` for the kinds page.
- Add roundtrip and snapshot tests next to the existing ones in
  `crates/phux-protocol/tests/it/`.
- Say in the PR what an older client sees, and what an older server does
  with the new request.
