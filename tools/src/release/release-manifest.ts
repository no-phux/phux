// Build and publish the channel manifests (ADR-0176) from git and GitHub.
//
// `stable` derives channels/stable.json from the newest published releases.
// `next` publishes one product's build onto the moving `next` prerelease and
// records it in channels/next.json.
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { parseArgs } from "node:util";
import { describe, Failure, required, UsageError, type Command } from "../lib/command.ts";
import { git } from "../lib/git.ts";
import { gh, ghLines } from "../lib/github.ts";
import { status } from "../lib/output.ts";
import { ProcessError } from "../lib/process.ts";
import { isTable, list, str, table, type Table } from "../lib/value.ts";
import {
  assetName,
  bridgeRecord,
  DIGEST,
  entry,
  fixtures,
  isProduct,
  manifest,
  now,
  PRODUCTS,
  PROTOCOL,
  render,
  SHA,
  targets,
  valid,
  VERSION,
  type Channel,
  type Product,
} from "./manifest.ts";
import { compareVersions, parseTag, type Version } from "./publish-plan.ts";

const REPO = "no-phux/phux";
const PROTOCOL_SOURCE =
  /pub const PROTOCOL_VERSION: Version = Version \{\s*major: (\d+),\s*minor: (\d+),/;

// --- git ---------------------------------------------------------------------

const gitHere = (args: string[]): string => git(args, process.cwd()).trim();

/** The commit `tag` names, fetching the tag when this checkout predates it. */
function tagCommit(tag: string): string {
  try {
    return gitHere(["rev-parse", `${tag}^{commit}`]);
  } catch {
    gitHere(["fetch", "--quiet", "--no-tags", "origin", `refs/tags/${tag}:refs/tags/${tag}`]);
    return gitHere(["rev-parse", `${tag}^{commit}`]);
  }
}

const showAt = (ref: string, path: string): string => gitHere(["show", `${ref}:${path}`]);

function protocolAt(ref: string, product: Product): string {
  if (product === "desktop") {
    const pin = showAt(ref, "clients/desktop/phux-protocol");
    return valid(PROTOCOL, pin, `clients/desktop/phux-protocol at ${ref}`);
  }
  const match = PROTOCOL_SOURCE.exec(showAt(ref, "crates/phux-protocol/src/lib.rs"));
  if (match === null) throw new Failure(`PROTOCOL_VERSION not found at ${ref}`);
  return `${Number(match[1])}.${Number(match[2])}`;
}

function versionAt(ref: string, product: Product): string {
  const cargo = (): Table => table(Bun.TOML.parse(showAt(ref, "Cargo.toml")));
  const version =
    product === "desktop"
      ? table(JSON.parse(showAt(ref, "clients/desktop/package.json")))["version"]
      : table(table(cargo()["workspace"])["package"])["version"];
  return valid(VERSION, version, `${product} version at ${ref}`);
}

// --- GitHub ------------------------------------------------------------------

const ghRelease = (...args: string[]): string => gh(["release", ...args, "--repo", REPO]);

/** The release named `tag`, or undefined when there is none; any other failure throws. */
function release(tag: string): Table | undefined {
  try {
    return table(JSON.parse(gh(["api", `repos/${REPO}/releases/tags/${tag}`])));
  } catch (error) {
    if (error instanceof ProcessError && error.stderr.includes("HTTP 404")) return undefined;
    throw error;
  }
}

const assetsOf = (release: Table | undefined): Table[] => list(release?.["assets"]).map(table);

/** `{target: hex}` from the SHA-256 GitHub records for each of a build's assets. */
export function assetDigests(
  assets: readonly unknown[],
  product: Product,
  channel: Channel,
  id: string,
): Record<string, string> {
  const recorded = new Map(assets.map(table).map((asset) => [asset["name"], asset["digest"]]));
  const digest = (name: string): string => {
    if (!recorded.has(name)) throw new Failure(`${name} is not published`);
    const value = str(recorded.get(name)) ?? "";
    if (!value.startsWith("sha256:")) throw new Failure(`${name} has no recorded sha256`);
    return valid(DIGEST, value.slice("sha256:".length), `${name} sha256`);
  };
  return Object.fromEntries(
    targets(product, channel, id).map((target) => [
      target,
      digest(assetName(product, channel, id, target)),
    ]),
  );
}

/** Create a prerelease that is never Latest, unless it already exists. */
function ensurePrerelease(tag: string, notes: string, target?: string): void {
  if (release(tag) !== undefined) return;
  const args = ["create", tag, "--prerelease", "--latest=false", "--title", tag, "--notes", notes];
  try {
    ghRelease(...args, ...(target ? ["--target", target] : []));
  } catch (error) {
    // release.yml and next.yml can race to create `channels`.
    if (release(tag) === undefined) throw error;
  }
}

/** Upload files written to a scratch directory, then remove it. */
function uploadFiles(tag: string, files: Record<string, string>): void {
  const dir = mkdtempSync(join(tmpdir(), "release-manifest-"));
  try {
    const paths = Object.entries(files).map(([name, text]) => {
      writeFileSync(join(dir, name), text);
      return join(dir, name);
    });
    ghRelease("upload", tag, ...paths, "--clobber");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

/** The manifest clients see now, or undefined before the first publish. */
function publishedManifest(channel: Channel): Table | undefined {
  const name = `${channel}.json`;
  if (!assetsOf(release("channels")).some((asset) => asset["name"] === name)) return undefined;
  return table(JSON.parse(ghRelease("download", "channels", "--pattern", name, "--output", "-")));
}

function publishManifest(document: Table, current: Table | undefined): void {
  const name = `${String(document["channel"])}.json`;
  const timeless = (value: Table): Table => ({ ...value, generated_at: null });
  if (current !== undefined && Bun.deepEquals(timeless(current), timeless(document), true)) {
    console.log(`${name} is already current`);
    return;
  }
  ensurePrerelease("channels", "Release manifests: stable.json and next.json. Not a release.");
  uploadFiles("channels", { [name]: render(document) });
  console.log(`published ${name}`);
}

// --- stable ------------------------------------------------------------------

/** The newest published `vX.Y.Z` and `desktop-vX.Y.Z` release rows. */
export function newestStable(rows: Iterable<unknown>): Partial<Record<Product, Table>> {
  const newest: Partial<Record<Product, { version: Version; row: Table }>> = {};
  for (const row of [...rows].map(table)) {
    const identity = parseTag(str(row["tag_name"]) ?? "");
    if (row["draft"] || row["prerelease"] || !identity || !isProduct(identity.component)) continue;
    const best = newest[identity.component];
    if (best === undefined || compareVersions(identity.version, best.version) > 0) {
      newest[identity.component] = { version: identity.version, row };
    }
  }
  return Object.fromEntries(PRODUCTS.flatMap((p) => (newest[p] ? [[p, newest[p].row]] : [])));
}

function stableEntry(product: Product, row: Table): Table {
  const tag = str(row["tag_name"]) ?? "";
  const version = parseTag(tag)?.version.join(".") ?? "";
  const sha = tagCommit(tag);
  const digests = assetDigests(list(row["assets"]), product, "stable", version);
  return entry(product, "stable", version, sha, protocolAt(sha, product), digests);
}

function stable(out: string, publish: boolean): number {
  const listing = ghLines([
    "api",
    "--paginate",
    `repos/${REPO}/releases?per_page=100`,
    "--jq",
    ".[] | {tag_name, draft, prerelease, assets: [.assets[] | {name, digest}]}",
  ]);
  const current = publishedManifest("stable");
  const products: Table = {};
  let failed = false;
  for (const [product, row] of Object.entries(newestStable(listing)) as [Product, Table][]) {
    try {
      products[product] = stableEntry(product, row);
    } catch (error) {
      // Keep what clients see now; the red job says the lane needs a look.
      status("error", `stable ${product}: ${describe(error)}`);
      failed = true;
      const previous = table(current?.["products"])[product];
      if (isTable(previous) && Object.keys(previous).length > 0) products[product] = previous;
    }
  }
  if (Object.keys(products).length === 0) throw new Failure("nothing is published on stable");
  const document = manifest("stable", products, now());
  writeFileSync(out, render(document));
  console.log(`wrote ${out}`);
  if (publish) publishManifest(document, current);
  return failed ? 1 : 0;
}

// --- next --------------------------------------------------------------------

/**
 * `{target: digest}` for each built asset whose sidecar matches it and
 * carries a signature (ADR-0178).
 */
export function verifiedDist(product: Product, sha: string, dist: string): Record<string, string> {
  const verified: Record<string, string> = {};
  for (const target of targets(product)) {
    const name = assetName(product, "next", sha, target);
    const path = join(dist, name);
    const digest = new Bun.CryptoHasher("sha256").update(readFileSync(path)).digest("hex");
    const sidecar = readFileSync(`${path}.sha256`, "utf8").trim().split(/\s+/);
    if (sidecar.length !== 2 || sidecar[0] !== digest || sidecar[1] !== name) {
      throw new Failure(`${name}.sha256 does not match ${name}`);
    }
    const signature = `${path}.sha256.minisig`;
    if (!existsSync(signature) || !statSync(signature).isFile()) {
      throw new Failure(`${name}.sha256 is not signed`);
    }
    verified[target] = digest;
  }
  return verified;
}

/**
 * Drop `product`'s next assets except the commits in `keep`, so a client
 * still downloading the previous build does not 404.
 */
function pruneNext(product: Product, keep: readonly string[]): void {
  const family = product === "phux" ? "phux-next." : "phux-desktop-next.";
  const next = release("next");
  if (next === undefined) throw new Failure("the next release is missing");
  for (const name of assetsOf(next).map((asset) => str(asset["name"]) ?? "")) {
    if (name.startsWith(family) && !keep.some((sha) => name.startsWith(`${family}${sha}-`))) {
      ghRelease("delete-asset", "next", name, "--yes");
      console.log(`pruned ${name}`);
    }
  }
}

export function nextSha(product: Product): string {
  return str(table(table(publishedManifest("next")?.["products"])[product])["sha"]) ?? "";
}

/**
 * Publish `product` built at `sha` to the next channel.
 *
 * next.yml runs one at a time, so replacing this product's entry in
 * the published next.json cannot lose the other product's update.
 */
function publishNext(product: Product, sha: string, dist: string): void {
  valid(SHA, sha, "sha");
  const built = verifiedDist(product, sha, dist);
  const [version, protocol] = [versionAt(sha, product), protocolAt(sha, product)];
  const current = publishedManifest("next");

  ensurePrerelease("next", "Moving next channel (green main). Not a stable release.", sha);
  const paths = targets(product).map((target) =>
    join(dist, assetName(product, "next", sha, target)),
  );
  const files = paths.flatMap((path) => [path, `${path}.sha256`, `${path}.sha256.minisig`]);
  ghRelease("upload", "next", ...files, "--clobber");
  const digests = assetDigests(assetsOf(release("next")), product, "next", sha);
  if (targets(product).some((target) => digests[target] !== built[target])) {
    throw new Failure("the uploaded bytes do not match the build");
  }

  const products: Table = { ...table(current?.["products"]) };
  const previousSha = str(table(products[product])["sha"]);
  products[product] = entry(product, "next", version, sha, protocol, digests);
  publishManifest(manifest("next", products, now()), current);

  const [name, text] = bridgeRecord(product, sha, version, digests);
  uploadFiles("next", { [name]: text });

  if (previousSha) pruneNext(product, [sha, previousSha]);
  console.log(`published ${product} ${version} at ${sha} on next`);
}

// --- CLI ---------------------------------------------------------------------

function product(value: string | undefined): Product {
  if (value === undefined || !isProduct(value)) {
    throw new UsageError(`product must be phux or desktop, got ${String(value)}`);
  }
  return value;
}

export const releaseManifest: Command = {
  summary: "Build or publish the stable/next channel manifests, or check their fixtures.",
  usage: "stable|next|next-sha|fixtures [args]",
  help: `  stable --out PATH [--publish]            build stable.json from the published releases
  next --product P --sha SHA --dist DIR    publish one product's build to the next channel
  next-sha PRODUCT                         print the commit next.json names for a product
  fixtures [--check]                       rewrite or check the golden fixtures

PRODUCT is one of: ${PRODUCTS.join(", ")}`,
  run(args) {
    const flag = { type: "string" } as const;
    const options = { out: flag, product: flag, sha: flag, dist: flag };
    const switches = { publish: { type: "boolean" }, check: { type: "boolean" } } as const;
    const { values, positionals } = parseArgs({
      args,
      options: { ...options, ...switches },
      allowPositionals: true,
    });
    const [command, ...rest] = positionals;
    if (rest.length !== (command === "next-sha" ? 1 : 0)) {
      throw new UsageError(
        command === "next-sha" ? "next-sha takes one PRODUCT" : "too many arguments",
      );
    }
    switch (command) {
      case "stable":
        return stable(required(values.out, "--out"), values.publish === true);
      case "next": {
        const which = product(required(values.product, "--product"));
        publishNext(which, required(values.sha, "--sha"), required(values.dist, "--dist"));
        return 0;
      }
      case "next-sha":
        console.log(nextSha(product(rest[0])));
        return 0;
      case "fixtures":
        return fixtures(values.check === true);
      default:
        throw new UsageError(`unknown command ${String(command)}`);
    }
  },
};
