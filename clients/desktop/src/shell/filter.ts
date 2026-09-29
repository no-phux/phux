/**
 * Palette ranking. Every whitespace-separated token must match the item's
 * text, as a substring or failing that as an in-order subsequence. Substring
 * hits at a word start rank highest; scattered subsequences rank lowest.
 */
export interface Ranked<T> {
  item: T;
  score: number;
}

export function rank<T>(items: readonly T[], query: string, text: (item: T) => string): T[] {
  const tokens = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (tokens.length === 0) return [...items];
  const ranked: Ranked<T>[] = [];
  items.forEach((item) => {
    const score = matchScore(text(item).toLowerCase(), tokens);
    if (score !== undefined) ranked.push({ item, score });
  });
  // Array.prototype.sort is stable, so equal scores keep registry order.
  return ranked.sort((a, b) => b.score - a.score).map((entry) => entry.item);
}

export function matchScore(haystack: string, tokens: readonly string[]): number | undefined {
  let total = 0;
  for (const token of tokens) {
    const score = tokenScore(haystack, token);
    if (score === undefined) return undefined;
    total += score;
  }
  return total;
}

function tokenScore(haystack: string, token: string): number | undefined {
  const index = haystack.indexOf(token);
  if (index >= 0) {
    const wordStart = index === 0 || /[\s/:._-]/.test(haystack[index - 1] ?? "");
    return (wordStart ? 200 : 120) - Math.min(index, 60);
  }
  let cursor = 0;
  let gaps = 0;
  for (const char of token) {
    const found = haystack.indexOf(char, cursor);
    if (found < 0) return undefined;
    gaps += found - cursor;
    cursor = found + 1;
  }
  return Math.max(1, 60 - gaps);
}
