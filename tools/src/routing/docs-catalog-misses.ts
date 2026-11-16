// New uncatalogued flat pages between the planner's base and head.
//
// website `discover()` throws on the first uncatalogued flat page and does not
// export that list. This walks the throw, moving each page aside, until the
// tree is clean. A page already uncatalogued on the base is not a new miss.
// A delete never is. Loading the catalog module is a different failure: the
// job fails, and the message is not the ORIENTATION line.
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { describe } from "../lib/command.ts";
import { git } from "../lib/git.ts";
import { ROOT } from "../lib/repo.ts";
import { plannerRange } from "./detect.ts";

export const LOAD_MESSAGE =
  "website docs catalog module failed to load; fix the website checkout of scripts/docs-catalog.ts and re-run";
export const ORIENTATION_MESSAGE =
  "open a no-phux/website PR that adds the ORIENTATION entry for the page first, then re-run";
export const BASE_MESSAGE =
  "website docs catalog could not resolve the planner base; failing the job";
export const CHECK_PREFIX = "website docs catalog check failed:";

const UNCATALOGUED = /^uncatalogued documentation: (docs\/[^/\s;]+\.md)\b/;
const MAX_MISSES = 1000;

export type Discover = (docsDir: string) => Promise<unknown>;

export class CatalogLoadError extends Error {}

/** Pages `discover` rejects, in the order it reports them. Mutates a copy. */
export async function collectUncatalogued(docsDir: string, discover: Discover): Promise<string[]> {
  if (!existsSync(docsDir)) return [];
  const scratch = mkdtempSync(join(tmpdir(), "phux-docs-catalog-"));
  const dir = join(scratch, "docs");
  cpSync(docsDir, dir, { recursive: true });
  const found: string[] = [];
  try {
    for (;;) {
      try {
        await discover(dir);
        return found;
      } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        const page = UNCATALOGUED.exec(message)?.[1];
        if (page === undefined || found.includes(page)) {
          throw new Error(`${CHECK_PREFIX} ${message}`);
        }
        found.push(page);
        if (found.length > MAX_MISSES)
          throw new Error(`${CHECK_PREFIX} too many uncatalogued pages`);
        rmSync(join(dir, page.slice("docs/".length)), { force: true });
      }
    }
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

/** Head pages that the base did not already leave uncatalogued. */
export function newMisses(base: readonly string[], head: readonly string[]): string[] {
  const old = new Set(base);
  return head.filter((page) => !old.has(page)).sort();
}

export function ignoredMisses(base: readonly string[], head: readonly string[]): string[] {
  const old = new Set(base);
  return head.filter((page) => old.has(page)).sort();
}

export function formatReport(
  base: readonly string[],
  head: readonly string[],
): {
  code: number;
  lines: string[];
} {
  const fresh = newMisses(base, head);
  if (fresh.length > 0) {
    return {
      code: 1,
      lines: [...fresh.map((page) => `uncatalogued documentation: ${page}`), ORIENTATION_MESSAGE],
    };
  }
  return {
    code: 0,
    lines: [
      ...ignoredMisses(base, head).map(
        (page) => `website docs catalog: ignored pre-existing uncatalogued page ${page}`,
      ),
      "website docs catalog: no new uncatalogued page",
    ],
  };
}

export async function loadDiscover(catalogPath: string): Promise<Discover> {
  try {
    const loaded: unknown = await import(pathToFileURL(resolve(catalogPath)).href);
    const discover = (loaded as { discover?: unknown }).discover;
    if (typeof discover !== "function") {
      throw new Error("docs-catalog.ts does not export discover");
    }
    return discover as Discover;
  } catch (error) {
    throw new CatalogLoadError(describe(error));
  }
}

function extractDocs(sha: string, cwd: string): string {
  const dest = mkdtempSync(join(tmpdir(), "phux-docs-tree-"));
  const archive = Bun.spawnSync(["git", "archive", sha, "docs"], {
    cwd,
    stdout: "pipe",
    stderr: "pipe",
  });
  if (archive.exitCode !== 0) {
    mkdirSync(join(dest, "docs"), { recursive: true });
    return join(dest, "docs");
  }
  const extracted = Bun.spawnSync(["tar", "-x", "-C", dest], { stdin: archive.stdout });
  if (extracted.exitCode !== 0) {
    throw new Error(`${CHECK_PREFIX} could not extract docs at ${sha}`);
  }
  const docs = join(dest, "docs");
  if (!existsSync(docs)) mkdirSync(docs, { recursive: true });
  return docs;
}

export async function compareDocs(options: {
  repo: string;
  base: string;
  head: string;
  discover: Discover;
}): Promise<{ code: number; lines: string[] }> {
  const baseDir = extractDocs(options.base, options.repo);
  const headDir = extractDocs(options.head, options.repo);
  try {
    const basePages = await collectUncatalogued(baseDir, options.discover);
    const headPages = await collectUncatalogued(headDir, options.discover);
    return formatReport(basePages, headPages);
  } finally {
    rmSync(dirname(baseDir), { recursive: true, force: true });
    rmSync(dirname(headDir), { recursive: true, force: true });
  }
}

function resolveRange(cwd: string): [string, string] {
  const base = process.env["BASE_SHA"];
  const head = process.env["HEAD_SHA"];
  if (base && head) return [base, head];
  const eventPath = process.env["GITHUB_EVENT_PATH"];
  if (eventPath) {
    try {
      return plannerRange(
        process.env["GITHUB_EVENT_NAME"] ?? "",
        JSON.parse(readFileSync(eventPath, "utf8")),
        cwd,
      );
    } catch (error) {
      console.error(`website docs catalog: planner range unavailable (${describe(error)})`);
    }
  }
  const tip = git(["rev-parse", "HEAD"], cwd).trim();
  try {
    return [git(["rev-parse", `${tip}^`], cwd).trim(), tip];
  } catch {
    throw new Error(BASE_MESSAGE);
  }
}

/** Exit code for the docs-catalog job. */
export async function main(cwd = process.env["DOCS_CATALOG_REPO"] ?? ROOT): Promise<number> {
  const catalog = process.env["CATALOG"] ?? join(cwd, "website/scripts/docs-catalog.ts");
  let discover: Discover;
  try {
    discover = await loadDiscover(catalog);
  } catch (error) {
    console.error(LOAD_MESSAGE);
    console.error(error instanceof Error ? error.message : String(error));
    return 1;
  }
  let range: [string, string];
  try {
    range = resolveRange(cwd);
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    return 1;
  }
  try {
    const report = await compareDocs({ repo: cwd, base: range[0], head: range[1], discover });
    for (const line of report.lines) console.log(line);
    return report.code;
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    return 1;
  }
}
