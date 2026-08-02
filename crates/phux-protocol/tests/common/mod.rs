//! Shared byte-building helpers for the wire test files.
//!
//! One authoritative copy of the TLV primitives from
//! `docs/spec/appendix-encoding.md` (message bodies are field-tagged:
//! `field_id: varint || wire_type: u8 (4 = BYTES) || varint length || value`)
//! plus the hand-rolled ATTACHED/SessionSnapshot/LayoutNode encoder used to
//! build malformed and edge-case frames the real encoder refuses to produce.

// Each integration-test binary compiles this module independently and none of
// them uses every helper; `pub` is idiomatic for a tests/common module even
// though each binary is its own crate root.
#![allow(dead_code, unreachable_pub, clippy::unwrap_used)]

/// Append an unsigned LEB128 varint.
pub fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

/// Append one TLV field: `field_id || wire_type(4 = BYTES) || len || value`.
pub fn tlv_field(out: &mut Vec<u8>, field_id: u32, value: &[u8]) {
    put_varint(out, u64::from(field_id));
    out.push(4); // wire_type BYTES
    put_varint(out, value.len() as u64);
    out.extend_from_slice(value);
}

/// Wrap a `type_byte` + field-tagged `body` in the outer length frame
/// (`u32 length || type || body`), where `length` covers the type byte + body.
pub fn framed_tlv(type_byte: u8, fields: &[u8]) -> Vec<u8> {
    let mut body = vec![type_byte];
    body.extend_from_slice(fields);
    let mut frame = Vec::new();
    frame.extend_from_slice(&u32::try_from(body.len()).unwrap().to_be_bytes());
    frame.extend_from_slice(&body);
    frame
}

/// Hand-roll an ATTACHED frame whose single `WindowInfo` carries `layout` —
/// the positional `LayoutNode` bytes, without the leading `Some` presence
/// byte (this helper writes it). Under field-tagged TLV the message body is
/// two fields — SNAPSHOT (id 1) and `INITIAL_CLIENT_ID` (id 2) — but the
/// `SessionSnapshot` value is still positional, so the inner bytes mirror
/// `info::encode_session_snapshot` exactly. Keep in sync when the snapshot
/// wire shape changes.
pub fn attached_with_layout(layout: &[u8]) -> Vec<u8> {
    let mut win = Vec::new();
    win.extend_from_slice(&1u32.to_be_bytes()); // window id
    win.extend_from_slice(&1u32.to_be_bytes()); // session id
    win.extend_from_slice(&0u16.to_be_bytes()); // index
    win.extend_from_slice(&0u32.to_be_bytes()); // name len 0
    win.push(0); // active_pane None
    win.push(1); // layout Some
    win.extend_from_slice(layout);

    // Positional SessionSnapshot (the value of the ATTACHED SNAPSHOT field
    // under field-tagged TLV — the snapshot itself stays positional).
    let mut snap = Vec::new();
    snap.extend_from_slice(&0u32.to_be_bytes()); // sessions 0
    snap.extend_from_slice(&1u32.to_be_bytes()); // windows 1
    snap.extend_from_slice(&win);
    snap.extend_from_slice(&0u32.to_be_bytes()); // panes 0
    snap.extend_from_slice(&1u32.to_be_bytes()); // focused_session
    snap.extend_from_slice(&1u32.to_be_bytes()); // focused_window
    snap.push(0); // focused_pane tag local
    snap.extend_from_slice(&1u32.to_be_bytes());

    // Field-tagged ATTACHED body: SNAPSHOT (id 1) + INITIAL_CLIENT_ID (id 2).
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &snap);
    tlv_field(&mut fields, 2, &7u32.to_be_bytes());
    framed_tlv(0x81, &fields)
}
