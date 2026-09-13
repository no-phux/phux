import assert from "node:assert/strict";
import test from "node:test";

import { truncateLines, truncateText } from "../src/truncate.js";

test("truncation helpers bound output and retain newest lines", () => {
  const text = truncateText("abcdefghij", 5);
  assert.equal(Array.from(text.text).length, 5);
  assert.equal(text.truncated, true);
  assert.equal(text.omittedChars, 5);

  const marked = truncateText("x".repeat(100), 50);
  const match = /\.\.\. (\d+) characters omitted \.\.\./.exec(marked.text);
  assert.notEqual(match, null);
  const markerLength = Array.from(match?.[0] ?? "").length + 2; // surrounding newlines
  assert.equal(marked.omittedChars, 100 - (50 - markerLength));
  assert.equal(Number(match?.[1]), marked.omittedChars);

  assert.deepEqual(truncateLines(["old", "middle", "new"], 2), {
    lines: ["middle", "new"],
    truncated: true,
    omittedLines: 1,
  });
});
