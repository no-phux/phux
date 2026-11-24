// Keep checkout paths out of compiled library and test code.
//
// env!("CARGO_MANIFEST_DIR") and env!("CARGO_BIN_EXE_*") compile the current
// checkout's absolute path into the crate. A content-addressed build cache
// (mbx, docs/development.md#build-cache-mbx) then keys that crate, and every
// crate that links it, to one checkout, so each new worktree recompiles them
// instead of restoring them: before this gate, phux-config's one baked path
// rebuilt phux-server and everything above it in every fresh worktree.
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
import { ProcessError } from "../lib/process.ts";
import { ROOT } from "../lib/repo.ts";

const BAKED = String.raw`env!\("(CARGO_MANIFEST_DIR|CARGO_BIN_EXE_[^"]+)"\)`;
const LEAVES = [":!crates/*/examples/**", ":!crates/*/src/main.rs", ":!crates/*/src/bin/**"];

/** `path:line:text` for each compile-time checkout path in library or test code. */
function bakedPaths(root: string): string[] {
  try {
    const hits = git(["grep", "-n", "-E", BAKED, "--", "crates/**/*.rs", ...LEAVES], root);
    return hits.split("\n").filter((line) => line !== "");
  } catch (error) {
    // git grep exits 1 when nothing matches.
    if (error instanceof ProcessError && error.stderr === "") return [];
    throw error;
  }
}

test("only bin targets and examples compile a checkout path", () => {
  const root = mkdtempSync(join(tmpdir(), "phux-cache-portable-"));
  try {
    const files: Record<string, string> = {
      "crates/a/src/lib.rs": 'const P: &str = env!("CARGO_MANIFEST_DIR");\n',
      "crates/a/tests/it.rs": 'let bin = env!("CARGO_BIN_EXE_phux");\n',
      "crates/a/src/main.rs": 'let p = env!("CARGO_MANIFEST_DIR");\n',
      "crates/a/src/bin/tool.rs": 'let p = env!("CARGO_MANIFEST_DIR");\n',
      "crates/a/examples/demo.rs": 'let p = env!("CARGO_MANIFEST_DIR");\n',
      "crates/a/src/runtime.rs": 'let p = std::env::var("CARGO_MANIFEST_DIR");\n',
    };
    for (const [path, text] of Object.entries(files)) {
      mkdirSync(join(root, dirname(path)), { recursive: true });
      writeFileSync(join(root, path), text);
    }
    git(["init", "--quiet"], root);
    git(["add", "--all"], root);
    expect(bakedPaths(root).toSorted()).toEqual([
      `crates/a/src/lib.rs:1:${files["crates/a/src/lib.rs"]?.trimEnd()}`,
      `crates/a/tests/it.rs:1:${files["crates/a/tests/it.rs"]?.trimEnd()}`,
    ]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("no library or test code compiles a checkout path (read it at run time)", () => {
  expect(bakedPaths(ROOT)).toEqual([]);
});
