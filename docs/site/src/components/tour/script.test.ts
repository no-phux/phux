import { describe, expect, test } from "bun:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import Tour from "./Tour";
import { CHAPTERS, QUICK_RATE, SIGNAL_SECONDS, advanceTour, frameAt, type Playback } from "./script";

describe("landing tour", () => {
  test("settled frames are deterministic and have no travelling signals", () => {
    for (let chapter = 0; chapter < CHAPTERS.length; chapter++) {
      const frame = frameAt(chapter, Infinity);
      expect(frame).toEqual(frameAt(chapter, Infinity));
      expect(frame.signals).toEqual([]);
      expect(frame.tap).toBeNull();
      expect(Object.values(frame.panes).flatMap((pane) => pane.lines).some((line) => line.typing)).toBe(false);
    }
  });

  test("every animated signal remains bounded and frames do not mutate the script", () => {
    const before = JSON.stringify(CHAPTERS);
    for (let chapter = 0; chapter < CHAPTERS.length; chapter++) {
      for (let time = 0; time <= CHAPTERS[chapter]!.seconds; time += 0.1) {
        for (const signal of frameAt(chapter, time).signals) {
          expect(signal.progress).toBeGreaterThanOrEqual(0);
          expect(signal.progress).toBeLessThan(1);
          expect(signal.from).not.toBe(signal.to);
        }
      }
    }
    expect(JSON.stringify(CHAPTERS)).toBe(before);
  });

  test("the opening lap takes about six seconds, then stays in walkthrough pace", () => {
    expect(CHAPTERS.reduce((seconds, chapter) => seconds + chapter.seconds, 0) / QUICK_RATE).toBeCloseTo(5.9);
    let position: Playback = { chapter: 0, time: 0, pace: "quick" };
    for (let tick = 0; tick < 60; tick++) position = advanceTour(position, 0.1);
    expect(position.pace).toBe("walkthrough");
    expect(position.chapter).toBe(0);
    expect(position.time).toBeCloseTo(0.1);
    for (let tick = 0; tick < 200; tick++) position = advanceTour(position, 0.1);
    expect(position.pace).toBe("walkthrough");
  });

  test("pace changes retain wall-clock overshoot", () => {
    const position = advanceTour({ chapter: CHAPTERS.length - 1, time: CHAPTERS.at(-1)!.seconds - 0.06, pace: "quick" }, 0.04);
    expect(position).toMatchObject({ chapter: 0, pace: "walkthrough" });
    expect(position.time).toBeCloseTo(0.02);
    expect(advanceTour(position, 0.1).time).toBeCloseTo(0.12);
    expect(advanceTour({ chapter: 0, time: 0, pace: "quick" }, 0.1).time).toBeCloseTo(0.3);
  });

  test("connections are brisk, with at least 1.5 seconds of settled reading time per chapter", () => {
    expect(SIGNAL_SECONDS).toBeLessThanOrEqual(0.2);
    for (let chapter = 0; chapter < CHAPTERS.length; chapter++) {
      const script = CHAPTERS[chapter]!;
      expect(frameAt(chapter, script.seconds - 1.5)).toEqual(frameAt(chapter, Infinity));
      for (const [index, step] of script.steps.entries()) {
        const next = script.steps[index + 1];
        if ("type" in step && next) {
          expect(frameAt(chapter, next.at).panes[step.pane].lines.some((line) => line.typing)).toBe(false);
        }
      }
    }
  });

  test("static artwork offers a real link without misrepresenting background hosts as live", () => {
    const html = renderToStaticMarkup(createElement(Tour));
    expect(html).toContain('aria-label="Open terminal demo"');
    expect(html).toContain('href="/embed"');
    expect(html).toContain('class="tour-distance" aria-hidden="true"');
    expect(html.match(/class="tour-distant-host /g)).toHaveLength(3);
    expect(html).toContain("Scripted illustration, not a live session.");
    expect(html).not.toContain('class="tour-bar"');
  });
});
