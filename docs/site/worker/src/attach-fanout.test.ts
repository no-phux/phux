import { describe, expect, test } from "bun:test";
import { ViewFanout } from "./attach-fanout";
import {
  TYPE_ATTACH,
  TYPE_ATTACHED,
  TYPE_ATTACH_READY,
  TYPE_BOOTSTRAP_CHUNK,
  TYPE_ERROR,
  TYPE_HELLO_OK,
  TYPE_RESIZE_TERMINAL,
} from "./wire";

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

const attach = (id: number) => frame(TYPE_ATTACH, [[5, u32(id)]]);
const attached = (id: number) => frame(TYPE_ATTACHED, [[3, u32(id)]]);
const attachReady = (id: number) => frame(TYPE_ATTACH_READY, [[1, u32(id)]]);
const output = () => frame(0x90, [[1, u32(3)], [3, [104, 105]]]);
const resize = () => frame(TYPE_RESIZE_TERMINAL, [[1, u32(3)], [2, u32(46)]]);
const helloOk = () => frame(TYPE_HELLO_OK, [[1, [1]]]);
const bootstrap = () => frame(TYPE_BOOTSTRAP_CHUNK, [[1, u32(3)], [5, [27, 91, 50, 74]]]);

describe("session view fan-out", () => {
  test("addresses attachment frames to the socket that attached", () => {
    const fanout = new ViewFanout<string>();
    fanout.add("desktop", 1);
    fanout.add("phone", 2);
    expect(fanout.fromClient("phone", attach(7))).toBe(true);

    expect(fanout.targets(attached(7), "phone")).toEqual(["phone"]);
    expect(fanout.targets(attachReady(7), "phone")).toEqual(["phone"]);
    // No view owns another attachment's frames, so they are dropped, not shown.
    expect(fanout.targets(attached(9), "phone")).toEqual([]);
  });

  test("broadcasts session frames to every attached view", () => {
    const fanout = new ViewFanout<string>();
    fanout.add("desktop", 1);
    fanout.add("phone", 2);
    expect(fanout.targets(output(), "phone")).toEqual(["desktop", "phone"]);
  });

  test("sends errors back to the view that caused them", () => {
    const fanout = new ViewFanout<string>();
    fanout.add("desktop", 1);
    fanout.add("phone", 2);
    expect(fanout.targets(frame(TYPE_ERROR, [[2, [1]]]), "phone")).toEqual(["phone"]);
  });

  test("keeps one view's handshake and bootstrap out of the other view", () => {
    const fanout = new ViewFanout<string>();
    fanout.add("desktop", 1);
    fanout.add("phone", 2);
    // A second HELLO_OK is a protocol error to a client that already greeted,
    // and one view's bootstrap is not the other view's screen.
    expect(fanout.targets(helloOk(), "phone")).toEqual(["phone"]);
    expect(fanout.targets(bootstrap(), "phone")).toEqual(["phone"]);
    expect(fanout.targets(bootstrap(), "desktop")).toEqual(["desktop"]);
  });

  test("lets only the newest connection resize the shared terminal", () => {
    const fanout = new ViewFanout<string>();
    fanout.add("desktop", 1);
    expect(fanout.fromClient("desktop", resize())).toBe(true);
    fanout.add("phone", 2);
    // The phone attached second, so it owns the shared geometry.
    expect(fanout.fromClient("desktop", resize())).toBe(false);
    expect(fanout.fromClient("phone", resize())).toBe(true);
    // Reattaching on an existing connection does not steal the geometry; only a
    // new connection is newer.
    expect(fanout.fromClient("desktop", attach(11))).toBe(true);
    expect(fanout.fromClient("desktop", resize())).toBe(false);
  });

  test("restores sole-socket resizing when the newest view leaves", () => {
    const fanout = new ViewFanout<string>();
    fanout.add("desktop", 1);
    fanout.add("phone", 2);
    fanout.fromClient("phone", attach(7));
    fanout.remove("phone");
    expect(fanout.count).toBe(1);
    expect(fanout.targets(attached(7), "desktop")).toEqual([]);
    expect(fanout.fromClient("desktop", resize())).toBe(true);
  });

  test("re-registers attachments recorded before the object hibernated", () => {
    // A wake rebuilds registration from what each socket persisted.
    const fanout = new ViewFanout<string>();
    fanout.add("desktop", 1);
    fanout.add("phone", 2);
    fanout.claim("phone", 7);
    expect(fanout.targets(attached(7), "phone")).toEqual(["phone"]);
    expect(fanout.fromClient("desktop", resize())).toBe(false);
    expect(fanout.fromClient("phone", resize())).toBe(true);
  });

  test("ignores frames from a socket that is not attached", () => {
    const fanout = new ViewFanout<string>();
    fanout.add("desktop", 1);
    expect(fanout.fromClient("stranger", attach(1))).toBe(false);
    expect(fanout.claim("stranger", 2)).toBeUndefined();
    fanout.remove("stranger");
    expect(fanout.count).toBe(1);
  });
});
