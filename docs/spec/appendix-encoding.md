---
audience: consumers, contributors, agents
stability: stable
last-reviewed: 2026-06-07
---

# Appendix A — Encoding primitives

**TL;DR.** The normative encoding for every phux message body: **field-tagged
TLV**. Each top-level field is `field_id: varint || wire_type: u8 ||
length-delimited value`. A decoder matches fields by stable id and skips any
field id it does not recognise by that field's declared length; optional and
trailing fields are simply absent. Leaf primitives are big-endian inside a
field's value; a leaf `str` / `bytes` is length-prefixed with a **u32
big-endian** count (distinct from the **varint** length the TLV envelope uses
for a `BYTES`-typed field). Nested tagged unions / sub-records stay positional
within a field.

---

## 1. Encoding shape (normative)

Every message body SHALL be encoded as a sequence of **field-tagged TLV
fields**. Each top-level field is written as:

```text
field_id: varint  ||  wire_type: u8  ||  value
```

where `value` for the length-delimited wire types is `varint length || bytes`.
A decoder reads fields by their `field_id` rather than by position, and a
field id it does not recognise it SHALL skip by reading the field's declared
length and advancing past that many bytes. This is the extensibility
mechanism: id-matched, order-independent fields with skip-by-length over
unknown ids — not positional append-only fields.

**Field-id allocation.** Field ids are scoped **per message** (the way the
type byte already scopes the body): each message's fields are numbered from
`1`, contiguously, in field-declaration order. Field ids are **stable within a
major protocol version** — an additive minor-version change MAY append a new
id after the existing ones but MUST NOT renumber or reuse an existing id; a
removed field's id is retired, not recycled.

**Optional and trailing fields** are simply-absent tagged fields. An encoder
writes no field for a logically-absent value (`None`, or an empty trailing
field), and a decoder SHALL apply the documented default when an id is absent,
so a peer encoding an older or newer body round-trips by id rather than by
position.

Security-critical signed subprotocols MAY close this extensibility rule by
requiring one canonical field image and a new profile identifier for evolution.
`phux-workload/v1` does so: its challenge and response reject unknown,
duplicate, out-of-order, missing, non-minimal, or trailing data before proof
verification ([workload-auth.md §7](./workload-auth.md)). That exception applies
only to the authentication frames; ordinary message bodies keep skip-by-length.

The wire types, carried in the `wire_type` byte after each `field_id`, are:

| wire_type | Name       | Encoding                                          |
|-----------|------------|---------------------------------------------------|
| 0         | `VARINT`   | LEB128 unsigned integer                           |
| 1         | `SVARINT`  | LEB128 zig-zag signed integer                     |
| 2         | `FIXED32`  | 4 bytes, big-endian                               |
| 3         | `FIXED64`  | 8 bytes, big-endian                               |
| 4         | `BYTES`    | `varint length || bytes`                          |
| 5         | `MESSAGE`  | `varint length || nested encoded fields`          |
| 6         | `LIST`     | `varint length || elements with type prefix`      |
| 7         | `TAGGED`   | `varint tag || nested encoded fields`             |

Every wire type phux emits at the message-body level is **length-delimited**:
the reference implementation writes every top-level field as `BYTES` (`4`),
the field's value being the positional encoding of that logical field captured
as an opaque length-delimited blob. Because the value is length-delimited, a
decoder skips an unknown field id without needing to know the field's logical
type. The fixed-width and varint scalar wire types are reserved for future
nested use.

Primitive widths and the `varint` / `bytes` / `str` / `bool` /
`optional<T>` conventions are defined in [proto.md §Conventions](./proto.md).
Inside a field's value, multi-byte integers are big-endian on the wire, chosen
for hex-dump readability and network feel; fixed widths exist where natural
(for example timestamps and color channels) so the wire matches the
conceptual width.

A canonical hex dump of a `HELLO_OK` selecting the full tier set with an
opaque `server_id` is committed at
`crates/phux-protocol/tests/snapshots/frame_wire_snapshots__snap_hello_ok.snap`
and pinned by the `snap_hello_ok` snapshot test; any wire-format change
surfaces there as a reviewable diff.

---

## 2. Nested encoding (positional within a field)

Only the **message body** is field-tagged. A field's value MAY itself be a
nested tagged union or sub-record (`TerminalId`, `ViewportInfo`,
`AttachTarget`, `Scope`, `Command` / `CommandResult` / `CommandValue`,
`SpawnResult`, `AgentEvent`, `SessionSnapshot`, `LayoutNode`, ...); these are
encoded **positionally** inside the field's length-delimited value, with their
own one-byte discriminant tags where they are tagged unions. A decoder reads
such a value with a positional decoder bounded by the field's length, so a
malformed nested value cannot read past its field, and an over-declared inner
list length errors on end-of-input rather than over-reserving (a decode-path
denial-of-service guard).

Forward-compatibility *inside* a nested value still uses the older
append-only-trailing-field convention bounded by the field length (for
example `Command::GetScreen`'s trailing `cells` flag): a value that ends
before a trailing nested field decodes that field at its documented default.
The field-tagged skip-by-length rule of §1 applies at the message-body level.

### 2.1 Leaf `str` / `bytes` length prefix (u32, not varint)

A `str` or `bytes` value appearing as a leaf inside a positional value is
length-prefixed with a **`u32` big-endian** byte count, then the raw bytes —
the encoding the reference codec writes (`Encoder::write_bytes` /
`write_str`) and reads (`Decoder::read_bytes` / `read_str`). An empty value
encodes as a four-byte zero header. A declared length that exceeds the
remaining input or the protocol frame cap is rejected (`LengthOverflow`).

This is deliberately **not** the varint length the TLV envelope uses. The
varint applies only to the envelope machinery of §1 — the `field_id`, the
`wire_type` byte, and the length of a `BYTES`-typed message-body field. Leaf
primitives carried inside that field's positional value use the fixed-width
`u32` prefix above. Keep the two distinct when hand-encoding: a leaf string's
length is four big-endian bytes, never a LEB128 varint.
