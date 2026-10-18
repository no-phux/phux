// The path-dependency graph, read from Cargo manifests. Routing needs only
// which local crates a build compiles, so this reads `path` edges (direct or
// through `workspace = true`) and leaves version resolution to Cargo.
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { ROOT } from "./repo.ts";
import { isTable, str, strings, table, type Table } from "./value.ts";

export type DependencyKind = "dependencies" | "dev-dependencies" | "build-dependencies";
export const ALL_KINDS: readonly DependencyKind[] = [
  "dependencies",
  "dev-dependencies",
  "build-dependencies",
];
// Cargo builds dev dependencies only for the package under test, never for
// the packages it depends on.
const TRANSITIVE_KINDS: readonly DependencyKind[] = ["dependencies", "build-dependencies"];

export interface Manifest {
  /** Repository-relative directory, e.g. `crates/phux-protocol`. */
  readonly dir: string;
  readonly document: Table;
  readonly name: string | undefined;
}

export function readManifest(dir: string, root = ROOT): Manifest {
  const document = table(Bun.TOML.parse(readFileSync(join(root, dir, "Cargo.toml"), "utf8")));
  return { dir, document, name: str(table(document["package"])["name"]) };
}

/** Members of the workspace whose root manifest is `dir` (default: the repo root). */
export function workspaceMembers(dir = ".", root = ROOT): Manifest[] {
  const workspace = table(readManifest(dir, root).document["workspace"]);
  const members = new Set<string>();
  for (const pattern of strings(workspace["members"])) {
    const glob = new Bun.Glob(join(pattern, "Cargo.toml"));
    for (const path of glob.scanSync({ cwd: join(root, dir), onlyFiles: true })) {
      members.add(join(dir, dirname(path)));
    }
  }
  return [...members].sort().map((member) => readManifest(member, root));
}

/** The nearest manifest at or above `dir` that declares `[workspace]`. */
function workspaceRoot(dir: string, root: string): Manifest | undefined {
  for (let current = dir; ; current = dirname(current)) {
    if (existsSync(join(root, current, "Cargo.toml"))) {
      const manifest = readManifest(current, root);
      if (isTable(manifest.document["workspace"])) return manifest;
    }
    if (current === "." || current === "/" || current === "") return undefined;
  }
}

/** The top-level table and every `[target.*]` table: where dependencies are declared. */
export function dependencyTables(document: Table): Table[] {
  return [document, ...Object.values(table(document["target"])).map(table)];
}

/** Repository-relative directories of `manifest`'s path dependencies of `kinds`. */
export function pathDependencies(
  manifest: Manifest,
  kinds: readonly DependencyKind[],
  root = ROOT,
): Set<string> {
  const found = new Set<string>();
  let inherited: Table | undefined;
  const inheritedFrom = (): Table => {
    inherited ??= table(
      table(workspaceRoot(manifest.dir, root)?.document["workspace"])["dependencies"],
    );
    return inherited;
  };
  for (const tables of dependencyTables(manifest.document)) {
    for (const kind of kinds) {
      for (const [name, spec] of Object.entries(table(tables[kind]))) {
        const path = dependencyPath(manifest, name, spec, inheritedFrom, root);
        if (path !== undefined) found.add(path);
      }
    }
  }
  return found;
}

function dependencyPath(
  manifest: Manifest,
  name: string,
  spec: unknown,
  inherited: () => Table,
  root: string,
): string | undefined {
  if (!isTable(spec)) return undefined;
  const direct = str(spec["path"]);
  if (direct !== undefined) return normalize(join(manifest.dir, direct), root);
  if (spec["workspace"] !== true) return undefined;
  const base = workspaceRoot(manifest.dir, root);
  const path = str(table(inherited()[name])["path"]);
  return base && path !== undefined ? normalize(join(base.dir, path), root) : undefined;
}

function normalize(path: string, root: string): string {
  return relative(root, resolve(root, path)) || ".";
}

/** A crate a lane builds; `dev` when the lane builds its tests or examples. */
export interface BuildRoot {
  readonly dir: string;
  readonly dev: boolean;
}

/**
 * Every local crate directory a build of `roots` compiles: the roots, their
 * path dependencies (dev ones too where `dev`), and from there the normal and
 * build dependencies.
 */
export function pathClosure(roots: readonly BuildRoot[], root = ROOT): Set<string> {
  const closure = new Set<string>();
  const pending: string[] = [];
  for (const { dir, dev } of roots) {
    closure.add(dir);
    const kinds = dev ? ALL_KINDS : TRANSITIVE_KINDS;
    pending.push(...pathDependencies(readManifest(dir, root), kinds, root));
  }
  for (let dir = pending.pop(); dir !== undefined; dir = pending.pop()) {
    if (closure.has(dir)) continue;
    closure.add(dir);
    pending.push(...pathDependencies(readManifest(dir, root), TRANSITIVE_KINDS, root));
  }
  return closure;
}
