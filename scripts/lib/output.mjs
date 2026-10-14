// Human diagnostics only; machine documents and child output bypass this module.
const styles = { error: 31, warn: 33, ok: 32, info: 36 };

export function colorEnabled(stream) {
  if (process.env.NO_COLOR || process.env.TERM === "dumb") return false;
  if (!["", "0"].includes(process.env.CLICOLOR_FORCE ?? "")) return true;
  return process.env.CLICOLOR !== "0" && Boolean(stream.isTTY);
}

export function status(level, message) {
  const label = `phux: ${level}:`;
  const styled = colorEnabled(process.stderr)
    ? `\x1b[1mphux\x1b[0m: \x1b[${styles[level]}m${level}\x1b[0m:`
    : label;
  process.stderr.write(`${styled} ${message}\n`);
}

export function hint(message) {
  for (const line of String(message).split(/\r?\n/)) {
    process.stderr.write(`  hint: ${line.trimStart()}\n`);
  }
}
