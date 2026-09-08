const TYPE_TERMINAL_OUTPUT = 0x90;
const TYPE_TERMINAL_SNAPSHOT = 0x91;

// Exact phux 0.2 HELLO and CreateIfMissing ATTACH frames used by phux-web.
export const HELLO_FRAME = Uint8Array.from([
  0, 0, 0, 41, 1, 1, 4, 13, 112, 104, 117, 120, 45, 116, 117, 105, 47, 116,
  101, 115, 116, 2, 4, 2, 0, 0, 3, 4, 2, 0, 2, 4, 4, 2, 0, 0, 5, 4, 6,
  0, 7, 7, 3, 1, 0,
]);
export const ATTACH_FRAME = Uint8Array.from([
  0, 0, 0, 34, 2, 1, 4, 10, 3, 0, 0, 0, 3, 100, 101, 118, 0, 0, 2, 4, 6,
  0, 80, 0, 24, 0, 0, 3, 4, 1, 0, 4, 4, 4, 0, 0, 0, 0,
]);

interface TerminalPayload {
  terminalId?: Uint8Array;
  bytes: Uint8Array;
}

function readVarint(data: Uint8Array, offset: number): [number, number] {
  let value = 0;
  for (let shift = 0; shift <= 28 && offset < data.length; shift += 7) {
    const byte = data[offset++];
    value |= (byte & 0x7f) << shift;
    if ((byte & 0x80) === 0) return [value, offset];
  }
  throw new Error("invalid phux varint");
}

export function parseTerminalPayload(frame: Uint8Array): TerminalPayload | null {
  if (frame.length < 5) throw new Error("truncated phux frame");
  const bodyLength = new DataView(frame.buffer, frame.byteOffset, 4).getUint32(0);
  if (bodyLength !== frame.length - 4) throw new Error("invalid phux frame length");
  const type = frame[4];
  if (type !== TYPE_TERMINAL_OUTPUT && type !== TYPE_TERMINAL_SNAPSHOT) return null;

  let offset = 5;
  let terminalId: Uint8Array | undefined;
  let bytes = new Uint8Array();
  const contentField = type === TYPE_TERMINAL_OUTPUT ? 3 : 4;
  while (offset < frame.length) {
    let fieldId: number;
    [fieldId, offset] = readVarint(frame, offset);
    if (frame[offset++] !== 4) throw new Error("unsupported phux wire type");
    let length: number;
    [length, offset] = readVarint(frame, offset);
    if (offset + length > frame.length) throw new Error("truncated phux field");
    const value = frame.slice(offset, offset + length);
    if (fieldId === 1) terminalId = value;
    if (fieldId === contentField) bytes = value;
    offset += length;
  }
  return { terminalId, bytes };
}

function u32(value: number): number[] {
  return [(value >>> 24) & 255, (value >>> 16) & 255, (value >>> 8) & 255, value & 255];
}

function wrapInput(terminalId: Uint8Array, event: number[]): Uint8Array {
  const body = [0x10, 1, 4, terminalId.length, ...terminalId, 2, 4, event.length, ...event];
  return Uint8Array.from([...u32(body.length), ...body]);
}

export function inputTextFrames(
  terminalId: Uint8Array,
  text: string,
): Uint8Array[] {
  const frames = [...text].map((character) => {
    const encoded = new TextEncoder().encode(character);
    if (encoded.length !== 1) throw new Error("smoke marker must be ASCII");
    const codepoint = encoded[0];
    return wrapInput(terminalId, [
      ...u32(1),
      ...u32(20),
      0,
      0,
      0,
      0,
      0,
      1,
      ...u32(1),
      codepoint,
      1,
      ...u32(codepoint),
    ]);
  });
  frames.push(
    wrapInput(terminalId, [
      ...u32(1),
      ...u32(58),
      0,
      0,
      0,
      0,
      0,
      0,
      0,
    ]),
  );
  return frames;
}
