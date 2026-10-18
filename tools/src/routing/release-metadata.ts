// Release-please version bumps are not compile inputs.
//
// A root release PR rewrites version fields, the changelog, and skill version
// lines; treating those as binary inputs rebuilt FFI, Zig and the CLI. Fail
// closed: any other hunk, path, or unreadable side keeps the normal routes.
import { deepEqual, isTable, str, type Table } from "../lib/value.ts";

const SEMVER = /^[0-9]+\.[0-9]+\.[0-9]+$/;
// Desktop shipped alphas before stable; a diff across the graduation counts.
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

/** Remove and return `version` from a parsed document's `path` table. */
function takeVersion(document: unknown, path: string[]): string | undefined {
  let current = document;
  for (const key of path) current = isTable(current) ? current[key] : undefined;
  if (!isTable(current)) return undefined;
  const version = str(current["version"]);
  delete current["version"];
  return version;
}

/** Both sides are equal apart from a valid `version` under `path`. */
function versionOnly(format: "toml" | "json", pattern: RegExp, path: string[] = []): Check {
  return (before, after) => {
    const [old, now] = [parse(before, format), parse(after, format)];
    const versions = [takeVersion(old, path), takeVersion(now, path)];
    return versions.every((version) => pattern.test(version ?? "")) && deepEqual(old, now);
  };
}

export const workspaceManifestOnly = versionOnly("toml", SEMVER, ["workspace", "package"]);
const desktopPackageOnly = versionOnly("json", DESKTOP_VERSION);

/** Path packages by name and registry packages in order, or undefined if malformed. */
function splitLock(text: string): { path: Map<string, Table>; registry: Table[] } | undefined {
  const document = parse(text, "toml");
  const packages = isTable(document) ? (document["package"] ?? []) : undefined;
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
  return [...old.path].every(([name, { version: _old, ...previous }]) => {
    const { version, ...current } = now.path.get(name) ?? {};
    return now.path.has(name) && deepEqual(previous, current) && SEMVER.test(str(version) ?? "");
  });
};

export const manifestOnly: Check = (before, after) => {
  const [old, now] = [parse(before, "json"), parse(after, "json")];
  if (!isTable(old) || !isTable(now)) return false;
  const keys = Object.keys(old);
  if (!deepEqual(keys.toSorted(), Object.keys(now).toSorted())) return false;
  return keys.every((path) =>
    [old[path], now[path]].every((value) => {
      const version = str(value) ?? "";
      const pattern = path === "clients/desktop" && version !== "0.0.0" ? DESKTOP_VERSION : SEMVER;
      return pattern.test(version);
    }),
  );
};

export const skillOnly: Check = (before, after) => {
  const [old, now] = [before.split(/\r?\n/), after.split(/\r?\n/)];
  return (
    old.length === now.length &&
    old.every((line, index) => {
      const next = now[index] ?? "";
      return line === next || (SKILL_VERSION.test(line) && SKILL_VERSION.test(next));
    })
  );
};

const CHECKS: Record<string, Check> = {
  "Cargo.toml": workspaceManifestOnly,
  "Cargo.lock": workspaceLockOnly,
  "clients/phux-web/Cargo.lock": workspaceLockOnly,
  "clients/desktop/package.json": desktopPackageOnly,
  ".release-please-manifest.json": manifestOnly,
  ".agents/skills/using-phux/SKILL.md": skillOnly,
  ".agents/skills/using-phux-mcp/SKILL.md": skillOnly,
};

export type ReadPair = (
  path: string,
) => readonly [string | undefined, string | undefined] | undefined;

/** True when every path is release metadata and each side differs only in versions. */
export function releaseMetadataOnly(paths: readonly string[], readPair: ReadPair): boolean {
  return (
    paths.length > 0 &&
    paths.every((path) => {
      const check = /(^|\/)CHANGELOG\.md$/.test(path) ? () => true : CHECKS[path];
      const [before, after] = (check && readPair(path)) ?? [];
      return (
        check !== undefined && before !== undefined && after !== undefined && check(before, after)
      );
    })
  );
}
