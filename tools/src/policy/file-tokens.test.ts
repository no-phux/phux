// The file-token gate on ordinary commits: the byte boundary, the growth
// ratchet, base mode and the exemptions. Merges are in file-tokens-merge.test.ts.
import { expect, test } from "bun:test";
import { mkdirSync } from "node:fs";
import { scratch } from "./file-tokens.fixture.ts";
import { MAX_BYTES, rejection } from "./file-tokens.ts";

const SLOW = 30_000;

test(
  "the boundary is exact and only staged bytes count",
  () => {
    scratch((newRepo) => {
      const repo = newRepo("ordinary");
      repo.fill("big.md", MAX_BYTES);
      repo.add("big.md");
      repo.passes();
      repo.append("big.md", "x");
      repo.passes(); // unstaged oversized bytes do not affect the staged boundary
      repo.add("big.md");
      repo.rejects(["big.md"]);
      repo.fill("big.md", MAX_BYTES);
      repo.rejects(["big.md"]); // an unstaged shrink does not hide staged bytes
      repo.add("big.md");
      repo.commit("boundary");
      repo.append("big.md", "x");
      repo.add("big.md");
      repo.rejects(["big.md"]); // a 20001-byte modification fails
      repo.commit("oversized-fixture");
      repo.append("big.md", "x");
      repo.add("big.md");
      repo.rejects(["big.md"]); // an existing oversized HEAD grants no exemption
    });
  },
  SLOW,
);

test(
  "an oversized file may change without growing, under its old path or a rename",
  () => {
    scratch((newRepo) => {
      const repo = newRepo("ratchet");
      repo.fill("big.md", 25000);
      repo.append("big.md", "keep\n");
      repo.passes(); // nothing staged
      repo.add("big.md");
      repo.rejects(["big.md"]); // a new oversized file
      repo.commit("oversized-fixture");
      repo.write("big.md", `${repo.read("big.md").slice(0, 25000)}edit\n`);
      repo.add("big.md");
      repo.passes(); // a same-size edit
      repo.fill("big.md", 24000);
      repo.add("big.md");
      repo.passes(); // shrinking while still over the limit
      repo.commit("shrink");
      mkdirSync(repo.path("moved"));
      repo.g("mv", "big.md", "moved/renamed.md");
      repo.passes(); // a pure rename
      repo.append("moved/renamed.md", "small edit\n");
      repo.add("moved/renamed.md");
      repo.rejects(["moved/renamed.md"]); // growing through a rename
      repo.g("reset", "-q", "--hard");
      repo.fill("unrelated.md", 30000);
      repo.add("unrelated.md");
      repo.rejects(["unrelated.md"]); // a similar-sized new file borrows no budget
    });
  },
  SLOW,
);

test(
  "base mode checks HEAD against a commit instead of the index",
  () => {
    scratch((newRepo) => {
      const repo = newRepo("base-mode");
      const start = repo.blob("HEAD");
      repo.fill("big.md", 25000);
      repo.add("big.md");
      repo.g("commit", "-q", "--no-verify", "-m", "oversized");
      repo.passes(); // a clean index has nothing to check
      repo.rejects(["big.md"], start);
      repo.passes("HEAD");
    });
  },
  SLOW,
);

test(
  "lockfiles, binaries, generated headers and cached attributes are exempt",
  () => {
    scratch((newRepo) => {
      const repo = newRepo("exemptions");
      const locks = ["Cargo.lock", "bun.lockb", "package-lock.json", "pnpm-lock.yaml"];
      locks.push(
        "dependency.lock.json",
        "go.sum",
        "Package.resolved",
        "npm-shrinkwrap.json",
        "gradle.lockfile",
      );
      for (const file of locks) repo.fill(file, 25000);
      repo.write("binary.dat", `\0${"\0".repeat(25000)}`);
      for (const header of ["@generated", "Code generated", "auto-generated", "DO NOT EDIT"]) {
        repo.write(
          `generated-${header.replaceAll(" ", "-")}.md`,
          `${header}\n${"x".repeat(25000)}`,
        );
      }
      repo.fill("generated.md", 25000);
      repo.fill("vendored.md", 25000);
      repo.write(
        ".gitattributes",
        "generated.md linguist-generated=true\nvendored.md linguist-vendored\n",
      );
      repo.add("-A");
      // Attribute exemptions, like content checks, must read the index.
      repo.write(".gitattributes", "");
      repo.passes();
      repo.commit("exempt-fixtures");
      repo.g("checkout", "-q", "-b", "feature");
      repo.write("feature.md", "feature\n");
      repo.add("feature.md");
      repo.commit("feature");
      repo.g("checkout", "-q", "main");
      repo.append("README.md", "main\n");
      repo.add("README.md");
      repo.commit("main");
      repo.g("merge", "-q", "--no-ff", "--no-commit", "feature");
      // New exempt files, not just unchanged parent blobs, still pass in a merge.
      repo.fill("new.lock", 25000);
      repo.write("new-generated.md", `Code generated\n${"x".repeat(25000)}`);
      repo.add("new.lock", "new-generated.md");
      repo.passes();
    });
  },
  SLOW,
);

test(
  "only the first 2048 bytes can mark a file generated",
  () => {
    scratch((newRepo) => {
      const repo = newRepo("generated-prefix");
      const mebibyte = "x".repeat(1_048_576);
      repo.write("early.md", `Code generated\n${mebibyte}`);
      repo.write("boundary.md", `${"x".repeat(2034)}Code generated${mebibyte}`);
      repo.add("early.md", "boundary.md");
      repo.passes(); // a marker ending at byte 2048 counts
      repo.write("late.md", `${"x".repeat(2048)}Code generated\n${mebibyte}`);
      repo.write("plain.md", mebibyte);
      repo.add("late.md", "plain.md");
      repo.rejects(["late.md", "plain.md"]);
      repo.notNamed("early.md");
      repo.notNamed("boundary.md");
    });
  },
  SLOW,
);

test("the rejection names each file and how to split it", () => {
  const message = rejection(["  big.md  ~5001 tokens (20004 bytes)"]);
  expect(message).toStartWith(
    "pre-commit: 1 staged file(s) are new or grew past the 5000-token limit (tokens = bytes/4, so 20000 bytes):\n  big.md  ~5001 tokens (20004 bytes)\n\nSplit each file",
  );
  expect(message).toEndWith("  path/to/file linguist-generated=true\n");
});
