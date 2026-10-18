// Whether the desktop host lock (clients/desktop/native, its own workspace)
// still satisfies a `--locked` build of the root path crates it shares. This
// reads only the two locks and the root manifests, so it runs on a bare
// checkout. For each shared path crate: every unconditional (non-dev,
// non-optional) dependency must be an edge in the host lock, and each
// dependency both locks select must stay in one semver-compatible range.
// Feature-gated and dev edges legitimately differ and are not checked.
import { dependencyTables, readManifest, workspaceMembers } from "../lib/cargo.ts";
import { isTable, str, table, type Table } from "../lib/value.ts";
import { DEPENDENCIES, parseLock, replaceOnce, withVersion, type Package } from "./lock.ts";

/** Rewrites a crate's host lock block. */
type Fix = (pkg: Package) => string;

export interface Finding {
  readonly crate: string;
  readonly message: string;
  /** Absent when only re-resolving the host lock can repair it. */
  readonly fix: Fix | undefined;
}

type Versions = ReadonlyMap<string, readonly string[]>;

function versionsByName(packages: readonly Package[]): Versions {
  const versions = new Map<string, string[]>();
  for (const { name, version } of packages)
    versions.set(name, [...(versions.get(name) ?? []), version]);
  return versions;
}

/** The semver compatibility range of a version: major, or 0.minor. */
function compat(version: string): string {
  const [major = "", minor = "0"] = version.split(".");
  return major === "0" ? `0.${minor}` : major;
}

/** A package's dependency entries by name, with the version each selects when unambiguous. */
function selections(
  pkg: Package,
  versions: Versions,
): Map<string, { entry: string; version?: string }> {
  return new Map(
    pkg.dependencies.map((entry) => {
      const space = entry.indexOf(" ");
      const name = space === -1 ? entry : entry.slice(0, space);
      const candidates = versions.get(name) ?? [];
      const unique = candidates.length === 1 ? candidates[0] : undefined;
      const version = space === -1 ? unique : entry.slice(space + 1);
      return [name, version === undefined ? { entry } : { entry, version }];
    }),
  );
}

/** The package a manifest entry names, or undefined when it is optional. */
export function requiredPackage(key: string, spec: unknown, inherited: Table): string | undefined {
  if (!isTable(spec)) return key;
  if (spec["optional"] === true) return undefined;
  const resolved = spec["workspace"] === true ? (inherited[key] ?? {}) : spec;
  return isTable(resolved) ? (str(resolved["package"]) ?? key) : key;
}

/** Each root workspace crate's unconditional (non-dev, non-optional) packages. */
export function requiredDependencies(root: string): Map<string, Set<string>> {
  const inherited = table(table(readManifest(".", root).document["workspace"])["dependencies"]);
  const required = new Map<string, Set<string>>();
  for (const manifest of workspaceMembers(".", root)) {
    if (manifest.name === undefined) continue;
    const names = new Set<string>();
    for (const tables of dependencyTables(manifest.document)) {
      for (const kind of ["dependencies", "build-dependencies"]) {
        for (const [key, spec] of Object.entries(table(tables[kind]))) {
          const name = requiredPackage(key, spec, inherited);
          if (name !== undefined) names.add(name);
        }
      }
    }
    required.set(manifest.name, names);
  }
  return required;
}

/** Add `dependency` to a crate's dependency list, sorted as Cargo writes it. */
function addEdge(dependency: string): Fix {
  return (pkg) => {
    const body = [...pkg.dependencies, dependency].toSorted().map((entry) => ` "${entry}",\n`);
    return pkg.block.replace(DEPENDENCIES, () => `dependencies = [\n${body.join("")}]`);
  };
}

interface Locks {
  readonly root: Versions;
  readonly host: Versions;
}

function compareCrate(root: Package, host: Package, locks: Locks, required: ReadonlySet<string>) {
  const findings: Finding[] = [];
  const found = (message: string, fix: Fix | undefined) =>
    findings.push({ crate: root.name, message, fix });
  if (root.version !== host.version) {
    found(`path crate ${root.name} is ${host.version}; root workspace is ${root.version}`, (pkg) =>
      withVersion(pkg.block, root.version),
    );
  }
  const rootSelected = selections(root, locks.root);
  const hostSelected = selections(host, locks.host);
  for (const dependency of [...required].filter((name) => !hostSelected.has(name)).toSorted()) {
    // A name the host lock carries once is spelled bare, and adding the edge
    // respells nothing else; anything more needs real resolution.
    const fixable = locks.host.get(dependency)?.length === 1 && DEPENDENCIES.test(host.block);
    found(
      `${root.name} depends on ${dependency}, but the host lock records no such edge ` +
        "(cargo --locked would have to add it)",
      fixable ? addEdge(dependency) : undefined,
    );
  }
  for (const dependency of [...rootSelected.keys()].toSorted()) {
    const rootVersion = rootSelected.get(dependency)?.version;
    const selected = hostSelected.get(dependency);
    if (rootVersion === undefined || selected?.version === undefined) continue;
    if (compat(rootVersion) === compat(selected.version)) continue;
    const replacement = ` "${dependency} ${rootVersion}",`;
    found(
      `${root.name} selects ${dependency} ${selected.version}, but the root lock selects ${rootVersion}`,
      locks.host.get(dependency)?.includes(rootVersion)
        ? (pkg) => replaceOnce(pkg.block, ` "${selected.entry}",`, replacement)
        : undefined,
    );
  }
  return findings;
}

export function compare(
  rootText: string,
  hostText: string,
  /** Crate name to the packages it depends on unconditionally. */
  required: ReadonlyMap<string, ReadonlySet<string>> = new Map(),
): Finding[] {
  const rootPackages = parseLock(rootText);
  const hostPackages = parseLock(hostText);
  const hostPath = new Map(hostPackages.filter((pkg) => pkg.isPath).map((pkg) => [pkg.name, pkg]));
  const locks = { root: versionsByName(rootPackages), host: versionsByName(hostPackages) };
  return rootPackages.flatMap((crate) => {
    const host = crate.isPath ? hostPath.get(crate.name) : undefined;
    return host ? compareCrate(crate, host, locks, required.get(crate.name) ?? new Set()) : [];
  });
}

/** The host lock text with every repairable finding applied. */
export function applyFixes(hostText: string, findings: readonly Finding[]): string {
  let text = hostText;
  for (const { crate, fix } of findings) {
    const pkg = parseLock(text).find((candidate) => candidate.isPath && candidate.name === crate);
    if (fix !== undefined && pkg !== undefined) text = replaceOnce(text, pkg.block, fix(pkg));
  }
  return text;
}
