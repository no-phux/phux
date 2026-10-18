// Build and publish the channel manifests (ADR-0176).
//
// `stable` derives channels/stable.json from the newest published releases.
// `next` publishes one product's build onto the moving `next` prerelease and
// records it in channels/next.json. `next-sha` prints the commit next.json
// names for a product. `fixtures` renders the golden manifests every client
// tests.
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { git, GitError } from "../lib/git.ts";
import { gh, GhError, ghLines } from "../lib/github.ts";
import { ROOT } from "../lib/repo.ts";
import { isTable, list, str, table, type Table } from "../lib/value.ts";
import { compareVersions, parseTag, type Version } from "./publish-plan.ts";

const REPO = "no-phux/phux";
const DOWNLOAD = `https://github.com/${REPO}/releases/download`;
export const CHANNELS = ["stable", "next"] as const;
export type Channel = (typeof CHANNELS)[number];
export const PRODUCTS = ["phux", "desktop"] as const;
export type Product = (typeof PRODUCTS)[number];
export const TARGETS: Record<Product, readonly string[]> = {
  phux: ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"],
  desktop: ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"],
};
export const DESKTOP_ARCHIVES: Record<string, string> = {
  "aarch64-apple-darwin": "macos-arm64.zip",
  "x86_64-unknown-linux-gnu": "linux-x64.tar.gz",
  "aarch64-unknown-linux-gnu": "linux-arm64.tar.gz",
};
const FIXTURES = "scripts/fixtures/release-manifest";

