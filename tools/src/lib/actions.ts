// GitHub Actions step outputs: `key=value` lines appended to $GITHUB_OUTPUT.

export type Outputs = Record<string, string | boolean>;

export function formatOutputs(outputs: Outputs): string {
  return Object.entries(outputs)
    .map(([key, value]) => `${key}=${String(value)}`)
    .join("\n");
}
