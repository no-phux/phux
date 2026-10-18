// GitHub Actions step outputs: `key=value` lines.
import { appendFileSync } from "node:fs";

export type Outputs = Record<string, string | boolean>;

export function formatOutputs(outputs: Outputs): string {
  return Object.entries(outputs)
    .map(([key, value]) => `${key}=${String(value)}`)
    .join("\n");
}

/** Append to $GITHUB_OUTPUT inside Actions; print to stdout anywhere else. */
export function writeOutputs(outputs: Outputs): void {
  const text = `${formatOutputs(outputs)}\n`;
  const path = process.env["GITHUB_OUTPUT"];
  if (path === undefined || path === "") process.stdout.write(text);
  else appendFileSync(path, text);
}
