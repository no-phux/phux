// Extract one Keep-a-Changelog version section for Linear release notes. The
// notes come from the tagged changelog section, never a SHA stub.

import { readFileSync, writeFileSync } from "node:fs";

const HEADING = /^## \[([^\]]+)\]/;
// Python's str.splitlines boundaries, which the notes have always split on.
// oxlint-disable-next-line no-control-regex -- \v, \f and \x1c-\x1e are line breaks there.
const LINE_BREAK = /\r\n|[\n\v\f\r\x1c-\x1e\x85\u2028\u2029]/;

export function versionFromTag(tag: string): string {
  if (/^v[0-9]/.test(tag)) return tag.slice(1);
  const marker = tag.lastIndexOf("-v");
  if (marker >= 0 && /[0-9]/.test(tag[marker + 2] ?? "")) return tag.slice(marker + 2);
  throw new Error(`unsupported release tag '${tag}'; expected vX.Y.Z or <component>-vX.Y.Z`);
}

export function extractSection(text: string, version: string): string {
  const lines = text.split(LINE_BREAK);
  const start = lines.findIndex((line) => HEADING.exec(line)?.[1] === version);
  if (start < 0) throw new Error(`no changelog heading for ${version}`);
  const next = lines.findIndex((line, index) => index > start && HEADING.test(line));
  const section = lines.slice(start, next < 0 ? lines.length : next).join("\n");
  return `${section.trim()}\n`;
}

/** The CLI: an unreadable file or a missing section is exit 1, never empty notes. */
export function writeSection(tag: string, changelog: string, output: string | undefined): number {
  try {
    const section = extractSection(readFileSync(changelog, "utf8"), versionFromTag(tag));
    if (output) writeFileSync(output, section);
    else process.stdout.write(section);
    return 0;
  } catch (error) {
    console.error(`error: ${error instanceof Error ? error.message : String(error)}`);
    return 1;
  }
}
