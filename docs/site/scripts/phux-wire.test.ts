import { describe, expect, test } from "bun:test";
import {
  ATTACH_FRAME,
  HELLO_FRAME,
  TYPE_HELLO_OK,
  frameType,
  inputTextFrames,
  parseTerminalPayload,
} from "./phux-wire";

describe("synthetic phux wire parser", () => {
  test("extracts terminal id and bytes from resource output", () => {
    const frame = Uint8Array.from([
      0, 0, 0, 17, 0x90, 1, 4, 5, 0, 0, 0, 0, 1, 3, 4, 5, 104, 101, 108,
      108, 111,
    ]);
    expect(parseTerminalPayload(frame)).toEqual({
      terminalId: Uint8Array.from([0, 0, 0, 0, 1]),
      bytes: Uint8Array.from([104, 101, 108, 108, 111]),
    });
  });

  test("extracts bootstrap chunk payload used for the edge greeting", () => {
    const frame = Uint8Array.from([
      0, 0, 0, 17, 0x94, 1, 4, 5, 0, 0, 0, 0, 1, 5, 4, 5, 116, 111, 117,
      114, 33,
    ]);
    expect(parseTerminalPayload(frame)).toEqual({
      terminalId: Uint8Array.from([0, 0, 0, 0, 1]),
      bytes: Uint8Array.from([116, 111, 117, 114, 33]),
    });
  });

  test("ignores handshake frames and encodes ASCII command input", () => {
    expect(parseTerminalPayload(HELLO_FRAME)).toBeNull();
    expect(frameType(HELLO_FRAME)).toBe(1);
    expect(ATTACH_FRAME[4]).toBe(2);
    expect(TYPE_HELLO_OK).toBe(0x80);
    expect(() => parseTerminalPayload(Uint8Array.from([0, 0, 0, 9, 0x90]))).toThrow();
    const frames = inputTextFrames(Uint8Array.from([0, 0, 0, 0, 1]), "OK");
    expect(frames).toHaveLength(3);
    expect(frames.every((frame) => frame[4] === 0x10)).toBe(true);
  });

  test("HELLO advertises protocol 0.9", () => {
    expect(HELLO_FRAME[31]).toBe(0);
    expect(HELLO_FRAME[32]).toBe(9);
  });
});
