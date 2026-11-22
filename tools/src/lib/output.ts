// Human diagnostics only; machine documents and child output bypass this module.
const STYLES = { error: 31, warn: 33, ok: 32, info: 36 } as const;
export type Level = keyof typeof STYLES;

export function colorEnabled(stream: { isTTY?: boolean }): boolean {
  const env = process.env;
  if (env["NO_COLOR"] || env["TERM"] === "dumb") return false;
  if (!["", "0"].includes(env["CLICOLOR_FORCE"] ?? "")) return true;
  return env["CLICOLOR"] !== "0" && Boolean(stream.isTTY);
}

export function status(level: Level, message: string): void {
  const label = colorEnabled(process.stderr)
    ? `\x1b[1mphux\x1b[0m: \x1b[${STYLES[level]}m${level}\x1b[0m:`
    : `phux: ${level}:`;
  process.stderr.write(`${label} ${message}\n`);
}

export function hint(message: string): void {
  for (const line of message.split(/\r?\n/)) {
    process.stderr.write(`  hint: ${line.trimStart()}\n`);
  }
}
