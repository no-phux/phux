#!/usr/bin/env bun
/**
 * check-links.ts — verify every internal href/src in the built site resolves.
 *
 * Walks every .html under dist/, extracts internal URLs, and checks each maps onto a
 * file in dist (build.format "file": /wire/l1 -> dist/wire/l1.html). Exits 1
 * with a report when anything would 404. Run after `bun run build`.
 */
import { readdir, readFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { join, resolve } from "node:path";

const DIST = resolve(import.meta.dir, "../dist");

async function walk(dir: string): Promise<string[]> {
  const out: string[] = [];
  for (const ent of await readdir(dir, { withFileTypes: true })) {
    const abs = join(dir, ent.name);
    if (ent.isDirectory()) out.push(...(await walk(abs)));
    else if (ent.name.endsWith(".html")) out.push(abs);
  }
  return out;
}

/** Does an internal path (no hash/query) exist in dist? */
function resolves(path: string): boolean {
  if (path === "/") return existsSync(join(DIST, "index.html"));
  const clean = path.replace(/\/+$/, "");
  return (
    existsSync(join(DIST, clean)) || // asset or already-extensioned file
    existsSync(join(DIST, `${clean}.html`)) || // build.format "file" page
    existsSync(join(DIST, clean, "index.html"))
  );
}

const files = await walk(DIST);
const broken: { file: string; href: string }[] = [];
const seen = new Set<string>();

for (const file of files) {
  const html = await readFile(file, "utf8");
  for (const m of html.matchAll(/(?:href|src)="([^"]+)"/g)) {
    const href = m[1];
    if (/^(https?:|mailto:|#|data:|\/\/)/.test(href)) continue;
    const path = href.split(/[?#]/)[0];
    if (!path.startsWith("/")) continue; // no relative links emitted; skip if any
    const key = path;
    if (seen.has(`${key}|ok`)) continue;
    if (resolves(path)) {
      seen.add(`${key}|ok`);
    } else {
      broken.push({ file: file.slice(DIST.length), href });
    }
  }
}

if (broken.length) {
  console.error(`check-links: ${broken.length} broken internal link(s):`);
  for (const b of broken) console.error(`  ${b.file} -> ${b.href}`);
  process.exit(1);
}
console.log(`check-links: ${files.length} page(s), all internal links resolve`);
