// Release-please version bumps are not compile inputs.
//
// A root release PR rewrites version fields, the changelog, and skill version
// lines. Treating those bytes as shared binary inputs rebuilt FFI, the Zig
// test graph, and the CLI (PR 853: 18 minutes, cancelled still linking). Fail
// closed: any other hunk, path, or unreadable side keeps the normal routes.
import { deepEqual, isTable, str, type Table } from "../lib/value.ts";

const SEMVER = /^[0-9]+\.[0-9]+\.[0-9]+$/;
// The desktop train shipped alphas and graduated to stable. Accept both shapes
// so a release-metadata diff across the graduation stays compile-free.
const DESKTOP_VERSION = /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-alpha\.[1-9]\d*)?$/;
const SKILL_VERSION =
  /^(\s*)version:\s*"([0-9]+\.[0-9]+\.[0-9]+)"\s*#\s*x-release-please-version\s*$/;

type Check = (before: string, after: string) => boolean;

function parse(text: string, format: "toml" | "json"): unknown {
  try {
    return format === "toml" ? Bun.TOML.parse(text) : JSON.parse(text);
  } catch {
    return undefined;
  }
}

/** Remove and return `key` from a parsed document's `path` table. */
function takeVersion(document: unknown, ...path: string[]): string | undefined {
  let current = document;
  for (const key of path) current = isTable(current) ? current[key] : undefined;
  if (!isTable(current)) return undefined;
  const version = str(current["version"]);
  delete current["version"];
  return version;
}

export const workspaceManifestOnly: Check = (before, after) => {
  const [old, now] = [parse(before, "toml"), parse(after, "toml")];
  const [oldVersion, newVersion] = [
    takeVersion(old, "workspace", "package"),
    takeVersion(now, "workspace", "package"),
  ];
  return SEMVER.test(oldVersion ?? "") && SEMVER.test(newVersion ?? "") && deepEqual(old, now);
};

/** Path packages by name and registry packages in order, or undefined if malformed. */
function splitLock(text: string): { path: Map<string, Table>; registry: Table[] } | undefined {
  const document = parse(text, "toml");
  if (!isTable(document)) return undefined;
  const packages = document["package"] ?? [];
  if (!Array.isArray(packages)) return undefined;
  const path = new Map<string, Table>();
  const registry: Table[] = [];
  for (const entry of packages) {
    const name = isTable(entry) ? str(entry["name"]) : undefined;
    if (!isTable(entry) || name === undefined) return undefined;
    if ("source" in entry) registry.push(entry);
    else if (path.has(name)) return undefined;
    else path.set(name, entry);
  }
  return { path, registry };
}

export const workspaceLockOnly: Check = (before, after) => {
  const [old, now] = [splitLock(before), splitLock(after)];
  if (!old || !now || !deepEqual(old.registry, now.registry) || old.path.size !== now.path.size)
    return false;
  for (const [name, previous] of old.path) {
    const current = now.path.get(name);
    if (current === undefined) return false;
    const { version: _old, ...previousRest } = previous;
    const { version, ...currentRest } = current;
    if (!deepEqual(previousRest, currentRest) || !SEMVER.test(str(version) ?? "")) return false;
  }
  return true;
};

export const manifestOnly: Check = (before, after) => {
  const [old, now] = [parse(before, "json"), parse(after, "json")];
  if (!isTable(old) || !isTable(now)) return false;
  const keys = Object.keys(old);
  if (!deepEqual(keys.toSorted(), Object.keys(now).toSorted())) return false;
  return keys.every((path) =>
    [old[path], now[path]].every((value) => {
      const version = str(value);
      if (version === undefined) return false;
      const pattern = path === "clients/desktop" && version !== "0.0.0" ? DESKTOP_VERSION : SEMVER;
      return pattern.test(version);
    }),
  );
};

export const desktopPackageOnly: Check = (before, after) => {
  const [old, now] = [parse(before, "json"), parse(after, "json")];
  if (!isTable(old) || !isTable(now)) return false;
  const [oldVersion, newVersion] = [takeVersion(old), takeVersion(now)];
  return (
    DESKTOP_VERSION.test(oldVersion ?? "") &&
    DESKTOP_VERSION.test(newVersion ?? "") &&
    deepEqual(old, now)
  );
};

export const skillOnly: Check = (before, after) => {
  const [old, now] = [before.split(/\r?\n/), after.split(/\r?\n/)];
  if (old.length !== now.length) return false;
  return old.every((line, index) => {
    const next = now[index] ?? "";
    return line === next || (SKILL_VERSION.test(line) && SKILL_VERSION.test(next));
  });
};

const changelogOnly: Check = () => true;

const EXACT: Record<string, Check> = {
  "Cargo.toml": workspaceManifestOnly,
  "Cargo.lock": workspaceLockOnly,
  "clients/phux-web/Cargo.lock": workspaceLockOnly,
  "clients/desktop/package.json": desktopPackageOnly,
  ".release-please-manifest.json": manifestOnly,
  ".agents/skills/using-phux/SKILL.md": skillOnly,
  ".agents/skills/using-phux-mcp/SKILL.md": skillOnly,
};

function checkFor(path: string): Check | undefined {
  if (path === "CHANGELOG.md" || path.endsWith("/CHANGELOG.md")) return changelogOnly;
  return EXACT[path];
}

export type ReadPair = (
  path: string,
) => readonly [string | undefined, string | undefined] | undefined;

/** True when every path is release metadata and each side differs only in versions. */
export function releaseMetadataOnly(paths: readonly string[], readPair: ReadPair): boolean {
  if (paths.length === 0) return false;
  return paths.every((path) => {
    const check = checkFor(path);
    const pair = check && readPair(path);
    return (
      check !== undefined &&
      pair?.[0] !== undefined &&
      pair[1] !== undefined &&
      check(pair[0], pair[1])
    );
  });
}
