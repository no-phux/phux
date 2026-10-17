/**
 * The third-party notice Phux.app ships: every crate the desktop executable
 * is built from (`cargo metadata` for the shipping target, dev-dependencies
 * excluded), its declared license, and the license and notice files its
 * source carries, with identical texts printed once.
 */
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join } from "node:path";

export interface Crate {
  name: string;
  version: string;
  /** The SPDX expression, or the license file a manifest names instead. */
  license: string;
  repository: string;
  /** The crate's source directory. */
  dir: string;
  /** Built from this repository, so covered by its LICENSE and NOTICE. */
  firstParty: boolean;
}

type Json = Record<string, unknown>;

function object(value: unknown, what: string): Json {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`cargo metadata: ${what} is not an object`);
  }
  return Object.fromEntries(Object.entries(value));
}

function array(value: unknown, what: string): unknown[] {
  if (!Array.isArray(value)) throw new Error(`cargo metadata: ${what} is not an array`);
  return value;
}

function text(value: unknown): string {
  return typeof value === "string" ? value : "";
}

/** Package ids reachable from the root over normal and build edges. */
function shippedIds(resolve: Json): Set<string> {
  const edges = new Map<string, string[]>();
  for (const raw of array(resolve.nodes, "resolve.nodes")) {
    const node = object(raw, "resolve node");
    const deps = array(node.deps, "node deps").flatMap((rawDep) => {
      const dep = object(rawDep, "dependency");
      const kinds = array(dep.dep_kinds, "dep_kinds").map((kind) => object(kind, "dep kind").kind);
      return kinds.some((kind) => kind !== "dev") ? [text(dep.pkg)] : [];
    });
    edges.set(text(node.id), deps);
  }
  const root = text(resolve.root);
  const seen = new Set<string>([root]);
  const queue = [root];
  for (let id = queue.pop(); id !== undefined; id = queue.pop()) {
    for (const next of edges.get(id) ?? []) {
      if (!seen.has(next)) {
        seen.add(next);
        queue.push(next);
      }
    }
  }
  return seen;
}

/**
 * The shipped crates in `metadata` (`cargo metadata --format-version 1`
 * output), sorted by name and version. Throws when any crate declares no
 * license: a dependency without one cannot be redistributed.
 */
export function shippedCrates(metadata: string): Crate[] {
  const document = object(JSON.parse(metadata), "metadata");
  const ids = shippedIds(object(document.resolve, "resolve"));
  const crates = array(document.packages, "packages")
    .map((raw) => object(raw, "package"))
    .filter((pkg) => ids.has(text(pkg.id)))
    .map((pkg): Crate => ({
      name: text(pkg.name),
      version: text(pkg.version),
      license: text(pkg.license) || text(pkg.license_file),
      repository: text(pkg.repository),
      dir: dirname(text(pkg.manifest_path)),
      firstParty: pkg.source === null && text(pkg.name).startsWith("phux"),
    }))
    .sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version));
  const unlicensed = crates.filter((crate) => !crate.license);
  if (unlicensed.length > 0) {
    const names = unlicensed.map((crate) => `${crate.name} ${crate.version}`).join(", ");
    throw new Error(`Crates with no declared license cannot ship: ${names}`);
  }
  return crates;
}

const NOTICE_FILE = /^(licen[cs]e|copying|notice|unlicense)([-._].*)?$/i;

/** The license and notice files at the top of a crate's source, by name. */
export function licenseTexts(dir: string): Map<string, string> {
  const texts = new Map<string, string>();
  if (!existsSync(dir)) return texts;
  for (const name of readdirSync(dir).sort()) {
    const path = join(dir, name);
    if (NOTICE_FILE.test(name) && statSync(path).isFile()) {
      texts.set(name, readFileSync(path, "utf8").trimEnd());
    }
  }
  return texts;
}

/** Render the notice; `read` returns a crate's license and notice texts. */
export function renderNotices(
  crates: Crate[],
  read: (crate: Crate) => Map<string, string> = (crate) => licenseTexts(crate.dir),
): string {
  const thirdParty = crates.filter((crate) => !crate.firstParty);
  const users = new Map<string, string[]>();
  for (const crate of thirdParty) {
    for (const [name, body] of read(crate)) {
      const label = `${crate.name} ${crate.version} (${name})`;
      users.set(body, [...(users.get(body) ?? []), label]);
    }
  }
  const lines = [
    "Phux Desktop third-party notices",
    "",
    "Phux Desktop is licensed under the Apache License, Version 2.0; see LICENSE",
    "and NOTICE beside this file. It is built from the following crates. Each is",
    "listed with the license its manifest declares; the license and notice texts",
    "the crates ship follow the list, each printed once; a crate that ships no",
    "license file is listed with its declared license only.",
    "",
    ...thirdParty.map((crate) =>
      [crate.name, crate.version, crate.license, crate.repository].filter(Boolean).join("  "),
    ),
  ];
  for (const [body, labels] of users) {
    lines.push("", "=".repeat(78), ...labels, "-".repeat(78), body);
  }
  return `${lines.join("\n")}\n`;
}
