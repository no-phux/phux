// Keep checkout paths out of compiled library and test code.
//
// env!("CARGO_MANIFEST_DIR") and env!("CARGO_BIN_EXE_*") compile the current
// checkout's absolute path into the crate. A content-addressed build cache
// (mbx, docs/development.md#build-cache-mbx) then keys that crate, and every
// crate that links it, to one checkout, so each new worktree recompiles them
// instead of restoring them: phux-config's one baked path once rebuilt
// phux-server and everything above it in every fresh worktree.
//
// Read the value at run time instead. Tests: `cargo test` and nextest both
// export CARGO_MANIFEST_DIR, and CARGO_BIN_EXE_<name> / NEXTEST_BIN_EXE_<name>
// (crates/phux/tests/common/runner.rs). Binaries: pass the path in from a bin
// target, which nothing links (crates/phux/src/main.rs).
//
// Allowed: bin targets and examples, which are leaves. insta's snapshot macros
// expand env!("CARGO_MANIFEST_DIR") themselves and are not matched here.
import { expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { git } from "../lib/git.ts";
import { ROOT } from "../lib/repo.ts";

/** Tracked lines that compile a checkout path into a library or test. */
function bakedPaths(cwd: string): string[] {
  const result = Bun.spawnSync(
    [
      "git",
      "grep",
      "-n",
      "-E",
      String.raw`env!\("(CARGO_MANIFEST_DIR|CARGO_BIN_EXE_[^"]+)"\)`,
      "--",
      "crates/**/*.rs",
      ":!crates/*/examples/**",
      ":!crates/*/src/main.rs",
      ":!crates/*/src/bin/**",
    ],
    { cwd, stdout: "pipe", stderr: "pipe" },
  );
  // git grep exits 1 for no match; anything else is a failure to look.
  if (result.exitCode === 1) return [];
  if (result.exitCode !== 0) throw new Error(`git grep failed: ${result.stderr.toString()}`);
  return result.stdout.toString().trim().split("\n");
}

const READ_AT_RUN_TIME =
  "these lines compile a checkout path into a library or test; read it at run time instead (see tools/src/policy/cache-portable.test.ts)";

test("no compile-time checkout paths in library or test code", () => {
  expect(bakedPaths(ROOT).map((hit) => `${hit}: ${READ_AT_RUN_TIME}`)).toEqual([]);
});

test("library and test code is refused; bin targets and examples are leaves", () => {
  const root = mkdtempSync(join(tmpdir(), "phux-cache-portable-"));
  try {
    git(["init", "--quiet"], root);
    const baked = 'const DIR: &str = env!("CARGO_MANIFEST_DIR");\n';
    const files = {
      "crates/lib/src/lib.rs": baked,
      "crates/lib/tests/it.rs": 'let exe = env!("CARGO_BIN_EXE_phux");\n',
      "crates/lib/src/main.rs": baked,
      "crates/lib/src/bin/tool.rs": baked,
      "crates/lib/examples/demo.rs": baked,
      "crates/lib/src/runtime.rs": 'let dir = std::env::var("CARGO_MANIFEST_DIR");\n',
    };
    for (const [path, text] of Object.entries(files)) {
      mkdirSync(join(root, dirname(path)), { recursive: true });
      writeFileSync(join(root, path), text);
    }
    git(["add", "."], root);
    expect(bakedPaths(root).map((hit) => hit.split(":")[0])).toEqual([
      "crates/lib/src/lib.rs",
      "crates/lib/tests/it.rs",
    ]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
