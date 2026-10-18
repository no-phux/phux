// Extract one Keep-a-Changelog version section for Linear release notes. The
// notes come from the tagged changelog section, never a SHA stub.
import { writeFileSync } from "node:fs";
import { Failure } from "../lib/command.ts";
import { show } from "../lib/git.ts";

const HEADING = /^## \[([^\]]+)\]/;

export function versionFromTag(tag: string): string {
  const version = /^(?:.+-)?v([0-9].*)$/.exec(tag)?.[1];
  if (version === undefined) {
    throw new Failure(`unsupported release tag '${tag}'; expected vX.Y.Z or <component>-vX.Y.Z`);
  }
  return version;
}

export function extractSection(text: string, version: string): string {
  const lines = text.split(/\r?\n/);
  const start = lines.findIndex((line) => HEADING.exec(line)?.[1] === version);
  if (start < 0) throw new Failure(`no changelog heading for ${version}`);
  const next = lines.findIndex((line, index) => index > start && HEADING.test(line));
  return `${lines
    .slice(start, next < 0 ? undefined : next)
    .join("\n")
    .trim()}\n`;
}

/** Print or write `tag`'s section of `changelog` as of `ref`; never empty notes. */
export function writeSection(tag: string, changelog: string, ref: string, output?: string): number {
  const text = show(ref, changelog);
  if (text === undefined) throw new Failure(`cannot read ${ref}:${changelog}`);
  const section = extractSection(text, versionFromTag(tag));
  if (output) writeFileSync(output, section);
  else process.stdout.write(section);
  return 0;
}
