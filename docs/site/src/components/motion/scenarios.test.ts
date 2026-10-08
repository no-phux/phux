import { describe, expect, test } from "bun:test";
import {
  BEAT_SECONDS,
  SCENARIOS,
  beatAt,
  duration,
  signalProgress,
  smootherstep,
} from "./scenarios";
import { curvePoint, port } from "./Diagram";

describe("explainer clock", () => {
  test("every scenario has four seekable beats and a documentation route", () => {
    for (const scenario of SCENARIOS) {
      expect(scenario.beats).toHaveLength(4);
      expect(scenario.href.startsWith("/")).toBe(true);
      expect(beatAt(scenario, -1)).toBe(0);
      expect(beatAt(scenario, duration(scenario))).toBe(3);
      scenario.beats.forEach((beat, index) => {
        expect(beatAt(scenario, index * BEAT_SECONDS)).toBe(index);
        expect(beat.text.length).toBeGreaterThan(20);
        expect(beat.detail.length).toBeGreaterThan(20);
        expect(Boolean(beat.from)).toBe(Boolean(beat.to));
      });
    }
  });
  test("motion is deterministic, clamped, monotonic and holds at the ports", () => {
    expect(smootherstep(-1)).toBe(0);
    expect(smootherstep(2)).toBe(1);
    expect(smootherstep(0.5)).toBeCloseTo(0.5);
    expect(signalProgress(0.3)).toBe(0);
    expect(signalProgress(1.8)).toBe(1);
    expect(signalProgress(BEAT_SECONDS)).toBe(1);
    let previous = 0;
    for (let time = 0; time < 2.5; time += 0.01) {
      const value = signalProgress(time);
      expect(value).toBeGreaterThanOrEqual(previous);
      expect(value).toBe(signalProgress(time));
      previous = value;
    }
  });
  test("signals start and land on card edges, not hidden centers", () => {
    expect(port([125, 160], [440, 160])).toEqual([224, 160]);
    expect(port([160, 90], [160, 225])).toEqual([160, 128]);
    expect(curvePoint([0, 0], [10, 20], [20, 0], 0)).toEqual([0, 0]);
    expect(curvePoint([0, 0], [10, 20], [20, 0], 1)).toEqual([20, 0]);
    expect(curvePoint([0, 0], [10, 20], [20, 0], 0.5)).toEqual([10, 10]);
  });
});
