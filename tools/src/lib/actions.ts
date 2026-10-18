// GitHub Actions step outputs: `key=value` lines appended to $GITHUB_OUTPUT.

export type Outputs = Record<string, string | boolean>;

export function formatOutputs(outputs: Outputs): string {
  return Object.entries(outputs)
    .map(([key, value]) => `${key}=${String(value)}`)
    .join("\n");
}

export function parseOutputs(text: string): Record<string, string> {
  const parsed: Record<string, string> = {};
  for (const line of text.split("\n")) {
    const split = line.indexOf("=");
    if (split > 0) parsed[line.slice(0, split)] = line.slice(split + 1);
  }
  return parsed;
}
