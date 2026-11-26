// The file-token gate during merges: a merge may carry an oversized blob
// unchanged at the same path from either parent; anything new must fit.
import { expect, test } from "bun:test";
import { existsSync, rmSync, writeFileSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { conflictingMerge, Repo, scratch } from "./file-tokens.fixture.ts";

const SLOW = 30_000;

test(
  "conflict resolutions inherit only a parent's exact blob at the same path",
  () => {
    scratch((newRepo) => {
      const repo = newRepo("conflicts");
      const base = conflictingMerge(repo);
      repo.resolveFrom("MERGE_HEAD");
      repo.passes();
      repo.append("big.md", "x");
      repo.passes(); // an unstaged edit of the inherited blob is ignored
      repo.add("big.md");
      repo.rejects(["big.md"]); // a one-byte edit
      repo.write("big.md", repo.show("MERGE_HEAD", "big.md"));
      repo.rejects(["big.md"]); // unstaged parent content cannot hide staged bytes
      repo.resolveFrom("HEAD");
      // Content inherited at another path is not grandfathered.
      repo.write("other.md", repo.show("MERGE_HEAD", "big.md"));
      repo.add("other.md");
      repo.rejects(["other.md"]);
      repo.notNamed("big.md");
      repo.g("rm", "-q", "--cached", "other.md");
      rmSync(repo.path("other.md"));
      repo.write("big.md", `${base.slice(0, 25001)}main\nfeature\n`);
      repo.add("big.md");
      repo.novelBlob("big.md");
      repo.rejects(["big.md"]); // a manual resolution combining parents
      repo.fill("big.md", 20000);
      repo.add("big.md");
      repo.passes(); // a novel resolution at the exact boundary
      repo.resolveFrom("MERGE_HEAD");
      repo.fill("born-in-merge.md", 20001);
      repo.add("born-in-merge.md");
      repo.rejects(["born-in-merge.md"]);
      repo.notNamed("big.md");
    });
  },
  SLOW,
);

test(
  "a clean merge inherits an oversized blob from the one parent that has it",
  () => {
    scratch((newRepo) => {
      const repo = newRepo("clean-inheritance");
      repo.g("checkout", "-q", "-b", "feature");
      repo.fill("big.md", 25000);
      repo.add("big.md");
      repo.commit("feature");
      repo.g("checkout", "-q", "main");
      repo.append("README.md", "main\n");
      repo.add("README.md");
      repo.commit("main");
      repo.g("merge", "-q", "--no-ff", "--no-commit", "feature");
      repo.sameBlob("big.md", "MERGE_HEAD");
      repo.passes();
      repo.append("big.md", "x");
      repo.add("big.md");
      repo.rejects(["big.md"]);
    });
  },
  SLOW,
);

// Clean merges can create oversized blobs from either legal or large parents.
test.each([
  ["clean-legal", 19970, true],
  ["clean-oversized", 25000, false],
])(
  "%s: a clean automatic combination over budget fails",
  (name, middle, parentsPass) => {
    scratch((newRepo) => {
      const repo = newRepo(name);
      const parent = (): void => (parentsPass ? repo.passes() : repo.rejects(["combined.md"]));
      repo.write("combined.md", `start\n${"x".repeat(middle)}\nend\n`);
      repo.add("combined.md");
      repo.commit("base");
      repo.g("checkout", "-q", "-b", "feature");
      repo.write("combined.md", `feature addition\n${repo.show("HEAD", "combined.md")}`);
      repo.add("combined.md");
      parent();
      repo.commit("feature");
      repo.g("checkout", "-q", "main");
      repo.append("combined.md", "main addition\n");
      repo.add("combined.md");
      parent();
      repo.commit("main");
      repo.g("merge", "-q", "--no-ff", "--no-commit", "feature");
      repo.novelBlob("combined.md");
      repo.rejects(["combined.md"]);
    });
  },
  SLOW,
);

// A mode-only staged change exercises inheritance from HEAD, not MERGE_HEAD.
test(
  "a HEAD blob with only a mode change passes, in a merge and after it",
  () => {
    scratch((newRepo) => {
      const repo = newRepo("head-parent");
      repo.fill("big.md", 25000);
      repo.add("big.md");
      repo.commit("base");
      repo.g("checkout", "-q", "-b", "feature");
      repo.g("rm", "-q", "big.md");
      repo.commit("delete");
      repo.g("checkout", "-q", "main");
      repo.append("big.md", "x");
      repo.add("big.md");
      repo.commit("modify");
      expect(repo.tries("merge", "--no-ff", "--no-commit", "feature")).toBe(false);
      repo.resolveFrom("HEAD");
      repo.g("update-index", "--chmod=+x", "big.md");
      repo.passes();
      repo.g("merge", "--quit");
      repo.passes();
    });
  },
  SLOW,
);

test(
  "only valid full commit IDs in MERGE_HEAD grant inheritance",
  () => {
    scratch((newRepo) => {
      const repo = newRepo("metadata");
      conflictingMerge(repo);
      repo.resolveFrom("MERGE_HEAD");
      const relative = repo.g("rev-parse", "--git-path", "MERGE_HEAD").trim();
      const metadata = isAbsolute(relative) ? relative : join(repo.dir, relative);
      const parent = repo.blob("MERGE_HEAD");
      // Only commit identity matters; a branch ref is not MERGE_HEAD metadata.
      const invalid = ["", "feature", parent.slice(0, 12), "0".repeat(40)];
      invalid.push(repo.blob("MERGE_HEAD:big.md"), repo.blob("MERGE_HEAD^{tree}"));
      for (const entry of invalid) {
        writeFileSync(metadata, `${entry}\n`);
        repo.rejects(["big.md"]);
      }
      writeFileSync(metadata, "");
      repo.rejects(["big.md"]); // empty
      writeFileSync(metadata, `${parent}\ninvalid\n`);
      repo.rejects(["big.md"]); // one invalid entry invalidates all parents
      rmSync(metadata);
      repo.rejects(["big.md"]); // absent
      writeFileSync(metadata, parent);
      repo.passes(); // no final newline
      repo.resolveFrom("MERGE_HEAD");
      repo.g("branch", "-D", "feature");
      repo.passes(); // the parent needs no branch name
    });
  },
  SLOW,
);

test(
  "a linked worktree reads its own MERGE_HEAD",
  () => {
    scratch((newRepo) => {
      const source = newRepo("linked-source");
      conflictingMerge(source);
      source.g("merge", "--abort");
      const dir = join(source.dir, "..", "linked");
      source.g("worktree", "add", "-q", "-b", "linked", dir, "main");
      const repo = new Repo(dir);
      expect(repo.tries("merge", "--no-ff", "--no-commit", "feature")).toBe(false);
      const metadata = repo.g("rev-parse", "--git-path", "MERGE_HEAD").trim();
      expect(
        existsSync(join(dir, ".git")) &&
          existsSync(isAbsolute(metadata) ? metadata : join(dir, metadata)),
      ).toBe(true);
      repo.resolveFrom("MERGE_HEAD");
      repo.passes();
      repo.append("big.md", "x");
      repo.add("big.md");
      repo.rejects(["big.md"]);
      repo.g("merge", "--abort");
      source.g("worktree", "remove", dir);
    });
  },
  SLOW,
);
