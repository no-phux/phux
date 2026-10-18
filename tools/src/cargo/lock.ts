// Cargo.lock text, edited in place. Both lock commands rewrite single lines and
// must leave every other byte untouched, so packages are matched as text
// blocks rather than parsed and re-serialized.
import { Failure } from "../lib/command.ts";

/** One `[[package]]` block, through to the next block or the end of the file. */
export const PACKAGE = /^\[\[package\]\]\n.*?(?=^\[\[package\]\]|(?![\s\S]))/gms;
const NAME = /^name = "([^"]+)"$/m;
export const VERSION = /^version = "([^"]+)"$/m;
const SOURCE = /^source = /m;
export const DEPENDENCIES = /^dependencies = \[\n(.*?)^\]/ms;
const DEPENDENCY_LINE = /^ "(.+)",$/gm;

export class LockError extends Failure {}

export interface Package {
  readonly block: string;
  readonly name: string;
  readonly version: string;
  /** A path package carries no `source` line. */
  readonly isPath: boolean;
  /** The raw `dependencies` entries: `name`, `name version`, or with a source. */
  readonly dependencies: readonly string[];
}

function field(block: string, pattern: RegExp, label: string): string {
  const value = pattern.exec(block)?.[1];
  if (value === undefined) throw new LockError(`lock package without a ${label}:\n${block}`);
  return value;
}

export function parsePackage(block: string): Package {
  const list = DEPENDENCIES.exec(block)?.[1];
  return {
    block,
    name: field(block, NAME, "name"),
    version: field(block, VERSION, "version"),
    isPath: !SOURCE.test(block),
    dependencies:
      list === undefined ? [] : [...list.matchAll(DEPENDENCY_LINE)].map(([, entry]) => entry ?? ""),
  };
}

export function parseLock(text: string): Package[] {
  return [...text.matchAll(PACKAGE)].map(([block]) => parsePackage(block));
}

/** Replace the first `version = "..."` line of a package block. */
export function withVersion(block: string, version: string): string {
  return block.replace(VERSION, () => `version = "${version}"`);
}

/** Replace the first occurrence of `from` in `text`, literally. */
export function replaceOnce(text: string, from: string, to: string): string {
  return text.replace(from, () => to);
}

/** The version of every path package in a lock, by name. */
export function pathVersions(text: string): Map<string, string> {
  const versions = new Map<string, string>();
  for (const { name, version, isPath } of parseLock(text)) {
    if (isPath) versions.set(name, version);
  }
  return versions;
}

/**
 * Rewrite the version of every path package in `text` that `versions` names,
 * leaving every other byte untouched.
 */
export function syncPathVersions(text: string, versions: ReadonlyMap<string, string>): string {
  return text.replace(PACKAGE, (block) => {
    const { name, isPath } = parsePackage(block);
    const version = versions.get(name);
    return isPath && version !== undefined ? withVersion(block, version) : block;
  });
}
