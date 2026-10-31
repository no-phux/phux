/**
 * The third-party notice a desktop app ships: every crate its executable
 * is built from (`cargo metadata` for the shipping target, dev-dependencies
 * excluded), its declared license, and the license and notice files its
 * source carries, with identical texts printed once. A crate that ships no
 * license file gets the canonical text of a license it declares, from
 * `packaging/licenses/`; one with no such license fails the build.
 */
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { desktopRoot } from "./host.ts";

export interface Crate {
  name: string;
  version: string;
  /** The SPDX expression, or the license file a manifest names instead. */
  license: string;
  repository: string;
  authors: string[];
  /** The crate's source directory. */
  dir: string;
  /** Built from this repository, so covered by its LICENSE and NOTICE. */
  firstParty: boolean;
  /**
   * Reached only through build scripts or procedural macros: it runs while
   * the app is compiled and is not linked into the executable.
   */
  buildOnly: boolean;
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

interface Edge {
  pkg: string;
  /** The dependency kinds, `null` for a normal dependency. */
  kinds: unknown[];
}

function edgesOf(resolve: Json): Map<string, Edge[]> {
  const edges = new Map<string, Edge[]>();
  for (const raw of array(resolve.nodes, "resolve.nodes")) {
    const node = object(raw, "resolve node");
    const deps = array(node.deps, "node deps").map((rawDep): Edge => {
      const dep = object(rawDep, "dependency");
      const kinds = array(dep.dep_kinds, "dep_kinds").map((kind) => object(kind, "dep kind").kind);
      return { pkg: text(dep.pkg), kinds };
    });
    edges.set(text(node.id), deps);
  }
  return edges;
}

/** Package ids reachable from `root` over the edges `follow` accepts. */
function reachable(
  root: string,
  edges: Map<string, Edge[]>,
  follow: (edge: Edge) => boolean,
): Set<string> {
  const seen = new Set<string>([root]);
  const queue = [root];
  for (let id = queue.pop(); id !== undefined; id = queue.pop()) {
    for (const edge of edges.get(id) ?? []) {
      if (follow(edge) && !seen.has(edge.pkg)) {
        seen.add(edge.pkg);
        queue.push(edge.pkg);
      }
    }
  }
  return seen;
}

function isProcMacro(pkg: Json): boolean {
  return array(pkg.targets ?? [], "targets").some((raw) =>
    array(object(raw, "target").kind, "target kind").includes("proc-macro"),
  );
}

/**
 * The shipped crates in `metadata` (`cargo metadata --format-version 1`
 * output), sorted by name and version. Throws when any crate declares no
 * license: a dependency without one cannot be redistributed.
 */
export function shippedCrates(metadata: string): Crate[] {
  const document = object(JSON.parse(metadata), "metadata");
  const resolve = object(document.resolve, "resolve");
  const root = text(resolve.root);
  const edges = edgesOf(resolve);
  const packages = array(document.packages, "packages").map((raw) => object(raw, "package"));
  const procMacros = new Set(packages.filter(isProcMacro).map((pkg) => text(pkg.id)));
  const shipped = reachable(root, edges, (edge) => edge.kinds.some((kind) => kind !== "dev"));
  const linked = reachable(
    root,
    edges,
    (edge) => edge.kinds.includes(null) && !procMacros.has(edge.pkg),
  );
  const crates = packages
    .filter((pkg) => shipped.has(text(pkg.id)))
    .map((pkg): Crate => ({
      name: text(pkg.name),
      version: text(pkg.version),
      license: text(pkg.license) || text(pkg.license_file),
      repository: text(pkg.repository),
      authors: array(pkg.authors ?? [], "authors")
        .map(text)
        .filter(Boolean),
      dir: dirname(text(pkg.manifest_path)),
      firstParty: pkg.source === null && text(pkg.name).startsWith("phux"),
      buildOnly: !linked.has(text(pkg.id)),
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

/** Canonical license texts; `{holders}` in the MIT text names the copyright holders. */
export interface CanonicalLicenses {
  "Apache-2.0": string;
  MIT: string;
  "CC0-1.0": string;
  "MPL-2.0": string;
}

export function canonicalLicenses(
  dir = join(desktopRoot, "packaging/licenses"),
): CanonicalLicenses {
  const read = (id: string) =>
    readFileSync(join(dir, `${id}.txt`), "utf8")
      .replace(/^\n+/, "")
      .trimEnd();
  return {
    "Apache-2.0": read("Apache-2.0"),
    MIT: read("MIT"),
    "CC0-1.0": read("CC0-1.0"),
    "MPL-2.0": read("MPL-2.0"),
  };
}

/** One SPDX alternative per entry; `/` is the legacy crates.io spelling of OR. */
const ALTERNATIVE = /\s+OR\s+|\s*\/\s*/;

/**
 * The license with a canonical text that `expression` lets the app be
 * distributed under: Apache-2.0 when offered (its text names no copyright
 * holder), else MIT, CC0 or MPL. Compound terms (AND, WITH, parentheses) elect nothing.
 */
export function electedLicense(expression: string): keyof CanonicalLicenses | undefined {
  const options = expression.trim().split(ALTERNATIVE);
  if (options.some((option) => /[\s()]/.test(option))) return undefined;
  return ELECTABLE.find((id) => options.includes(id));
}

const ELECTABLE: (keyof CanonicalLicenses)[] = ["Apache-2.0", "MIT", "CC0-1.0", "MPL-2.0"];

function copyrightHolders(crate: Crate): string {
  const names = crate.authors.map((author) => author.replace(/\s*<[^>]*>/g, "").trim());
  return names.filter(Boolean).join(", ") || `The ${crate.name} authors`;
}

/**
 * The texts the notice prints for `crate`: the license and notice files it
 * ships, or, when it ships none, the canonical text of the license it
 * declares, with its authors as the MIT copyright holders. Empty when it
 * ships none and declares no license with a canonical text.
 */
export function noticeTexts(crate: Crate, canonical: CanonicalLicenses): Map<string, string> {
  const shipped = licenseTexts(crate.dir);
  if (shipped.size > 0) return shipped;
  const elected = electedLicense(crate.license);
  if (elected === undefined) return shipped;
  const body = canonical[elected].replace("{holders}", copyrightHolders(crate));
  return new Map([[`${elected}, canonical text`, body]]);
}

function listing(crate: Crate): string {
  return [crate.name, crate.version, crate.license, crate.repository].filter(Boolean).join("  ");
}

/**
 * Render the notice; `read` returns a crate's license and notice texts.
 * Throws when it returns none for a third-party crate.
 */
export function renderNotices(
  crates: Crate[],
  read: (crate: Crate) => Map<string, string> = (crate) => noticeTexts(crate, canonicalLicenses()),
  product = "Phux Desktop",
): string {
  const thirdParty = crates.filter((crate) => !crate.firstParty);
  const users = new Map<string, string[]>();
  const textless: string[] = [];
  for (const crate of thirdParty) {
    const texts = read(crate);
    if (texts.size === 0) textless.push(`${crate.name} ${crate.version} (${crate.license})`);
    for (const [name, body] of texts) {
      const label = `${crate.name} ${crate.version} (${name})`;
      users.set(body, [...(users.get(body) ?? []), label]);
    }
  }
  if (textless.length > 0) {
    throw new Error(
      "Crates ship no license text and declare no license with a canonical text in " +
        `packaging/licenses: ${textless.join(", ")}`,
    );
  }
  const lines = [
    `${product} third-party notices`,
    "",
    `${product} is licensed under the Apache License, Version 2.0; see LICENSE`,
    "and NOTICE beside this file. Its executable links the crates in the first",
    "list. The second list holds build-time tools (build scripts and procedural",
    "macros) that run while the app is compiled and are not part of the",
    "executable; they are listed for completeness. Each crate is listed with",
    "the license its manifest declares. The license and notice texts follow the",
    "lists, each printed once. For a crate that ships no license file, the",
    "canonical text of a license it offers is printed, naming its manifest",
    "authors as the copyright holders where that license requires one.",
    "",
    `Linked into ${product}:`,
    "",
    ...thirdParty.filter((crate) => !crate.buildOnly).map(listing),
    "",
    "Build-time tools, not part of the executable:",
    "",
    ...thirdParty.filter((crate) => crate.buildOnly).map(listing),
  ];
  for (const [body, labels] of users) {
    lines.push("", "=".repeat(78), ...labels, "-".repeat(78), body);
  }
  return `${lines.join("\n")}\n`;
}
