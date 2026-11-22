import assert from "node:assert/strict";
import test from "node:test";

import { truncateLines, truncateText } from "../src/truncate.js";

test("truncation helpers bound output and retain newest lines", () => {
  const text = truncateText("abcdefghij", 5);
  assert.equal(Array.from(text.text).length, 5);
  assert.equal(text.truncated, true);
  assert.equal(text.omittedChars, 5);

  // The 31-character marker spends part of the budget, so 81 characters go.
  assert.deepEqual(truncateText("x".repeat(100), 50), {
    text: `${"x".repeat(10)}\n... 81 characters omitted ...\n${"x".repeat(9)}`,
    truncated: true,
    omittedChars: 81,
  });

  assert.deepEqual(truncateLines(["old", "middle", "new"], 2), {
    lines: ["middle", "new"],
    truncated: true,
    omittedLines: 1,
  });
});
