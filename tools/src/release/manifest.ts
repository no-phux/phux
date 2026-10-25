// The channel manifest document (ADR-0176): names, entries, rendering and the
// golden fixtures every client tests. No git or GitHub here.
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { Failure } from "../lib/command.ts";
import { ROOT } from "../lib/repo.ts";
import { isTable, type Table } from "../lib/value.ts";

const DOWNLOAD = "https://github.com/no-phux/phux/releases/download";
export const CHANNELS = ["stable", "next"] as const;
export type Channel = (typeof CHANNELS)[number];
export const PRODUCTS = ["phux", "desktop"] as const;
export type Product = (typeof PRODUCTS)[number];
export const DESKTOP_ARCHIVES: Record<string, string> = {
  "aarch64-apple-darwin": "macos-arm64.zip",
  "x86_64-unknown-linux-gnu": "linux-x64.tar.gz",
  "aarch64-unknown-linux-gnu": "linux-arm64.tar.gz",
  "x86_64-pc-windows-msvc": "windows-x64.zip",
};
/** CLI releases remain Unix-only; Windows runs that CLI inside WSL. */
export const TARGETS = [
  "aarch64-apple-darwin",
  "x86_64-unknown-linux-gnu",
  "aarch64-unknown-linux-gnu",
];
export function targets(product: Product): string[] {
  return product === "desktop" ? Object.keys(DESKTOP_ARCHIVES) : TARGETS;
}
const FIXTURES = "scripts/fixtures/release-manifest";

// The POSIX installers parse the rendered file with awk, so values stay in
// this alphabet and the document has no arrays.
const SAFE = /^[A-Za-z0-9._:/+-]*$/;
const KEY = /^[A-Za-z0-9_-]+$/;
export const VERSION = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/;
export const SHA = /^[0-9a-f]{40}$/;
export const DIGEST = /^[0-9a-f]{64}$/;
export const PROTOCOL = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/;

export function isProduct(value: string): value is Product {
  return (PRODUCTS as readonly string[]).includes(value);
}

export function valid(pattern: RegExp, value: unknown, what: string): string {
  if (typeof value !== "string" || !pattern.test(value)) {
    throw new Failure(`${what} is not valid: ${JSON.stringify(value) ?? String(value)}`);
  }
  return value;
}

/** The naming rule every client derives on its own (ADR-0176 decision 1). */
export function assetName(product: Product, channel: Channel, id: string, target: string): string {
  const identity = channel === "next" ? `next.${id}` : product === "phux" ? `v${id}` : id;
  if (product === "phux") return `phux-${identity}-${target}.tar.gz`;
  const archive = DESKTOP_ARCHIVES[target];
  if (archive === undefined) throw new Failure(`no desktop archive for ${target}`);
  return `phux-desktop-${identity}-${archive}`;
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
  const given = Object.keys(digests);
  const required = targets(product);
  if (given.length !== required.length || !required.every((target) => target in digests)) {
    throw new Failure(`${product} needs digests for exactly ${required.join(", ")}`);
  }
  const release =
    channel === "next" ? "next" : `${product === "phux" ? "" : "desktop-"}v${version}`;
  const artifacts: Table = {};
  for (const target of required) {
    const name = assetName(product, channel, channel === "next" ? sha : version, target);
    const sha256 = valid(DIGEST, digests[target], `${name} sha256`);
    artifacts[target] = { name, url: `${DOWNLOAD}/${release}/${name}`, sha256 };
  }
  return {
    version: valid(VERSION, version, `${product} version`),
    release,
    sha: valid(SHA, sha, `${product} sha`),
    protocol: valid(PROTOCOL, protocol, `${product} protocol`),
    ...(product === "desktop" ? { minimum_macos: "26.0" } : {}),
    artifacts,
  };
}

export function manifest(channel: Channel, products: Table, generatedAt: string): Table {
  const ordered: Table = {};
  for (const name of PRODUCTS) if (name in products) ordered[name] = products[name];
  return { schema_version: 1, channel, generated_at: generatedAt, products: ordered };
}

function checkRenderable(value: unknown, path: string): void {
  if (typeof value === "string") {
    valid(SAFE, value, path);
  } else if (isTable(value)) {
    if (Object.keys(value).length === 0) throw new Failure(`${path} is an empty object`);
    for (const [key, child] of Object.entries(value)) {
      valid(KEY, key, `key at ${path}`);
      checkRenderable(child, `${path}.${key}`);
    }
  } else if (!Number.isInteger(value)) {
    throw new Failure(`${path} has unsupported value ${JSON.stringify(value)}`);
  }
}

export function render(document: Table): string {
  checkRenderable(document, "manifest");
  return `${JSON.stringify(document, null, 2)}\n`;
}

export function now(): string {
  return new Date().toISOString().replace(/\.\d{3}Z$/, "Z");
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
    record["archive"] = assetName("desktop", "next", sha, "aarch64-apple-darwin");
    record["sha256"] = digests["aarch64-apple-darwin"];
    record["minimum_macos"] = "26.0";
  }
  record["published_at"] = now();
  const name = product === "phux" ? "channel.json" : "desktop-channel.json";
  return [name, `${JSON.stringify(record, null, 2)}\n`];
}

/** Placeholder single-hex-digit digests, wrapping after f. */
function fakeDigests(first: number, product: Product): Record<string, string> {
  return Object.fromEntries(
    targets(product).map((target, index) => [
      target,
      ((index + first) % 16).toString(16).repeat(64),
    ]),
  );
}

export function fixture(channel: Channel): Table {
  return manifest(
    channel,
    {
      phux: entry("phux", channel, "9.8.7", "a".repeat(40), "0.9", fakeDigests(1, "phux")),
      desktop: entry(
        "desktop",
        channel,
        "1.2.3",
        "b".repeat(40),
        "0.9",
        fakeDigests(13, "desktop"),
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
    throw new Failure(
      `stale: ${stale.join(", ")}; run bun tools/main.ts release-manifest fixtures`,
    );
  }
  return 0;
}
