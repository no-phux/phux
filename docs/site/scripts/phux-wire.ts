const TYPE_RESOURCE_OUTPUT = 0x90;
const TYPE_BOOTSTRAP_CHUNK = 0x94;
export const TYPE_HELLO_OK = 0x80;
export const TYPE_ERROR = 0xc1;

// Protocol 0.9 HELLO (`ClientCapabilities::new`) and CreateIfMissing ATTACH
// (`name=default`, 80x24) encoded by phux-protocol. Kept in lockstep with
// `docs/site/edge` `smoke_hello_and_attach_bytes_match_the_site_smoke_client`.
export const HELLO_FRAME = Uint8Array.from([
  0, 0, 0, 65, 1, 1, 4, 15, 112, 104, 117, 120, 45, 115, 105, 116, 101, 45, 115,
  109, 111, 107, 101, 2, 4, 2, 0, 0, 3, 4, 2, 0, 9, 4, 4, 2, 0, 0, 5, 4, 28, 0,
  1, 7, 3, 1, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 16, 0,
  0,
]);
export const ATTACH_FRAME = Uint8Array.from([
  0, 0, 0, 45, 2, 1, 4, 14, 3, 0, 0, 0, 7, 100, 101, 102, 97, 117, 108, 116, 0,
  0, 2, 4, 6, 0, 80, 0, 24, 0, 0, 3, 4, 1, 1, 4, 4, 4, 0, 0, 19, 136, 5, 4, 4, 0,
  0, 0, 1,
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

function contentField(type: number): number | undefined {
  if (type === TYPE_RESOURCE_OUTPUT) return 3;
  if (type === TYPE_BOOTSTRAP_CHUNK) return 5;
  return undefined;
}

export function frameType(frame: Uint8Array): number | undefined {
  if (frame.length < 5) return undefined;
  const bodyLength = new DataView(frame.buffer, frame.byteOffset, 4).getUint32(0);
  if (bodyLength !== frame.length - 4) return undefined;
  return frame[4];
}

export function parseTerminalPayload(frame: Uint8Array): TerminalPayload | null {
  if (frame.length < 5) throw new Error("truncated phux frame");
  const bodyLength = new DataView(frame.buffer, frame.byteOffset, 4).getUint32(0);
  if (bodyLength !== frame.length - 4) throw new Error("invalid phux frame length");
  const type = frame[4];
  const field = contentField(type);
  if (field === undefined) return null;

  let offset = 5;
  let terminalId: Uint8Array | undefined;
  let bytes = new Uint8Array();
  while (offset < frame.length) {
    let fieldId: number;
    [fieldId, offset] = readVarint(frame, offset);
    if (frame[offset++] !== 4) throw new Error("unsupported phux wire type");
    let length: number;
    [length, offset] = readVarint(frame, offset);
    if (offset + length > frame.length) throw new Error("truncated phux field");
    const value = frame.slice(offset, offset + length);
    if (fieldId === 1) terminalId = value;
    if (fieldId === field) bytes = value;
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

function physicalKey(character: string): number {
  if (character >= "a" && character <= "z") return 20 + (character.charCodeAt(0) - 97);
  if (character >= "A" && character <= "Z") return 20 + (character.charCodeAt(0) - 65);
  if (character >= "0" && character <= "9") return 6 + (character.charCodeAt(0) - 48);
  if (character === " ") return 63;
  return 0;
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
      ...u32(physicalKey(character)),
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
