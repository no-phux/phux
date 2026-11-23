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
function workspaceRoot(
  dir: string,
  manifestAt: (dir: string) => Manifest | undefined,
): Manifest | undefined {
  for (let current = dir; ; current = dirname(current)) {
    const manifest = manifestAt(current);
    if (manifest && isTable(manifest.document["workspace"])) return manifest;
    if (current === "." || current === "/" || current === "") return undefined;
  }
}

/** The top-level table and every `[target.*]` table: where dependencies are declared. */
export function dependencyTables(document: Table): Table[] {
  return [document, ...Object.values(table(document["target"])).map(table)];
}

function normalize(path: string, root: string): string {
  return relative(root, resolve(root, path)) || ".";
}

/** A crate a lane builds; `dev` when the lane builds its tests or examples. */
export interface BuildRoot {
  readonly dir: string;
  readonly dev: boolean;
  readonly features?: readonly string[];
  readonly defaultFeatures?: boolean;
}

/** Optional dependency keys and dependency features enabled by a feature set. */
function activated(
  document: Table,
  requested: ReadonlySet<string>,
): {
  dependencies: Set<string>;
  forwarded: Map<string, Set<string>>;
} {
  const features = table(document["features"]);
  const seen = new Set<string>();
  const dependencies = new Set<string>();
  const forwarded = new Map<string, Set<string>>();
  const pending = [...requested];
  for (let feature = pending.pop(); feature !== undefined; feature = pending.pop()) {
    if (seen.has(feature)) continue;
    seen.add(feature);
    if (!(feature in features)) {
      // An optional dependency's implicit feature.
      dependencies.add(feature);
      continue;
    }
    for (const item of strings(features[feature])) {
      if (item.startsWith("dep:")) dependencies.add(item.slice(4));
      else if (item.includes("/")) {
        const [key = "", sub = ""] = item.split("/", 2);
        if (!key.endsWith("?")) {
          dependencies.add(key);
          // Cargo also enables a feature with the dependency key's name.
          pending.push(key);
        }
        const dependency = key.replace(/\?$/, "");
        const enabled = forwarded.get(dependency) ?? new Set<string>();
        enabled.add(sub);
        forwarded.set(dependency, enabled);
      } else pending.push(item);
    }
  }
  return { dependencies, forwarded };
}

interface DependencyRequest {
  readonly dir: string;
  readonly features: Set<string>;
}

/** Features requested on an edge, including inherited and forwarded features. */
function dependencyFeatures(
  spec: Table,
  base: Table,
  forwarded: ReadonlySet<string> | undefined,
): Set<string> {
  const defaults = spec["default-features"] ?? base["default-features"] ?? true;
  return new Set([
    ...strings(base["features"]),
    ...strings(spec["features"]),
    ...(defaults ? ["default"] : []),
    ...(forwarded ?? []),
  ]);
}

/** Active local dependency edges in manifest order, across every target. */
function activePathDependencies(
  manifest: Manifest,
  requested: ReadonlySet<string>,
  kinds: readonly DependencyKind[],
  workspace: Manifest | undefined,
  root: string,
): DependencyRequest[] {
  const inherited = table(table(workspace?.document["workspace"])["dependencies"]);
  const { dependencies, forwarded } = activated(manifest.document, requested);
  const found: DependencyRequest[] = [];
  for (const tables of dependencyTables(manifest.document)) {
    for (const kind of kinds) {
      for (const [key, value] of Object.entries(table(tables[kind]))) {
        const spec = table(value);
        if (spec["optional"] === true && !dependencies.has(key)) continue;
        const base = spec["workspace"] === true ? table(inherited[key]) : {};
        const direct = str(spec["path"]);
        const path = direct ?? str(base["path"]);
        const from = direct === undefined ? workspace?.dir : manifest.dir;
        if (path === undefined || from === undefined) continue;
        found.push({
          dir: normalize(join(from, path), root),
          features: dependencyFeatures(spec, base, forwarded.get(key)),
        });
      }
    }
  }
  return found;
}

/**
 * Every local crate directory a build compiles. Optional edges count only
 * when enabled; dependency features are unified to a fixed point. Dev edges
 * count only at roots, target-specific edges on every platform (fail closed).
 */
export function pathClosure(roots: readonly BuildRoot[], root = ROOT): Set<string> {
  const manifests = new Map<string, Manifest | undefined>();
  const manifestAt = (dir: string): Manifest | undefined => {
    if (!manifests.has(dir)) {
      const manifest = existsSync(join(root, dir, "Cargo.toml"))
        ? readManifest(dir, root)
        : undefined;
      manifests.set(dir, manifest);
    }
    return manifests.get(dir);
  };
  const devRoots = new Set(roots.filter(({ dev }) => dev).map(({ dir }) => dir));
  const enabled = new Map<string, Set<string>>();
  const pending = roots.map(({ dir, features = [], defaultFeatures = true }) => ({
    dir,
    features: new Set([...features, ...(defaultFeatures ? ["default"] : [])]),
  }));
  for (let next = pending.pop(); next !== undefined; next = pending.pop()) {
    const { dir, features } = next;
    const previous = enabled.get(dir);
    if (previous && [...features].every((feature) => previous.has(feature))) continue;
    const requested = new Set([...(previous ?? []), ...features]);
    enabled.set(dir, requested);
    // Keep the existing fail-closed error for a declared path without a manifest.
    const manifest = manifestAt(dir) ?? readManifest(dir, root);
    const kinds = devRoots.has(dir) ? ALL_KINDS : TRANSITIVE_KINDS;
    pending.push(
      ...activePathDependencies(manifest, requested, kinds, workspaceRoot(dir, manifestAt), root),
    );
  }
  return new Set(enabled.keys());
}
