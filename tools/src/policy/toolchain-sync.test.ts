import { expect, test } from "bun:test";
import { cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { git } from "../lib/git.ts";
import { ROOT } from "../lib/repo.ts";
import { toolchainSync } from "./toolchain-sync.ts";

const INPUTS = [
  "rust-toolchain.toml",
  ".config/zig-toolchain.json",
  "mise.toml",
  "flake.nix",
  "clippy.toml",
  "tools/package.json",
  "Cargo.toml",
  "clients/*/Cargo.toml",
  "plugins/index/Cargo.toml",
  ".github/workflows/*.yml",
];

/** A copy of just the files the check reads, as a git repository. */
function copy(run: (root: string) => void): void {
  const root = mkdtempSync(join(tmpdir(), "phux-toolchain-sync-"));
  try {
    const files = git(["ls-files", "--", ...INPUTS])
      .split("\n")
      .filter((path) => path !== "");
    for (const file of files) cpSync(join(ROOT, file), join(root, file), { recursive: true });
    git(["init", "--quiet"], root);
    git(["add", "--all"], root);
    run(root);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function edit(root: string, file: string, from: RegExp, to: string): void {
  const path = join(root, file);
  const before = readFileSync(path, "utf8");
  const after = before.replace(from, to);
  expect(after).not.toBe(before);
  writeFileSync(path, after);
}

test("the checkout's toolchain pins agree", () => {
  expect(toolchainSync(ROOT).failures).toEqual([]);
});

test("a drifted pin names the input that disagrees", () => {
  copy((root) => {
    expect(toolchainSync(root).failures).toEqual([]);
    edit(root, "mise.toml", /^bun = "[^"]+"/m, 'bun = "9.9.9"');
    edit(root, "clippy.toml", /msrv = "[^"]+"/, 'msrv = "1.0"');
    edit(root, ".github/workflows/ci.yml", /node-version: \d+/, "node-version: 1");
    const { failures } = toolchainSync(root);
    expect(failures).toContain(
      "toolchain sync: flake.nix bunDigests has no entry for Bun 9.9.9; add the per-system hashes (see the comment there)",
    );
    expect(failures).toContain("toolchain sync: tools/package.json engines.bun must be 9.9.9");
    expect(failures).toContain("toolchain sync: .github/workflows/ci.yml Node version must be 26");
    expect(
      failures.some((failure) => failure.startsWith("toolchain sync: clippy MSRV must be")),
    ).toBe(true);
  });
});
