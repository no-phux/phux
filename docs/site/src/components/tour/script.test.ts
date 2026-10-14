import { describe, expect, test } from "bun:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import Tour from "./Tour";
import { CHAPTERS, frameAt } from "./script";

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
