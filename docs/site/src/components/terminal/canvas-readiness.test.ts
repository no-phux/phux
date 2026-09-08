import { describe, expect, test } from "bun:test";
import {
  hasMeaningfulCanvasPaint,
  waitForMeaningfulCanvasPaint,
} from "./canvas-readiness";

function canvasWith(pixels: Uint8ClampedArray, width = pixels.length / 4) {
  return {
    width,
    height: 1,
    getContext: () => ({
      getImageData: () => ({ data: pixels }) as ImageData,
    }),
  };
}

function fakeClock() {
  let time = 0;
  let nextId = 1;
  const timers = new Map<number, { callback: () => void; due: number }>();

  return {
    now: () => time,
    setTimer(callback: () => void, delay?: number) {
      const id = nextId++;
      timers.set(id, { callback, due: time + (delay ?? 0) });
      return id as unknown as ReturnType<typeof setTimeout>;
    },
    clearTimer(id: number | ReturnType<typeof setTimeout>) {
      timers.delete(id as unknown as number);
    },
    advance(milliseconds: number) {
      time += milliseconds;
      for (const [id, timer] of [...timers]) {
        if (timer.due <= time) {
          timers.delete(id);
          timer.callback();
        }
      }
    },
    pending: () => timers.size,
  };
}

describe("canvas readiness", () => {
  test("rejects a blank transparent canvas", () => {
    expect(hasMeaningfulCanvasPaint(canvasWith(new Uint8ClampedArray(400)))).toBe(false);
  });

  test("rejects a uniformly painted background", () => {
    const pixels = new Uint8ClampedArray(400);
    for (let index = 0; index < pixels.length; index += 4) {
      pixels.set([8, 8, 11, 255], index);
    }
    expect(hasMeaningfulCanvasPaint(canvasWith(pixels))).toBe(false);
  });

  test("accepts non-uniform terminal content", () => {
    const pixels = new Uint8ClampedArray(800);
    for (let index = 0; index < pixels.length; index += 4) {
      pixels.set(index < 200 ? [220, 220, 225, 255] : [8, 8, 11, 255], index);
    }
    expect(hasMeaningfulCanvasPaint(canvasWith(pixels))).toBe(true);
  });

  test("rejects a sparse cursor-only paint", () => {
    const pixels = new Uint8ClampedArray(880 * 512 * 4);
    for (let index = 0; index < pixels.length; index += 4) {
      pixels.set(index < 8 * 16 * 4 ? [255, 255, 255, 255] : [0, 0, 0, 255], index);
    }
    expect(hasMeaningfulCanvasPaint(canvasWith(pixels, 880))).toBe(false);
  });

  test("times out and clears its timer", async () => {
    const clock = fakeClock();
    const readiness = waitForMeaningfulCanvasPaint(
      canvasWith(new Uint8ClampedArray(400)),
      { timeoutMs: 50, pollIntervalMs: 10, ...clock },
    );

    clock.advance(50);
    const error = await readiness.catch((caught) => caught);
    expect(error).toBeInstanceOf(Error);
    expect((error as Error).message).toContain("readiness deadline");
    expect(clock.pending()).toBe(0);
  });

  test("resolves after paint and clears its timer", async () => {
    const clock = fakeClock();
    const pixels = new Uint8ClampedArray(800);
    const readiness = waitForMeaningfulCanvasPaint(canvasWith(pixels), {
      timeoutMs: 50,
      pollIntervalMs: 10,
      ...clock,
    });

    for (let index = 0; index < 200; index += 4) {
      pixels.set([220, 220, 225, 255], index);
    }
    clock.advance(10);
    await readiness;
    expect(clock.pending()).toBe(0);
  });

  test("aborts and clears its timer", async () => {
    const clock = fakeClock();
    const controller = new AbortController();
    const readiness = waitForMeaningfulCanvasPaint(
      canvasWith(new Uint8ClampedArray(400)),
      { signal: controller.signal, ...clock },
    );

    controller.abort();
    const error = await readiness.catch((caught) => caught);
    expect(error).toMatchObject({ name: "AbortError" });
    expect(clock.pending()).toBe(0);
  });
});
