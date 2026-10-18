// Fail when the desktop host lock can no longer satisfy a `--locked` build.
//
// `clients/desktop/native` is its own Cargo workspace with its own lockfile,
// kept separate so the GPUI and GPUI Kit graph does not enter every other phux
// consumer. Its path dependencies are the root phux crates, so when a root
// crate changes a registry dependency's semver requirement the host lock must
// be re-resolved. Nothing does that automatically, and `just desktop-app` then
// aborts before it compiles: cargo refuses to update the host lock because
// `--locked` was passed.
//
// This is a STATIC check. It reads the root and host lockfiles and the root
// crates' manifests only, so it runs on a bare checkout with no GPUI snapshot,
// no network and no Rust (CI runs it that way). For every path crate present
// in both locks it checks two things:
//
// - Every non-optional normal or build dependency the crate's Cargo.toml
//   declares (for any target; a lockfile is target-independent) is an edge in
//   the host lock. Such a dependency is unconditional, so a missing edge means
//   `--locked` would have to add it: a root crate gained a dependency the host
//   lock never resolved.
// - The version selected for each dependency name both locks record stays
//   within one semver compatibility boundary (major, or major.minor while
//   major is 0); otherwise the host lock no longer satisfies the root crate's
//   requirement. Patch drift within a boundary (thiserror 2.0.20 versus
//   2.0.21) stays valid under `--locked` and is not reported.
//
// Dev-dependencies and optional dependencies are not required: the host builds
// the root crates as path dependencies with its own feature set, so the root
// lock's feature union and dev graph legitimately exceed the host's. A new
// dependency behind a feature the host enables is therefore invisible here and
// surfaces at `just desktop-lock-refresh` or the `--locked` host build.
//
// `--fix` rewrites repairable drift in the host lock: a missing edge is added,
// and a stale edge rewritten to the root's selected version, when the host
// lock already carries that package; a path crate version is aligned with the
// root workspace. Anything left is not repairable without resolving against
// the bootstrapped GPUI snapshot (`just desktop-source`).
import { writeFileSync } from "node:fs";
import { dependencyTables, readManifest, workspaceMembers } from "../lib/cargo.ts";
import { hint, status } from "../lib/output.ts";
import { isTable, str, table, type Table } from "../lib/value.ts";
import {
  DEPENDENCIES,
  LockError,
  parseLock,
  replaceOnce,
  withVersion,
  type Package,
} from "./lock.ts";

type Fix = (text: string) => string;

export interface Finding {
  readonly message: string;
  /** Rewrites the host lock text; absent when only a refresh can repair it. */
  readonly fix: Fix | undefined;
}

/** Crate name to the packages it depends on unconditionally. */
export type Required = ReadonlyMap<string, ReadonlySet<string>>;

type Versions = ReadonlyMap<string, readonly string[]>;

function versionsByName(packages: readonly Package[]): Versions {
  const versions = new Map<string, string[]>();
  for (const { name, version } of packages) {
    const known = versions.get(name);
    if (known === undefined) versions.set(name, [version]);
    else known.push(version);
  }
  return versions;
}

/** The semver compatibility boundary of a resolved version. */
export function compat(version: string): string {
  const core = version.split("+")[0] ?? "";
  const dot = core.indexOf(".");
  const major = dot === -1 ? core : core.slice(0, dot);
  const rest = dot === -1 ? "" : core.slice(dot + 1);
  const minor = rest === "" ? "0" : (rest.split(".")[0] ?? "");
  return major === "0" ? `${major}.${minor}` : major;
}

interface Selection {
  readonly entry: string;
  /** Undefined when the bare name is ambiguous in its lock. */
  readonly version: string | undefined;
}

