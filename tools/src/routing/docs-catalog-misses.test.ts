import { afterAll, expect, test } from "bun:test";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { git } from "../lib/git.ts";
import { repoPath } from "../lib/repo.ts";
import {
  BASE_MESSAGE,
  collectUncatalogued,
  compareDocs,
  formatReport,
  LOAD_MESSAGE,
  loadDiscover,
  newMisses,
  ORIENTATION_MESSAGE,
  type Discover,
} from "./docs-catalog-misses.ts";

const scratch = mkdtempSync(join(tmpdir(), "phux-catalog-miss-"));

afterAll(() => {
  rmSync(scratch, { recursive: true, force: true });
});

function discoverRejecting(unknown: readonly string[]): Discover {
  const banned = new Set(unknown);
  return (docsDir: string) => {
    for (const name of readdirSync(docsDir)
      .filter((entry) => entry.endsWith(".md"))
      .sort()) {
      if (banned.has(name)) {
        throw new Error(
          `uncatalogued documentation: docs/${name}; add it to ORIENTATION and navigation`,
        );
      }
    }
    return Promise.resolve();
  };
}

test("collect walks every uncatalogued page, not only the first", async () => {
  const docs = join(scratch, "collect");
  mkdirSync(docs, { recursive: true });
  writeFileSync(join(docs, "onepassword.md"), "# existing\n");
  writeFileSync(join(docs, "zz-uncatalogued-probe.md"), "# probe\n");
  writeFileSync(join(docs, "kept.md"), "# kept\n");
  const pages = await collectUncatalogued(
    docs,
    discoverRejecting(["onepassword.md", "zz-uncatalogued-probe.md"]),
  );
  expect(pages.toSorted()).toEqual(["docs/onepassword.md", "docs/zz-uncatalogued-probe.md"]);
  expect(existsSync(join(docs, "onepassword.md"))).toBe(true);
});

test("a pre-existing miss and a delete are not new misses", () => {
  expect(newMisses(["docs/onepassword.md"], ["docs/onepassword.md"])).toEqual([]);
  expect(newMisses(["docs/onepassword.md", "docs/gone.md"], ["docs/onepassword.md"])).toEqual([]);
  expect(
    newMisses(["docs/onepassword.md"], ["docs/onepassword.md", "docs/zz-uncatalogued-probe.md"]),
  ).toEqual(["docs/zz-uncatalogued-probe.md"]);
  const fresh = formatReport(
    ["docs/onepassword.md"],
    ["docs/onepassword.md", "docs/zz-uncatalogued-probe.md"],
  );
  expect(fresh.code).toBe(1);
  expect(fresh.lines).toEqual([
    "uncatalogued documentation: docs/zz-uncatalogued-probe.md",
    ORIENTATION_MESSAGE,
  ]);
  const deleted = formatReport(["docs/onepassword.md", "docs/gone.md"], ["docs/onepassword.md"]);
  expect(deleted.code).toBe(0);
  expect(deleted.lines.join("\n")).not.toContain(ORIENTATION_MESSAGE);
  expect(deleted.lines.join("\n")).toContain(
    "ignored pre-existing uncatalogued page docs/onepassword.md",
  );
});

function commit(repo: string, files: Record<string, string>): string {
  for (const [path, text] of Object.entries(files)) {
    mkdirSync(join(repo, path, ".."), { recursive: true });
    writeFileSync(join(repo, path), text);
  }
  git(["add", "--all"], repo);
  git(["commit", "--quiet", "--allow-empty", "-m", "fixture"], repo);
  return git(["rev-parse", "HEAD"], repo).trim();
}

test("a delete-only tree passes and a new page fails, through git", async () => {
  const repo = join(scratch, "repo");
  mkdirSync(repo, { recursive: true });
  git(["init", "--quiet"], repo);
  git(["config", "user.email", "catalog@example.invalid"], repo);
  git(["config", "user.name", "Catalog"], repo);
  git(["config", "commit.gpgsign", "false"], repo);
  git(["config", "core.hooksPath", "/dev/null"], repo);
  const base = commit(repo, {
    "docs/onepassword.md": "# existing\n",
    "docs/kept.md": "# kept\n",
  });
  git(["rm", "--quiet", "docs/kept.md"], repo);
  git(["commit", "--quiet", "-m", "delete"], repo);
  const deletedHead = git(["rev-parse", "HEAD"], repo).trim();
  const added = commit(repo, { "docs/zz-uncatalogued-probe.md": "# probe\n" });
  const discover = discoverRejecting(["onepassword.md", "zz-uncatalogued-probe.md"]);
  const pass = await compareDocs({ repo, base, head: deletedHead, discover });
  expect(pass.code).toBe(0);
  expect(pass.lines.join("\n")).not.toContain("zz-uncatalogued-probe");
  expect(pass.lines.join("\n")).not.toContain("docs/kept.md");
  const fail = await compareDocs({ repo, base, head: added, discover });
  expect(fail.code).toBe(1);
  expect(fail.lines[0]).toBe("uncatalogued documentation: docs/zz-uncatalogued-probe.md");
});

test("a missing catalog module fails to load", async () => {
  let failed = false;
  try {
    await loadDiscover(join(scratch, "missing-docs-catalog.ts"));
  } catch {
    failed = true;
  }
  expect(failed).toBe(true);
});

test("main reports a load failure distinctly when the catalog path is missing", async () => {
  const repo = join(scratch, "load-repo");
  mkdirSync(join(repo, "docs"), { recursive: true });
  git(["init", "--quiet"], repo);
  git(["config", "user.email", "catalog@example.invalid"], repo);
  git(["config", "user.name", "Catalog"], repo);
  git(["config", "commit.gpgsign", "false"], repo);
  git(["config", "core.hooksPath", "/dev/null"], repo);
  commit(repo, { "docs/kept.md": "# kept\n" });
  const head = git(["rev-parse", "HEAD"], repo).trim();
  const proc = Bun.spawnSync(["bun", repoPath("scripts/ci/docs-catalog-new-misses.ts")], {
    cwd: repoPath("."),
    env: {
      ...process.env,
      DOCS_CATALOG_REPO: repo,
      CATALOG: join(scratch, "no-such-catalog.ts"),
      BASE_SHA: head,
      HEAD_SHA: head,
    },
    stdout: "pipe",
    stderr: "pipe",
  });
  const text = `${proc.stdout.toString()}${proc.stderr.toString()}`;
  expect(proc.exitCode).toBe(1);
  expect(text).toContain(LOAD_MESSAGE);
  expect(text).not.toContain(ORIENTATION_MESSAGE);
  expect(text).not.toContain(BASE_MESSAGE);
});