// The POSIX installers parse the rendered file with awk, so values stay in
// this alphabet and the document has no arrays.
const SAFE = /^[A-Za-z0-9._:/+-]*$/;
const KEY = /^[A-Za-z0-9_-]+$/;
const VERSION = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/;
const SHA = /^[0-9a-f]{40}$/;
const DIGEST = /^[0-9a-f]{64}$/;
const PROTOCOL = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/;
const PROTOCOL_SOURCE =
  /pub const PROTOCOL_VERSION: Version = Version \{\s*major: (\d+),\s*minor: (\d+),/;

export class ManifestError extends Error {}

export function isProduct(value: string): value is Product {
  return (PRODUCTS as readonly string[]).includes(value);
}

function require(pattern: RegExp, value: unknown, what: string): string {
  if (typeof value !== "string" || !pattern.test(value)) {
    throw new ManifestError(`${what} is not valid: ${JSON.stringify(value) ?? String(value)}`);
  }
  return value;
}

function releaseOf(product: Product, channel: Channel, version: string): string {
  if (channel === "next") return "next";
  return product === "phux" ? `v${version}` : `desktop-v${version}`;
}

/** The naming rule every client derives on its own (ADR-0176 decision 1). */
export function assetName(
  product: Product,
  channel: Channel,
  version: string | undefined,
  sha: string | undefined,
  target: string,
): string {
  const next = `next.${sha}`;
  if (product === "phux") {
    return `phux-${channel === "next" ? next : `v${version}`}-${target}.tar.gz`;
  }
  const archive = DESKTOP_ARCHIVES[target];
  if (archive === undefined) throw new ManifestError(`no desktop archive for ${target}`);
  return `phux-desktop-${channel === "next" ? next : version}-${archive}`;
}

/**
 * One product's entry. `release`, `name`, `url` and `minimum_macos` are
 * derived, and current clients ignore them; v0.87.0 and Desktop 0.26.0 check
 * them (ADR-0176 decision 8).
 */
export function entry(
  product: Product,
  channel: Channel,
  version: string,
  sha: string,
  protocol: string,
  digests: Record<string, string>,
): Table {
  const targets = TARGETS[product];
  const given = Object.keys(digests);
  if (given.length !== targets.length || !targets.every((target) => target in digests)) {
    throw new ManifestError(`${product} needs digests for exactly ${targets.join(", ")}`);
  }
  const release = releaseOf(product, channel, version);
  const artifacts: Table = {};
  for (const target of targets) {
    const name = assetName(product, channel, version, sha, target);
    artifacts[target] = {
      name,
      url: `${DOWNLOAD}/${release}/${name}`,
      sha256: require(DIGEST, digests[target], `${name} sha256`),
    };
  }
  const document: Table = {
    version: require(VERSION, version, `${product} version`),
    release,
    sha: require(SHA, sha, `${product} sha`),
    protocol: require(PROTOCOL, protocol, `${product} protocol`),
  };
  if (product === "desktop") document["minimum_macos"] = "27.0";
  document["artifacts"] = artifacts;
  return document;
}

export function manifest(channel: Channel, products: Table, generatedAt: string): Table {
  const ordered: Table = {};
  for (const name of PRODUCTS) if (name in products) ordered[name] = products[name];
  return { schema_version: 1, channel, generated_at: generatedAt, products: ordered };
}

function checkRenderable(value: unknown, path: string): void {
  if (isTable(value)) {
    if (Object.keys(value).length === 0) throw new ManifestError(`${path} is an empty object`);
    for (const [key, child] of Object.entries(value)) {
      require(KEY, key, `key at ${path}`);
      checkRenderable(child, `${path}.${key}`);
    }
  } else if (typeof value === "string") {
    require(SAFE, value, path);
  } else if (!Number.isInteger(value)) {
    const type = Array.isArray(value) ? "array" : value === null ? "null" : typeof value;
    throw new ManifestError(`${path} has unsupported type ${type}`);
  }
}

export function render(document: Table): string {
  checkRenderable(document, "manifest");
  return `${JSON.stringify(document, null, 2)}\n`;
}

function now(): string {
  return new Date().toISOString().replace(/\.\d{3}Z$/, "Z");
}

// --- git ---------------------------------------------------------------------

/** The commit `tag` names, fetching the tag when this checkout predates it. */
function tagCommit(tag: string): string {
  const cwd = process.cwd();
  try {
    return git(["rev-parse", `${tag}^{commit}`], cwd).trim();
  } catch (error) {
    if (!(error instanceof GitError)) throw error;
    git(["fetch", "--quiet", "--no-tags", "origin", `refs/tags/${tag}:refs/tags/${tag}`], cwd);
    return git(["rev-parse", `${tag}^{commit}`], cwd).trim();
  }
}

function showAt(ref: string, path: string): string {
  return git(["show", `${ref}:${path}`], process.cwd());
}

function protocolAt(ref: string, product: Product): string {
  if (product === "desktop") {
    const pin = showAt(ref, "clients/desktop/phux-protocol").trim();
    return require(PROTOCOL, pin, `clients/desktop/phux-protocol at ${ref}`);
  }
  const match = PROTOCOL_SOURCE.exec(showAt(ref, "crates/phux-protocol/src/lib.rs"));
  if (match === null) throw new ManifestError(`PROTOCOL_VERSION not found at ${ref}`);
  return `${Number(match[1])}.${Number(match[2])}`;
}

function versionAt(ref: string, product: Product): string {
  const version =
    product === "desktop"
      ? table(JSON.parse(showAt(ref, "clients/desktop/package.json")))["version"]
      : table(table(table(Bun.TOML.parse(showAt(ref, "Cargo.toml")))["workspace"])["package"])[
          "version"
        ];
  return require(VERSION, version, `${product} version at ${ref}`);
}

// --- GitHub ------------------------------------------------------------------

function ghRelease(...args: string[]): string {
  return gh(["release", ...args, "--repo", REPO]);
}

/**
 * The release named `tag`, or undefined when there is none. Any other
 * failure throws: an unreadable release is not an unpublished one.
 */
function release(tag: string): Table | undefined {
  try {
    return table(JSON.parse(gh(["api", `repos/${REPO}/releases/tags/${tag}`])));
  } catch (error) {
    if (error instanceof GhError && error.stderr.includes("HTTP 404")) return undefined;
    throw error;
  }
}

function assetsOf(release: Table | undefined): Table[] {
  return list(release?.["assets"]).map(table);
}

/** `{name: hex}` from the SHA-256 GitHub records for each uploaded asset. */
export function assetDigests(
  assets: readonly unknown[],
  names: Iterable<string>,
): Record<string, string> {
  const recorded = new Map<string, string>();
  for (const asset of assets.map(table)) {
    const name = str(asset["name"]);
    if (name !== undefined) recorded.set(name, str(asset["digest"]) ?? "");
  }
  const digests: Record<string, string> = {};
  for (const name of names) {
    const digest = recorded.get(name);
    if (digest === undefined) throw new ManifestError(`${name} is not published`);
    if (!digest.startsWith("sha256:")) throw new ManifestError(`${name} has no recorded sha256`);
    digests[name] = require(DIGEST, digest.slice("sha256:".length), `${name} sha256`);
  }
  return digests;
}

/** Create a prerelease that is never Latest, unless it already exists. */
function ensurePrerelease(tag: string, notes: string, target?: string): void {
  if (release(tag) !== undefined) return;
  const args = ["create", tag, "--prerelease", "--latest=false", "--title", tag, "--notes", notes];
  try {
    ghRelease(...args, ...(target ? ["--target", target] : []));
  } catch (error) {
    // publish.yml and next-release.yml can race to create `channels`.
    if (release(tag) === undefined) throw error;
  }
}

function upload(tag: string, paths: readonly string[]): void {
  ghRelease("upload", tag, ...paths, "--clobber");
}

/** The manifest clients see now, or undefined before the first publish. */
function publishedManifest(channel: Channel): Table | undefined {
  const channels = release("channels");
  const name = `${channel}.json`;
  if (channels === undefined || !assetsOf(channels).some((asset) => asset["name"] === name)) {
    return undefined;
  }
  return table(JSON.parse(ghRelease("download", "channels", "--pattern", name, "--output", "-")));
}

function withScratch<T>(body: (dir: string) => T): T {
  const dir = mkdtempSync(join(tmpdir(), "release-manifest-"));
  try {
    return body(dir);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

function sameIgnoringTime(left: Table, right: Table): boolean {
  return Bun.deepEquals({ ...left, generated_at: null }, { ...right, generated_at: null }, true);
}

function publishManifest(document: Table, current: Table | undefined): void {
  const name = `${String(document["channel"])}.json`;
  if (current !== undefined && sameIgnoringTime(current, document)) {
    console.log(`${name} is already current`);
    return;
  }
  ensurePrerelease("channels", "Release manifests: stable.json and next.json. Not a release.");
  withScratch((scratch) => {
    const path = join(scratch, name);
    writeFileSync(path, render(document));
    upload("channels", [path]);
  });
  console.log(`published ${name}`);
}

// --- stable ------------------------------------------------------------------

/** The newest published `vX.Y.Z` and `desktop-vX.Y.Z` release rows. */
export function newestStable(rows: Iterable<unknown>): Partial<Record<Product, Table>> {
  const newest: Partial<Record<Product, { version: Version; row: Table }>> = {};
  for (const row of [...rows].map(table)) {
    const identity = parseTag(str(row["tag_name"]) ?? "");
    if (row["draft"] || row["prerelease"] || identity === undefined) continue;
    const product = identity.component;
    if (!isProduct(product)) continue;
    const best = newest[product];
    if (best === undefined || compareVersions(identity.version, best.version) > 0) {
      newest[product] = { version: identity.version, row };
    }
  }
  const result: Partial<Record<Product, Table>> = {};
  for (const product of PRODUCTS) {
    const best = newest[product];
    if (best !== undefined) result[product] = best.row;
  }
  return result;
}

function stableEntry(product: Product, row: Table): Table {
  const tag = str(row["tag_name"]) ?? "";
  const identity = parseTag(tag);
  if (identity === undefined) throw new ManifestError(`${tag} is not a release tag`);
  const version = identity.version.join(".");
  const sha = tagCommit(tag);
  const names = Object.fromEntries(
    TARGETS[product].map((target) => [
      target,
      assetName(product, "stable", version, undefined, target),
    ]),
  );
  const digests = assetDigests(list(row["assets"]), Object.values(names));
  return entry(
    product,
    "stable",
    version,
    sha,
    protocolAt(sha, product),
    Object.fromEntries(
      Object.entries(names).map(([target, name]) => [target, digests[name] ?? ""]),
    ),
  );
}

function stable(out: string, publish: boolean): number {
  const listing = ghLines([
    "api",
    "--paginate",
    `repos/${REPO}/releases?per_page=100`,
    "--jq",
    ".[] | {tag_name, draft, prerelease, assets: [.assets[] | {name, digest}]}",
  ]);
  const newest = newestStable(listing);
  const current = publishedManifest("stable");
  const products: Table = {};
  let failed = false;
  for (const product of PRODUCTS) {
    const row = newest[product];
    if (row === undefined) continue;
    try {
      products[product] = stableEntry(product, row);
    } catch (error) {
      // Keep what clients see now; the red job says the lane needs a look.
      console.error(`error: stable ${product}: ${describe(error)}`);
      failed = true;
      const previous = table(current?.["products"])[product];
      if (isTable(previous) && Object.keys(previous).length > 0) products[product] = previous;
    }
  }
  if (Object.keys(products).length === 0) throw new ManifestError("nothing is published on stable");
  const document = manifest("stable", products, now());
  writeFileSync(out, render(document));
  console.log(`wrote ${out}`);
  if (publish) publishManifest(document, current);
  return failed ? 1 : 0;
}

// --- next --------------------------------------------------------------------

interface Built {
  readonly path: string;
  readonly digest: string;
}

/**
 * `{target: {path, digest}}` for each built asset whose sidecar matches it and
 * carries a signature (ADR-0178).
 */
export function verifiedDist(product: Product, sha: string, dist: string): Record<string, Built> {
  const verified: Record<string, Built> = {};
  for (const target of TARGETS[product]) {
    const name = assetName(product, "next", undefined, sha, target);
    const path = join(dist, name);
    const digest = new Bun.CryptoHasher("sha256").update(readFileSync(path)).digest("hex");
    const sidecar = readFileSync(join(dist, `${name}.sha256`), "utf8")
      .trim()
      .split(/\s+/);
    if (sidecar.length !== 2 || sidecar[0] !== digest || sidecar[1] !== name) {
      throw new ManifestError(`${name}.sha256 does not match ${name}`);
    }
    const signature = join(dist, `${name}.sha256.minisig`);
    if (!existsSync(signature) || !statSync(signature).isFile()) {
      throw new ManifestError(`${name}.sha256 is not signed`);
    }
    verified[target] = { path, digest };
  }
  return verified;
}

/** The pre-manifest next pointer an older binary reads (ADR-0176 decision 8). */
export function bridgeRecord(
  product: Product,
  sha: string,
  version: string,
  digests: Record<string, string>,
): [string, string] {
  const record: Table = { schema_version: 1, channel: "next", product, sha, version };
  if (product === "desktop") {
    record["archive"] = assetName("desktop", "next", undefined, sha, "aarch64-apple-darwin");
    record["sha256"] = digests["aarch64-apple-darwin"];
    record["minimum_macos"] = "27.0";
  }
  record["published_at"] = now();
  const name = product === "phux" ? "channel.json" : "desktop-channel.json";
  return [name, `${JSON.stringify(record, null, 2)}\n`];
}

/**
 * Drop `product`'s next assets except the commits in `keep`, so a client
 * still downloading the previous build does not 404.
 */
function pruneNext(product: Product, keep: readonly string[]): void {
  const family = product === "phux" ? "phux-next." : "phux-desktop-next.";
  const kept = keep.map((sha) => `${family}${sha}-`);
  const next = release("next");
  if (next === undefined) throw new ManifestError("the next release is missing");
  for (const asset of assetsOf(next)) {
    const name = str(asset["name"]) ?? "";
    if (name.startsWith(family) && !kept.some((prefix) => name.startsWith(prefix))) {
      ghRelease("delete-asset", "next", name, "--yes");
      console.log(`pruned ${name}`);
    }
  }
}

export function nextSha(product: Product): string {
  const current = publishedManifest("next") ?? {};
  return str(table(table(current["products"])[product])["sha"]) ?? "";
}

/**
 * Publish `product` built at `sha` to the next channel.
 *
 * next-release.yml runs one at a time, so replacing this product's entry in
 * the published next.json cannot lose the other product's update.
 */
function publishNext(product: Product, sha: string, dist: string): void {
  require(SHA, sha, "sha");
  const built = verifiedDist(product, sha, dist);
  const [version, protocol] = [versionAt(sha, product), protocolAt(sha, product)];
  const current = publishedManifest("next");

  ensurePrerelease("next", "Moving next channel (green main). Not a stable release.", sha);
  upload(
    "next",
    Object.values(built).flatMap(({ path }) => [path, `${path}.sha256`, `${path}.sha256.minisig`]),
  );
  const recorded = assetDigests(
    assetsOf(release("next")),
    Object.values(built).map(({ path }) => basename(path)),
  );
  if (Object.values(built).some(({ path, digest }) => recorded[basename(path)] !== digest)) {
    throw new ManifestError("the uploaded bytes do not match the build");
  }
  const digests = Object.fromEntries(
    Object.entries(built).map(([target, { digest }]) => [target, digest]),
  );

  const products: Table = { ...table(current?.["products"]) };
  const previousSha = str(table(products[product])["sha"]);
  products[product] = entry(product, "next", version, sha, protocol, digests);
  publishManifest(manifest("next", products, now()), current);

  withScratch((scratch) => {
    const [name, text] = bridgeRecord(product, sha, version, digests);
    writeFileSync(join(scratch, name), text);
    upload("next", [join(scratch, name)]);
  });

  if (previousSha) pruneNext(product, [sha, previousSha]);
  console.log(`published ${product} ${version} at ${sha} on next`);
}

// --- fixtures ----------------------------------------------------------------

export function fixture(channel: Channel): Table {
  const digests = Object.fromEntries(
    TARGETS.phux.map((target, index) => [target, (index + 1).toString(16).repeat(64)]),
  );
  return manifest(
    channel,
    {
      phux: entry("phux", channel, "9.8.7", "a".repeat(40), "0.9", digests),
      desktop: entry(
        "desktop",
        channel,
        "1.2.3",
        "b".repeat(40),
        "0.9",
        Object.fromEntries(
          TARGETS.desktop.map((target, index) => [target, (index + 13).toString(16).repeat(64)]),
        ),
      ),
    },
    "2026-01-01T00:00:00Z",
  );
}

/** Rewrite the golden fixtures, or with `check` fail when one is stale. */
export function fixtures(check: boolean, root = ROOT): number {
  const stale: string[] = [];
  for (const channel of CHANNELS) {
    const relative = `${FIXTURES}/${channel}.json`;
    const path = join(root, relative);
    const text = render(fixture(channel));
    if (!check) writeFileSync(path, text);
    else if (!existsSync(path) || readFileSync(path, "utf8") !== text) stale.push(relative);
  }
  if (stale.length > 0) {
    throw new ManifestError(
      `stale: ${stale.join(", ")}; run bun tools/main.ts release-manifest fixtures`,
    );
  }
  return 0;
}

export function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export const USAGE = `usage: bun tools/main.ts release-manifest <command>

  stable --out PATH [--publish]            build stable.json from the published releases
  next --product P --sha SHA --dist DIR    publish one product's build to the next channel
  next-sha PRODUCT                         print the commit next.json names for a product
  fixtures [--check]                       rewrite or check the golden fixtures

PRODUCT is one of: ${PRODUCTS.join(", ")}`;

export type Invocation =
  | { command: "stable"; out: string; publish: boolean }
  | { command: "next"; product: Product; sha: string; dist: string }
  | { command: "next-sha"; product: Product }
  | { command: "fixtures"; check: boolean };

export function execute(invocation: Invocation): number {
  try {
    switch (invocation.command) {
      case "stable":
        return stable(invocation.out, invocation.publish);
      case "next":
        publishNext(invocation.product, invocation.sha, invocation.dist);
        return 0;
      case "next-sha":
        console.log(nextSha(invocation.product));
        return 0;
      case "fixtures":
        return fixtures(invocation.check);
    }
  } catch (error) {
    console.error(`error: ${describe(error)}`);
    return 1;
  }
}
