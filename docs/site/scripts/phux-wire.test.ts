import { describe, expect, test } from "bun:test";
import { inputTextFrames, parseTerminalPayload } from "./phux-wire";

describe("synthetic phux wire parser", () => {
  test("extracts terminal id and bytes from a snapshot", () => {
    const frame = Uint8Array.from([
      0, 0, 0, 17, 0x91, 1, 4, 5, 0, 0, 0, 0, 1, 4, 4, 5, 104, 101, 108,
      108, 111,
    ]);
    expect(parseTerminalPayload(frame)).toEqual({
      terminalId: Uint8Array.from([0, 0, 0, 0, 1]),
      bytes: Uint8Array.from([104, 101, 108, 108, 111]),
    });
  });

  test("rejects malformed lengths and encodes ASCII command input", () => {
    expect(() => parseTerminalPayload(Uint8Array.from([0, 0, 0, 9, 0x91]))).toThrow();
    const frames = inputTextFrames(Uint8Array.from([0, 0, 0, 0, 1]), "OK");
    expect(frames).toHaveLength(3);
    expect(frames.every((frame) => frame[4] === 0x10)).toBe(true);
  });
});
