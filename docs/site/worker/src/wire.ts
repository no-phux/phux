// Minimal reader for the phux wire frames the demo's session multiplexer has to
// understand.
//
// The SessionDO used to be a pure pipe: one browser socket in, one WASM session
// out, and every returned frame went back up that same socket. Pairing a second
// view means one session now fans out to several sockets, and two frame families
// are addressed per attachment:
//
//   ATTACH        (0x02, client → server) carries the client's correlation id
//   ATTACHED      (0x81, server → client) echoes it with the session snapshot
//   ATTACH_READY  (0x83, server → client) closes that attachment's bootstrap
//
// Those go only to the socket that asked; everything else (resource output,
// bootstrap streams, bells) is session state and is broadcast, because every
// attached view is looking at the same terminals.
//
// Frame layout (`crates/phux-protocol/src/wire/framing.rs`): a big-endian u32 body
// length, the type byte, then fields of `varint id, u8 wire type, varint length,
// value`. Wire type 4 is a byte string, which is how the protocol spells a u32.
// Only the fields priced above are decoded; the rest of each frame stays opaque.

/** Client-chosen attach correlation id (`field::attach::ATTACH_ID`). */
const ATTACH_ATTACH_ID = 5;
/** `field::attached::ATTACH_ID`. */
const ATTACHED_ATTACH_ID = 3;
/** `field::attach_ready::ATTACH_ID`. */
const ATTACH_READY_ATTACH_ID = 1;

export const TYPE_ATTACH = 0x02;
export const TYPE_ATTACHED = 0x81;
export const TYPE_ATTACH_READY = 0x83;
export const TYPE_ERROR = 0xc1;
export const TYPE_RESIZE_TERMINAL = 0x23;

/** Byte-string wire type in a frame field. */
const WIRE_BYTES = 4;

/** One decoded frame field: `id`, and the raw value bytes. */
export interface FrameField {
  id: number;
  value: Uint8Array;
}

/**
 * Frame type, or undefined when the buffer is not one whole well-framed frame.
 * Mirrors `scripts/phux-wire.ts`, which decodes the same header for the smokes.
 */
export function frameType(frame: Uint8Array): number | undefined {
  if (frame.length < 5) return undefined;
  const bodyLength = new DataView(
    frame.buffer,
    frame.byteOffset,
    frame.byteLength,
  ).getUint32(0);
  if (bodyLength !== frame.length - 4) return undefined;
  return frame[4];
}

function readVarint(
  frame: Uint8Array,
  offset: number,
): [number, number] | undefined {
  let value = 0;
  for (let shift = 0; shift <= 28 && offset < frame.length; shift += 7) {
    const byte = frame[offset++]!;
    value |= (byte & 0x7f) << shift;
    if ((byte & 0x80) === 0) return [value, offset];
  }
  return undefined;
}

/** Decode one frame's fields; invalid framing yields no fields. */
export function frameFields(frame: Uint8Array): FrameField[] {
  if (frameType(frame) === undefined) return [];
  const fields: FrameField[] = [];
  let offset = 5;
  while (offset < frame.length) {
    const id = readVarint(frame, offset);
    if (!id) return [];
    offset = id[1];
    const wire = frame[offset++];
    if (wire !== WIRE_BYTES) return [];
    const length = readVarint(frame, offset);
    if (!length) return [];
    offset = length[1];
    if (offset + length[0] > frame.length) return [];
    fields.push({ id: id[0], value: frame.slice(offset, offset + length[0]) });
    offset += length[0];
  }
  return fields;
}

function fieldBytes(frame: Uint8Array, id: number): Uint8Array | undefined {
  for (const field of frameFields(frame)) {
    if (field.id === id) return field.value;
  }
  return undefined;
}

function u32Of(value: Uint8Array | undefined): number | undefined {
  if (!value || value.byteLength !== 4) return undefined;
  return (
    ((value[0]! << 24) | (value[1]! << 16) | (value[2]! << 8) | value[3]!) >>> 0
  );
}

/**
 * The attach correlation id carried by a frame, when that frame is one of the
 * three that have one. Returns undefined for every other frame, which the caller
 * treats as session-wide traffic.
 */
export function attachIdOf(frame: Uint8Array): number | undefined {
  switch (frameType(frame)) {
    case TYPE_ATTACH:
      return u32Of(fieldBytes(frame, ATTACH_ATTACH_ID));
    case TYPE_ATTACHED:
      return u32Of(fieldBytes(frame, ATTACHED_ATTACH_ID));
    case TYPE_ATTACH_READY:
      return u32Of(fieldBytes(frame, ATTACH_READY_ATTACH_ID));
    default:
      return undefined;
  }
}
