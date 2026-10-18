// Cargo.lock text, edited in place: packages are matched as text blocks and
// single lines rewritten, so every other byte survives.
import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { Failure } from "../lib/command.ts";
import { status } from "../lib/output.ts";

/** One `[[package]]` block, through to the next block or the end of the file. */
const PACKAGE = /^\[\[package\]\]\n.*?(?=^\[\[package\]\]|(?![\s\S]))/gms;
const VERSION = /^version = "([^"]+)"$/m;
export const DEPENDENCIES = /^dependencies = \[\n(.*?)^\]/ms;

export interface Package {
  readonly block: string;
  readonly name: string;
  readonly version: string;
  /** A path package carries no `source` line. */
  readonly isPath: boolean;
  /** The raw `dependencies` entries: `name`, `name version`, or with a source. */
  readonly dependencies: readonly string[];
}

function field(block: string, pattern: RegExp): string {
  const value = pattern.exec(block)?.[1];
  if (value === undefined) throw new Failure(`malformed lock package:\n${block}`);
  return value;
}

function parsePackage(block: string): Package {
  const list = DEPENDENCIES.exec(block)?.[1] ?? "";
  return {
    block,
    name: field(block, /^name = "([^"]+)"$/m),
    version: field(block, VERSION),
    isPath: !/^source = /m.test(block),
    dependencies: [...list.matchAll(/^ "(.+)",$/gm)].map((match) => match[1] ?? ""),
  };
}

export function parseLock(text: string): Package[] {
  return [...text.matchAll(PACKAGE)].map(([block]) => parsePackage(block));
}

export function withVersion(block: string, version: string): string {
  return block.replace(VERSION, () => `version = "${version}"`);
}

/** Replace the first occurrence of `from` in `text`, literally. */
export function replaceOnce(text: string, from: string, to: string): string {
  return text.replace(from, () => to);
}

/** Rewrite the version of every path package `versions` names. */
export function syncPathVersions(text: string, versions: ReadonlyMap<string, string>): string {
  return text.replace(PACKAGE, (block) => {
    const { name, isPath } = parsePackage(block);
    const version = versions.get(name);
    return isPath && version !== undefined ? withVersion(block, version) : block;
  });
}

/**
 * Align each target lock's path crate versions with the root lock's (with
 * `check`, only compare). A standalone workspace such as the desktop host
 * cannot `cargo update` on a bare checkout, yet a release bump changes its
 * root path crates. Returns whether every target was already in step.
 */
export function syncLocks(
  rootLock: string,
  targets: readonly string[],
  check: boolean,
  cwd = process.cwd(),
): boolean {
  const paths = parseLock(readFileSync(resolve(cwd, rootLock), "utf8")).filter((pkg) => pkg.isPath);
  const versions = new Map(paths.map((pkg) => [pkg.name, pkg.version]));
  let inStep = true;
  for (const target of targets) {
    const path = resolve(cwd, target);
    const before = readFileSync(path, "utf8");
    const after = syncPathVersions(before, versions);
    if (after === before) continue;
    if (check) {
      status("error", `${target}: path crate versions differ from ${rootLock}`);
      inStep = false;
    } else {
      writeFileSync(path, after);
      status("ok", `updated ${target}`);
    }
  }
  return inStep;
}