/** A package's dependency entries by name, each with the version it selects. */
function selections(pkg: Package, versions: Versions): Map<string, Selection> {
  const resolved = new Map<string, Selection>();
  for (const entry of pkg.dependencies) {
    const space = entry.indexOf(" ");
    if (space !== -1) {
      resolved.set(entry.slice(0, space), { entry, version: entry.slice(space + 1) });
      continue;
    }
    const candidates = versions.get(entry) ?? [];
    resolved.set(entry, { entry, version: candidates.length === 1 ? candidates[0] : undefined });
  }
  return resolved;
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

function blockFor(text: string, crate: string): Package {
  const found = parseLock(text).find((pkg) => pkg.isPath && pkg.name === crate);
  if (found === undefined) throw new LockError(`${crate} not found in host lock`);
  return found;
}

function versionFix(crate: string, version: string): Fix {
  return (text) => {
    const { block } = blockFor(text, crate);
    return replaceOnce(text, block, withVersion(block, version));
  };
}

function edgeFix(crate: string, hostEntry: string, dependency: string, version: string): Fix {
  return (text) => {
    const { block } = blockFor(text, crate);
    const edge = replaceOnce(block, ` "${hostEntry}",`, ` "${dependency} ${version}",`);
    return replaceOnce(text, block, edge);
  };
}

/** Add an edge to a package the host lock carries once, spelled as cargo does. */
function missingEdgeFix(crate: string, dependency: string, versions: Versions): Fix | undefined {
  // A name the lock carries once is written bare; adding an edge leaves the
  // package set, and so every other entry's spelling, unchanged. A name it
  // lacks or carries at several versions needs real resolution.
  if (versions.get(dependency)?.length !== 1) return undefined;
  return (text) => {
    const pkg = blockFor(text, crate);
    const list = DEPENDENCIES.exec(pkg.block)?.[1];
    if (list === undefined) throw new LockError(`${crate} has no dependency list in the host lock`);
    // Code-unit order, as Cargo (and Python's sorted) spell it.
    const entries = [...pkg.dependencies, dependency].toSorted();
    const body = entries.map((entry) => ` "${entry}",\n`).join("");
    return replaceOnce(text, pkg.block, replaceOnce(pkg.block, list, body));
  };
}

function compareCrate(
  crate: Package,
  hostCrate: Package,
  locks: { root: Versions; host: Versions; hostIndex: ReadonlySet<string> },
  required: ReadonlySet<string>,
): Finding[] {
  const findings: Finding[] = [];
  if (crate.version !== hostCrate.version) {
    findings.push({
      message: `path crate ${crate.name} is ${hostCrate.version}; root workspace is ${crate.version}`,
      fix: versionFix(crate.name, crate.version),
    });
  }
  const rootSelected = selections(crate, locks.root);
  const hostSelected = selections(hostCrate, locks.host);
  for (const dependency of [...required].filter((name) => !hostSelected.has(name)).toSorted()) {
    findings.push({
      message:
        `${crate.name} depends on ${dependency}, but the host lock records no ` +
        "such edge (cargo --locked would have to add it)",
      fix: missingEdgeFix(crate.name, dependency, locks.host),
    });
  }
  const shared = [...rootSelected.keys()].filter((name) => hostSelected.has(name)).toSorted();
  for (const dependency of shared) {
    const rootVersion = rootSelected.get(dependency)?.version;
    const host = hostSelected.get(dependency);
    if (rootVersion === undefined || host?.version === undefined) continue;
    if (compat(rootVersion) === compat(host.version)) continue;
    const fixable = locks.hostIndex.has(`${dependency} ${rootVersion}`);
    findings.push({
      message:
        `${crate.name} selects ${dependency} ${host.version}, but the root lock ` +
        `selects ${rootVersion}`,
      fix: fixable ? edgeFix(crate.name, host.entry, dependency, rootVersion) : undefined,
    });
  }
  return findings;
}

export function compare(
  rootText: string,
  hostText: string,
  required: Required = new Map(),
): Finding[] {
  const rootPackages = parseLock(rootText);
  const hostPackages = parseLock(hostText);
  const hostPath = new Map(hostPackages.filter((pkg) => pkg.isPath).map((pkg) => [pkg.name, pkg]));
  const locks = {
    root: versionsByName(rootPackages),
    host: versionsByName(hostPackages),
    hostIndex: new Set(hostPackages.map(({ name, version }) => `${name} ${version}`)),
  };
  return rootPackages.flatMap((crate) => {
    const hostCrate = crate.isPath ? hostPath.get(crate.name) : undefined;
    if (hostCrate === undefined) return [];
    return compareCrate(crate, hostCrate, locks, required.get(crate.name) ?? new Set());
  });
}

/** Apply every repairable finding; write and report only when something changed. */
export function repairHostLock(
  rootText: string,
  hostText: string,
  required: Required,
  hostLock: string,
): string {
  let text = hostText;
  let changed = 0;
  for (const { fix } of compare(rootText, hostText, required)) {
    if (fix === undefined) continue;
    text = fix(text);
    changed += 1;
  }
  if (changed > 0) {
    writeFileSync(hostLock, text);
    status("ok", `fixed ${changed} entr${changed === 1 ? "y" : "ies"} in ${hostLock}`);
  }
  return text;
}

export function reportFindings(
  findings: readonly Finding[],
  hostLock: string,
  rootLock: string,
): number {
  if (findings.length === 0) {
    status("ok", `${hostLock} is in step with ${rootLock}`);
    return 0;
  }
  for (const { message, fix } of findings) {
    status("error", `${hostLock}: ${message}`);
    hint(
      fix === undefined
        ? "refresh the host lock: just desktop-lock-refresh"
        : "repair with: just desktop-lock-fix",
    );
  }
  return 1;
}
