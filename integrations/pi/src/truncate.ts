export interface TruncatedText {
  readonly text: string;
  readonly truncated: boolean;
  readonly omittedChars: number;
}

/** Keep useful context from both ends while bounding model-facing text. */
export function truncateText(input: string, maxChars: number): TruncatedText {
  if (!Number.isSafeInteger(maxChars) || maxChars < 0) {
    throw new RangeError("maxChars must be a non-negative safe integer");
  }

  const chars = Array.from(input);
  if (chars.length <= maxChars) {
    return { text: input, truncated: false, omittedChars: 0 };
  }
  if (maxChars === 0) {
    return { text: "", truncated: true, omittedChars: chars.length };
  }

  let omittedChars = chars.length - maxChars;
  let marker = `\n... ${omittedChars} characters omitted ...\n`;
  let markerChars = Array.from(marker);
  if (markerChars.length >= maxChars) {
    return {
      text: chars.slice(0, maxChars).join(""),
      truncated: true,
      omittedChars,
    };
  }

  // The marker consumes part of maxChars, so more source characters are
  // omitted than `input.length - maxChars`. Iterate because the corrected
  // count can itself change the marker's digit width.
  for (;;) {
    const contentBudget = maxChars - markerChars.length;
    const corrected = chars.length - contentBudget;
    if (corrected === omittedChars) break;
    omittedChars = corrected;
    marker = `\n... ${omittedChars} characters omitted ...\n`;
    markerChars = Array.from(marker);
    if (markerChars.length >= maxChars) {
      return {
        text: chars.slice(0, maxChars).join(""),
        truncated: true,
        omittedChars: chars.length - maxChars,
      };
    }
  }

  const contentBudget = maxChars - markerChars.length;
  const headLength = Math.ceil(contentBudget / 2);
  const tailLength = contentBudget - headLength;
  return {
    text: `${chars.slice(0, headLength).join("")}${marker}${chars.slice(chars.length - tailLength).join("")}`,
    truncated: true,
    omittedChars,
  };
}

export interface TruncatedLines {
  readonly lines: readonly string[];
  readonly truncated: boolean;
  readonly omittedLines: number;
}

/** Keep the newest lines, which usually contain the current prompt or failure. */
export function truncateLines(lines: readonly string[], maxLines: number): TruncatedLines {
  if (!Number.isSafeInteger(maxLines) || maxLines < 0) {
    throw new RangeError("maxLines must be a non-negative safe integer");
  }
  if (lines.length <= maxLines) {
    return { lines: [...lines], truncated: false, omittedLines: 0 };
  }
  return {
    lines: lines.slice(lines.length - maxLines),
    truncated: true,
    omittedLines: lines.length - maxLines,
  };
}
