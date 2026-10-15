import { describe, expect, test } from "bun:test";
import {
  attachIdOf,
  frameFields,
  frameType,
  TYPE_ATTACH,
  TYPE_ATTACHED,
  TYPE_ATTACH_READY,
  TYPE_ERROR,
  TYPE_RESIZE_TERMINAL,
} from "./wire";
// The pinned bytes the smoke client sends, which a Rust test keeps in lockstep
// with the protocol encoder. These are one half of the contract; hand-built
// frames are the other half and can agree with a wrong field id.
import { ATTACH_FRAME, HELLO_FRAME } from "../../scripts/phux-wire";

function varint(value: number): number[] {
  const out: number[] = [];
  let rest = value;
  do {
    const byte = rest & 0x7f;
    rest >>>= 7;
    out.push(rest > 0 ? byte | 0x80 : byte);
  } while (rest > 0);
  return out;
}

/** One frame: u32 body length, type byte, then `id, wire, length, value` fields. */
function frame(type: number, fields: Array<[number, number[]]>): Uint8Array {
  const body = [type];
  for (const [id, value] of fields) {
    body.push(...varint(id), 4, ...varint(value.length), ...value);
  }
  const length = body.length;
  return Uint8Array.from([
    (length >>> 24) & 255,
    (length >>> 16) & 255,
    (length >>> 8) & 255,
    length & 255,
    ...body,
  ]);
}

function u32(value: number): number[] {
  return [
    (value >>> 24) & 255,
    (value >>> 16) & 255,
    (value >>> 8) & 255,
    value & 255,
  ];
}

describe("phux frame reader", () => {
  test("reads the type of a well-framed frame only", () => {
    const attach = frame(TYPE_ATTACH, [[4, u32(7)]]);
    expect(frameType(attach)).toBe(TYPE_ATTACH);
    // A body length that disagrees with the buffer is not one whole frame.
    expect(frameType(attach.slice(0, attach.length - 1))).toBeUndefined();
    expect(frameType(Uint8Array.from([0, 0, 0, 1]))).toBeUndefined();
  });

  test("decodes fields, including multi-byte field ids", () => {
    const frameWithWideId = frame(TYPE_ATTACH, [
      [300, [1, 2, 3]],
      [5, u32(19)],
    ]);
    expect(frameFields(frameWithWideId)).toEqual([
      { id: 300, value: Uint8Array.from([1, 2, 3]) },
      { id: 5, value: Uint8Array.from(u32(19)) },
    ]);
    expect(attachIdOf(frameWithWideId)).toBe(19);
  });

  test("reads the correlation id from ATTACH, ATTACHED and ATTACH_READY", () => {
    // `field::attach::ATTACH_ID` is 5; field 4 is `scrollback_limit_lines`, and
    // reading that as an attach id routes an attachment's frames to nobody.
    expect(attachIdOf(frame(TYPE_ATTACH, [[5, u32(19)]]))).toBe(19);
    expect(attachIdOf(frame(TYPE_ATTACH, [[4, u32(5000)], [5, u32(1)]]))).toBe(1);
    expect(attachIdOf(frame(TYPE_ATTACHED, [[3, u32(9)]]))).toBe(9);
    expect(attachIdOf(frame(TYPE_ATTACH_READY, [[1, u32(9)]]))).toBe(9);
  });

  test("a scrollback limit is never mistaken for an attachment", () => {
    expect(attachIdOf(frame(TYPE_ATTACH, [[4, u32(5000)]]))).toBeUndefined();
  });

  test("has no correlation id for session-wide or input frames", () => {
    // RESOURCE_OUTPUT carries the terminal id, not an attachment.
    expect(attachIdOf(frame(0x90, [[1, u32(3)], [3, [27, 91, 50, 74]]]))).toBeUndefined();
    expect(attachIdOf(frame(TYPE_RESIZE_TERMINAL, [[1, u32(3)]]))).toBeUndefined();
    expect(attachIdOf(frame(TYPE_ERROR, [[2, [1]]]))).toBeUndefined();
  });

  test("reads the real client's pinned ATTACH and HELLO frames", () => {
    // The encoder is the authority: if the field id this reader uses is wrong,
    // these pinned bytes disagree, which is how a single-view session silently
    // stopped attaching in production.
    expect(frameType(HELLO_FRAME)).toBe(0x01);
    expect(frameType(ATTACH_FRAME)).toBe(TYPE_ATTACH);
    expect(attachIdOf(ATTACH_FRAME)).toBe(1);
  });

  test("tolerates a malformed field stream without throwing", () => {    // A field whose declared length runs past the end of the frame.
    const truncated = Uint8Array.from([0, 0, 0, 7, TYPE_ATTACH, 4, 4, 200, 1]);
    expect(frameFields(truncated)).toEqual([]);
    expect(attachIdOf(truncated)).toBeUndefined();
    // A field with a wire type the protocol does not use here.
    const wrongWire = Uint8Array.from([0, 0, 0, 6, TYPE_ATTACH, 4, 3, 0]);
    expect(frameFields(wrongWire)).toEqual([]);
    // A u32 field that is not four bytes long.
    expect(attachIdOf(frame(TYPE_ATTACHED, [[3, [1, 2]]]))).toBeUndefined();
  });
});
